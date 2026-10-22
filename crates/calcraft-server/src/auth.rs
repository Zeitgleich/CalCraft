//! Password authentication for DAV clients (HTTP Basic, RFC 7617).
//!
//! DAV clients send credentials with every request. To keep Argon2 from
//! becoming a denial-of-service lever, successful verifications are cached
//! for a short time under a keyed BLAKE3 hash (the key never leaves the
//! process), concurrent Argon2 runs are bounded, and repeated failures lock
//! the account for a while.
//!
//! Trade-off: anyone who knows a username can lock that account for the
//! lockout window by sending wrong passwords. This is accepted in favour of
//! stopping online guessing; per-source limits follow together with the
//! trusted-proxy configuration.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_core::Username;
use calcraft_store::{Store, User};

use crate::secrets::{hash_secret, hashes_equal};
use tokio::sync::{Notify, Semaphore};

/// Prefix of app passwords: `ccap_<id>_<secret>`.
pub const APP_PASSWORD_PREFIX: &str = "ccap_";

/// Current time in Unix seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Minimum length of new passwords.
pub const MIN_PASSWORD_LEN: usize = 12;
/// Longest accepted password; bounds hashing cost and header size.
pub const MAX_PASSWORD_LEN: usize = 1024;

const MAX_AUTH_HEADER_LEN: usize = 2048;
const CACHE_TTL: Duration = Duration::from_secs(300);
const MAX_FAILURES: u32 = 5;
const LOCKOUT_WINDOW: Duration = Duration::from_secs(15 * 60);
const MAX_TRACKED: usize = 10_000;
const MAX_CONCURRENT_HASHES: usize = 4;
/// Requests of one account that may wait for an attempt slot at once.
const MAX_WAITERS: u32 = 8;
/// How long a request waits for an attempt slot at most.
const MAX_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("password must be at least {MIN_PASSWORD_LEN} characters long")]
    TooShort,
    #[error("password must be at most {MAX_PASSWORD_LEN} bytes long")]
    TooLong,
    #[error("password must not start with {APP_PASSWORD_PREFIX} (reserved for app passwords)")]
    Reserved,
    #[error("password hashing failed")]
    Hash,
}

/// Hashes a new password with Argon2id (default parameters: 19 MiB, t=2, p=1).
pub fn hash_password(password: &str) -> Result<String, PasswordError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(PasswordError::TooShort);
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err(PasswordError::TooLong);
    }
    if password.starts_with(APP_PASSWORD_PREFIX) {
        return Err(PasswordError::Reserved);
    }
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|_| PasswordError::Hash)
}

/// [`hash_password`] on a blocking thread, for use in request handlers.
pub async fn hash_password_async(password: String) -> Result<String, PasswordError> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|_| PasswordError::Hash)?
}

/// Stored as password hash of accounts created by SSO: not a PHC string,
/// so no password ever matches. An administrator can set a real password
/// later (`calcraft user passwd`).
pub const UNUSABLE_PASSWORD_HASH: &str = "!sso-only";

/// Whether a stored hash can match any password.
pub fn has_usable_password(hash: &str) -> bool {
    PasswordHash::new(hash).is_ok()
}

fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).ok().is_some_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// Result of an authentication attempt.
#[derive(Debug)]
pub enum AuthOutcome {
    Authenticated(User),
    /// Missing, malformed or wrong credentials.
    Denied,
    /// Too many failed attempts for this account.
    LockedOut,
    /// Storage or hashing failure.
    Error,
}

/// Parsed `Authorization: Basic` credentials.
struct BasicCredentials {
    username: Username,
    password: String,
}

fn parse_basic(header: &[u8]) -> Option<BasicCredentials> {
    if header.len() > MAX_AUTH_HEADER_LEN {
        return None;
    }
    let (scheme, encoded) = header.split_at_checked(6)?;
    if !scheme.eq_ignore_ascii_case(b"basic ") {
        return None;
    }
    let decoded = STANDARD.decode(encoded.trim_ascii()).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    if password.is_empty() || password.len() > MAX_PASSWORD_LEN {
        return None;
    }
    Some(BasicCredentials {
        // Logins are case-insensitive; stored names are lowercase.
        username: username.to_ascii_lowercase().parse().ok()?,
        password: password.to_owned(),
    })
}

