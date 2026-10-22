//! Single sign-on via OpenID Connect (Authorization Code flow with PKCE).
//!
//! Flow: `begin` creates `state`, `nonce` and a PKCE verifier, keeps them
//! server-side (bound to a random browser cookie, valid for 10 minutes,
//! single use) and returns the authorization URL. `complete` consumes the
//! state, checks the browser binding, exchanges the code at the token
//! endpoint and verifies the ID token. Mapping to local accounts happens in
//! the API layer (`api::oidc`).
//!
//! Provider metadata comes from the issuer's discovery document; its
//! `issuer` must equal the configured one and its endpoints must be https
//! on the issuer's host (see [`http::EndpointPolicy`]). The JWKS is cached
//! for an hour and refetched (at most once a minute) when a token names an
//! unknown key, which covers key rotation.
//!
//! Tokens, codes, secrets and the client secret are never logged.

pub mod http;
pub mod jwt;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use axum::http::{HeaderValue, StatusCode, Uri};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use calcraft_core::{Username, api::OidcMethod};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{
    auth::now,
    config::{OidcConfig, TokenAuthMethod},
    secrets::random_bytes,
    validate::{MAX_DISPLAY_NAME, clean_text},
};
use http::{EndpointPolicy, HttpClient, HttpError};
use jwt::{Alg, IdToken, Jwk, TokenError};

/// Relative URL that starts the login.
pub const LOGIN_PATH: &str = "/api/v1/auth/oidc/login";
/// Cookie that binds a pending login to the browser that started it.
pub const BINDING_COOKIE: &str = "calcraft_oidc";
/// Lifetime of a pending login (state, nonce, PKCE verifier).
pub const PENDING_TTL: Duration = Duration::from_secs(600);
/// Upper bound of concurrently pending logins (memory bound).
const MAX_PENDING: usize = 10_000;
const PROVIDER_TTL: Duration = Duration::from_secs(3600);
const JWKS_TTL: Duration = Duration::from_secs(3600);
/// Minimum time between JWKS fetches triggered by unknown key ids.
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(60);
/// Minimum time between discovery attempts after a failure.
const PROVIDER_RETRY: Duration = Duration::from_secs(10);
const MAX_PARAM_LEN: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("identity provider unavailable: {0}")]
    Unavailable(String),
    #[error("unknown, expired, replayed or foreign login state")]
    InvalidState,
    #[error("identity provider returned an error")]
    IdpError,
    #[error("malformed callback")]
    BadCallback,
    #[error("code exchange failed: {0}")]
    Exchange(String),
    #[error("ID token rejected: {0}")]
    Token(#[from] TokenError),
}

/// Query parameters of the redirect back from the IdP.
#[derive(Default, Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    /// RFC 9207 issuer identification.
    pub iss: Option<String>,
}

impl std::fmt::Debug for CallbackParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |v: &Option<String>| v.as_ref().map(|_| "<redacted>");
        f.debug_struct("CallbackParams")
            .field("code", &redact(&self.code))
            .field("state", &redact(&self.state))
            .field("error", &self.error.as_deref().map(log_safe))
            .field("iss", &self.iss)
            .finish()
    }
}

/// Start of a login: where to send the browser and the binding cookie value.
pub struct LoginStart {
    pub location: String,
    pub binding: String,
}

#[derive(Debug, Clone)]
struct Provider {
    authorization_endpoint: String,
    token_endpoint: Uri,
    jwks_uri: Uri,
}

#[derive(Default)]
struct ProviderCache {
    provider: Option<Arc<Provider>>,
    fetched: Option<Instant>,
    last_failure: Option<Instant>,
}

#[derive(Default)]
struct JwksCache {
    keys: Arc<Vec<Jwk>>,
    fetched: Option<Instant>,
}

struct Pending {
    binding: blake3::Hash,
    nonce: String,
    verifier: String,
    expires: Instant,
}

/// Configured OIDC relying party.
pub struct Oidc {
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
    auth_method: TokenAuthMethod,
    redirect_uri: String,
    scope: String,
    username_claim: String,
    username_strip_suffix: Option<String>,
    admin: Option<(String, String)>,
    required: Option<(String, String)>,
    auto_create_users: bool,
    require_local_totp: bool,
    algs: Vec<Alg>,
    leeway: i64,
    label: String,
    discovery_url: Uri,
    policy: EndpointPolicy,
    http: HttpClient,
    provider: tokio::sync::Mutex<ProviderCache>,
    jwks: tokio::sync::Mutex<JwksCache>,
    pending: Mutex<HashMap<[u8; 32], Pending>>,
    jwks_min_refresh: Duration,
}

