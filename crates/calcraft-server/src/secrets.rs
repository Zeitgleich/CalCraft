//! Random secrets, token hashing, Base32 and TOTP (RFC 6238).
//!
//! Cryptographic primitives (HMAC-SHA1, BLAKE3) come from vetted crates;
//! this module only composes them.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

/// TOTP time step in seconds.
pub const TOTP_STEP: i64 = 30;
/// Accepted clock drift in steps on either side.
const TOTP_DRIFT: i64 = 1;
const TOTP_DIGITS: u32 = 6;

/// Cryptographically secure random bytes.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    // getrandom only fails if the OS has no entropy source at all, in which
    // case no secret can be generated safely.
    #[allow(clippy::expect_used)]
    getrandom::fill(&mut bytes).expect("operating system random source unavailable");
    bytes
}

/// Hex hash of a high-entropy secret (session token, app password,
/// recovery code). Such secrets cannot be brute-forced, so a fast hash
/// is sufficient for storage.
pub fn hash_secret(secret: &str) -> String {
    blake3::hash(secret.as_bytes()).to_hex().to_string()
}

/// Constant-time comparison of two stored hashes.
pub fn hashes_equal(a: &str, b: &str) -> bool {
    match (blake3::Hash::from_hex(a), blake3::Hash::from_hex(b)) {
        // `blake3::Hash` compares in constant time.
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Base32 (RFC 4648) without padding, as used in `otpauth://` URIs.
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(
                BASE32_ALPHABET[((buffer >> bits) & 0x1f) as usize],
            ));
        }
    }
    if bits > 0 {
        out.push(char::from(
            BASE32_ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize],
        ));
    }
    out
}

/// Decodes unpadded Base32 (case-insensitive); `None` on invalid input.
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for c in text.bytes() {
        let value = BASE32_ALPHABET
            .iter()
            .position(|&a| a == c.to_ascii_uppercase())?;
        buffer = (buffer << 5) | u32::try_from(value).ok()?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// Lowercase Base32 for tokens shown to users (no ambiguous characters).
pub fn random_token<const N: usize>() -> String {
    base32_encode(&random_bytes::<N>()).to_ascii_lowercase()
}

/// HOTP value (RFC 4226) for a counter.
fn hotp(secret: &[u8], counter: u64) -> u32 {
    // HMAC accepts keys of any length; this cannot fail.
    #[allow(clippy::expect_used)]
    let mut mac =
        <Hmac<Sha1> as KeyInit>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let code = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    code % 10u32.pow(TOTP_DIGITS)
}

/// The TOTP code for a time step, zero-padded to six digits.
pub fn totp_code(secret: &[u8], step: i64) -> String {
    let counter = u64::try_from(step).unwrap_or(0);
    format!("{:06}", hotp(secret, counter))
}

/// Checks a TOTP code at `now` (Unix seconds), allowing one step of clock
/// drift. Returns the matching time step, which the caller must record to
/// prevent replay.
pub fn totp_verify(secret: &[u8], code: &str, now: i64) -> Option<i64> {
    let code = code.trim();
    if code.len() != TOTP_DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let current = now.div_euclid(TOTP_STEP);
    (current - TOTP_DRIFT..=current + TOTP_DRIFT).find(|&step| {
        step >= 0 && constant_time_eq(totp_code(secret, step).as_bytes(), code.as_bytes())
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `otpauth://` URI for authenticator apps.
pub fn totp_uri(secret: &[u8], username: &str) -> String {
    // The username is restricted to [a-z0-9._-] and needs no escaping.
    format!(
        "otpauth://totp/CalCraft:{username}?secret={}&issuer=CalCraft&algorithm=SHA1&digits=6&period=30",
        base32_encode(secret)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_matches_rfc_4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "MY"),
            ("fo", "MZXQ"),
            ("foo", "MZXW6"),
            ("foob", "MZXW6YQ"),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI"),
        ] {
            assert_eq!(base32_encode(input.as_bytes()), expected, "{input}");
            assert_eq!(
                base32_decode(expected).unwrap(),
                input.as_bytes(),
                "{input}"
            );
        }
        assert_eq!(base32_decode("mzxw6").unwrap(), b"foo");
        assert!(base32_decode("MZ1W").is_none());
    }

    #[test]
    fn totp_matches_rfc_6238_vectors() {
        // RFC 6238 Appendix B, SHA-1 key, last six of the eight digits.
        let secret = b"12345678901234567890";
        for (time, code) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            assert_eq!(totp_code(secret, time / TOTP_STEP), code, "t={time}");
        }
    }

    #[test]
    fn totp_verify_allows_one_step_of_drift() {
        let secret = b"12345678901234567890";
        let now = 1_111_111_111;
        let step = now / TOTP_STEP;
        assert_eq!(totp_verify(secret, "050471", now), Some(step));
        assert_eq!(
            totp_verify(secret, &totp_code(secret, step - 1), now),
            Some(step - 1)
        );
        assert_eq!(
            totp_verify(secret, &totp_code(secret, step + 1), now),
            Some(step + 1)
        );
        assert_eq!(totp_verify(secret, &totp_code(secret, step + 2), now), None);
        for bad in ["", "12345", "1234567", "abcdef", "05047 "] {
            assert_eq!(totp_verify(secret, bad, now), None, "{bad:?}");
        }
    }

    #[test]
    fn hashes_and_tokens() {
        let a = hash_secret("token");
        assert!(hashes_equal(&a, &hash_secret("token")));
        assert!(!hashes_equal(&a, &hash_secret("other")));
        assert!(!hashes_equal("zz", "zz"));
        let token = random_token::<20>();
        assert_eq!(token.len(), 32);
        assert_ne!(token, random_token::<20>());
    }
}
