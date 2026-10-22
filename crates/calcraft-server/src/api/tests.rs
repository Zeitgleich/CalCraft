use super::*;

#[test]
fn reads_session_cookie() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        "a=1; calcraft_session=abc; b=2".parse().unwrap(),
    );
    assert_eq!(session_token(&headers), Some("abc"));
    let mut other = HeaderMap::new();
    other.insert(header::COOKIE, "calcraft_sessionx=abc".parse().unwrap());
    assert_eq!(session_token(&other), None);
}

#[test]
fn cookie_attributes() {
    let cookie = session_cookie(Some("t"), true);
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Secure")
    );
    assert!(session_cookie(None, false).contains("Max-Age=0"));
}

#[test]
fn openapi_documents_every_route() {
    let spec: serde_json::Value = serde_json::from_str(OPENAPI).unwrap();
    let paths = spec["paths"].as_object().unwrap();
    for (method, path) in ROUTES {
        assert!(
            paths.get(*path).and_then(|p| p.get(*method)).is_some(),
            "{method} {path} missing in openapi.json"
        );
    }
    let documented: usize = paths.values().map(|p| p.as_object().unwrap().len()).sum();
    assert_eq!(
        documented,
        ROUTES.len(),
        "openapi.json documents routes that do not exist"
    );
}
