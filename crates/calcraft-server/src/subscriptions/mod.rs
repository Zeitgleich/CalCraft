//! Calendar subscriptions (webcal/ICS feeds), feature `subscriptions`.
//!
//! The server fetches a feed and stores its events as ordinary objects of a
//! read-only calendar ([`calcraft_store::Subscription`]), so every CalDAV
//! client sees the subscription like any other calendar, including
//! `sync-collection`. Refreshes only touch objects that changed.
//!
//! Fetching is the dangerous part (SSRF): see [`ssrf`] for the URL and
//! address rules and [`fetch`] for DNS pinning, redirects and limits. Feed
//! URLs may carry secret tokens, so logs name the host only, never the URL.
//!
//! A background task refreshes due subscriptions every minute, at most
//! `max_concurrent_fetches` at a time, with per-subscription intervals
//! (15 minutes to 7 days; the owner's choice, else the feed's
//! `REFRESH-INTERVAL`/`X-PUBLISHED-TTL`, else the default), random jitter
//! and back-off after failures. A failed refresh keeps the last good
//! content and records the error for the owner.

pub mod fetch;
pub mod ssrf;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use calcraft_core::Slug;
use calcraft_store::{
    FeedState, NewSubscription, Store, StoreError, Subscription, SubscriptionFailure, User,
};
use tokio::sync::Semaphore;

use crate::{
    auth::now,
    config::{MAX_SUBSCRIPTION_REFRESH_SECS, MIN_SUBSCRIPTION_REFRESH_SECS, SubscriptionsConfig},
    secrets::random_bytes,
    transfer::{FeedObjects, TransferError, feed_objects, import_slot},
};

pub use fetch::{Conditional, Fetched, Fetcher, Lookup, Resolver, SystemResolver};
pub use ssrf::{FeedUrl, Host, MAX_URL_LEN, Policy, is_public};

/// Component types of a subscription calendar.
pub const COMPONENTS: &str = "VEVENT,VTODO";
/// Shortest time between two refreshes of one subscription ("refresh now"
/// included).
pub const MIN_REFRESH_GAP_SECS: i64 = 60;
/// Subscribe attempts (that fetch something) per user within
/// [`CREATE_WINDOW_SECS`]: failed attempts are not limited by the
/// per-user cap, and every attempt makes the server send a request.
pub const MAX_CREATE_ATTEMPTS: u32 = 10;
const CREATE_WINDOW_SECS: i64 = 600;
/// Users tracked for the attempt limit before stale entries are dropped.
const MAX_TRACKED_USERS: usize = 10_000;
/// How often the background task looks for due subscriptions.
const TICK: Duration = Duration::from_secs(60);
/// Due subscriptions picked up per tick.
const BATCH: u32 = 32;

/// Why a feed could not be subscribed to or refreshed. The messages are
/// generic on purpose: they must not turn the server into a port scanner
/// for its users (connection refused, TLS errors and timeouts all read
/// "could not be fetched").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubscriptionError {
    #[error("invalid feed URL: {0}")]
    InvalidUrl(&'static str),
    #[error("the feed's address is not allowed")]
    BlockedAddress,
    #[error("the feed could not be fetched")]
    FetchFailed(Option<&'static str>),
    #[error("the feed server answered HTTP {0}")]
    HttpStatus(u16),
    #[error("the feed is not an iCalendar file")]
    NotICalendar,
    #[error("the feed is too large")]
    TooLarge,
    #[error("limit reached: {0}")]
    LimitReached(&'static str),
    #[error("too many attempts; please wait a moment")]
    RateLimited,
}

impl SubscriptionError {
    /// Stable machine-readable code (API, stored as `last_error`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidUrl(_) => "invalid_url",
            Self::BlockedAddress => "blocked_address",
            Self::FetchFailed(_) | Self::HttpStatus(_) => "fetch_failed",
            Self::NotICalendar => "not_icalendar",
            Self::TooLarge => "too_large",
            Self::LimitReached(_) => "limit_reached",
            Self::RateLimited => "rate_limited",
        }
    }

    /// Short server-generated detail (never the URL or response content).
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::InvalidUrl(reason) | Self::LimitReached(reason) => Some((*reason).to_owned()),
            Self::FetchFailed(detail) => detail.map(str::to_owned),
            Self::HttpStatus(status) => Some(format!("HTTP {status}")),
            _ => None,
        }
    }
}

