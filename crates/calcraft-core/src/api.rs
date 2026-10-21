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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_reject_unknown_fields() {
        let login: LoginRequest =
            serde_json::from_str(r#"{"username":"a","password":"b"}"#).unwrap();
        assert_eq!(login.code, None);
        assert!(
            serde_json::from_str::<LoginRequest>(r#"{"username":"a","password":"b","admin":true}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<UpdateUser>(r#"{"is_admin":true,"role":"root"}"#).is_err());
    }

    #[test]
    fn collection_kind_is_lowercase() {
        let create: CreateCollection =
            serde_json::from_str(r#"{"kind":"addressbook","slug":"team"}"#).unwrap();
        assert_eq!(create.kind, CollectionKind::Addressbook);
    }
}
