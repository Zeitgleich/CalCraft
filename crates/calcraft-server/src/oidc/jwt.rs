//! ID token (JWS compact serialisation) verification.
//!
//! Only the composition lives here; signature checks use `ring`, the same
//! crypto library that backs the TLS client. Accepted algorithms come from
//! a fixed allow-list: `none` and the HMAC algorithms are never accepted,
//! so a token cannot downgrade to an unsigned or secret-keyed signature.

use std::{fmt, str::FromStr};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde_json::{Map, Value};

/// Longest accepted ID token.
pub const MAX_TOKEN_LEN: usize = 16 * 1024;
/// At most this many keys of a JWKS are used.
pub const MAX_KEYS: usize = 64;
const MAX_SUBJECT_LEN: usize = 255;

/// Signature algorithms CalCraft can verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    Rs256,
    Rs384,
    Rs512,
    Ps256,
    Ps384,
    Ps512,
    Es256,
    Es384,
}

impl FromStr for Alg {
    type Err = TokenError;

    fn from_str(alg: &str) -> Result<Self, Self::Err> {
        Ok(match alg {
            "RS256" => Self::Rs256,
            "RS384" => Self::Rs384,
            "RS512" => Self::Rs512,
            "PS256" => Self::Ps256,
            "PS384" => Self::Ps384,
            "PS512" => Self::Ps512,
            "ES256" => Self::Es256,
            "ES384" => Self::Es384,
            // Includes "none" and HS256/384/512.
            _ => return Err(TokenError::UnsupportedAlg),
        })
    }
}

impl Alg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rs256 => "RS256",
            Self::Rs384 => "RS384",
            Self::Rs512 => "RS512",
            Self::Ps256 => "PS256",
            Self::Ps384 => "PS384",
            Self::Ps512 => "PS512",
            Self::Es256 => "ES256",
            Self::Es384 => "ES384",
        }
    }
}

/// A public key from the IdP's JWKS.
#[derive(Clone, PartialEq, Eq)]
pub struct Jwk {
    pub kid: Option<String>,
    /// `alg` of the key, if the JWKS restricts it.
    pub alg: Option<String>,
    pub key: PublicKey,
}

impl fmt::Debug for Jwk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Jwk")
            .field("kid", &self.kid)
            .field("alg", &self.alg)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum PublicKey {
    /// Modulus and exponent, big-endian without leading zeros.
    Rsa {
        n: Vec<u8>,
        e: Vec<u8>,
    },
    /// Uncompressed point `04 || x || y`.
    P256(Vec<u8>),
    P384(Vec<u8>),
}

impl PublicKey {
    fn fits(&self, alg: Alg) -> bool {
        match self {
            Self::Rsa { .. } => matches!(
                alg,
                Alg::Rs256 | Alg::Rs384 | Alg::Rs512 | Alg::Ps256 | Alg::Ps384 | Alg::Ps512
            ),
            Self::P256(_) => alg == Alg::Es256,
            Self::P384(_) => alg == Alg::Es384,
        }
    }

    fn verify(&self, alg: Alg, message: &[u8], sig: &[u8]) -> bool {
        match self {
            Self::Rsa { n, e } => {
                let params: &signature::RsaParameters = match alg {
                    Alg::Rs256 => &signature::RSA_PKCS1_2048_8192_SHA256,
                    Alg::Rs384 => &signature::RSA_PKCS1_2048_8192_SHA384,
                    Alg::Rs512 => &signature::RSA_PKCS1_2048_8192_SHA512,
                    Alg::Ps256 => &signature::RSA_PSS_2048_8192_SHA256,
                    Alg::Ps384 => &signature::RSA_PSS_2048_8192_SHA384,
                    Alg::Ps512 => &signature::RSA_PSS_2048_8192_SHA512,
                    Alg::Es256 | Alg::Es384 => return false,
                };
                RsaPublicKeyComponents { n, e }
                    .verify(params, message, sig)
                    .is_ok()
            }
            Self::P256(point) if alg == Alg::Es256 => {
                UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                    .verify(message, sig)
                    .is_ok()
            }
            Self::P384(point) if alg == Alg::Es384 => {
                UnparsedPublicKey::new(&signature::ECDSA_P384_SHA384_FIXED, point)
                    .verify(message, sig)
                    .is_ok()
            }
            _ => false,
        }
    }
}

