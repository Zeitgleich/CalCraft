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
//! stopping online guessing.
//!
//! In addition, failed logins are counted per client address
//! (`server.max_login_failures_per_client` within the lockout window, for
//! known and unknown usernames alike). A blocked client gets the same
//! answer as for wrong credentials, without spending a password hash: the
//! block concerns the client, not an account, so it reveals nothing about
//! accounts. Successful logins do not reset this counter, so one valid
//! account cannot be used to keep guessing others.

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

use crate::{
    limits::FailureTable,
    net::ClientKey,
    secrets::{hash_secret, hashes_equal},
};
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
/// Requests of one account name that may wait for an attempt slot at once.
const MAX_WAITERS: u32 = 8;
/// How long a request waits for an attempt slot at most.
const MAX_WAIT: Duration = Duration::from_secs(5);
/// Default of `server.max_login_failures_per_client`.
const MAX_CLIENT_FAILURES: u32 = 20;

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
    /// Random input for the dummy verification of locked accounts.
    dummy_guess: String,
    /// Bounds concurrent Argon2 runs. Owned permits move into the blocking
    /// task, so a cancelled request keeps its permit until hashing ends.
    hashing: Arc<Semaphore>,
    cache: Mutex<HashMap<Username, CacheEntry>>,
    failures: Mutex<HashMap<Username, Failures>>,
    /// Failed logins per client address.
    client_failures: Mutex<FailureTable>,
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

    /// Sets the failed logins allowed per client within the lockout window
    /// and the most clients tracked (`server.max_login_failures_per_client`,
    /// `server.max_tracked_clients`).
    pub fn set_client_failure_limit(&self, max_failures: u32, max_clients: usize) {
        if let Ok(mut table) = self.client_failures.lock() {
            table.configure(max_clients, max_failures);
        }
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
        getrandom::fill(&mut dummy)
            .map_err(|err| anyhow::anyhow!("no secure random source: {err}"))?;
        let dummy_guess = STANDARD.encode(dummy);
        Ok(Arc::new(Self {
            store,
            cache_key,
            dummy_hash,
            dummy_guess,
            hashing: Arc::new(Semaphore::new(MAX_CONCURRENT_HASHES)),
            cache: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            client_failures: Mutex::new(FailureTable::new(
                MAX_TRACKED,
                MAX_CLIENT_FAILURES,
                LOCKOUT_WINDOW,
            )),
            password_login: AtomicBool::new(true),
        }))
    }

    /// Authenticates the `Authorization` header of a DAV request from
    /// `client` (`None` if unknown: no per-client counting).
    ///
    /// DAV clients may use the account password only while two-factor
    /// authentication is off; with 2FA they must use an app password.
    pub async fn authenticate(
        &self,
        header: Option<&[u8]>,
        client: Option<ClientKey>,
    ) -> AuthOutcome {
        let Some(credentials) = header.and_then(parse_basic) else {
            return AuthOutcome::Denied;
        };
        let username = &credentials.username;
        // A blocked client is refused before anything else: no attempt
        // slot, no waiting, no Argon2 (see `reserve`).
        if self.client_blocked(client) {
            return self.refused(Refusal::Client).await;
        }
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
        let attempt = match self.reserve(username, client).await {
            Ok(attempt) => attempt,
            Err(refusal) => return self.refused(refusal).await,
        };
        if let Some((id, secret)) = parse_app_password(&credentials.password) {
            return self.check_app_password(&attempt, id, secret).await;
        }
        if !self.password_login() {
            attempt.settle();
            return AuthOutcome::Denied;
        }
        match self
            .check_first_factor_reserved(&attempt, &credentials.password)
            .await
        {
            // The right password, but DAV clients need an app password with 2FA.
            AuthOutcome::Authenticated(user) if user.totp_enabled => {
                attempt.settle();
                AuthOutcome::Denied
            }
            AuthOutcome::Authenticated(user) => {
                self.clear_failures(username);
                attempt.settle();
                AuthOutcome::Authenticated(user)
            }
            outcome => outcome,
        }
    }

    /// Reserves a login attempt for `username` from `client`. Attempts in
    /// progress count towards the account limit together with recorded
    /// failures, so concurrent requests cannot bypass the lockout. Fails
    /// when the client or the account is blocked; a blocked client is
    /// refused at once, without taking a slot or waiting.
    ///
    /// When only attempts in progress fill the account limit, this waits
    /// until one of them ends instead of failing: a client sending more than
    /// `MAX_FAILURES` requests at once with correct credentials (sync
    /// clients do) must not get 401 for its own parallel requests. Waiting
    /// is bounded: at most `MAX_WAITERS` requests per account name (known
    /// or not) wait, each for at most `MAX_WAIT`; beyond that the account
    /// refusal applies. The client block is checked again after waiting.
    pub async fn reserve(
        &self,
        username: &Username,
        client: Option<ClientKey>,
    ) -> Result<Attempt<'_>, Refusal> {
        let deadline = tokio::time::Instant::now() + MAX_WAIT;
        let mut waiter: Option<Waiter<'_>> = None;
        loop {
            if self.client_blocked(client) {
                return Err(Refusal::Client);
            }
            let released = {
                let Ok(mut failures) = self.failures.lock() else {
                    return Err(Refusal::Account);
                };
                match try_reserve(&mut failures, username, waiter.is_some()) {
                    Reservation::Reserved => {
                        drop(failures);
                        // Leave the queue only after the slot is taken.
                        drop(waiter);
                        return Ok(Attempt::new(self, username, client));
                    }
                    Reservation::Locked => {
                        drop(failures);
                        // Requests for a locked account are guesses, too. Once
                        // the client is blocked, it gets the client refusal
                        // (no dummy hash), so it cannot keep the hash permits
                        // busy.
                        self.record_client_failure(client);
                        return Err(if self.client_blocked(client) {
                            Refusal::Client
                        } else {
                            Refusal::Account
                        });
                    }
                    Reservation::Full => return Err(Refusal::Account),
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
                return Err(Refusal::Account);
            }
        }
    }

    /// Response for a refused attempt; looks like wrong credentials.
    pub async fn refused(&self, refusal: Refusal) -> AuthOutcome {
        match refusal {
            Refusal::Account => self.locked_out().await,
            Refusal::Client => {
                tracing::debug!("login attempt from a client with too many failed logins");
                AuthOutcome::LockedOut
            }
        }
    }

    /// Response for a locked account. It does the same work as a wrong
    /// password, so neither the status nor the timing reveals the lockout
    /// (and with it, whether the account exists).
    pub async fn locked_out(&self) -> AuthOutcome {
        let hash = self.dummy_hash.clone();
        let guess = self.dummy_guess.clone();
        if let Ok(permit) = self.hashing.clone().acquire_owned().await {
            let _ = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                verify_password(&guess, &hash)
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
            Err(()) => {
                attempt.settle();
                AuthOutcome::Error
            }
            Ok(Some(user)) => {
                self.clear_failures(username);
                attempt.settle();
                AuthOutcome::Authenticated(user)
            }
            Ok(None) => {
                // Count only for existing accounts (see `check_first_factor_reserved`).
                if matches!(self.store.user_credentials(username).await, Ok(Some(_))) {
                    attempt.mark_known();
                }
                attempt.fail();
                AuthOutcome::Denied
            }
        }
    }

    /// Verifies the account password of a reserved attempt, with cache,
    /// without resetting the failure counter: with 2FA the counter must also
    /// cover wrong second factors, so callers reset it only after full
    /// success. A wrong password settles the attempt as a failure; after
    /// success the caller settles it ([`Attempt::settle`] or
    /// [`Attempt::fail`]).
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
                attempt.settle();
                return AuthOutcome::Error;
            }
        };
        if record.is_some() {
            // From here on, a cancelled request counts for the account too.
            attempt.mark_known();
        }

        // Binding the cache entry to the stored hash invalidates it as soon
        // as the password changes.
        let digest = record
            .as_ref()
            .map(|record| self.cache_digest(password, &record.password_hash));
        if let (Some(record), Some(digest)) = (&record, &digest)
            && self.cache_hit(&username, digest)
        {
            if record.user.disabled {
                attempt.settle();
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
            attempt.settle();
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
                attempt.settle();
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
            Some(_) | None => {
                attempt.fail();
                AuthOutcome::Denied
            }
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

    /// Whether `client` has used up its failed logins (`false` if unknown).
    pub fn client_blocked(&self, client: Option<ClientKey>) -> bool {
        client.is_some_and(|client| {
            self.client_failures
                .lock()
                .map_or(true, |table| table.is_blocked(client, Instant::now()))
        })
    }

    /// Records a failed attempt of `client` (known or unknown username).
    pub fn record_client_failure(&self, client: Option<ClientKey>) {
        let Some(client) = client else {
            return;
        };
        if let Ok(mut table) = self.client_failures.lock()
            && table.record(client, Instant::now())
        {
            tracing::warn!(%client, "client blocked after repeated failed logins");
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

/// Result of [`try_reserve`].
enum Reservation {
    Reserved,
    /// Too many recorded failures: the account is locked.
    Locked,
    /// Too many waiters or tracked names.
    Full,
    /// The limit is filled by attempts still in progress: wait for this.
    Busy(Arc<Notify>),
}

/// Takes an attempt slot of an account name if possible. `queued`: the
/// caller already counts as a waiter of this name.
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
            return Reservation::Full;
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
                return Reservation::Full;
            }
            entry.waiting += 1;
        }
        return Reservation::Busy(entry.released.clone());
    }
    entry.in_flight += 1;
    // Several slots may have been freed before the waiters ran, but
    // `notify_one` keeps only one wake-up: pass it on while slots are left.
    let others_waiting = entry.waiting.saturating_sub(u32::from(queued));
    if others_waiting > 0 && entry.count + entry.in_flight < MAX_FAILURES {
        entry.released.notify_one();
    }
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

/// Why [`Authenticator::reserve`] refused an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Too many failures for the account (or the tracking table is full).
    Account,
    /// Too many failures from the client address.
    Client,
}

