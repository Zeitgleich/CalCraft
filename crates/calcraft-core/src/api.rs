//! JSON types of the REST API, shared by the server and the web UI.
//!
//! Request types reject unknown fields, so that typos are reported
//! instead of being silently ignored.

use serde::{Deserialize, Serialize};

use crate::CollectionKind;

/// Header that every state-changing API request must carry (CSRF
/// protection: cross-site forms cannot set custom headers, and CORS is off).
pub const CSRF_HEADER: &str = "x-calcraft-request";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Stable machine-readable code, e.g. `invalid_credentials`.
    pub error: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub name: String,
    pub version: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    /// TOTP code or recovery code, required when 2FA is enabled.
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub username: String,
    pub display_name: Option<String>,
    pub is_admin: bool,
    pub totp_enabled: bool,
    /// Link of this account to the configured single sign-on; `None` if
    /// SSO is not configured. Never contains the IdP subject.
    #[serde(default)]
    pub sso: Option<SsoLink>,
}

/// Whether an account is bound to an identity of the configured OpenID
/// Connect issuer (`GET /api/v1/me`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoLink {
    /// The account is bound to an identity of `issuer`.
    pub linked: bool,
    /// The configured issuer URL (public; the same as in the IdP discovery).
    pub issuer: String,
    /// When the identity was bound (Unix seconds).
    #[serde(default)]
    pub linked_at: Option<i64>,
    /// Last SSO login (Unix seconds).
    #[serde(default)]
    pub last_login_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangePassword {
    pub current: String,
    pub new: String,
    /// TOTP or recovery code, required when 2FA is enabled.
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionInfo {
    pub kind: CollectionKind,
    pub slug: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    /// Component types of a calendar (`VEVENT`, `VTODO`, …); empty for address books.
    pub components: Vec<String>,
    /// DAV URL of the collection, for client setup.
    pub href: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCollection {
    pub kind: CollectionKind,
    pub slug: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

/// Fields that are present are changed; an empty string clears a value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateCollection {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPasswordInfo {
    pub id: i64,
    pub name: String,
    /// Unix seconds.
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAppPassword {
    pub name: String,
}

/// Returned once on creation; the password cannot be retrieved again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedAppPassword {
    pub id: i64,
    pub name: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotpSetup {
    /// Base32 secret for manual entry.
    pub secret: String,
    /// `otpauth://` URI for QR codes.
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TotpEnable {
    pub code: String,
}

/// Shown once when 2FA is enabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCodes {
    pub codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TotpDisable {
    pub password: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminUser {
    pub username: String,
    pub display_name: Option<String>,
    pub is_admin: bool,
    pub disabled: bool,
    pub totp_enabled: bool,
    /// The account is bound to an SSO (OpenID Connect) identity.
    #[serde(default)]
    pub sso_linked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateUser {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub is_admin: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateUser {
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub is_admin: Option<bool>,
}

/// Result of the optional update check (`GET /api/v1/admin/update`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInfo {
    /// Whether `[updates] check` is on (and compiled in).
    pub enabled: bool,
    /// Version of the running server.
    pub current: String,
    /// Latest stable release, once a check has succeeded.
    pub latest: Option<String>,
    /// `latest` is newer than `current`.
    pub newer: bool,
    /// Release page on github.com.
    pub url: Option<String>,
    /// Time of the last successful check (Unix seconds).
    pub checked_at: Option<i64>,
}

/// Login methods offered by the server, so the UI can show an SSO button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthMethods {
    /// Login with username and password (`POST /api/v1/auth/login`).
    pub password: bool,
    /// Single sign-on via OpenID Connect; `None` if not configured.
    pub oidc: Option<OidcMethod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcMethod {
    /// Text for the login button, e.g. "Sign in with Keycloak".
    pub label: String,
    /// Relative URL that starts the login (a top-level browser navigation).
    pub login_url: String,
}

/// Result of importing a file into a collection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub created: u64,
    pub replaced: u64,
    /// Objects whose UID already existed (with `on_conflict=skip`).
    pub skipped: u64,
    /// Objects that failed validation or limits.
    pub rejected: u64,
    /// Reasons for the first rejections (at most 20).
    pub errors: Vec<ImportRejection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportRejection {
    /// UID of the object, if known (shortened to 200 characters).
    pub uid: Option<String>,
    pub reason: String,
}

/// What a trash entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrashKind {
    /// A calendar object or vCard.
    Item,
    /// A calendar or address book with its objects.
    Collection,
}

/// One entry of the trash (`GET /api/v1/trash`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashEntryInfo {
    /// Opaque identifier for restore and delete.
    pub id: String,
    pub kind: TrashKind,
    /// Kind of the collection the entry belongs to (or is).
    pub collection_kind: CollectionKind,
    /// Short name of that collection.
    pub collection: String,
    /// Resource name of an object; `None` for collections.
    pub name: Option<String>,
    /// SUMMARY / FN / collection display name, if any (plain text).
    pub summary: Option<String>,
    /// Bytes of content.
    pub size: i64,
    /// Objects in a collection entry (0 for objects).
    pub items: i64,
    /// Unix seconds.
    pub deleted_at: i64,
    /// When the entry is purged (Unix seconds).
    pub expires_at: i64,
}

/// A page of the trash, newest entries first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashList {
    pub entries: Vec<TrashEntryInfo>,
    /// Entries in total (all pages).
    pub total: i64,
    /// Bytes in the trash; they count towards the storage quota.
    pub bytes: i64,
    /// Days entries are kept; 0 means the trash is disabled.
    pub retention_days: u32,
    pub offset: u32,
    pub limit: u32,
}

/// Result of `POST /api/v1/trash/{id}/restore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoredTrashEntry {
    pub kind: TrashKind,
    pub collection_kind: CollectionKind,
    pub collection: String,
    pub name: Option<String>,
    /// Entity tag of a restored object (unchanged).
    pub etag: Option<String>,
    /// DAV URL of the restored object or collection.
    pub href: String,
}

#[cfg(test)]
mod tests;