/// Why a token was rejected. Never contains token contents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("malformed token")]
    Malformed,
    #[error("unsupported or disallowed signature algorithm")]
    UnsupportedAlg,
    #[error("no matching signing key")]
    UnknownKey,
    #[error("invalid signature")]
    BadSignature,
    #[error("wrong issuer")]
    Issuer,
    #[error("wrong audience")]
    Audience,
    #[error("token expired")]
    Expired,
    #[error("token not yet valid")]
    NotYetValid,
    #[error("nonce mismatch")]
    Nonce,
    #[error("missing or invalid claim {0}")]
    Claim(&'static str),
}

fn b64(part: &str) -> Result<Vec<u8>, TokenError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| TokenError::Malformed)
}

fn strip_leading_zeros(bytes: &[u8]) -> Vec<u8> {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    bytes[start..].to_vec()
}

fn str_field<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    object.get(name).and_then(Value::as_str)
}

/// Parses a JWKS document. Keys that CalCraft cannot use (other types,
/// encryption keys, malformed entries) are skipped.
pub fn parse_jwks(json: &[u8]) -> Result<Vec<Jwk>, TokenError> {
    let doc: Value = serde_json::from_slice(json).map_err(|_| TokenError::Malformed)?;
    let keys = doc
        .get("keys")
        .and_then(Value::as_array)
        .ok_or(TokenError::Malformed)?;
    Ok(keys
        .iter()
        .filter_map(Value::as_object)
        .filter_map(parse_jwk)
        .take(MAX_KEYS)
        .collect())
}

fn parse_jwk(object: &Map<String, Value>) -> Option<Jwk> {
    if str_field(object, "use").is_some_and(|u| u != "sig") {
        return None;
    }
    if let Some(ops) = object.get("key_ops") {
        let verify = ops
            .as_array()
            .is_some_and(|ops| ops.iter().any(|op| op.as_str() == Some("verify")));
        if !verify {
            return None;
        }
    }
    let key = match str_field(object, "kty")? {
        "RSA" => {
            let n = strip_leading_zeros(&b64(str_field(object, "n")?).ok()?);
            let e = strip_leading_zeros(&b64(str_field(object, "e")?).ok()?);
            if n.is_empty() || e.is_empty() {
                return None;
            }
            PublicKey::Rsa { n, e }
        }
        "EC" => {
            let (len, wrap): (usize, fn(Vec<u8>) -> PublicKey) = match str_field(object, "crv")? {
                "P-256" => (32, PublicKey::P256),
                "P-384" => (48, PublicKey::P384),
                _ => return None,
            };
            let x = b64(str_field(object, "x")?).ok()?;
            let y = b64(str_field(object, "y")?).ok()?;
            if x.len() != len || y.len() != len {
                return None;
            }
            let mut point = Vec::with_capacity(1 + 2 * len);
            point.push(0x04);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            wrap(point)
        }
        _ => return None,
    };
    Some(Jwk {
        kid: str_field(object, "kid").map(str::to_owned),
        alg: str_field(object, "alg").map(str::to_owned),
        key,
    })
}

/// The JOSE header fields CalCraft looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub alg: Alg,
    pub kid: Option<String>,
}