impl std::fmt::Debug for Oidc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Oidc")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// `application/x-www-form-urlencoded` encoding of one value.
pub fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn random_b64<const N: usize>() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes::<N>())
}

/// PKCE S256 challenge (RFC 7636).
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ))
}

/// Short, log-safe version of an error code sent by the IdP.
fn log_safe(code: &str) -> String {
    code.chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '?'
            }
        })
        .collect()
}

/// A claim by name, or by dotted path into nested objects
/// (`realm_access.roles`). An exact top-level name wins, so URL-style claim
/// names with dots still work.
pub fn claim_at<'a>(claims: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    if let Some(value) = claims.get(path) {
        return Some(value);
    }
    let mut parts = path.split('.');
    let mut value = claims.get(parts.next()?)?;
    for part in parts {
        value = value.as_object()?.get(part)?;
    }
    Some(value)
}

/// Whether a claim equals `wanted` or is an array containing it.
pub fn claim_contains(claims: &Map<String, Value>, path: &str, wanted: &str) -> bool {
    match claim_at(claims, path) {
        Some(Value::String(s)) => s == wanted,
        Some(Value::Array(list)) => list.iter().any(|v| v.as_str() == Some(wanted)),
        _ => false,
    }
}

impl Oidc {
    /// Builds the relying party from a validated configuration; reads the
    /// client secret file and the trust store. No network access.
    pub fn new(config: &OidcConfig) -> anyhow::Result<Arc<Self>> {
        Self::with_jwks_min_refresh(config, JWKS_MIN_REFRESH)
    }