/// A reserved login attempt; released when dropped.
///
/// Its outcome is recorded even if the request is cancelled (client hangs
/// up, request timeout) while the password is being verified: an attempt
/// dropped without [`Attempt::settle`] or [`Attempt::fail`] counts as a
/// failure, for the client and, once known to exist, for the account.
/// Otherwise hanging up early would allow guessing without a lockout.
pub struct Attempt<'a> {
    auth: &'a Authenticator,
    username: Username,
    client: Option<ClientKey>,
    settled: AtomicBool,
    /// The account exists (failures are only counted for existing accounts).
    known: AtomicBool,
}

impl<'a> Attempt<'a> {
    fn new(auth: &'a Authenticator, username: &Username, client: Option<ClientKey>) -> Self {
        Self {
            auth,
            username: username.clone(),
            client,
            settled: AtomicBool::new(false),
            known: AtomicBool::new(false),
        }
    }

    pub fn username(&self) -> &Username {
        &self.username
    }

    pub fn client(&self) -> Option<ClientKey> {
        self.client
    }

    /// The account exists: failures from now on count for it, too.
    pub fn mark_known(&self) {
        self.known.store(true, Ordering::Release);
    }

    /// Ends the attempt without a failure (success, or not a guess).
    pub fn settle(&self) {
        self.settled.store(true, Ordering::Release);
    }

    /// Records a failed guess (client, and account if known) and ends the attempt.
    pub fn fail(&self) {
        if !self.settled.swap(true, Ordering::AcqRel) {
            self.auth.record_client_failure(self.client);
            if self.known.load(Ordering::Acquire) {
                self.auth.record_failure(self.username.clone());
            }
        }
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        // Recorded before the slot is freed, so no other attempt slips in.
        self.fail();
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
mod tests;
