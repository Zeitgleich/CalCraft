//! Single sign-on helpers: the login button and the error codes the
//! server's SSO callback reports back (`/?sso_error=<code>`).
//!
//! The query value comes from the URL and is therefore untrusted: it is
//! only ever compared against the fixed list of codes, never shown.

use crate::text;

/// Query parameter the server uses to report a failed SSO login.
pub const SSO_ERROR_PARAM: &str = "sso_error";

/// Where the SSO login starts if the server sends no usable URL.
pub const DEFAULT_LOGIN_URL: &str = "/api/v1/auth/oidc/login";

/// Fixed text for an SSO error code. Unknown codes get a generic text, so
/// nothing from the URL is ever reflected.
pub fn error_message(code: &str) -> &'static str {
    match code {
        "invalid_request" => text::SSO_ERR_INVALID_REQUEST,
        "denied" => text::SSO_ERR_DENIED,
        "unavailable" => text::SSO_ERR_UNAVAILABLE,
        "login_failed" => text::SSO_ERR_LOGIN_FAILED,
        "no_account" => text::SSO_ERR_NO_ACCOUNT,
        "account_exists" => text::SSO_ERR_ACCOUNT_EXISTS,
        "local_2fa_required" => text::SSO_ERR_LOCAL_2FA_REQUIRED,
        _ => text::SSO_ERR_UNKNOWN,
    }
}

/// Splits `location.search` (`?a=1&sso_error=x`) into the SSO error code,
/// if present, and the remaining query string (with leading `?`, or empty).
pub fn take_error(search: &str) -> (Option<String>, String) {
    let query = search.strip_prefix('?').unwrap_or(search);
    let mut code = None;
    let mut rest = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=') {
            Some((SSO_ERROR_PARAM, value)) => code = Some(value.to_owned()),
            None if pair == SSO_ERROR_PARAM => code = Some(String::new()),
            _ => rest.push(pair),
        }
    }
    let rest = if rest.is_empty() {
        String::new()
    } else {
        format!("?{}", rest.join("&"))
    };
    (code, rest)
}

/// Text of the SSO button: the configured label, or a generic one.
pub fn button_label(label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        text::SSO_LOGIN_GENERIC.to_owned()
    } else {
        label.to_owned()
    }
}

/// The login URL from the server, if it is a same-origin absolute path;
/// anything else (other origins, `//host`, schemes) falls back to the
/// default path.
pub fn login_url(url: &str) -> &str {
    let same_origin_path = url.starts_with('/')
        && !url.starts_with("//")
        && !url.starts_with("/\\")
        && !url.chars().any(|c| c.is_control() || c == '\\');
    if same_origin_path {
        url
    } else {
        DEFAULT_LOGIN_URL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_have_own_texts() {
        let codes = [
            "invalid_request",
            "denied",
            "unavailable",
            "login_failed",
            "no_account",
            "account_exists",
            "local_2fa_required",
        ];
        let mut seen = Vec::new();
        for code in codes {
            let message = error_message(code);
            assert_ne!(message, text::SSO_ERR_UNKNOWN, "{code}");
            assert!(!seen.contains(&message), "{code} shares a text");
            seen.push(message);
        }
    }

    #[test]
    fn unknown_codes_are_not_reflected() {
        for code in [
            "",
            "LOGIN_FAILED",
            "<script>alert(1)</script>",
            "login_failed ",
        ] {
            assert_eq!(error_message(code), text::SSO_ERR_UNKNOWN);
        }
    }

    #[test]
    fn takes_error_from_query() {
        assert_eq!(take_error(""), (None, String::new()));
        assert_eq!(take_error("?"), (None, String::new()));
        assert_eq!(
            take_error("?sso_error=login_failed"),
            (Some("login_failed".to_owned()), String::new())
        );
        assert_eq!(
            take_error("?a=1&sso_error=denied&b=2"),
            (Some("denied".to_owned()), "?a=1&b=2".to_owned())
        );
        assert_eq!(
            take_error("?sso_error"),
            (Some(String::new()), String::new())
        );
        assert_eq!(take_error("?a=1"), (None, "?a=1".to_owned()));
        assert_eq!(
            take_error("?sso_errorx=1"),
            (None, "?sso_errorx=1".to_owned())
        );
    }

    #[test]
    fn labels_fall_back() {
        assert_eq!(
            button_label("Sign in with Keycloak"),
            "Sign in with Keycloak"
        );
        assert_eq!(button_label("  "), text::SSO_LOGIN_GENERIC);
    }

    #[test]
    fn login_url_stays_on_origin() {
        assert_eq!(
            login_url("/api/v1/auth/oidc/login"),
            "/api/v1/auth/oidc/login"
        );
        for bad in [
            "https://evil.example/",
            "//evil.example/",
            "/\\evil.example",
            "javascript:alert(1)",
            "",
            "/a\nb",
        ] {
            assert_eq!(login_url(bad), DEFAULT_LOGIN_URL, "{bad:?}");
        }
    }
}
