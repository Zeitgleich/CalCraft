use super::*;

#[test]
fn endpoints_must_be_on_the_issuer_host() {
    let issuer: Uri = "https://idp.example/realms/main".parse().unwrap();
    let policy = EndpointPolicy::new(&issuer, &["keys.example".to_owned()], false).unwrap();
    for ok in [
        "https://idp.example/realms/main/protocol/openid-connect/token",
        "https://IDP.example:443/certs",
        "https://keys.example/jwks",
    ] {
        policy.check(ok).unwrap();
    }
    for bad in [
        "http://idp.example/token",
        "https://idp.example:8443/token",
        "https://idp.example.evil.example/token",
        "https://evil.example/token",
        "https://user@idp.example/token",
        "https://169.254.169.254/latest/meta-data",
        "https://keys.example:8443/jwks",
        "/relative",
        "file:///etc/passwd",
    ] {
        assert!(policy.check(bad).is_err(), "{bad}");
    }
    let insecure = EndpointPolicy::new(&issuer, &[], true).unwrap();
    insecure.check("http://127.0.0.1:9/token").unwrap();
    assert!(insecure.check("file:///etc/passwd").is_err());
}
