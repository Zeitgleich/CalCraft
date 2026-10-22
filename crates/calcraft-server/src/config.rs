//! Server configuration.
//!
//! Sources, in increasing priority: built-in defaults, TOML file, environment
//! variables (`CALCRAFT_` prefix, `__` as section separator) and CLI flags.
//! Unknown keys are rejected so that typos in security-relevant settings do
//! not go unnoticed.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

use crate::net::Cidr;

/// Environment variable prefix for configuration overrides.
pub const ENV_PREFIX: &str = "CALCRAFT_";

/// Complete server configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub security: SecurityConfig,
    pub web_ui: WebUiConfig,
    pub api: ApiConfig,
    pub oidc: OidcConfig,
    pub updates: UpdatesConfig,
    pub subscriptions: SubscriptionsConfig,
    pub scheduling: SchedulingConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Address to listen on. Defaults to loopback only.
    pub bind: SocketAddr,
    /// Maximum accepted request body size in bytes.
    pub max_body_bytes: usize,
    /// Maximum time to handle a single request, in seconds.
    pub request_timeout_secs: u64,
    /// Requests handled at the same time; further requests get `503`
    /// immediately (`/healthz` and `/readyz` are exempt).
    pub max_concurrent_requests: usize,
    /// Requests of one client handled at the same time; further requests
    /// get `429`.
    pub max_concurrent_requests_per_client: u32,
    /// Requests one client may send in a burst (token bucket size).
    pub client_rate_burst: u32,
    /// Sustained requests per second and client (token bucket refill).
    pub client_rate_per_second: u32,
    /// Failed logins per client within 15 minutes; afterwards every login
    /// attempt of that client fails like a wrong password until the window
    /// ends. Applies in addition to the per-account lockout.
    pub max_login_failures_per_client: u32,
    /// IPv6 clients are grouped by network of this prefix length, because
    /// one host usually controls a whole /64.
    pub client_ipv6_prefix: u8,
    /// Most clients tracked at once for the per-client limits; idle and
    /// then least recently seen entries are evicted beyond that.
    pub max_tracked_clients: usize,
    /// Reverse proxies (CIDR ranges) whose `X-Forwarded-For` header is used
    /// to determine the client address. Default: loopback, for a reverse
    /// proxy on the same host (only local processes can connect from
    /// there). Empty: the header is never used.
    pub trusted_proxies: Vec<Cidr>,
}

/// Bounds of `server.client_ipv6_prefix`.
pub const CLIENT_IPV6_PREFIX: std::ops::RangeInclusive<u8> = 32..=128;

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 5232)),
            max_body_bytes: 10 * 1024 * 1024,
            request_timeout_secs: 30,
            max_concurrent_requests: 256,
            max_concurrent_requests_per_client: 32,
            client_rate_burst: 120,
            client_rate_per_second: 20,
            max_login_failures_per_client: 20,
            client_ipv6_prefix: 64,
            max_tracked_clients: 10_000,
            trusted_proxies: Cidr::loopback(),
        }
    }
}

impl ServerConfig {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Path of the SQLite database file. Created on first start.
    pub database: PathBuf,
    /// Back up the database next to it before applying migrations
    /// (`<database>.pre-migration-<version>-<timestamp>`).
    pub pre_migration_backup: bool,
    /// Number of pre-migration backups to keep; older ones are deleted.
    pub pre_migration_backups_keep: usize,
    /// Days deleted objects and collections stay in the trash before they
    /// are purged; 0 disables the trash (deletions are final, entries left
    /// in the trash are purged).
    pub trash_retention_days: u32,
    /// Largest accepted vCard in bytes (`CARDDAV:max-resource-size`),
    /// including embedded photos. At most [`MAX_VCARD_BYTES`] and
    /// `server.max_body_bytes`. Unset: [`DEFAULT_MAX_VCARD_BYTES`], or
    /// `server.max_body_bytes` if that is smaller (see
    /// [`Config::max_vcard_bytes`]). Calendar objects are limited to 1 MiB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_vcard_bytes: Option<usize>,
}

/// Largest accepted `storage.trash_retention_days`.
pub const MAX_TRASH_RETENTION_DAYS: u32 = 3650;

