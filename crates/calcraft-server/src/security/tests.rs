use super::CSP;

#[test]
fn csp_is_same_origin_only() {
    let directives: Vec<&str> = CSP.split("; ").collect();
    for expected in [
        "default-src 'none'",
        "script-src 'self' 'wasm-unsafe-eval'",
        "style-src 'self'",
        "img-src 'self' data:",
        "manifest-src 'self'",
        "frame-ancestors 'none'",
    ] {
        assert!(directives.contains(&expected), "{expected}");
    }
    assert!(!CSP.contains("unsafe-inline"));
    assert!(!CSP.contains("http"), "no external origins");
    assert!(!CSP.contains('*'), "no wildcards");
}
