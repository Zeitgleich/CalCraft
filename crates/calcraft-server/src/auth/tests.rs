use super::*;

fn basic(user: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

impl Authenticator {
    async fn authenticate_test(&self, header: Option<&[u8]>) -> AuthOutcome {
        self.authenticate(header, None).await
    }
}

fn client(n: u8) -> Option<ClientKey> {
    Some(ClientKey::new(std::net::IpAddr::from([192, 0, 2, n]), 64))
}

#[tokio::test]
async fn clients_are_blocked_after_repeated_failures() {
    let auth = setup().await;
    auth.set_client_failure_limit(3, 100);
    let good = basic("alice", "correct horse battery");
    // Unknown names count for the client, too.
    for name in ["ghost1", "ghost2", "ghost3"] {
        let fake = basic(name, "whatever password");
        assert!(matches!(
            auth.authenticate(Some(fake.as_bytes()), client(1)).await,
            AuthOutcome::Denied
        ));
    }
    // The account itself is not locked: another client logs in fine.
    assert!(matches!(
        auth.authenticate(Some(good.as_bytes()), client(2)).await,
        AuthOutcome::Authenticated(_)
    ));
    // The blocked client is refused even with the right password, and
    // the answer is the same as for a locked account.
    assert!(matches!(
        auth.authenticate(Some(good.as_bytes()), client(1)).await,
        AuthOutcome::LockedOut
    ));
    // A success elsewhere does not reset the client's counter.
    assert!(matches!(
        auth.reserve(&"alice".parse().unwrap(), client(1)),
        Err(Refusal::Client)
    ));
    assert!(auth.client_blocked(client(1)));
    assert!(!auth.client_blocked(client(2)) && !auth.client_blocked(None));
    // Requests without a known client only use the account lockout.
    assert!(matches!(
        auth.authenticate(Some(good.as_bytes()), None).await,
        AuthOutcome::Authenticated(_)
    ));
}

async fn setup() -> Arc<Authenticator> {
    let store = Store::in_memory().await.unwrap();
    let hash = hash_password("correct horse battery").unwrap();
    store
        .create_user(&"alice".parse().unwrap(), &hash, None)
        .await
        .unwrap();
    Authenticator::new(store).unwrap()
}

#[test]
fn password_policy() {
    assert!(matches!(
        hash_password("short"),
        Err(PasswordError::TooShort)
    ));
    assert!(matches!(
        hash_password(&"x".repeat(MAX_PASSWORD_LEN + 1)),
        Err(PasswordError::TooLong)
    ));
    let hash = hash_password("a long enough password").unwrap();
    assert!(hash.starts_with("$argon2id$"));
    assert!(verify_password("a long enough password", &hash));
    assert!(!verify_password("a long enough passwore", &hash));
    assert!(!verify_password("x", "not a hash"));
}

#[test]
fn parses_basic_header() {
    let creds = parse_basic(basic("Alice", "pw:with:colons").as_bytes()).unwrap();
    assert_eq!(creds.username.as_str(), "alice");
    assert_eq!(creds.password, "pw:with:colons");
    assert!(parse_basic(b"Bearer abc").is_none());
    assert!(parse_basic(b"Basic !!!").is_none());
    assert!(parse_basic(basic("../etc", "x").as_bytes()).is_none());
    assert!(parse_basic(basic("alice", "").as_bytes()).is_none());
    assert!(parse_basic(b"Basic").is_none());
}

#[tokio::test]
async fn authenticates_valid_credentials() {
    let auth = setup().await;
    let header = basic("alice", "correct horse battery");
    for _ in 0..2 {
        // Second round hits the cache.
        assert!(matches!(
            auth.authenticate_test(Some(header.as_bytes())).await,
            AuthOutcome::Authenticated(user) if user.username.as_str() == "alice"
        ));
    }
}

#[tokio::test]
async fn rejects_wrong_or_missing_credentials() {
    let auth = setup().await;
    assert!(matches!(
        auth.authenticate_test(None).await,
        AuthOutcome::Denied
    ));
    let wrong = basic("alice", "wrong password!!");
    assert!(matches!(
        auth.authenticate_test(Some(wrong.as_bytes())).await,
        AuthOutcome::Denied
    ));
    let unknown = basic("mallory", "correct horse battery");
    assert!(matches!(
        auth.authenticate_test(Some(unknown.as_bytes())).await,
        AuthOutcome::Denied
    ));
}

#[tokio::test]
async fn cached_login_does_not_accept_other_passwords() {
    let auth = setup().await;
    let good = basic("alice", "correct horse battery");
    let bad = basic("alice", "correct horse batterx");
    assert!(matches!(
        auth.authenticate_test(Some(good.as_bytes())).await,
        AuthOutcome::Authenticated(_)
    ));
    assert!(matches!(
        auth.authenticate_test(Some(bad.as_bytes())).await,
        AuthOutcome::Denied
    ));
}

#[tokio::test]
async fn locks_out_after_repeated_failures() {
    let auth = setup().await;
    let wrong = basic("alice", "wrong password!!");
    for _ in 0..MAX_FAILURES {
        auth.authenticate_test(Some(wrong.as_bytes())).await;
    }
    let good = basic("alice", "correct horse battery");
    assert!(matches!(
        auth.authenticate_test(Some(good.as_bytes())).await,
        AuthOutcome::LockedOut
    ));
}

#[tokio::test]
async fn password_change_invalidates_cache() {
    let auth = setup().await;
    let old = basic("alice", "correct horse battery");
    assert!(matches!(
        auth.authenticate_test(Some(old.as_bytes())).await,
        AuthOutcome::Authenticated(_)
    ));
    let hash = hash_password("a brand new password").unwrap();
    auth.store
        .set_password_hash(&"alice".parse().unwrap(), &hash)
        .await
        .unwrap();
    assert!(matches!(
        auth.authenticate_test(Some(old.as_bytes())).await,
        AuthOutcome::Denied
    ));
}

#[tokio::test]
async fn unknown_usernames_cannot_disable_the_lockout() {
    let auth = setup().await;
    // Flooding with made-up names must not stop counting for real accounts.
    for i in 0..50 {
        let fake = basic(&format!("ghost{i}"), "whatever password");
        auth.authenticate_test(Some(fake.as_bytes())).await;
    }
    assert!(auth.failures.lock().unwrap().is_empty());
    let wrong = basic("alice", "wrong password!!");
    for _ in 0..MAX_FAILURES {
        auth.authenticate_test(Some(wrong.as_bytes())).await;
    }
    let good = basic("alice", "correct horse battery");
    assert!(matches!(
        auth.authenticate_test(Some(good.as_bytes())).await,
        AuthOutcome::LockedOut
    ));
}

#[tokio::test]
async fn disabled_users_are_denied() {
    let auth = setup().await;
    let good = basic("alice", "correct horse battery");
    assert!(matches!(
        auth.authenticate_test(Some(good.as_bytes())).await,
        AuthOutcome::Authenticated(_)
    ));
    auth.store
        .set_disabled(&"alice".parse().unwrap(), true)
        .await
        .unwrap();
    assert!(matches!(
        auth.authenticate_test(Some(good.as_bytes())).await,
        AuthOutcome::Denied
    ));
}