/// Default of `storage.max_vcard_bytes`: room for a high-resolution
/// contact photo (base64 adds a third).
pub const DEFAULT_MAX_VCARD_BYTES: usize = 2 * 1024 * 1024;
/// Upper bound of `storage.max_vcard_bytes`.
pub const MAX_VCARD_BYTES: usize = calcraft_ical::MAX_VCARD_SIZE;
/// Lower bound of `storage.max_vcard_bytes`.
pub const MIN_VCARD_BYTES: usize = 64 * 1024;

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            database: PathBuf::from("calcraft.db"),
            pre_migration_backup: true,
            pre_migration_backups_keep: 3,
            trash_retention_days: calcraft_store::DEFAULT_TRASH_RETENTION_DAYS,
            max_vcard_bytes: None,
        }
    }
}

impl StorageConfig {
    pub fn store_options(&self) -> calcraft_store::StoreOptions {
        calcraft_store::StoreOptions {
            pre_migration_backup: self.pre_migration_backup,
            pre_migration_backups_keep: self.pre_migration_backups_keep,
            trash_retention_days: self.trash_retention_days,
            ..calcraft_store::StoreOptions::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    /// Send `Strict-Transport-Security`. Enable only when served over HTTPS.
    pub hsts: bool,
    /// Mark session cookies `Secure`. Browsers also accept them on
    /// `http://localhost`; disable only for plain-HTTP tests on other hosts.
    pub secure_cookies: bool,
    /// Allow logging in with the account password (REST API login and DAV
    /// Basic auth). `false` means SSO only; app passwords keep working.
    /// Requires `[oidc]` to be enabled.
    pub password_login: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            hsts: false,
            secure_cookies: true,
            password_login: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebUiConfig {
    pub enabled: bool,
}

impl Default for WebUiConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    pub enabled: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Path of the OIDC callback; `oidc.redirect_uri` must end with it.
pub const OIDC_CALLBACK_PATH: &str = "/api/v1/auth/oidc/callback";

/// ID token signature algorithms CalCraft can verify. `none` and the
/// HMAC algorithms (`HS*`) are never accepted.
pub const OIDC_SUPPORTED_ALGS: &[&str] = &[
    "RS256", "RS384", "RS512", "PS256", "PS384", "PS512", "ES256", "ES384",
];

/// Largest accepted clock skew for ID token time claims.
pub const OIDC_MAX_CLOCK_SKEW_SECS: u64 = 60;

/// A secret from the configuration; never printed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// How CalCraft authenticates at the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenAuthMethod {
    /// HTTP Basic with client id and secret (OIDC default).
    ClientSecretBasic,
    /// Client id and secret in the form body.
    ClientSecretPost,
    /// Public client without secret (PKCE only).
    None,
}

/// Single sign-on via OpenID Connect (Authorization Code flow with PKCE).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OidcConfig {
    pub enabled: bool,
    /// Issuer URL exactly as in the ID token's `iss`, e.g.
    /// `https://keycloak.example.com/realms/main`. Discovery is fetched from
    /// `<issuer>/.well-known/openid-configuration`.
    pub issuer: String,
    pub client_id: String,
    /// Client secret; prefer `client_secret_file` or the environment
    /// variable `CALCRAFT_OIDC__CLIENT_SECRET`.
    pub client_secret: Option<Secret>,
    /// File containing the client secret (surrounding whitespace is ignored).
    pub client_secret_file: Option<PathBuf>,
    pub token_auth_method: TokenAuthMethod,
    /// Redirect URI registered at the IdP, matched exactly:
    /// `<public base URL>/api/v1/auth/oidc/callback`.
    pub redirect_uri: String,
    /// Requested scopes; must contain `openid`.
    pub scopes: Vec<String>,
    /// ID token claim used as local username.
    pub username_claim: String,
    /// Suffix removed from the username claim, e.g. `@example.com` for
    /// UPN-style names. Claims without this suffix are rejected.
    pub username_strip_suffix: Option<String>,
    /// Claim (dotted path, e.g. `groups` or `realm_access.roles`) that
    /// decides admin rights together with `admin_value`. If set, admin
    /// rights are granted and revoked on every SSO login.
    pub admin_claim: Option<String>,
    pub admin_value: Option<String>,
    /// If set, only users whose claim (dotted path) contains
    /// `required_value` may log in via SSO.
    pub required_claim: Option<String>,
    pub required_value: Option<String>,
    /// Create unknown users on their first SSO login. Without this, an
    /// administrator has to create the account first.
    pub auto_create_users: bool,
    /// Refuse SSO logins of accounts that have local two-factor
    /// authentication enabled. Default: SSO relies on the IdP's MFA and does
    /// not ask for the local TOTP code.
    pub require_local_totp: bool,
    /// Accepted ID token signature algorithms.
    pub signing_algs: Vec<String>,
    /// Allowed clock skew for `exp`, `iat` and `nbf` (at most 60 s).
    pub clock_skew_secs: u64,
    /// Additional hosts (besides the issuer's) that discovery may point the
    /// token and JWKS endpoints to, e.g. `www.googleapis.com`.
    pub extra_endpoint_hosts: Vec<String>,
    /// PEM file with additional trusted CA certificates for the IdP
    /// (private CA). The system trust store is always used.
    pub ca_file: Option<PathBuf>,
    /// Text of the login button.
    pub button_label: String,
    /// Allow `http://` issuers and endpoints on any host. Only for tests
    /// with a local mock IdP; never in production.
    pub allow_insecure_issuer_for_testing: bool,
}

impl Default for OidcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: String::new(),
            client_id: String::new(),
            client_secret: None,
            client_secret_file: None,
            token_auth_method: TokenAuthMethod::ClientSecretBasic,
            redirect_uri: String::new(),
            scopes: vec!["openid".to_owned(), "profile".to_owned()],
            username_claim: "preferred_username".to_owned(),
            username_strip_suffix: None,
            admin_claim: None,
            admin_value: None,
            required_claim: None,
            required_value: None,
            auto_create_users: false,
            require_local_totp: false,
            signing_algs: vec!["RS256".to_owned(), "ES256".to_owned()],
            clock_skew_secs: 30,
            extra_endpoint_hosts: Vec::new(),
            ca_file: None,
            button_label: "Single sign-on".to_owned(),
            allow_insecure_issuer_for_testing: false,
        }
    }
}

