use super::*;
use serde_json::json;

#[test]
fn pkce_s256() {
    // BASE64URL(SHA-256(verifier)), cross-checked with Python's hashlib.
    assert_eq!(
        pkce_challenge("dBjftJeZ4CVP-mJ92K9yLhaWhakdbzR9ScVpaEAdR3c"),
        "xJ-76oyO0oUJKpdl-o0BKrwm_N_u3uUoQRAOOcQf6bQ"
    );
}

#[test]
fn callback_debug_hides_code_and_state() {
    let params = CallbackParams {
        code: Some("secret-code".to_owned()),
        state: Some("secret-state".to_owned()),
        ..CallbackParams::default()
    };
    let debug = format!("{params:?}");
    assert!(!debug.contains("secret") && debug.contains("<redacted>"));
}

#[test]
fn form_encoding() {
    assert_eq!(form_encode("a b&c=d/é~"), "a%20b%26c%3Dd%2F%C3%A9~");
    assert_eq!(form(&[("a", "1"), ("b c", "2")]), "a=1&b%20c=2");
}

#[test]
fn claims_by_path() {
    let claims = json!({
        "groups": ["/staff", "/calcraft-admins"],
        "realm_access": {"roles": ["admin"]},
        "https://example.com/role": "boss",
        "flag": true,
    });
    let claims = claims.as_object().unwrap();
    assert!(claim_contains(claims, "groups", "/calcraft-admins"));
    assert!(!claim_contains(claims, "groups", "calcraft-admins"));
    assert!(claim_contains(claims, "realm_access.roles", "admin"));
    assert!(claim_contains(claims, "https://example.com/role", "boss"));
    assert!(!claim_contains(claims, "flag", "true"));
    assert!(!claim_contains(claims, "missing.path", "x"));
}

#[test]
fn discovery_is_checked() {
    let issuer = "https://idp.example/realms/main";
    let policy = EndpointPolicy::new(&issuer.parse().unwrap(), &[], false).unwrap();
    let doc = |iss: &str, token: &str, methods: Value| {
        json!({
            "issuer": iss,
            "authorization_endpoint": format!("{issuer}/auth"),
            "token_endpoint": token,
            "jwks_uri": format!("{issuer}/certs"),
            "code_challenge_methods_supported": methods,
        })
        .to_string()
    };
    let good = doc(issuer, &format!("{issuer}/token"), json!(["plain", "S256"]));
    parse_discovery(good.as_bytes(), issuer, &policy).unwrap();
    for bad in [
        doc(
            "https://evil.example",
            &format!("{issuer}/token"),
            json!(["S256"]),
        ),
        doc(issuer, "https://evil.example/token", json!(["S256"])),
        doc(issuer, "http://idp.example/token", json!(["S256"])),
        doc(issuer, &format!("{issuer}/token"), json!(["plain"])),
        "{}".to_owned(),
    ] {
        assert!(
            parse_discovery(bad.as_bytes(), issuer, &policy).is_err(),
            "{bad}"
        );
    }
}

fn oidc(changes: impl FnOnce(&mut OidcConfig)) -> Arc<Oidc> {
    let mut config = OidcConfig {
        enabled: true,
        issuer: "https://idp.example/realms/main".to_owned(),
        client_id: "calcraft".to_owned(),
        client_secret: Some(crate::config::Secret::new("s")),
        redirect_uri: "https://cal.example/api/v1/auth/oidc/callback".to_owned(),
        ..OidcConfig::default()
    };
    changes(&mut config);
    Oidc::new(&config).unwrap()
}

#[test]
fn usernames_are_validated_not_rewritten() {
    let rp = oidc(|_| {});
    let name = |value: Value| {
        let claims = json!({ "preferred_username": value });
        rp.username(claims.as_object().unwrap())
            .map(|u| u.to_string())
    };
    assert_eq!(name(json!("Alice")).as_deref(), Some("alice"));
    assert_eq!(name(json!("bob.smith")).as_deref(), Some("bob.smith"));
    for bad in [
        json!("../admin"),
        json!("a b"),
        json!(""),
        json!(42),
        json!("ålice"),
    ] {
        assert_eq!(name(bad.clone()), None, "{bad}");
    }
    let upn = oidc(|c| c.username_strip_suffix = Some("@Example.com".to_owned()));
    let claims = json!({"preferred_username": "Alice@example.com"});
    assert_eq!(
        upn.username(claims.as_object().unwrap()).unwrap().as_str(),
        "alice"
    );
    let other = json!({"preferred_username": "alice@evil.example"});
    assert!(upn.username(other.as_object().unwrap()).is_none());
}

#[test]
fn pending_logins_are_single_use_and_browser_bound() {
    let rp = oidc(|_| {});
    let insert = |state: &str, binding: &str, ttl: Duration| {
        rp.remember_pending(
            state,
            Pending {
                binding: blake3::hash(binding.as_bytes()),
                nonce: "n".to_owned(),
                verifier: "v".to_owned(),
                expires: Instant::now() + ttl,
            },
        );
    };
    insert("s1", "b1", PENDING_TTL);
    assert!(rp.take_pending("s1", Some("b1")).is_ok());
    // Replay.
    assert!(rp.take_pending("s1", Some("b1")).is_err());
    // Other browser: rejected, and the state is gone afterwards.
    insert("s2", "b2", PENDING_TTL);
    assert!(rp.take_pending("s2", Some("other")).is_err());
    assert!(rp.take_pending("s2", Some("b2")).is_err());
    insert("s3", "b3", PENDING_TTL);
    assert!(rp.take_pending("s3", None).is_err());
    insert("s4", "b4", Duration::ZERO);
    assert!(rp.take_pending("s4", Some("b4")).is_err());

    // A flood of started logins evicts the oldest entries, but the
    // table stays bounded and new logins keep working.
    insert("victim", "bv", PENDING_TTL);
    for i in 0..MAX_PENDING {
        insert(
            &format!("flood{i}"),
            "bf",
            PENDING_TTL + Duration::from_secs(1),
        );
    }
    assert_eq!(rp.pending.lock().unwrap().len(), MAX_PENDING);
    assert!(rp.take_pending("victim", Some("bv")).is_err());
    insert("fresh", "bn", PENDING_TTL * 2);
    assert!(rp.take_pending("fresh", Some("bn")).is_ok());
}
