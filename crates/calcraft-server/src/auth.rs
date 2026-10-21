//! Password authentication for DAV clients (HTTP Basic, RFC 7617).
//!
//! DAV clients send credentials with every request. To keep Argon2 from
//! becoming a denial-of-service lever, successful verifications are cached
//! for a short time under a keyed BLAKE3 hash (the key never leaves the
//! process), concurrent Argon2 runs are bounded, and repeated failures lock
//! the account for a while.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_core::Username;
use calcraft_store::{Store, User};
use tokio::sync::Semaphore;

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

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("password must be at least {MIN_PASSWORD_LEN} characters long")]
    TooShort,
    #[error("password must be at most {MAX_PASSWORD_LEN} bytes long")]
    TooLong,
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
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|_| PasswordError::Hash)
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
    since: Instant,
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
    hashing: Semaphore,
    cache: Mutex<HashMap<Username, CacheEntry>>,
    failures: Mutex<HashMap<Username, Failures>>,
}

impl Authenticator {
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
            hashing: Semaphore::new(MAX_CONCURRENT_HASHES),
            cache: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
        }))
    }

    /// Authenticates the value of an `Authorization` header.
    pub async fn authenticate(&self, header: Option<&[u8]>) -> AuthOutcome {
        let Some(credentials) = header.and_then(parse_basic) else {
            return AuthOutcome::Denied;
        };
        let username = credentials.username;

        if self.is_locked_out(&username) {
            return AuthOutcome::LockedOut;
        }

        let record = match self.store.user_credentials(&username).await {
            Ok(record) => record,
            Err(err) => {
                tracing::error!(%err, "failed to load user");
                return AuthOutcome::Error;
            }
        };

        // Binding the cache entry to the stored hash invalidates it as soon
        // as the password changes.
        let digest = record.as_ref().map(|record| {
            blake3::Hasher::new_keyed(&self.cache_key)
                .update(credentials.password.as_bytes())
                .update(&[0xff])
                .update(record.password_hash.as_bytes())
                .finalize()
        });
        if let (Some(record), Some(digest)) = (&record, &digest)
            && self.cache_hit(&username, digest)
        {
            if record.user.disabled {
                return AuthOutcome::Denied;
            }
            return AuthOutcome::Authenticated(record.user.clone());
        }

        let hash = record
            .as_ref()
            .map_or_else(|| self.dummy_hash.clone(), |r| r.password_hash.clone());
        let Ok(_permit) = self.hashing.acquire().await else {
            return AuthOutcome::Error;
        };
        let password = credentials.password;
        let valid =
            match tokio::task::spawn_blocking(move || verify_password(&password, &hash)).await {
                Ok(valid) => valid,
                Err(err) => {
                    tracing::error!(%err, "password verification task failed");
                    return AuthOutcome::Error;
                }
            };

        match record {
            Some(record) if valid && !record.user.disabled => {
                self.clear_failures(&username);
                if let Some(digest) = digest {
                    self.remember(username, digest);
                }
                AuthOutcome::Authenticated(record.user)
            }
            _ => {
                self.record_failure(username);
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

    fn is_locked_out(&self, username: &Username) -> bool {
        let Ok(failures) = self.failures.lock() else {
            return true;
        };
        failures
            .get(username)
            .is_some_and(|f| f.count >= MAX_FAILURES && f.since.elapsed() < LOCKOUT_WINDOW)
    }

    fn record_failure(&self, username: Username) {
        let Ok(mut failures) = self.failures.lock() else {
            return;
        };
        if failures.len() >= MAX_TRACKED {
            failures.retain(|_, f| f.since.elapsed() < LOCKOUT_WINDOW);
        }
        if failures.len() >= MAX_TRACKED && !failures.contains_key(&username) {
            return;
        }
        let entry = failures.entry(username).or_insert(Failures {
            count: 0,
            since: Instant::now(),
        });
        if entry.since.elapsed() >= LOCKOUT_WINDOW {
            entry.count = 0;
            entry.since = Instant::now();
        }
        entry.count += 1;
        if entry.count == MAX_FAILURES {
            tracing::warn!("account locked after repeated failed logins");
        }
    }

    fn clear_failures(&self, username: &Username) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.remove(username);
        }
    }
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
