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
    let long = button_label(&"x".repeat(500));
    assert_eq!(long.chars().count(), 60);
    assert!(long.ends_with('…'));
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