    /// Like [`Oidc::new`] with another JWKS refetch rate limit (tests).
    #[doc(hidden)]
    pub fn with_jwks_min_refresh(
        config: &OidcConfig,
        jwks_min_refresh: Duration,
    ) -> anyhow::Result<Arc<Self>> {
        config.validate()?;
        let client_secret = match (&config.client_secret, &config.client_secret_file) {
            (Some(secret), _) => Some(secret.expose().to_owned()),
            (None, Some(path)) => {
                let secret = std::fs::read_to_string(path).map_err(|err| {
                    anyhow::anyhow!(
                        "failed to read oidc.client_secret_file {}: {err}",
                        path.display()
                    )
                })?;
                let secret = secret.trim().to_owned();
                anyhow::ensure!(
                    !secret.is_empty(),
                    "oidc.client_secret_file {} is empty",
                    path.display()
                );
                Some(secret)
            }
            (None, None) => None,
        };
        let issuer_uri: Uri = config.issuer.parse()?;
        let insecure = config.allow_insecure_issuer_for_testing;
        let policy = EndpointPolicy::new(&issuer_uri, &config.extra_endpoint_hosts, insecure)
            .ok_or_else(|| anyhow::anyhow!("oidc.issuer has no host"))?;
        let discovery_url: Uri = format!(
            "{}/.well-known/openid-configuration",
            config.issuer.trim_end_matches('/')
        )
        .parse()?;
        if insecure {
            tracing::warn!(
                "oidc.allow_insecure_issuer_for_testing is set: IdP traffic may use plain HTTP; never use this in production"
            );
        }
        let algs = config
            .signing_algs
            .iter()
            .map(|alg| alg.parse())
            .collect::<Result<Vec<Alg>, _>>()
            .map_err(|_| anyhow::anyhow!("unsupported oidc.signing_algs"))?;
        Ok(Arc::new(Self {
            issuer: config.issuer.clone(),
            client_id: config.client_id.clone(),
            client_secret,
            auth_method: config.token_auth_method,
            redirect_uri: config.redirect_uri.clone(),
            scope: config.scopes.join(" "),
            username_claim: config.username_claim.clone(),
            username_strip_suffix: config
                .username_strip_suffix
                .as_ref()
                .map(|s| s.to_ascii_lowercase()),
            admin: config.admin_claim.clone().zip(config.admin_value.clone()),
            required: config
                .required_claim
                .clone()
                .zip(config.required_value.clone()),
            auto_create_users: config.auto_create_users,
            require_local_totp: config.require_local_totp,
            algs,
            leeway: i64::try_from(config.clock_skew_secs.min(60)).unwrap_or(60),
            label: config.button_label.clone(),
            discovery_url,
            policy,
            http: HttpClient::new(config.ca_file.as_deref(), insecure)?,
            provider: tokio::sync::Mutex::default(),
            jwks: tokio::sync::Mutex::default(),
            pending: Mutex::default(),
            jwks_min_refresh,
        }))
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn auto_create_users(&self) -> bool {
        self.auto_create_users
    }

    pub fn require_local_totp(&self) -> bool {
        self.require_local_totp
    }

    /// Login button data for `GET /api/v1/auth/methods`.
    pub fn method(&self) -> OidcMethod {
        OidcMethod {
            label: self.label.clone(),
            login_url: LOGIN_PATH.to_owned(),
        }
    }

    /// Fetches discovery and keys once at startup, so that configuration
    /// problems show up in the log right away.
    pub async fn warm_up(&self) {
        let result = async {
            let provider = self.provider().await?;
            self.keys(&provider, false).await
        }
        .await;
        match result {
            Ok(keys) => {
                tracing::info!(issuer = %self.issuer, keys = keys.len(), "OIDC provider ready")
            }
            Err(err) => {
                tracing::warn!(issuer = %self.issuer, %err, "OIDC provider not reachable yet")
            }
        }
    }

    // --- provider metadata and keys -------------------------------------

    async fn provider(&self) -> Result<Arc<Provider>, OidcError> {
        let mut cache = self.provider.lock().await;
        if let (Some(provider), Some(fetched)) = (&cache.provider, cache.fetched)
            && fetched.elapsed() < PROVIDER_TTL
        {
            return Ok(provider.clone());
        }
        if cache
            .last_failure
            .is_some_and(|at| at.elapsed() < PROVIDER_RETRY)
        {
            return cache
                .provider
                .clone()
                .ok_or_else(|| OidcError::Unavailable("discovery failed recently".to_owned()));
        }
        match self.fetch_provider().await {
            Ok(provider) => {
                let provider = Arc::new(provider);
                cache.provider = Some(provider.clone());
                cache.fetched = Some(Instant::now());
                cache.last_failure = None;
                Ok(provider)
            }
            Err(err) => {
                cache.last_failure = Some(Instant::now());
                tracing::warn!(%err, "OIDC discovery failed");
                // Keep using the previous metadata while the IdP is down.
                cache.provider.clone().ok_or(err)
            }
        }
    }

    async fn fetch_provider(&self) -> Result<Provider, OidcError> {
        let body = self
            .http
            .get(&self.discovery_url)
            .await
            .map_err(|err| OidcError::Unavailable(err.to_string()))?;
        parse_discovery(&body, &self.issuer, &self.policy)
    }

    /// The cached keys; refetched after the TTL, or when `unknown_kid` is
    /// set and the last fetch is older than the refetch limit
    /// ([`JWKS_MIN_REFRESH`]).
    async fn keys(
        &self,
        provider: &Provider,
        unknown_kid: bool,
    ) -> Result<Arc<Vec<Jwk>>, OidcError> {
        let mut cache = self.jwks.lock().await;
        let age = cache.fetched.map(|at| at.elapsed());
        let fresh = age.is_some_and(|age| age < JWKS_TTL);
        let may_refresh = age.is_none_or(|age| age >= self.jwks_min_refresh);
        if (fresh && !unknown_kid) || !may_refresh {
            return Ok(cache.keys.clone());
        }
        let fetched = self
            .http
            .get(&provider.jwks_uri)
            .await
            .map_err(|err| OidcError::Unavailable(format!("JWKS: {err}")))
            .and_then(|body| jwt::parse_jwks(&body).map_err(OidcError::Token));
        // Rate limiting also applies to failed attempts.
        cache.fetched = Some(Instant::now());
        match fetched {
            Ok(keys) => {
                if keys.is_empty() {
                    tracing::warn!("OIDC JWKS contains no usable signing keys");
                }
                cache.keys = Arc::new(keys);
                Ok(cache.keys.clone())
            }
            Err(err) => {
                tracing::warn!(%err, "failed to fetch OIDC JWKS");
                if cache.keys.is_empty() {
                    Err(err)
                } else {
                    Ok(cache.keys.clone())
                }
            }
        }
    }

    // --- flow -----------------------------------------------------------

    /// Starts a login: returns the authorization URL and the value of the
    /// browser binding cookie.
    pub async fn begin(&self) -> Result<LoginStart, OidcError> {
        let provider = self.provider().await?;
        let state = random_b64::<32>();
        let nonce = random_b64::<32>();
        let verifier = random_b64::<32>();
        let binding = random_b64::<32>();
        let challenge = pkce_challenge(&verifier);
        self.remember_pending(
            &state,
            Pending {
                binding: blake3::hash(binding.as_bytes()),
                nonce: nonce.clone(),
                verifier,
                expires: Instant::now() + PENDING_TTL,
            },
        );
        let query = form(&[
            ("response_type", "code"),
            ("client_id", &self.client_id),
            ("redirect_uri", &self.redirect_uri),
            ("scope", &self.scope),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        let separator = if provider.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        Ok(LoginStart {
            location: format!("{}{separator}{query}", provider.authorization_endpoint),
            binding,
        })
    }

    /// Stores a pending login under the hash of its state, within
    /// [`MAX_PENDING`].
    fn remember_pending(&self, state: &str, entry: Pending) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.len() >= MAX_PENDING {
            let now = Instant::now();
            pending.retain(|_, p| p.expires > now);
        }
        // Still full (anyone can start logins): drop the oldest entry
        // rather than refusing all new logins for ten minutes.
        if pending.len() >= MAX_PENDING
            && let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, p)| p.expires)
                .map(|(key, _)| *key)
        {
            pending.remove(&oldest);
        }
        pending.insert(*blake3::hash(state.as_bytes()).as_bytes(), entry);
    }