/// Checks an absolute URL of the OIDC settings: http(s), a host, no user
/// info, no query and no fragment. Returns whether it uses https.
fn check_oidc_url(url: &str) -> Result<bool, ConfigError> {
    let uri: axum::http::Uri = url
        .parse()
        .map_err(|_| ConfigError::Invalid("oidc: invalid URL"))?;
    let https = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        _ => return Err(ConfigError::Invalid("oidc: URLs must use https")),
    };
    let authority = uri
        .authority()
        .ok_or(ConfigError::Invalid("oidc: URL without host"))?;
    if authority.as_str().contains('@') || authority.host().is_empty() {
        return Err(ConfigError::Invalid("oidc: URL must not contain user info"));
    }
    if url.contains('#') || uri.query().is_some() {
        return Err(ConfigError::Invalid(
            "oidc: URL must not contain a query or fragment",
        ));
    }
    Ok(https)
}

/// RFC 6749 scope token: printable ASCII except space, `"` and `\`.
fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && scope
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && b != b'"' && b != b'\\')
}

impl OidcConfig {
    /// Validates the section if SSO is enabled.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.enabled {
            return Ok(());
        }
        let insecure = self.allow_insecure_issuer_for_testing;
        if self.issuer.is_empty() || !check_oidc_url(&self.issuer)? && !insecure {
            return Err(ConfigError::Invalid(
                "oidc.issuer must be an https URL (allow_insecure_issuer_for_testing permits http in tests)",
            ));
        }
        if self.client_id.is_empty() || self.client_id.chars().any(char::is_control) {
            return Err(ConfigError::Invalid("oidc.client_id is required"));
        }
        if self.redirect_uri.is_empty() || !check_oidc_url(&self.redirect_uri)? && !insecure {
            return Err(ConfigError::Invalid(
                "oidc.redirect_uri must be an https URL",
            ));
        }
        if !self.redirect_uri.ends_with(OIDC_CALLBACK_PATH) {
            return Err(ConfigError::Invalid(
                "oidc.redirect_uri must end with /api/v1/auth/oidc/callback",
            ));
        }
        if !self.scopes.iter().any(|s| s == "openid") {
            return Err(ConfigError::Invalid("oidc.scopes must contain openid"));
        }
        if !self.scopes.iter().all(|s| valid_scope(s)) {
            return Err(ConfigError::Invalid(
                "oidc.scopes contains an invalid scope",
            ));
        }
        if self.username_claim.is_empty() {
            return Err(ConfigError::Invalid(
                "oidc.username_claim must not be empty",
            ));
        }
        if self.client_secret.is_some() && self.client_secret_file.is_some() {
            return Err(ConfigError::Invalid(
                "oidc: set only one of client_secret and client_secret_file",
            ));
        }
        let secret_given = self.client_secret.is_some() || self.client_secret_file.is_some();
        match self.token_auth_method {
            TokenAuthMethod::None if secret_given => {
                return Err(ConfigError::Invalid(
                    "oidc: token_auth_method = \"none\" does not use a client secret",
                ));
            }
            TokenAuthMethod::ClientSecretBasic | TokenAuthMethod::ClientSecretPost
                if !secret_given =>
            {
                return Err(ConfigError::Invalid(
                    "oidc: client_secret, client_secret_file or CALCRAFT_OIDC__CLIENT_SECRET is required",
                ));
            }
            _ => {}
        }
        if self.admin_claim.is_some() != self.admin_value.is_some() {
            return Err(ConfigError::Invalid(
                "oidc: set both admin_claim and admin_value, or neither",
            ));
        }
        if self.required_claim.is_some() != self.required_value.is_some() {
            return Err(ConfigError::Invalid(
                "oidc: set both required_claim and required_value, or neither",
            ));
        }
        if self.signing_algs.is_empty()
            || !self
                .signing_algs
                .iter()
                .all(|alg| OIDC_SUPPORTED_ALGS.contains(&alg.as_str()))
        {
            return Err(ConfigError::Invalid(
                "oidc.signing_algs: supported are RS256/384/512, PS256/384/512, ES256 and ES384 (never none or HS*)",
            ));
        }
        if self.clock_skew_secs > OIDC_MAX_CLOCK_SKEW_SECS {
            return Err(ConfigError::Invalid(
                "oidc.clock_skew_secs must be at most 60",
            ));
        }
        if self
            .extra_endpoint_hosts
            .iter()
            .any(|h| h.is_empty() || h.contains(['/', '@', ':', ' ']))
        {
            return Err(ConfigError::Invalid(
                "oidc.extra_endpoint_hosts must contain plain host names",
            ));
        }
        Ok(())
    }
}