struct Failures {
    count: u32,
    /// Attempts currently being verified.
    in_flight: u32,
    /// Requests waiting for an attempt slot, see [`Authenticator::reserve`].
    waiting: u32,
    since: Instant,
    /// Wakes one waiter when an attempt ends.
    released: Arc<Notify>,
}

impl Failures {
    fn new() -> Self {
        Self {
            count: 0,
            in_flight: 0,
            waiting: 0,
            since: Instant::now(),
            released: Arc::new(Notify::new()),
        }
    }

    /// Nothing to remember: the entry can be dropped.
    fn idle(&self) -> bool {
        self.count == 0 && self.in_flight == 0 && self.waiting == 0
    }
}

struct CacheEntry {
    digest: blake3::Hash,
    expires: Instant,
}

/// Verifies credentials against the store.
pub struct Authenticator {
    store: Store,
    cache_key: [u8; 32],
    dummy_hash: String,
    /// Bounds concurrent Argon2 runs. Owned permits move into the blocking
    /// task, so a cancelled request keeps its permit until hashing ends.
    hashing: Arc<Semaphore>,
    cache: Mutex<HashMap<Username, CacheEntry>>,
    failures: Mutex<HashMap<Username, Failures>>,
    /// Account passwords accepted (`security.password_login`).
    password_login: AtomicBool,
}

impl Authenticator {
    /// Enables or disables login with account passwords (REST login and
    /// DAV Basic auth); app passwords are unaffected.
    pub fn set_password_login(&self, enabled: bool) {
        self.password_login.store(enabled, Ordering::Relaxed);
    }

    pub fn password_login(&self) -> bool {
        self.password_login.load(Ordering::Relaxed)
    }

    pub fn new(store: Store) -> anyhow::Result<Arc<Self>> {
        let mut cache_key = [0u8; 32];
        getrandom::fill(&mut cache_key)
            .map_err(|err| anyhow::anyhow!("no secure random source: {err}"))?;
        // Unknown users are checked against this hash so that the response
        // time does not reveal whether an account exists.
        let mut dummy = [0u8; 24];
        getrandom::fill(&mut dummy)
            .map_err(|err| anyhow::anyhow!("no secure random source: {err}"))?;
        let dummy_hash = hash_password(&STANDARD.encode(dummy))?;
        Ok(Arc::new(Self {
            store,
            cache_key,
            dummy_hash,
            hashing: Arc::new(Semaphore::new(MAX_CONCURRENT_HASHES)),
            cache: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            password_login: AtomicBool::new(true),
        }))
    }

    /// Authenticates the `Authorization` header of a DAV request.
    ///
    /// DAV clients may use the account password only while two-factor
    /// authentication is off; with 2FA they must use an app password.
    pub async fn authenticate(&self, header: Option<&[u8]>) -> AuthOutcome {
        let Some(credentials) = header.and_then(parse_basic) else {
            return AuthOutcome::Denied;
        };
        let username = &credentials.username;
        // Valid credentials that need no Argon2 run (an app password, or an
        // account password verified recently) take no attempt slot, so the
        // parallel requests of a busy client never queue or get denied.
        // Anything else takes the counted path below.
        if !self.is_locked(username)
            && let Some(user) = self.verify_cheaply(&credentials).await
        {
            self.clear_failures(username);
            return AuthOutcome::Authenticated(user);
        }
        let Some(attempt) = self.reserve(username).await else {
            return self.locked_out().await;
        };
        if let Some((id, secret)) = parse_app_password(&credentials.password) {
            return self.check_app_password(&attempt, id, secret).await;
        }
        if !self.password_login() {
            return AuthOutcome::Denied;
        }
        match self
            .check_first_factor_reserved(&attempt, &credentials.password)
            .await
        {
            AuthOutcome::Authenticated(user) if user.totp_enabled => AuthOutcome::Denied,
            AuthOutcome::Authenticated(user) => {
                self.clear_failures(username);
                AuthOutcome::Authenticated(user)
            }
            outcome => outcome,
        }
    }

