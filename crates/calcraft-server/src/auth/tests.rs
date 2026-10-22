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
        auth.reserve(&"alice".parse().unwrap(), client(1)).await,
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

impl Authenticator {
    /// Takes a slot without waiting, for tests.
    fn try_reserve_now(&self, username: &Username) -> Attempt<'_> {
        let mut failures = self.failures.lock().unwrap();
        assert!(matches!(
            try_reserve(&mut failures, username, false),
            Reservation::Reserved
        ));
        Attempt::new(self, username, None)
    }

    fn waiting(&self, username: &Username) -> u32 {
        let failures = self.failures.lock().unwrap();
        failures.get(username).map_or(0, |entry| entry.waiting)
    }
}

/// Runs `n` authentications of `header` at once.
async fn parallel(auth: &Arc<Authenticator>, header: &str, n: u32) -> Vec<AuthOutcome> {
    let handles: Vec<_> = (0..n)
        .map(|_| {
            let (auth, header) = (auth.clone(), header.to_owned());
            tokio::spawn(async move { auth.authenticate_test(Some(header.as_bytes())).await })
        })
        .collect();
    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(handle.await.unwrap());
    }
    outcomes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_parallel_requests_with_valid_credentials_are_not_locked_out() {
    let auth = setup().await;
    let header = basic("alice", "correct horse battery");
    // Not yet cached: more than the attempt limit, some of them wait.
    for outcome in parallel(&auth, &header, MAX_FAILURES + 2).await {
        assert!(
            matches!(outcome, AuthOutcome::Authenticated(_)),
            "{outcome:?}"
        );
    }
    // Cached: verified without taking an attempt slot.
    for outcome in parallel(&auth, &header, 8 * MAX_FAILURES).await {
        assert!(
            matches!(outcome, AuthOutcome::Authenticated(_)),
            "{outcome:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waiting_for_an_attempt_slot_is_capped_and_bounded() {
    let auth = setup().await;
    // Known and unknown names are limited alike.
    for name in ["alice", "nobody"] {
        let username: Username = name.parse().unwrap();
        let held: Vec<_> = (0..MAX_FAILURES)
            .map(|_| auth.try_reserve_now(&username))
            .collect();
        let waiters: Vec<_> = (0..MAX_WAITERS)
            .map(|_| {
                let (auth, username) = (auth.clone(), username.clone());
                tokio::spawn(async move { auth.reserve(&username, None).await.is_ok() })
            })
            .collect();
        while auth.waiting(&username) < MAX_WAITERS {
            tokio::task::yield_now().await;
        }
        // Queue full: denied at once.
        let start = Instant::now();
        assert!(auth.reserve(&username, None).await.is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        if name == "alice" {
            // One slot freed: the waiters get it one after another.
            drop(held);
            for waiter in waiters {
                assert!(waiter.await.unwrap());
            }
        } else {
            // No slot freed: every waiter gives up after MAX_WAIT.
            for waiter in waiters {
                assert!(!waiter.await.unwrap());
            }
            assert!(start.elapsed() >= MAX_WAIT - Duration::from_millis(100));
            drop(held);
        }
        assert!(auth.reserve(&username, None).await.is_ok());
        assert_eq!(auth.waiting(&username), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_requests_keep_their_hash_permit_until_hashing_ends() {
    let auth = setup().await;
    let tasks: Vec<_> = (0..MAX_CONCURRENT_HASHES)
        .map(|i| {
            let auth = auth.clone();
            let header = basic(&format!("ghost{i}"), "some wrong password");
            tokio::spawn(async move { auth.authenticate_test(Some(header.as_bytes())).await })
        })
        .collect();
    while auth.hashing.available_permits() > 0 {
        tokio::task::yield_now().await;
    }
    // Client disconnects or timeouts drop the request futures ...
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        assert!(task.await.unwrap_err().is_cancelled());
    }
    // ... but the Argon2 runs go on and keep the bound.
    assert_eq!(auth.hashing.available_permits(), 0);
    while auth.hashing.available_permits() < MAX_CONCURRENT_HASHES {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocked_clients_are_refused_before_waiting_and_waiters_still_count() {
    let auth = setup().await;
    auth.set_client_failure_limit(2, 100);
    let username: Username = "alice".parse().unwrap();
    let held: Vec<_> = (0..MAX_FAILURES)
        .map(|_| auth.try_reserve_now(&username))
        .collect();
    // A wrong password that had to wait for a slot still counts for its client.
    let wrong = basic("alice", "wrong password!!");
    let waiting = {
        let (auth, wrong) = (auth.clone(), wrong.clone());
        tokio::spawn(async move { auth.authenticate(Some(wrong.as_bytes()), client(7)).await })
    };
    while auth.waiting(&username) == 0 {
        tokio::task::yield_now().await;
    }
    drop(held);
    assert!(matches!(waiting.await.unwrap(), AuthOutcome::Denied));
    assert!(matches!(
        auth.authenticate(Some(wrong.as_bytes()), client(7)).await,
        AuthOutcome::Denied
    ));
    assert!(auth.client_blocked(client(7)));
    // Blocked: refused at once even while an account is busy, without
    // waiting or taking a slot.
    let busy: Username = "bob".parse().unwrap();
    let held: Vec<_> = (0..MAX_FAILURES)
        .map(|_| auth.try_reserve_now(&busy))
        .collect();
    let start = Instant::now();
    assert!(matches!(
        auth.reserve(&busy, client(7)).await,
        Err(Refusal::Client)
    ));
    assert_eq!(auth.waiting(&busy), 0);
    let good = basic("alice", "correct horse battery");
    assert!(matches!(
        auth.authenticate(Some(good.as_bytes()), client(7)).await,
        AuthOutcome::LockedOut
    ));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(auth.waiting(&username), 0);
    drop(held);
}

/// Starts a verification of `header` and cancels it while Argon2 runs, like
/// a client that hangs up or a request timeout.
async fn cancel_while_hashing(auth: &Arc<Authenticator>, header: &str, client: Option<ClientKey>) {
    while auth.hashing.available_permits() < MAX_CONCURRENT_HASHES {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let task = {
        let (auth, header) = (auth.clone(), header.to_owned());
        tokio::spawn(async move { auth.authenticate(Some(header.as_bytes()), client).await })
    };
    while auth.hashing.available_permits() == MAX_CONCURRENT_HASHES {
        tokio::task::yield_now().await;
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_verifications_still_count_as_failures() {
    let auth = setup().await;
    let wrong = basic("alice", "wrong password!!");
    for _ in 0..MAX_FAILURES {
        cancel_while_hashing(&auth, &wrong, None).await;
    }
    // Hanging up early did not get around the account lockout.
    let good = basic("alice", "correct horse battery");
    assert!(matches!(
        auth.authenticate_test(Some(good.as_bytes())).await,
        AuthOutcome::LockedOut
    ));

    // Nor around the client block, also for unknown names (which never
    // get an account entry).
    let auth = setup().await;
    auth.set_client_failure_limit(3, 100);
    for name in ["ghost1", "ghost2", "ghost3"] {
        cancel_while_hashing(&auth, &basic(name, "whatever password"), client(5)).await;
    }
    assert!(auth.client_blocked(client(5)));
    assert_eq!(auth.failures.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn requests_for_a_locked_account_count_for_the_client() {
    let auth = setup().await;
    auth.set_client_failure_limit(2, 100);
    let alice: Username = "alice".parse().unwrap();
    for _ in 0..MAX_FAILURES {
        auth.record_failure(alice.clone());
    }
    assert!(matches!(
        auth.reserve(&alice, client(8)).await,
        Err(Refusal::Account)
    ));
    // The second one blocks the client: from now on no dummy hash is spent.
    assert!(matches!(
        auth.reserve(&alice, client(8)).await,
        Err(Refusal::Client)
    ));
    assert!(auth.client_blocked(client(8)));
}

#[tokio::test]
async fn names_only_in_flight_leave_no_entries() {
    let auth = setup().await;
    for i in 0..MAX_TRACKED + 10 {
        let name: Username = format!("ghost{i}").parse().unwrap();
        let attempt = auth.reserve(&name, None).await.unwrap();
        if i % 2 == 0 {
            attempt.fail();
        }
    }
    assert_eq!(auth.failures.lock().unwrap().len(), 0);
    assert!(auth.reserve(&"fresh".parse().unwrap(), None).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_freed_slot_wakes_a_waiter() {
    let auth = setup().await;
    let username: Username = "alice".parse().unwrap();
    let mut held: Vec<_> = (0..MAX_FAILURES)
        .map(|_| auth.try_reserve_now(&username))
        .collect();
    let (acquired, mut slots) = tokio::sync::mpsc::channel(8);
    let done = Arc::new(Semaphore::new(0));
    for _ in 0..3 {
        let (auth, username, acquired, done) = (
            auth.clone(),
            username.clone(),
            acquired.clone(),
            done.clone(),
        );
        tokio::spawn(async move {
            let attempt = auth.reserve(&username, None).await.unwrap();
            acquired.send(()).await.unwrap();
            let _ = done.acquire().await;
            drop(attempt);
        });
    }
    while auth.waiting(&username) < 3 {
        tokio::task::yield_now().await;
    }
    // Two slots freed at once: two waiters get one each, the third waits.
    held.truncate(MAX_FAILURES as usize - 2);
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(1), slots.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), slots.recv())
            .await
            .is_err()
    );
    assert_eq!(auth.waiting(&username), 1);
    done.add_permits(3);
    drop(held);
    tokio::time::timeout(Duration::from_secs(1), slots.recv())
        .await
        .unwrap()
        .unwrap();
}