    /// Consumes the pending login of `state` (single use, also on failure)
    /// and checks that it belongs to this browser.
    fn take_pending(&self, state: &str, binding: Option<&str>) -> Result<Pending, OidcError> {
        if state.len() > MAX_PARAM_LEN {
            return Err(OidcError::InvalidState);
        }
        let entry = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(blake3::hash(state.as_bytes()).as_bytes())
            .ok_or(OidcError::InvalidState)?;
        if entry.expires <= Instant::now() {
            return Err(OidcError::InvalidState);
        }
        let binding = binding.ok_or(OidcError::InvalidState)?;
        // `blake3::Hash` compares in constant time.
        if blake3::hash(binding.as_bytes()) != entry.binding {
            return Err(OidcError::InvalidState);
        }
        Ok(entry)
    }

    /// Finishes a login: validates the callback, redeems the code and
    /// verifies the ID token.
    pub async fn complete(
        &self,
        params: &CallbackParams,
        binding: Option<&str>,
    ) -> Result<IdToken, OidcError> {
        let state = params.state.as_deref().ok_or(OidcError::InvalidState)?;
        let pending = self.take_pending(state, binding)?;
        if let Some(error) = &params.error {
            tracing::info!(error = %log_safe(error), "OIDC login was not completed at the IdP");
            return Err(OidcError::IdpError);
        }
        // RFC 9207: if the IdP names itself, it must be our issuer (mix-up).
        if params.iss.as_deref().is_some_and(|iss| iss != self.issuer) {
            return Err(OidcError::BadCallback);
        }
        let code = params
            .code
            .as_deref()
            .filter(|c| !c.is_empty() && c.len() <= MAX_PARAM_LEN)
            .ok_or(OidcError::BadCallback)?;

        let provider = self.provider().await?;
        let id_token = self.redeem(&provider, code, &pending.verifier).await?;
        self.verify_id_token(&provider, &id_token, &pending.nonce)
            .await
    }