/// Errors of [`Subscriptions`]: a problem with the feed, or with storage.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Feed(#[from] SubscriptionError),
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The subscription does not exist (any more).
    #[error("subscription not found")]
    NotFound,
}

impl From<TransferError> for ServiceError {
    fn from(err: TransferError) -> Self {
        match err {
            TransferError::TooLarge => SubscriptionError::TooLarge.into(),
            TransferError::Store(err) => err.into(),
            TransferError::Invalid(_) | TransferError::Aborted => {
                SubscriptionError::NotICalendar.into()
            }
        }
    }
}

fn limit(err: StoreError) -> ServiceError {
    match err {
        StoreError::Limit(reason) => SubscriptionError::LimitReached(reason).into(),
        err => err.into(),
    }
}

/// A new subscription requested by a user.
#[derive(Debug, Clone)]
pub struct CreateRequest<'a> {
    pub url: &'a str,
    pub slug: Option<Slug>,
    pub display_name: Option<String>,
    pub color: Option<String>,
    pub refresh_secs: Option<i64>,
}

/// Removes a subscription from the in-flight set when dropped.
struct InFlight<'a> {
    set: &'a Mutex<HashSet<i64>>,
    id: i64,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// The subscription service: creation, refreshes and the background task.
pub struct Subscriptions {
    store: Store,
    fetcher: Fetcher,
    max_per_user: i64,
    default_refresh_secs: i64,
    fetches: Semaphore,
    in_flight: Mutex<HashSet<i64>>,
    /// Per user: start of the window and subscribe attempts in it.
    create_attempts: Mutex<HashMap<i64, (i64, u32)>>,
    max_create_attempts: u32,
    /// [`MIN_REFRESH_GAP_SECS`]; lowered by tests only.
    min_gap: i64,
}

impl std::fmt::Debug for Subscriptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscriptions")
            .field("fetcher", &self.fetcher)
            .finish_non_exhaustive()
    }
}

/// Clamps an interval to 15 minutes … 7 days.
pub fn clamp_interval(secs: i64) -> i64 {
    secs.clamp(MIN_SUBSCRIPTION_REFRESH_SECS, MAX_SUBSCRIPTION_REFRESH_SECS)
}

/// A random delay of up to a tenth of `secs`.
fn jitter(secs: i64) -> i64 {
    let max = u64::try_from(secs / 10).unwrap_or(0);
    let value = u64::from_le_bytes(random_bytes()) % (max + 1);
    i64::try_from(value).unwrap_or(0)
}

/// Delay after the `failures`-th failure in a row: 15 minutes, doubling,
/// at most the interval or a day, whichever is longer.
pub fn backoff(interval: i64, failures: i64) -> i64 {
    let exponent = u32::try_from(failures.saturating_sub(1).clamp(0, 16)).unwrap_or(16);
    let cap = interval.clamp(86_400, MAX_SUBSCRIPTION_REFRESH_SECS);
    MIN_SUBSCRIPTION_REFRESH_SECS
        .saturating_mul(1i64 << exponent)
        .min(cap)
}

/// A random slug for a new subscription calendar.
fn new_slug() -> Slug {
    let bytes = random_bytes::<5>();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    // `subscription-` plus hex digits is always a valid slug.
    format!("subscription-{hex}")
        .parse()
        .unwrap_or_else(|_| unreachable!("valid slug"))
}

impl Subscriptions {
    /// The service with the system resolver and the system trust store.
    pub fn new(store: Store, config: &SubscriptionsConfig) -> Arc<Self> {
        let tls = match crate::https::tls_config(None, false) {
            Ok(tls) => Some(tls),
            Err(err) => {
                tracing::warn!(%err, "subscriptions: https feeds cannot be fetched");
                None
            }
        };
        let fetcher = Fetcher::new(
            Policy::new(config),
            Arc::new(SystemResolver),
            tls,
            config.max_bytes,
            config.timeout(),
            config.max_redirects,
        );
        Self::with_fetcher(store, config, fetcher)
    }

