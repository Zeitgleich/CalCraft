use super::*;

#[test]
fn binding_cookie_attributes() {
    let cookie = binding_cookie(Some("v"), true);
    for part in [
        "HttpOnly",
        "SameSite=Lax",
        "Secure",
        "Path=/api/v1/auth/oidc",
        "Max-Age=600",
    ] {
        assert!(cookie.contains(part), "{part}");
    }
    assert!(binding_cookie(None, false).contains("Max-Age=0"));
    let mut headers = HeaderMap::new();
    headers.insert(COOKIE, "calcraft_oidc=abc; x=1".parse().unwrap());
    assert_eq!(binding_from(&headers), Some("abc"));
}