/// Bounds of `updates.interval_hours`.
pub const UPDATE_INTERVAL_HOURS: std::ops::RangeInclusive<u64> = 1..=168;

/// Optional update notice for administrators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatesConfig {
    /// Ask GitHub for the latest release (at startup and every
    /// `interval_hours`) and show a notice in the admin area. Off by default:
    /// GitHub sees the server's IP address and User-Agent.
    pub check: bool,
    /// Hours between checks (1 to 168).
    pub interval_hours: u64,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self {
            check: false,
            interval_hours: 24,
        }
    }
}

impl UpdatesConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_hours.saturating_mul(3600))
    }
}

/// Shortest refresh interval of a subscription (15 minutes).
pub const MIN_SUBSCRIPTION_REFRESH_SECS: i64 = 15 * 60;
/// Longest refresh interval of a subscription (7 days).
pub const MAX_SUBSCRIPTION_REFRESH_SECS: i64 = 7 * 86_400;
/// Bounds of `subscriptions.max_bytes`: at most the import limit.
pub const SUBSCRIPTION_MAX_BYTES: std::ops::RangeInclusive<usize> =
    64 * 1024..=calcraft_ical::MAX_IMPORT_SIZE;
/// Bounds of `subscriptions.max_per_user`.
pub const SUBSCRIPTIONS_PER_USER: std::ops::RangeInclusive<u32> = 1..=100;
/// Bounds of `subscriptions.max_concurrent_fetches`.
pub const SUBSCRIPTION_FETCHES: std::ops::RangeInclusive<usize> = 1..=16;
/// Bounds of `subscriptions.timeout_secs`.
pub const SUBSCRIPTION_TIMEOUT_SECS: std::ops::RangeInclusive<u64> = 5..=120;
/// Most redirects `subscriptions.max_redirects` may allow.
pub const MAX_SUBSCRIPTION_REDIRECTS: u8 = 5;