    /// Reserves a login attempt for `username`. Attempts in progress count
    /// towards the limit together with recorded failures, so concurrent
    /// requests cannot bypass the lockout. Returns `None` when locked; the
    /// caller then answers like a wrong password ([`Self::locked_out`]).
    ///
    /// When only attempts in progress fill the limit, this waits until one
    /// of them ends instead of failing: a client sending more than
    /// `MAX_FAILURES` requests at once with correct credentials (sync
    /// clients do) must not get 401 for its own parallel requests. Waiting
    /// is bounded: at most `MAX_WAITERS` requests per account name (known
    /// or not) wait, each for at most `MAX_WAIT`; beyond that it is `None`.
    pub async fn reserve(&self, username: &Username) -> Option<Attempt<'_>> {
        let deadline = tokio::time::Instant::now() + MAX_WAIT;
        let mut waiter: Option<Waiter<'_>> = None;
        loop {
            let released = {
                let Ok(mut failures) = self.failures.lock() else {
                    return None;
                };
                match try_reserve(&mut failures, username, waiter.is_some()) {
                    Reservation::Reserved => {
                        drop(failures);
                        // Leave the queue only after the slot is taken.
                        drop(waiter);
                        return Some(Attempt {
                            auth: self,
                            username: username.clone(),
                        });
                    }
                    Reservation::Locked => return None,
                    Reservation::Busy(released) => released,
                }
            };
            if waiter.is_none() {
                waiter = Some(Waiter {
                    auth: self,
                    username: username.clone(),
                });
            }
            // `notify_one` keeps a wake-up for the next waiter if nobody
            // waits yet, so a release right here is not missed.
            if tokio::time::timeout_at(deadline, released.notified())
                .await
                .is_err()
            {
                return None;
            }
        }
    }

    /// Response for a locked account. It does the same work as a wrong
    /// password, so neither the status nor the timing reveals the lockout
    /// (and with it, whether the account exists).
    pub async fn locked_out(&self) -> AuthOutcome {
        let hash = self.dummy_hash.clone();
        if let Ok(permit) = self.hashing.clone().acquire_owned().await {
            let _ = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                verify_password("locked", &hash)
            })
            .await;
        }
        tracing::debug!("login attempt for a locked account");
        AuthOutcome::LockedOut
    }

    /// Whether `username` has too many recorded failures (attempts in
    /// progress do not count here).
    fn is_locked(&self, username: &Username) -> bool {
        let Ok(failures) = self.failures.lock() else {
            return true;
        };
        failures.get(username).is_some_and(|entry| {
            entry.count >= MAX_FAILURES && entry.since.elapsed() < LOCKOUT_WINDOW
        })
    }

    /// The user, if the credentials are valid and can be checked without
    /// Argon2: an app password, or an account password in the cache.
    /// `None` also for storage errors; the counted path reports those.
    async fn verify_cheaply(&self, credentials: &BasicCredentials) -> Option<User> {
        let username = &credentials.username;
        if let Some((id, secret)) = parse_app_password(&credentials.password) {
            return self.app_password_user(id, secret, username).await.ok()?;
        }
        if !self.password_login() {
            return None;
        }
        let record = self.store.user_credentials(username).await.ok()??;
        let digest = self.cache_digest(&credentials.password, &record.password_hash);
        let user = record.user;
        (self.cache_hit(username, &digest) && !user.disabled && !user.totp_enabled).then_some(user)
    }

    /// Cache key of a verified password, bound to the stored hash so that it
    /// becomes invalid as soon as the password changes.
    fn cache_digest(&self, password: &str, password_hash: &str) -> blake3::Hash {
        blake3::Hasher::new_keyed(&self.cache_key)
            .update(password.as_bytes())
            .update(&[0xff])
            .update(password_hash.as_bytes())
            .finalize()
    }

    /// The enabled owner of a valid app password sent with `username`, and
    /// records its use. `Err` on storage errors (logged).
    async fn app_password_user(
        &self,
        id: i64,
        secret: &str,
        username: &Username,
    ) -> Result<Option<User>, ()> {
        let stored = self.store.app_password_secret(id).await.map_err(|err| {
            tracing::error!(%err, "failed to load app password");
        })?;
        let Some((user_id, hash)) = stored else {
            return Ok(None);
        };
        let user = self.store.user_by_id(user_id).await.map_err(|err| {
            tracing::error!(%err, "failed to load user");
        })?;
        // The app password must belong to the user name it is sent with.
        let user = user.filter(|user| {
            hashes_equal(&hash_secret(secret), &hash)
                && &user.username == username
                && !user.disabled
        });
        if user.is_some()
            && let Err(err) = self.store.touch_app_password(id, now()).await
        {
            tracing::warn!(%err, "failed to record app password use");
        }
        Ok(user)
    }

    /// Verifies an app password (`ccap_<id>_<secret>`) of the reserved user.
    async fn check_app_password(
        &self,
        attempt: &Attempt<'_>,
        id: i64,
        secret: &str,
    ) -> AuthOutcome {
        let username = &attempt.username;
        match self.app_password_user(id, secret, username).await {
            Err(()) => AuthOutcome::Error,
            Ok(Some(user)) => {
                self.clear_failures(username);
                AuthOutcome::Authenticated(user)
            }
            Ok(None) => {
                // Count only for existing accounts (see `check_first_factor_reserved`).
                if matches!(self.store.user_credentials(username).await, Ok(Some(_))) {
                    self.record_failure(username.clone());
                }
                AuthOutcome::Denied
            }
        }
    }

    /// Verifies the account password of a reserved attempt, with cache,
    /// without resetting the failure counter: with 2FA the counter must also
    /// cover wrong second factors, so callers reset it only after full
    /// success.
    pub async fn check_first_factor_reserved(
        &self,
        attempt: &Attempt<'_>,
        password: &str,
    ) -> AuthOutcome {
        let username = attempt.username.clone();
        let record = match self.store.user_credentials(&username).await {
            Ok(record) => record,
            Err(err) => {
                tracing::error!(%err, "failed to load user");
                return AuthOutcome::Error;
            }
        };

        // Binding the cache entry to the stored hash invalidates it as soon
        // as the password changes.
        let digest = record
            .as_ref()
            .map(|record| self.cache_digest(password, &record.password_hash));
        if let (Some(record), Some(digest)) = (&record, &digest)
            && self.cache_hit(&username, digest)
        {
            if record.user.disabled {
                return AuthOutcome::Denied;
            }
            return AuthOutcome::Authenticated(record.user.clone());
        }

        // Accounts without a usable password (SSO only) are checked against
        // the dummy hash, so they take as long as a wrong password.
        let usable = record
            .as_ref()
            .is_some_and(|r| has_usable_password(&r.password_hash));
        let hash = match &record {
            Some(record) if usable => record.password_hash.clone(),
            _ => self.dummy_hash.clone(),
        };
        let Ok(permit) = self.hashing.clone().acquire_owned().await else {
            return AuthOutcome::Error;
        };
        let password = password.to_owned();
        let valid = match tokio::task::spawn_blocking(move || {
            let _permit = permit;
            verify_password(&password, &hash)
        })
        .await
        {
            Ok(valid) => valid && usable,
            Err(err) => {
                tracing::error!(%err, "password verification task failed");
                return AuthOutcome::Error;
            }
        };

        match record {
            Some(record) if valid && !record.user.disabled => {
                if let Some(digest) = digest {
                    self.remember(username, digest);
                }
                AuthOutcome::Authenticated(record.user)
            }
            // Failures are only counted for existing accounts; entries of
            // unknown names disappear when their attempt ends, so made-up
            // names cannot fill the table.
            Some(_) => {
                self.record_failure(username);
                AuthOutcome::Denied
            }
            None => AuthOutcome::Denied,
        }
    }

    fn cache_hit(&self, username: &Username, digest: &blake3::Hash) -> bool {
        let Ok(cache) = self.cache.lock() else {
            return false;
        };
        // `blake3::Hash` compares in constant time.
        cache
            .get(username)
            .is_some_and(|entry| entry.expires > Instant::now() && entry.digest == *digest)
    }

    fn remember(&self, username: Username, digest: blake3::Hash) {
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        let now = Instant::now();
        if cache.len() >= MAX_TRACKED {
            cache.retain(|_, entry| entry.expires > now);
        }
        if cache.len() < MAX_TRACKED {
            cache.insert(
                username,
                CacheEntry {
                    digest,
                    expires: now + CACHE_TTL,
                },
            );
        }
    }

    /// Records a failed attempt of an existing account.
    pub fn record_failure(&self, username: Username) {
        let Ok(mut failures) = self.failures.lock() else {
            return;
        };
        let entry = failures.entry(username).or_insert_with(Failures::new);
        if entry.since.elapsed() >= LOCKOUT_WINDOW {
            entry.count = 0;
            entry.since = Instant::now();
        }
        entry.count += 1;
        if entry.count == MAX_FAILURES {
            tracing::warn!("account locked after repeated failed logins");
            // Waiters are denied now and need not wait any longer.
            entry.released.notify_waiters();
        }
    }

    /// Resets the failure count after a fully successful login.
    pub fn clear_failures(&self, username: &Username) {
        if let Ok(mut failures) = self.failures.lock()
            && let Some(entry) = failures.get_mut(username)
        {
            entry.count = 0;
            if entry.idle() {
                failures.remove(username);
            }
        }
    }

    fn release(&self, username: &Username) {
        if let Ok(mut failures) = self.failures.lock()
            && let Some(entry) = failures.get_mut(username)
        {
            entry.in_flight = entry.in_flight.saturating_sub(1);
            // One slot is free: wake one waiter (a waiter that is dropped
            // after being woken passes the wake-up on).
            entry.released.notify_one();
            if entry.idle() {
                failures.remove(username);
            }
        }
    }
}