    /// The service with a given fetcher (tests inject a resolver).
    pub fn with_fetcher(store: Store, config: &SubscriptionsConfig, fetcher: Fetcher) -> Arc<Self> {
        Arc::new(Self {
            store,
            fetcher,
            max_per_user: i64::from(config.max_per_user),
            default_refresh_secs: clamp_interval(config.default_refresh_secs()),
            fetches: Semaphore::new(config.max_concurrent_fetches),
            in_flight: Mutex::new(HashSet::new()),
            create_attempts: Mutex::new(HashMap::new()),
            max_create_attempts: MAX_CREATE_ATTEMPTS,
            min_gap: MIN_REFRESH_GAP_SECS,
        })
    }

    /// Like [`Subscriptions::with_fetcher`] without the rate limit of
    /// refreshes, for tests.
    #[cfg(test)]
    pub(crate) fn without_rate_limit(
        store: Store,
        config: &SubscriptionsConfig,
        fetcher: Fetcher,
    ) -> Arc<Self> {
        let mut this = Self::with_fetcher(store, config, fetcher);
        if let Some(inner) = Arc::get_mut(&mut this) {
            inner.min_gap = 0;
            inner.max_create_attempts = u32::MAX;
        }
        this
    }

    /// Counts a subscribe attempt of `user`; `false` once the limit for
    /// the current window is reached.
    fn allow_create(&self, user: i64, now: i64) -> bool {
        let mut attempts = self
            .create_attempts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if attempts.len() >= MAX_TRACKED_USERS {
            attempts.retain(|_, (start, _)| now - *start < CREATE_WINDOW_SECS);
        }
        let entry = attempts.entry(user).or_insert((now, 0));
        if now - entry.0 >= CREATE_WINDOW_SECS {
            *entry = (now, 0);
        }
        if entry.1 >= self.max_create_attempts {
            return false;
        }
        entry.1 += 1;
        true
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn max_per_user(&self) -> i64 {
        self.max_per_user
    }

    /// The interval in effect for a subscription.
    pub fn interval(&self, refresh_secs: Option<i64>, feed_ttl_secs: Option<i64>) -> i64 {
        clamp_interval(
            refresh_secs
                .or(feed_ttl_secs)
                .unwrap_or(self.default_refresh_secs),
        )
    }

    fn claim(&self, id: i64) -> Option<InFlight<'_>> {
        // The lock is released before an `InFlight` exists: its drop locks
        // the set again.
        let inserted = self
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id);
        inserted.then(|| InFlight {
            set: &self.in_flight,
            id,
        })
    }

    /// Fetches and converts a feed (no storage yet).
    async fn load(
        &self,
        url: &FeedUrl,
        conditional: &Conditional,
        collection: &calcraft_store::Collection,
    ) -> Result<Option<Loaded>, ServiceError> {
        let fetched = {
            let _permit = self
                .fetches
                .acquire()
                .await
                .map_err(|_| SubscriptionError::FetchFailed(Some("server is shutting down")))?;
            self.fetcher.fetch(url, conditional).await?
        };
        let Fetched::Body {
            body,
            etag,
            last_modified,
        } = fetched
        else {
            return Ok(None);
        };
        let hash = blake3::hash(&body).to_hex().to_string();
        let ttl = calcraft_ical::transfer::feed_refresh_interval(&body);
        // Parsing large feeds shares the import slots (memory and CPU).
        let slot = import_slot().await?;
        let owned = collection.clone();
        let objects = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            feed_objects(&owned, &body)
        })
        .await
        .map_err(|_| SubscriptionError::NotICalendar)??;
        // A feed whose objects are all broken is a broken feed: keep the
        // last good content rather than emptying the calendar.
        if objects.items.is_empty() && objects.skipped > 0 {
            return Err(SubscriptionError::NotICalendar.into());
        }
        Ok(Some(Loaded {
            objects,
            hash,
            ttl,
            etag,
            last_modified,
        }))
    }

    /// Subscribes `owner` to a feed: checks the URL, fetches it once and
    /// creates the calendar with its content. Nothing is stored on error.
    pub async fn create(
        &self,
        owner: &User,
        request: CreateRequest<'_>,
    ) -> Result<Subscription, ServiceError> {
        if self.store.count_subscriptions(owner.id).await? >= self.max_per_user {
            return Err(SubscriptionError::LimitReached("too many subscriptions").into());
        }
        let url = self.fetcher.policy().parse_url(request.url)?;
        let slug = request.slug.clone().unwrap_or_else(new_slug);
        // Checked before fetching anything; the store checks again.
        if self
            .store
            .collection(owner.id, calcraft_core::CollectionKind::Calendar, &slug)
            .await?
            .is_some()
        {
            return Err(StoreError::CollectionExists.into());
        }
        if !self.allow_create(owner.id, now()) {
            return Err(SubscriptionError::RateLimited.into());
        }
        // Placeholder collection for validation: the real one is created
        // together with the objects.
        let placeholder = calcraft_store::Collection {
            id: 0,
            owner_id: owner.id,
            kind: calcraft_core::CollectionKind::Calendar,
            slug: slug.clone(),
            display_name: None,
            description: None,
            color: None,
            sync_revision: 0,
            components: COMPONENTS.to_owned(),
            sync_floor: 0,
            token_salt: 0,
            read_only: true,
        };
        let loaded = match self.load(&url, &Conditional::default(), &placeholder).await {
            Ok(Some(loaded)) => loaded,
            // No validators were sent, so 304 is not a valid answer.
            Ok(None) => return Err(SubscriptionError::FetchFailed(None).into()),
            Err(err) => {
                if let ServiceError::Feed(err) = &err {
                    tracing::info!(host = %url.host_name(), code = err.code(), "subscription refused");
                }
                return Err(err);
            }
        };
        let now = now();
        let interval = self.interval(request.refresh_secs, loaded.ttl);
        let display_name = request
            .display_name
            .clone()
            .unwrap_or_else(|| url.host_name());
        let source = url.as_string();
        let state = FeedState {
            feed_ttl_secs: loaded.ttl,
            http_etag: loaded.etag.as_deref(),
            http_last_modified: loaded.last_modified.as_deref(),
            content_hash: &loaded.hash,
            skipped: loaded.objects.skipped,
            now,
            next_refresh_at: now + interval + jitter(interval),
        };
        let collection = self
            .store
            .create_subscription(
                &NewSubscription {
                    owner_id: owner.id,
                    slug: &slug,
                    display_name: Some(&display_name),
                    color: request.color.as_deref(),
                    components: COMPONENTS,
                    source_url: &source,
                    refresh_secs: request.refresh_secs.map(clamp_interval),
                    max_per_user: self.max_per_user,
                },
                &loaded.objects.items,
                &state,
            )
            .await
            .map_err(limit)?;
        tracing::info!(
            host = %url.host_name(),
            objects = loaded.objects.items.len(),
            skipped = loaded.objects.skipped,
            "subscription created"
        );
        self.store
            .subscription_by_collection(collection.id)
            .await?
            .ok_or(ServiceError::NotFound)
    }

    /// Refreshes one subscription now. Fails with
    /// [`SubscriptionError::RateLimited`] if it is being refreshed or was
    /// refreshed less than [`MIN_REFRESH_GAP_SECS`] ago. A failed fetch is
    /// recorded and returned as `Ok` (with `last_error` set).
    pub async fn refresh(&self, collection_id: i64) -> Result<Subscription, ServiceError> {
        let Some(_guard) = self.claim(collection_id) else {
            return Err(SubscriptionError::RateLimited.into());
        };
        let sub = self
            .store
            .subscription_by_collection(collection_id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let now = now();
        let interval = self.interval(sub.refresh_secs, sub.feed_ttl_secs);
        // The provisional next time also covers a refresh that never ends
        // (the process stops).
        if !self
            .store
            .claim_subscription_refresh(collection_id, now, self.min_gap, now + interval)
            .await?
        {
            return Err(SubscriptionError::RateLimited.into());
        }
        match self.refresh_claimed(&sub, now).await {
            Ok(()) => {}
            Err(ServiceError::Feed(err)) => {
                let failures = sub.failures.saturating_add(1);
                let detail = err.detail();
                self.store
                    .subscription_failed(
                        collection_id,
                        &SubscriptionFailure {
                            code: err.code(),
                            detail: detail.as_deref(),
                            now,
                            next_refresh_at: now + backoff(interval, failures),
                        },
                    )
                    .await?;
                tracing::info!(
                    host = %host_of(&self.fetcher, &sub.source_url),
                    code = err.code(),
                    failures,
                    "subscription refresh failed"
                );
            }
            Err(err) => return Err(err),
        }
        self.store
            .subscription_by_collection(collection_id)
            .await?
            .ok_or(ServiceError::NotFound)
    }

    async fn refresh_claimed(&self, sub: &Subscription, now: i64) -> Result<(), ServiceError> {
        // Checked again on every refresh: the rules may have changed.
        let url = self.fetcher.policy().parse_url(&sub.source_url)?;
        let conditional = if sub.content_hash.is_some() {
            Conditional {
                etag: sub.http_etag.clone(),
                last_modified: sub.http_last_modified.clone(),
            }
        } else {
            Conditional::default()
        };
        let id = sub.collection.id;
        let unchanged = |ttl: Option<i64>| {
            let interval = self.interval(sub.refresh_secs, ttl);
            now + interval + jitter(interval)
        };
        let Some(loaded) = self.load(&url, &conditional, &sub.collection).await? else {
            tracing::debug!(host = %url.host_name(), "subscription not modified");
            self.store
                .subscription_unchanged(id, now, unchanged(sub.feed_ttl_secs))
                .await?;
            return Ok(());
        };
        let next = unchanged(loaded.ttl);
        let state = FeedState {
            feed_ttl_secs: loaded.ttl,
            http_etag: loaded.etag.as_deref(),
            http_last_modified: loaded.last_modified.as_deref(),
            content_hash: &loaded.hash,
            skipped: loaded.objects.skipped,
            now,
            next_refresh_at: next,
        };
        let outcome = self
            .store
            .apply_subscription_feed(id, &loaded.objects.items, &state)
            .await
            .map_err(limit)?;
        if let Some(outcome) = outcome {
            tracing::debug!(
                host = %url.host_name(),
                added = outcome.added,
                updated = outcome.updated,
                removed = outcome.removed,
                "subscription refreshed"
            );
        }
        Ok(())
    }

    /// Refreshes the subscriptions that are due (one tick of the
    /// background task). Returns how many were started.
    pub async fn run_due(self: &Arc<Self>) -> usize {
        let due = match self.store.due_subscriptions(now(), BATCH).await {
            Ok(due) => due,
            Err(err) => {
                tracing::warn!(%err, "subscriptions: cannot list due refreshes");
                return 0;
            }
        };
        let mut started = 0;
        for id in due {
            let busy = self
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&id);
            if busy {
                continue;
            }
            started += 1;
            let this = self.clone();
            tokio::spawn(async move {
                match this.refresh(id).await {
                    Ok(_) | Err(ServiceError::Feed(_) | ServiceError::NotFound) => {}
                    Err(err) => tracing::warn!(%err, "subscription refresh failed"),
                }
            });
        }
        started
    }

    /// Starts the background refresher.
    pub fn spawn(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(TICK).await;
                this.run_due().await;
            }
        });
    }
}

/// A fetched and converted feed.
struct Loaded {
    objects: FeedObjects,
    hash: String,
    ttl: Option<i64>,
    etag: Option<String>,
    last_modified: Option<String>,
}

/// The host of a stored URL for logs.
fn host_of(fetcher: &Fetcher, url: &str) -> String {
    fetcher
        .policy()
        .parse_url(url)
        .map_or_else(|_| "<invalid>".to_owned(), |url| url.host_name())
}

#[cfg(test)]
mod tests;