/// Calendar subscriptions (webcal/ICS feeds fetched by the server).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubscriptionsConfig {
    /// Let users subscribe to calendar feeds. Off by default: the server
    /// then makes outbound requests to addresses its users choose (public
    /// addresses only, see README).
    pub enabled: bool,
    /// Also allow `http://` feeds (unencrypted). `webcal://` always means
    /// https.
    pub allow_http: bool,
    /// Ports feeds may use. Empty: 443, plus 80 if `allow_http`.
    pub allowed_ports: Vec<u16>,
    /// If not empty, only these hosts may be subscribed to (also after
    /// redirects). `example.com` matches exactly, `*.example.com` any
    /// subdomain.
    pub allowed_hosts: Vec<String>,
    /// Hosts that may never be subscribed to (same syntax).
    pub denied_hosts: Vec<String>,
    /// Largest accepted feed in bytes (64 KiB to 20 MiB).
    pub max_bytes: usize,
    /// Subscriptions per user (1 to 100).
    pub max_per_user: u32,
    /// Refresh interval when neither the user nor the feed sets one, in
    /// minutes (15 to 10080).
    pub default_refresh_minutes: u64,
    /// Feeds fetched at the same time (1 to 16).
    pub max_concurrent_fetches: usize,
    /// Deadline for one fetch including redirects and the body, in seconds
    /// (5 to 120).
    pub timeout_secs: u64,
    /// Redirects followed per fetch (0 to 5); every hop is checked again.
    pub max_redirects: u8,
}

impl Default for SubscriptionsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_http: false,
            allowed_ports: Vec::new(),
            allowed_hosts: Vec::new(),
            denied_hosts: Vec::new(),
            max_bytes: 10 * 1024 * 1024,
            max_per_user: 20,
            default_refresh_minutes: 360,
            max_concurrent_fetches: 2,
            timeout_secs: 30,
            max_redirects: 3,
        }
    }
}

/// `example.com`, `*.example.com` or `.example.com`: lower-case
/// letters, digits, `-` and `.`, no empty labels.
fn valid_host_pattern(pattern: &str) -> bool {
    let name = pattern
        .strip_prefix("*.")
        .or_else(|| pattern.strip_prefix('.'))
        .unwrap_or(pattern);
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

impl SubscriptionsConfig {
    /// Ports feeds may use.
    pub fn ports(&self) -> Vec<u16> {
        if !self.allowed_ports.is_empty() {
            return self.allowed_ports.clone();
        }
        if self.allow_http {
            vec![443, 80]
        } else {
            vec![443]
        }
    }

    pub fn default_refresh_secs(&self) -> i64 {
        i64::try_from(self.default_refresh_minutes.saturating_mul(60)).unwrap_or(i64::MAX)
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if !SUBSCRIPTION_MAX_BYTES.contains(&self.max_bytes) {
            return Err(ConfigError::Invalid(
                "subscriptions.max_bytes must be between 65536 (64 KiB) and 20971520 (20 MiB)",
            ));
        }
        if !SUBSCRIPTIONS_PER_USER.contains(&self.max_per_user) {
            return Err(ConfigError::Invalid(
                "subscriptions.max_per_user must be between 1 and 100",
            ));
        }
        let minutes = i64::try_from(self.default_refresh_minutes).unwrap_or(i64::MAX);
        if !(MIN_SUBSCRIPTION_REFRESH_SECS / 60..=MAX_SUBSCRIPTION_REFRESH_SECS / 60)
            .contains(&minutes)
        {
            return Err(ConfigError::Invalid(
                "subscriptions.default_refresh_minutes must be between 15 and 10080 (7 days)",
            ));
        }
        if !SUBSCRIPTION_FETCHES.contains(&self.max_concurrent_fetches) {
            return Err(ConfigError::Invalid(
                "subscriptions.max_concurrent_fetches must be between 1 and 16",
            ));
        }
        if !SUBSCRIPTION_TIMEOUT_SECS.contains(&self.timeout_secs) {
            return Err(ConfigError::Invalid(
                "subscriptions.timeout_secs must be between 5 and 120",
            ));
        }
        if self.max_redirects > MAX_SUBSCRIPTION_REDIRECTS {
            return Err(ConfigError::Invalid(
                "subscriptions.max_redirects must be at most 5",
            ));
        }
        if self.allowed_ports.contains(&0) {
            return Err(ConfigError::Invalid(
                "subscriptions.allowed_ports must not contain 0",
            ));
        }
        if !self
            .allowed_hosts
            .iter()
            .chain(&self.denied_hosts)
            .all(|h| valid_host_pattern(h))
        {
            return Err(ConfigError::Invalid(
                "subscriptions.allowed_hosts/denied_hosts: use lower-case host names like example.com or *.example.com",
            ));
        }
        Ok(())
    }
}

/// Bounds of `scheduling.max_attendees`.
pub const SCHEDULING_MAX_ATTENDEES: std::ops::RangeInclusive<u32> = 1..=1000;
/// Bounds of `scheduling.max_recipients_per_hour`.
pub const SCHEDULING_RECIPIENTS_PER_HOUR: std::ops::RangeInclusive<u32> = 1..=100_000;
/// Bounds of `scheduling.inbox_max_messages`.
pub const SCHEDULING_INBOX_MESSAGES: std::ops::RangeInclusive<u32> = 10..=100_000;
/// Bounds of `scheduling.max_freebusy_days`.
pub const SCHEDULING_FREEBUSY_DAYS: std::ops::RangeInclusive<u32> = 1..=1830;

/// CalDAV scheduling (RFC 6638) between users of this server: invitations,
/// replies and free/busy lookups. Off by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulingConfig {
    /// Process ORGANIZER/ATTENDEE changes of calendar objects and offer
    /// scheduling inboxes and outboxes.
    pub enabled: bool,
    /// Gives every user the calendar user address
    /// `mailto:<username>@<domain>` in addition to an e-mail address set by
    /// an administrator. Lower-case host name, e.g. `example.org`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Let users look up the busy times of other users of this server
    /// (only busy periods are revealed, never event details).
    pub free_busy: bool,
    /// Most distinct attendees of one scheduled object (1 to 1000).
    pub max_attendees: u32,
    /// Recipients one user may send invitations, cancellations, replies or
    /// free/busy lookups to per hour (1 to 100 000); beyond that messages
    /// are not delivered (`SCHEDULE-STATUS` 5.1).
    pub max_recipients_per_hour: u32,
    /// Messages kept in each user's scheduling inbox (10 to 100 000); the
    /// oldest are dropped beyond that.
    pub inbox_max_messages: u32,
    /// Longest time range of a free/busy lookup in days (1 to 1830).
    pub max_freebusy_days: u32,
}

