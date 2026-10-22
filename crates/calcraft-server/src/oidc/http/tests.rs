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

/// The shared HTTPS module gained the pinned fetcher of calendar
/// subscriptions; OIDC keeps its own rules: the administrator configures
/// the identity provider, which may live on an internal address, and the
/// subscription address classifier never applies to it.
#[test]
fn oidc_policy_is_independent_of_subscriptions() {
    for issuer in [
        "https://10.0.0.5:8443/realms/main",
        "https://keycloak.internal/realms/main",
        "https://[fd00::5]/realms/main",
    ] {
        let uri: Uri = issuer.parse().unwrap();
        let policy = EndpointPolicy::new(&uri, &[], false).unwrap();
        policy
            .check(&format!("{issuer}/protocol/openid-connect/token"))
            .unwrap();
        assert!(policy.check("https://10.0.0.6:8443/token").is_err());
        assert!(
            policy
                .check(&issuer.replace("https://", "http://"))
                .is_err()
        );
    }
    // The OIDC client is built as before (testing mode: http allowed).
    assert!(HttpClient::new(None, true).is_ok());
}