#[cfg(test)]
impl Authenticator {
    /// Takes a slot without waiting, for tests.
    fn try_reserve_now(&self, username: &Username) -> Attempt<'_> {
        let mut failures = self.failures.lock().unwrap();
        assert!(matches!(
            try_reserve(&mut failures, username, false),
            Reservation::Reserved
        ));
        Attempt {
            auth: self,
            username: username.clone(),
        }
    }

    fn waiting(&self, username: &Username) -> u32 {
        let failures = self.failures.lock().unwrap();
        failures.get(username).map_or(0, |entry| entry.waiting)
    }
}

/// Result of [`try_reserve`].
enum Reservation {
    Reserved,
    /// Too many recorded failures, waiters or tracked names.
    Locked,
    /// The limit is filled by attempts still in progress: wait for this.
    Busy(Arc<Notify>),
}

/// Takes an attempt slot if possible. `queued`: the caller already counts
/// as a waiter of this account name.
fn try_reserve(
    failures: &mut HashMap<Username, Failures>,
    username: &Username,
    queued: bool,
) -> Reservation {
    if failures.len() >= MAX_TRACKED {
        failures
            .retain(|_, f| f.in_flight > 0 || f.waiting > 0 || f.since.elapsed() < LOCKOUT_WINDOW);
        if failures.len() >= MAX_TRACKED && !failures.contains_key(username) {
            // Degrade safely under extreme load.
            return Reservation::Locked;
        }
    }
    let entry = failures
        .entry(username.clone())
        .or_insert_with(Failures::new);
    if entry.since.elapsed() >= LOCKOUT_WINDOW && entry.in_flight == 0 {
        entry.count = 0;
        entry.since = Instant::now();
    }
    if entry.count >= MAX_FAILURES {
        return Reservation::Locked;
    }
    if entry.count + entry.in_flight >= MAX_FAILURES {
        if !queued {
            if entry.waiting >= MAX_WAITERS {
                return Reservation::Locked;
            }
            entry.waiting += 1;
        }
        return Reservation::Busy(entry.released.clone());
    }
    entry.in_flight += 1;
    Reservation::Reserved
}

