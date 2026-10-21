//! Plain logic of the CalCraft web interface.
//!
//! Everything here is independent of the browser so that it can be unit
//! tested natively (`cargo test -p calcraft-ui`). The Leptos application
//! itself lives in the binary target and is only compiled for `wasm32`.

pub mod text;
pub mod validate;

use calcraft_core::api::ErrorBody;

/// A failed API call, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFailure {
    /// HTTP status, 0 if the request did not reach the server.
    pub status: u16,
    /// Error code from the response body (`invalid_credentials`, …), or a
    /// local code (`network`, `unexpected`).
    pub code: String,
    pub message: String,
}

impl ApiFailure {
    /// Interprets an error response (`{"error", "message"}`).
    pub fn from_response(status: u16, body: &str) -> Self {
        match serde_json::from_str::<ErrorBody>(body) {
            Ok(body) => Self {
                status,
                code: body.error,
                message: body.message,
            },
            Err(_) => Self {
                status,
                code: "unexpected".to_owned(),
                message: format!("unexpected response (HTTP {status})"),
            },
        }
    }

    /// The request failed before a response arrived.
    pub fn network(detail: impl Into<String>) -> Self {
        Self {
            status: 0,
            code: "network".to_owned(),
            message: detail.into(),
        }
    }

    /// A response that could not be understood.
    pub fn unexpected(detail: impl Into<String>) -> Self {
        Self {
            status: 0,
            code: "unexpected".to_owned(),
            message: detail.into(),
        }
    }

    /// The session is missing or expired.
    pub fn is_unauthorized(&self) -> bool {
        self.code == "unauthorized"
    }

    /// Login needs a second factor.
    pub fn is_totp_required(&self) -> bool {
        self.code == "totp_required"
    }

    /// Text for the user. Known codes get a fixed text; otherwise the
    /// server message is shown (always rendered as plain text).
    pub fn user_message(&self) -> String {
        match self.code.as_str() {
            "invalid_credentials" => text::ERR_INVALID_CREDENTIALS.to_owned(),
            "unauthorized" => text::ERR_SESSION_EXPIRED.to_owned(),
            "totp_required" => text::ERR_TOTP_REQUIRED.to_owned(),
            "forbidden" => text::ERR_FORBIDDEN.to_owned(),
            "csrf" => text::ERR_CSRF.to_owned(),
            "network" => text::ERR_NETWORK.to_owned(),
            "not_found" if self.status == 404 => text::ERR_NOT_FOUND.to_owned(),
            _ if self.message.is_empty() => text::ERR_UNKNOWN.to_owned(),
            _ => capitalize(&self.message),
        }
    }
}

fn capitalize(message: &str) -> String {
    let mut chars = message.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Formats Unix seconds as `YYYY-MM-DD HH:MM UTC`.
pub fn format_timestamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (H. Hinnant's
/// algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    // Both values are small and positive by construction.
    (year, month.unsigned_abs() as u32, day.unsigned_abs() as u32)
}

/// Splits a secret into groups of four for easier manual entry.
pub fn group_secret(secret: &str) -> String {
    secret
        .as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Pages of the application, addressed by the URL fragment (`#/settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Settings,
    AppPasswords,
    Admin,
}

impl Page {
    pub const ALL: [Page; 4] = [
        Page::Overview,
        Page::Settings,
        Page::AppPasswords,
        Page::Admin,
    ];

    /// Parses `location.hash`; unknown fragments show the overview.
    pub fn from_hash(hash: &str) -> Self {
        match hash.trim_start_matches('#').trim_start_matches('/') {
            "settings" => Self::Settings,
            "app-passwords" => Self::AppPasswords,
            "admin" => Self::Admin,
            _ => Self::Overview,
        }
    }

    pub fn hash(self) -> &'static str {
        match self {
            Self::Overview => "#/",
            Self::Settings => "#/settings",
            Self::AppPasswords => "#/app-passwords",
            Self::Admin => "#/admin",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => text::NAV_OVERVIEW,
            Self::Settings => text::NAV_SETTINGS,
            Self::AppPasswords => text::NAV_APP_PASSWORDS,
            Self::Admin => text::NAV_ADMIN,
        }
    }

    /// Whether the page is shown to the user.
    pub fn visible(self, is_admin: bool) -> bool {
        self != Self::Admin || is_admin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_error_bodies() {
        let failure =
            ApiFailure::from_response(401, r#"{"error":"totp_required","message":"code needed"}"#);
        assert!(failure.is_totp_required());
        assert_eq!(failure.user_message(), text::ERR_TOTP_REQUIRED);

        let conflict = ApiFailure::from_response(
            409,
            r#"{"error":"conflict","message":"user already exists"}"#,
        );
        assert_eq!(conflict.user_message(), "User already exists");

        let garbage = ApiFailure::from_response(502, "<html>bad gateway</html>");
        assert_eq!(garbage.code, "unexpected");
        assert!(garbage.user_message().contains("502"));
    }

    #[test]
    fn unauthorized_is_detected() {
        let failure = ApiFailure::from_response(
            401,
            r#"{"error":"unauthorized","message":"login required"}"#,
        );
        assert!(failure.is_unauthorized());
        assert!(!ApiFailure::network("offline").is_unauthorized());
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_timestamp(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(format_timestamp(1_792_581_240), "2026-10-21 11:14 UTC");
        assert_eq!(format_timestamp(-60), "1969-12-31 23:59 UTC");
    }

    #[test]
    fn groups_secrets() {
        assert_eq!(group_secret("ABCDEFGHIJ"), "ABCD EFGH IJ");
        assert_eq!(group_secret(""), "");
    }

    #[test]
    fn pages_round_trip() {
        for page in Page::ALL {
            assert_eq!(Page::from_hash(page.hash()), page);
        }
        assert_eq!(Page::from_hash(""), Page::Overview);
        assert_eq!(Page::from_hash("#/nonsense"), Page::Overview);
        assert!(!Page::Admin.visible(false));
        assert!(Page::Admin.visible(true));
    }
}