/// Decodes the header without verifying anything, to select the key.
pub fn decode_header(token: &str, allowed: &[Alg]) -> Result<Header, TokenError> {
    if token.len() > MAX_TOKEN_LEN {
        return Err(TokenError::Malformed);
    }
    let (header, _) = token.split_once('.').ok_or(TokenError::Malformed)?;
    let header: Value = serde_json::from_slice(&b64(header)?).map_err(|_| TokenError::Malformed)?;
    let header = header.as_object().ok_or(TokenError::Malformed)?;
    // Extensions we do not understand must not be ignored (RFC 7515 4.1.11).
    if header.contains_key("crit") {
        return Err(TokenError::Malformed);
    }
    let alg: Alg = str_field(header, "alg")
        .ok_or(TokenError::Malformed)?
        .parse()?;
    if !allowed.contains(&alg) {
        return Err(TokenError::UnsupportedAlg);
    }
    Ok(Header {
        alg,
        kid: str_field(header, "kid").map(str::to_owned),
    })
}

/// What a valid ID token must contain.
#[derive(Debug, Clone)]
pub struct Expected<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    /// The nonce sent in the authentication request.
    pub nonce: &'a str,
    /// Current time, Unix seconds.
    pub now: i64,
    /// Clock skew leeway in seconds (at most 60).
    pub leeway: i64,
    pub algs: &'a [Alg],
}

/// Claims of a verified ID token.
#[derive(Debug, Clone)]
pub struct IdToken {
    pub subject: String,
    pub claims: Map<String, Value>,
}

/// Constant-time string comparison (via BLAKE3, whose `Hash` compares in
/// constant time), so that secrets do not leak through timing.
pub fn ct_eq(a: &str, b: &str) -> bool {
    blake3::hash(a.as_bytes()) == blake3::hash(b.as_bytes())
}

fn time_claim(claims: &Map<String, Value>, name: &'static str) -> Result<Option<i64>, TokenError> {
    match claims.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_i64()
            // Fractional seconds are allowed by RFC 7519 (NumericDate).
            .or_else(|| value.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
            .map(Some)
            .ok_or(TokenError::Claim(name)),
    }
}