impl Default for SchedulingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            domain: None,
            free_busy: true,
            max_attendees: 100,
            max_recipients_per_hour: 1000,
            inbox_max_messages: 1000,
            max_freebusy_days: 366,
        }
    }
}

impl SchedulingConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(domain) = &self.domain
            && (domain.starts_with("*.") || domain.starts_with('.') || !valid_host_pattern(domain))
        {
            return Err(ConfigError::Invalid(
                "scheduling.domain: use a lower-case host name like example.org",
            ));
        }
        if !SCHEDULING_MAX_ATTENDEES.contains(&self.max_attendees) {
            return Err(ConfigError::Invalid(
                "scheduling.max_attendees must be between 1 and 1000",
            ));
        }
        if !SCHEDULING_RECIPIENTS_PER_HOUR.contains(&self.max_recipients_per_hour) {
            return Err(ConfigError::Invalid(
                "scheduling.max_recipients_per_hour must be between 1 and 100000",
            ));
        }
        if !SCHEDULING_INBOX_MESSAGES.contains(&self.inbox_max_messages) {
            return Err(ConfigError::Invalid(
                "scheduling.inbox_max_messages must be between 10 and 100000",
            ));
        }
        if !SCHEDULING_FREEBUSY_DAYS.contains(&self.max_freebusy_days) {
            return Err(ConfigError::Invalid(
                "scheduling.max_freebusy_days must be between 1 and 1830",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// Log filter, e.g. `info` or `calcraft_server=debug`. `RUST_LOG` takes precedence.
    pub level: String,
    pub format: LogFormat,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
            format: LogFormat::Pretty,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Pretty,
    Json,
}

/// Errors while loading or validating the configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration file {0} does not exist")]
    FileNotFound(String),
    #[error(transparent)]
    Parse(#[from] Box<figment::Error>),
    #[error("invalid configuration: {0}")]
    Invalid(&'static str),
}

impl Config {
    /// Loads the configuration from defaults, the optional file and the environment.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let mut figment = Figment::from(Serialized::defaults(Config::default()));
        if let Some(path) = path {
            if !path.is_file() {
                return Err(ConfigError::FileNotFound(path.display().to_string()));
            }
            figment = figment.merge(Toml::file(path));
        }
        // `CALCRAFT_CONFIG` selects the file and `CALCRAFT_UI_REQUIRED` is a build
        // switch (see `calcraft-web/build.rs`); neither is a configuration key.
        figment = figment.merge(
            Env::prefixed(ENV_PREFIX)
                .ignore(&["config", "ui_required"])
                .split("__"),
        );
        Self::from_figment(figment)
    }

    fn from_figment(figment: Figment) -> Result<Self, ConfigError> {
        let config: Config = figment.extract().map_err(Box::new)?;
        config.validate()?;
        Ok(config)
    }

    /// Effective `storage.max_vcard_bytes`: the configured value, or
    /// [`DEFAULT_MAX_VCARD_BYTES`] capped at `server.max_body_bytes`.
    pub fn max_vcard_bytes(&self) -> usize {
        self.storage
            .max_vcard_bytes
            .unwrap_or_else(|| DEFAULT_MAX_VCARD_BYTES.min(self.server.max_body_bytes))
    }

    /// Checks invariants that serde cannot express.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.server.max_body_bytes == 0 {
            return Err(ConfigError::Invalid(
                "server.max_body_bytes must be greater than 0",
            ));
        }
        if self.server.request_timeout_secs == 0 {
            return Err(ConfigError::Invalid(
                "server.request_timeout_secs must be greater than 0",
            ));
        }
        let server = &self.server;
        if server.max_concurrent_requests == 0
            || server.max_concurrent_requests_per_client == 0
            || server.client_rate_burst == 0
            || server.client_rate_per_second == 0
            || server.max_login_failures_per_client == 0
            || server.max_tracked_clients == 0
        {
            return Err(ConfigError::Invalid(
                "server: request and login limits must be greater than 0",
            ));
        }
        // Bounded by the semaphore and by the eviction scan over a full client
        // table, which runs under its lock for every new client.
        if server.max_concurrent_requests > 1_000_000 || server.max_tracked_clients > 100_000 {
            return Err(ConfigError::Invalid(
                "server: max_concurrent_requests (1 000 000) or max_tracked_clients (100 000) too large",
            ));
        }
        // Health checks bypass the global limit but still occupy client entries,
        // so leave room for them beyond the in-flight requests.
        if server.max_tracked_clients < server.max_concurrent_requests.saturating_mul(2) {
            return Err(ConfigError::Invalid(
                "server.max_tracked_clients must be at least twice server.max_concurrent_requests",
            ));
        }
        if !CLIENT_IPV6_PREFIX.contains(&server.client_ipv6_prefix) {
            return Err(ConfigError::Invalid(
                "server.client_ipv6_prefix must be between 32 and 128",
            ));
        }
        if !self.security.password_login && !self.oidc.enabled {
            return Err(ConfigError::Invalid(
                "security.password_login = false requires [oidc] enabled = true",
            ));
        }
        if self.storage.pre_migration_backups_keep == 0 {
            return Err(ConfigError::Invalid(
                "storage.pre_migration_backups_keep must be at least 1",
            ));
        }
        if self.storage.trash_retention_days > MAX_TRASH_RETENTION_DAYS {
            return Err(ConfigError::Invalid(
                "storage.trash_retention_days must be at most 3650 (0 disables the trash)",
            ));
        }
        // Only an explicit value is checked: the default adapts to a smaller
        // body limit, so existing configurations keep working.
        if let Some(vcard) = self.storage.max_vcard_bytes {
            if !(MIN_VCARD_BYTES..=MAX_VCARD_BYTES).contains(&vcard) {
                return Err(ConfigError::Invalid(
                    "storage.max_vcard_bytes must be between 65536 (64 KiB) and 10485760 (10 MiB)",
                ));
            }
            if vcard > self.server.max_body_bytes {
                return Err(ConfigError::Invalid(
                    "storage.max_vcard_bytes must not exceed server.max_body_bytes",
                ));
            }
        }
        if !UPDATE_INTERVAL_HOURS.contains(&self.updates.interval_hours) {
            return Err(ConfigError::Invalid(
                "updates.interval_hours must be between 1 and 168",
            ));
        }
        self.subscriptions.validate()?;
        self.scheduling.validate()?;
        self.oidc.validate()
    }
}

#[cfg(test)]
mod tests;