/// A request waiting in [`Authenticator::reserve`]; leaves the queue when
/// dropped, also when the request is cancelled.
struct Waiter<'a> {
    auth: &'a Authenticator,
    username: Username,
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        if let Ok(mut failures) = self.auth.failures.lock()
            && let Some(entry) = failures.get_mut(&self.username)
        {
            entry.waiting = entry.waiting.saturating_sub(1);
            if entry.idle() {
                failures.remove(&self.username);
            }
        }
    }
}

/// A reserved login attempt; released when dropped.
pub struct Attempt<'a> {
    auth: &'a Authenticator,
    username: Username,
}

impl Attempt<'_> {
    pub fn username(&self) -> &Username {
        &self.username
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        self.auth.release(&self.username);
    }
}

/// Splits an app password `ccap_<id>_<secret>` with a 32-character Base32
/// secret. Anything else is treated as an account password.
fn parse_app_password(password: &str) -> Option<(i64, &str)> {
    let (id, secret) = password
        .strip_prefix(APP_PASSWORD_PREFIX)?
        .split_once('_')?;
    let valid_secret = secret.len() == 32
        && secret
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
    if !valid_secret || id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((id.parse().ok()?, secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic(user: &str, password: &str) -> String {
        format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
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
                auth.authenticate(Some(header.as_bytes())).await,
                AuthOutcome::Authenticated(user) if user.username.as_str() == "alice"
            ));
        }
    }

    /// Runs `n` authentications of `header` at once.
    async fn parallel(auth: &Arc<Authenticator>, header: &str, n: u32) -> Vec<AuthOutcome> {
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let (auth, header) = (auth.clone(), header.to_owned());
                tokio::spawn(async move { auth.authenticate(Some(header.as_bytes())).await })
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
                    tokio::spawn(async move { auth.reserve(&username).await.is_some() })
                })
                .collect();
            while auth.waiting(&username) < MAX_WAITERS {
                tokio::task::yield_now().await;
            }
            // Queue full: denied at once.
            let start = Instant::now();
            assert!(auth.reserve(&username).await.is_none());
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
            assert!(auth.reserve(&username).await.is_some());
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
                tokio::spawn(async move { auth.authenticate(Some(header.as_bytes())).await })
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

    #[tokio::test]
    async fn rejects_wrong_or_missing_credentials() {
        let auth = setup().await;
        assert!(matches!(auth.authenticate(None).await, AuthOutcome::Denied));
        let wrong = basic("alice", "wrong password!!");
        assert!(matches!(
            auth.authenticate(Some(wrong.as_bytes())).await,
            AuthOutcome::Denied
        ));
        let unknown = basic("mallory", "correct horse battery");
        assert!(matches!(
            auth.authenticate(Some(unknown.as_bytes())).await,
            AuthOutcome::Denied
        ));
    }

    #[tokio::test]
    async fn cached_login_does_not_accept_other_passwords() {
        let auth = setup().await;
        let good = basic("alice", "correct horse battery");
        let bad = basic("alice", "correct horse batterx");
        assert!(matches!(
            auth.authenticate(Some(good.as_bytes())).await,
            AuthOutcome::Authenticated(_)
        ));
        assert!(matches!(
            auth.authenticate(Some(bad.as_bytes())).await,
            AuthOutcome::Denied
        ));
    }

    #[tokio::test]
    async fn locks_out_after_repeated_failures() {
        let auth = setup().await;
        let wrong = basic("alice", "wrong password!!");
        for _ in 0..MAX_FAILURES {
            auth.authenticate(Some(wrong.as_bytes())).await;
        }
        let good = basic("alice", "correct horse battery");
        assert!(matches!(
            auth.authenticate(Some(good.as_bytes())).await,
            AuthOutcome::LockedOut
        ));
    }

    #[tokio::test]
    async fn password_change_invalidates_cache() {
        let auth = setup().await;
        let old = basic("alice", "correct horse battery");
        assert!(matches!(
            auth.authenticate(Some(old.as_bytes())).await,
            AuthOutcome::Authenticated(_)
        ));
        let hash = hash_password("a brand new password").unwrap();
        auth.store
            .set_password_hash(&"alice".parse().unwrap(), &hash)
            .await
            .unwrap();
        assert!(matches!(
            auth.authenticate(Some(old.as_bytes())).await,
            AuthOutcome::Denied
        ));
    }

    #[tokio::test]
    async fn unknown_usernames_cannot_disable_the_lockout() {
        let auth = setup().await;
        // Flooding with made-up names must not stop counting for real accounts.
        for i in 0..50 {
            let fake = basic(&format!("ghost{i}"), "whatever password");
            auth.authenticate(Some(fake.as_bytes())).await;
        }
        assert!(auth.failures.lock().unwrap().is_empty());
        let wrong = basic("alice", "wrong password!!");
        for _ in 0..MAX_FAILURES {
            auth.authenticate(Some(wrong.as_bytes())).await;
        }
        let good = basic("alice", "correct horse battery");
        assert!(matches!(
            auth.authenticate(Some(good.as_bytes())).await,
            AuthOutcome::LockedOut
        ));
    }

    #[tokio::test]
    async fn disabled_users_are_denied() {
        let auth = setup().await;
        let good = basic("alice", "correct horse battery");
        assert!(matches!(
            auth.authenticate(Some(good.as_bytes())).await,
            AuthOutcome::Authenticated(_)
        ));
        auth.store
            .set_disabled(&"alice".parse().unwrap(), true)
            .await
            .unwrap();
        assert!(matches!(
            auth.authenticate(Some(good.as_bytes())).await,
            AuthOutcome::Denied
        ));
    }
}