    async fn redeem(
        &self,
        provider: &Provider,
        code: &str,
        verifier: &str,
    ) -> Result<String, OidcError> {
        let mut fields = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("code_verifier", verifier),
        ];
        let mut authorization = None;
        match (self.auth_method, &self.client_secret) {
            (TokenAuthMethod::ClientSecretBasic, Some(secret)) => {
                // RFC 6749 2.3.1: both parts are form-encoded first.
                let credentials =
                    format!("{}:{}", form_encode(&self.client_id), form_encode(secret));
                authorization = Some(
                    HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(credentials)))
                        .map_err(|_| {
                            OidcError::Exchange("invalid client credentials".to_owned())
                        })?,
                );
            }
            (TokenAuthMethod::ClientSecretPost, Some(secret)) => {
                fields.push(("client_id", &self.client_id));
                fields.push(("client_secret", secret));
            }
            _ => fields.push(("client_id", &self.client_id)),
        }
        let (status, body) = self
            .http
            .post_form(&provider.token_endpoint, form(&fields), authorization)
            .await
            .map_err(|err| match err {
                HttpError::NotAllowed(_) => OidcError::Exchange(err.to_string()),
                err => OidcError::Unavailable(format!("token endpoint: {err}")),
            })?;
        let response: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if status != StatusCode::OK {
            let error = response.get("error").and_then(Value::as_str).unwrap_or("-");
            return Err(OidcError::Exchange(format!(
                "status {status}, error {}",
                log_safe(error)
            )));
        }
        response
            .get("id_token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| OidcError::Exchange("no id_token in token response".to_owned()))
    }

    async fn verify_id_token(
        &self,
        provider: &Provider,
        token: &str,
        nonce: &str,
    ) -> Result<IdToken, OidcError> {
        let header = jwt::decode_header(token, &self.algs)?;
        let mut keys = self.keys(provider, false).await?;
        let known = |keys: &[Jwk]| {
            header
                .kid
                .as_ref()
                .is_none_or(|kid| keys.iter().any(|k| k.kid.as_ref() == Some(kid)))
        };
        if !known(&keys) {
            // Key rotation: the IdP may have published a new key.
            keys = self.keys(provider, true).await?;
        }
        let expected = jwt::Expected {
            issuer: &self.issuer,
            client_id: &self.client_id,
            nonce,
            now: now(),
            leeway: self.leeway,
            algs: &self.algs,
        };
        Ok(jwt::verify(token, &keys, &expected)?)
    }

    // --- account mapping helpers ----------------------------------------

    /// The local username from the configured claim: lowercased, optional
    /// suffix removed, and valid by the `Username` rules. Anything else is
    /// rejected rather than rewritten, so two IdP names cannot collapse
    /// into one local account.
    pub fn username(&self, claims: &Map<String, Value>) -> Option<Username> {
        let raw = claim_at(claims, &self.username_claim)?.as_str()?;
        let mut name = raw.trim().to_ascii_lowercase();
        if let Some(suffix) = &self.username_strip_suffix {
            name = name.strip_suffix(suffix.as_str())?.to_owned();
        }
        name.parse().ok()
    }

    /// Whether the user may log in at all (`required_claim`).
    pub fn allowed(&self, claims: &Map<String, Value>) -> bool {
        self.required
            .as_ref()
            .is_none_or(|(claim, value)| claim_contains(claims, claim, value))
    }

    /// Admin rights from the token, if `admin_claim` is configured.
    pub fn admin(&self, claims: &Map<String, Value>) -> Option<bool> {
        self.admin
            .as_ref()
            .map(|(claim, value)| claim_contains(claims, claim, value))
    }

    /// Display name for new accounts (`name` claim).
    pub fn display_name(claims: &Map<String, Value>) -> Option<String> {
        claims
            .get("name")
            .and_then(Value::as_str)
            .and_then(|name| clean_text(name, MAX_DISPLAY_NAME))
            .filter(|name| !name.is_empty())
    }
}

/// Parses and checks the discovery document (OIDC Discovery 4.3: the
/// `issuer` must be identical to the configured one).
fn parse_discovery(
    body: &[u8],
    issuer: &str,
    policy: &EndpointPolicy,
) -> Result<Provider, OidcError> {
    let doc: Value = serde_json::from_slice(body)
        .map_err(|_| OidcError::Unavailable("invalid discovery document".to_owned()))?;
    let field = |name: &str| {
        doc.get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| OidcError::Unavailable(format!("discovery lacks {name}")))
    };
    if field("issuer")? != issuer {
        return Err(OidcError::Unavailable(
            "discovery issuer differs from oidc.issuer".to_owned(),
        ));
    }
    if let Some(methods) = doc
        .get("code_challenge_methods_supported")
        .and_then(Value::as_array)
        && !methods.iter().any(|m| m.as_str() == Some("S256"))
    {
        return Err(OidcError::Unavailable(
            "IdP does not support PKCE S256".to_owned(),
        ));
    }
    let check = |name: &str| {
        let url = field(name)?;
        policy
            .check(url)
            .map_err(|err| OidcError::Unavailable(format!("{name}: {err}")))
    };
    let authorization = check("authorization_endpoint")?;
    Ok(Provider {
        authorization_endpoint: authorization.to_string(),
        token_endpoint: check("token_endpoint")?,
        jwks_uri: check("jwks_uri")?,
    })
}

#[cfg(test)]
mod tests;