/// Verifies signature and claims of an ID token (OIDC Core 3.1.3.7).
pub fn verify(token: &str, keys: &[Jwk], expected: &Expected<'_>) -> Result<IdToken, TokenError> {
    let header = decode_header(token, expected.algs)?;
    let mut parts = token.split('.');
    let (Some(header_b64), Some(payload_b64), Some(sig_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed);
    };
    let signature = b64(sig_b64)?;
    let signed = &token[..header_b64.len() + 1 + payload_b64.len()];

    let mut candidates = keys
        .iter()
        .filter(|jwk| header.kid.is_none() || jwk.kid == header.kid)
        .filter(|jwk| jwk.key.fits(header.alg))
        .filter(|jwk| jwk.alg.as_deref().is_none_or(|a| a == header.alg.as_str()))
        .peekable();
    if candidates.peek().is_none() {
        return Err(TokenError::UnknownKey);
    }
    if !candidates.any(|jwk| jwk.key.verify(header.alg, signed.as_bytes(), &signature)) {
        return Err(TokenError::BadSignature);
    }

    // Only verified data is parsed from here on.
    let payload: Value =
        serde_json::from_slice(&b64(payload_b64)?).map_err(|_| TokenError::Malformed)?;
    let Value::Object(claims) = payload else {
        return Err(TokenError::Malformed);
    };

    if str_field(&claims, "iss") != Some(expected.issuer) {
        return Err(TokenError::Issuer);
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::String(aud)) => vec![aud.as_str()],
        Some(Value::Array(list)) => list
            .iter()
            .map(|v| v.as_str().ok_or(TokenError::Audience))
            .collect::<Result<_, _>>()?,
        _ => return Err(TokenError::Audience),
    };
    if !audiences.contains(&expected.client_id) {
        return Err(TokenError::Audience);
    }
    // With several audiences the authorized party must be us; if present
    // it must always be us.
    match str_field(&claims, "azp") {
        Some(azp) if azp != expected.client_id => return Err(TokenError::Audience),
        None if audiences.len() > 1 => return Err(TokenError::Audience),
        _ => {}
    }

    let now = expected.now;
    let leeway = expected.leeway;
    let exp = time_claim(&claims, "exp")?.ok_or(TokenError::Claim("exp"))?;
    if exp.saturating_add(leeway) <= now {
        return Err(TokenError::Expired);
    }
    let iat = time_claim(&claims, "iat")?.ok_or(TokenError::Claim("iat"))?;
    if iat > now.saturating_add(leeway) {
        return Err(TokenError::NotYetValid);
    }
    if let Some(nbf) = time_claim(&claims, "nbf")?
        && nbf > now.saturating_add(leeway)
    {
        return Err(TokenError::NotYetValid);
    }

    match str_field(&claims, "nonce") {
        Some(nonce) if ct_eq(nonce, expected.nonce) => {}
        _ => return Err(TokenError::Nonce),
    }

    let subject = str_field(&claims, "sub")
        .filter(|s| !s.is_empty() && s.len() <= MAX_SUBJECT_LEN && !s.chars().any(char::is_control))
        .ok_or(TokenError::Claim("sub"))?
        .to_owned();
    Ok(IdToken { subject, claims })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{
            ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair, RSA_PKCS1_SHA256, RsaKeyPair,
        },
    };
    use serde_json::json;

    /// 2048-bit RSA test key (PKCS#1 DER), generated only for these tests.
    const RSA_DER: &[u8] = include_bytes!("testdata/rsa2048.der");

    pub(crate) enum Signer {
        Rsa(RsaKeyPair),
        Ec(EcdsaKeyPair),
    }

    impl Signer {
        pub(crate) fn rsa() -> Self {
            Self::Rsa(RsaKeyPair::from_der(RSA_DER).unwrap())
        }

        pub(crate) fn ec() -> Self {
            let rng = SystemRandom::new();
            let pkcs8 =
                EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
            Self::Ec(
                EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                    .unwrap(),
            )
        }

        pub(crate) fn alg(&self) -> &'static str {
            match self {
                Self::Rsa(_) => "RS256",
                Self::Ec(_) => "ES256",
            }
        }

        /// The public key as JWK JSON.
        pub(crate) fn jwk(&self, kid: &str) -> Value {
            match self {
                Self::Rsa(pair) => {
                    let (n, e) = rsa_components(pair.public_key().as_ref());
                    json!({"kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256",
                           "n": URL_SAFE_NO_PAD.encode(n), "e": URL_SAFE_NO_PAD.encode(e)})
                }
                Self::Ec(pair) => {
                    let point = pair.public_key().as_ref();
                    json!({"kty": "EC", "kid": kid, "crv": "P-256",
                           "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
                           "y": URL_SAFE_NO_PAD.encode(&point[33..65])})
                }
            }
        }

        pub(crate) fn sign(&self, header: &Value, claims: &Value) -> String {
            let input = format!(
                "{}.{}",
                URL_SAFE_NO_PAD.encode(header.to_string()),
                URL_SAFE_NO_PAD.encode(claims.to_string())
            );
            let rng = SystemRandom::new();
            let sig = match self {
                Self::Rsa(pair) => {
                    let mut sig = vec![0; pair.public().modulus_len()];
                    pair.sign(&RSA_PKCS1_SHA256, &rng, input.as_bytes(), &mut sig)
                        .unwrap();
                    sig
                }
                Self::Ec(pair) => pair.sign(&rng, input.as_bytes()).unwrap().as_ref().to_vec(),
            };
            format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig))
        }
    }

    /// Splits a DER `RSAPublicKey` (SEQUENCE { n INTEGER, e INTEGER }).
    fn rsa_components(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
        fn read(der: &[u8]) -> (u8, &[u8], &[u8]) {
            let tag = der[0];
            let (len, rest) = if der[1] < 0x80 {
                (usize::from(der[1]), &der[2..])
            } else {
                let count = usize::from(der[1] & 0x7f);
                let len = der[2..2 + count]
                    .iter()
                    .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
                (len, &der[2 + count..])
            };
            (tag, &rest[..len], &rest[len..])
        }
        let (_, seq, _) = read(der);
        let (_, n, rest) = read(seq);
        let (_, e, _) = read(rest);
        (strip_leading_zeros(n), strip_leading_zeros(e))
    }

    pub(crate) const ISS: &str = "https://idp.example/realms/main";
    const CLIENT: &str = "calcraft";
    const NONCE: &str = "n-0S6_WzA2Mj";
    const NOW: i64 = 1_800_000_000;
    const ALGS: &[Alg] = &[Alg::Rs256, Alg::Es256];

    fn claims() -> Value {
        json!({"iss": ISS, "aud": CLIENT, "sub": "248289761001", "nonce": NONCE,
               "iat": NOW - 5, "exp": NOW + 300, "preferred_username": "alice"})
    }

    fn expected() -> Expected<'static> {
        Expected {
            issuer: ISS,
            client_id: CLIENT,
            nonce: NONCE,
            now: NOW,
            leeway: 30,
            algs: ALGS,
        }
    }

    fn keys(signer: &Signer) -> Vec<Jwk> {
        parse_jwks(json!({"keys": [signer.jwk("k1")]}).to_string().as_bytes()).unwrap()
    }

    fn check(signer: &Signer, claims: &Value) -> Result<IdToken, TokenError> {
        let token = signer.sign(&json!({"alg": signer.alg(), "kid": "k1"}), claims);
        verify(&token, &keys(signer), &expected())
    }

    fn with(changes: Value) -> Value {
        let mut claims = claims();
        for (key, value) in changes.as_object().unwrap() {
            if value.is_null() {
                claims.as_object_mut().unwrap().remove(key);
            } else {
                claims[key] = value.clone();
            }
        }
        claims
    }

    #[test]
    fn valid_tokens_verify() {
        for signer in [Signer::rsa(), Signer::ec()] {
            let token = check(&signer, &claims()).unwrap();
            assert_eq!(token.subject, "248289761001");
            assert_eq!(token.claims["preferred_username"], "alice");
            // Audience list with matching azp.
            check(
                &signer,
                &with(json!({"aud": [CLIENT, "other"], "azp": CLIENT})),
            )
            .unwrap();
            // Within the leeway.
            check(&signer, &with(json!({"exp": NOW - 10, "iat": NOW + 10}))).unwrap();
        }
    }

    #[test]
    fn wrong_claims_are_rejected() {
        for signer in [Signer::rsa(), Signer::ec()] {
            let cases = [
                (json!({"iss": "https://evil.example"}), TokenError::Issuer),
                (json!({"iss": format!("{ISS}/")}), TokenError::Issuer),
                (json!({"iss": null}), TokenError::Issuer),
                (json!({"aud": "other"}), TokenError::Audience),
                (json!({"aud": [CLIENT, "other"]}), TokenError::Audience),
                (json!({"aud": CLIENT, "azp": "other"}), TokenError::Audience),
                (json!({"aud": null}), TokenError::Audience),
                (json!({"nonce": "other"}), TokenError::Nonce),
                (json!({"nonce": null}), TokenError::Nonce),
                (json!({"exp": NOW - 31}), TokenError::Expired),
                (json!({"exp": null}), TokenError::Claim("exp")),
                (json!({"exp": "soon"}), TokenError::Claim("exp")),
                (json!({"iat": NOW + 61}), TokenError::NotYetValid),
                (json!({"nbf": NOW + 61}), TokenError::NotYetValid),
                (json!({"iat": null}), TokenError::Claim("iat")),
                (json!({"sub": ""}), TokenError::Claim("sub")),
                (json!({"sub": null}), TokenError::Claim("sub")),
            ];
            for (change, error) in cases {
                assert_eq!(
                    check(&signer, &with(change.clone())).unwrap_err(),
                    error,
                    "{change}"
                );
            }
        }
    }

    #[test]
    fn signature_and_algorithm_attacks_are_rejected() {
        let rsa = Signer::rsa();
        let token = rsa.sign(&json!({"alg": "RS256", "kid": "k1"}), &claims());
        let keys = keys(&rsa);

        // alg=none with an empty signature.
        let mut parts: Vec<&str> = token.split('.').collect();
        let none_header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","kid":"k1"}"#);
        parts[0] = &none_header;
        let none = format!("{}.{}.", parts[0], parts[1]);
        assert_eq!(
            verify(&none, &keys, &expected()).unwrap_err(),
            TokenError::UnsupportedAlg
        );

        // HS256 "signed" with the public key (key confusion).
        let hs_header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","kid":"k1"}"#);
        let hs = format!("{hs_header}.{}.{}", parts[1], parts[2]);
        assert_eq!(
            verify(&hs, &keys, &expected()).unwrap_err(),
            TokenError::UnsupportedAlg
        );

        // Allowed by the library but not configured.
        let rs512 = rsa.sign(&json!({"alg": "RS512", "kid": "k1"}), &claims());
        assert_eq!(
            verify(&rs512, &keys, &expected()).unwrap_err(),
            TokenError::UnsupportedAlg
        );

        // Unknown kid.
        let other_kid = rsa.sign(&json!({"alg": "RS256", "kid": "k2"}), &claims());
        assert_eq!(
            verify(&other_kid, &keys, &expected()).unwrap_err(),
            TokenError::UnknownKey
        );

        // ES256 header but only an RSA key.
        let ec = Signer::ec().sign(&json!({"alg": "ES256", "kid": "k1"}), &claims());
        assert_eq!(
            verify(&ec, &keys, &expected()).unwrap_err(),
            TokenError::UnknownKey
        );

        // Signed by another EC key with the same kid.
        let ec_keys = self::keys(&Signer::ec());
        assert_eq!(
            verify(&ec, &ec_keys, &expected()).unwrap_err(),
            TokenError::BadSignature
        );

        // Tampered payload.
        let evil = URL_SAFE_NO_PAD.encode(with(json!({"sub": "admin"})).to_string());
        let tampered = format!("{}.{evil}.{}", token.split('.').next().unwrap(), parts[2]);
        assert_eq!(
            verify(&tampered, &keys, &expected()).unwrap_err(),
            TokenError::BadSignature
        );

        // Structural garbage.
        for bad in [
            "",
            "a.b",
            "a.b.c.d",
            &format!("{token}="),
            &format!("{token}.x"),
        ] {
            assert!(verify(bad, &keys, &expected()).is_err(), "{bad}");
        }
        let crit = rsa.sign(
            &json!({"alg": "RS256", "kid": "k1", "crit": ["exp"]}),
            &claims(),
        );
        assert_eq!(
            verify(&crit, &keys, &expected()).unwrap_err(),
            TokenError::Malformed
        );
        assert_eq!(
            verify(&"a".repeat(MAX_TOKEN_LEN + 1), &keys, &expected()).unwrap_err(),
            TokenError::Malformed
        );
    }

    #[test]
    fn key_selection() {
        let rsa = Signer::rsa();
        // Without kid, any matching key is tried.
        let token = rsa.sign(&json!({"alg": "RS256"}), &claims());
        let mut jwks = vec![Signer::ec().jwk("e1"), rsa.jwk("k1")];
        let keys = parse_jwks(json!({ "keys": jwks }).to_string().as_bytes()).unwrap();
        verify(&token, &keys, &expected()).unwrap();

        // Encryption keys and keys restricted to another alg are not used.
        let mut enc = rsa.jwk("k1");
        enc["use"] = json!("enc");
        let mut other_alg = rsa.jwk("k1");
        other_alg["alg"] = json!("PS256");
        jwks = vec![enc, other_alg, json!({"kty": "oct", "k": "c2VjcmV0"})];
        let keys = parse_jwks(json!({ "keys": jwks }).to_string().as_bytes()).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(
            verify(&token, &keys, &expected()).unwrap_err(),
            TokenError::UnknownKey
        );

        assert!(parse_jwks(b"{}").is_err());
        assert!(parse_jwks(b"not json").is_err());
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }
}
