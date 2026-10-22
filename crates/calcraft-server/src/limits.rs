//! Request limits: in-flight requests overall and per client, a request
//! rate per client (token bucket) and failed logins per client.
//!
//! Excess requests are rejected immediately instead of queued: overall with
//! `503 Service Unavailable`, per client with `429 Too Many Requests`, both
//! with `Retry-After`. `/healthz` and `/readyz` are exempt from the overall
//! limit (so that monitoring still sees an overloaded server), but not from
//! the per-client limits.
//!
//! Per-client state lives in tables with a hard cap on entries. Entries
//! that carry no information (no request in flight, bucket full again, no
//! recent failure) are swept periodically and whenever a table is full; if
//! that is not enough, the least recently used idle entry is evicted. IPv6
//! clients are grouped by network (`server.client_ipv6_prefix`), so
//! address hopping inside one network does not create new entries.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, StatusCode, header::RETRY_AFTER},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    config::ServerConfig,
    net::{Cidr, ClientIp, ClientKey, X_FORWARDED_FOR, client_ip, is_trusted},
};

/// How often idle entries are swept from the client tables.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// Least time between two extra sweeps of a full client table; in between,
/// a new client evicts a single entry, which still scans the table once
/// (O(`max_tracked_clients`), capped at 100 000 by the configuration).
const FULL_SWEEP_INTERVAL: Duration = Duration::from_secs(1);
/// Fixed-point scale of the token bucket (milli-tokens).
const TOKEN: u64 = 1000;
/// Least time between two "server overloaded" warnings in the log.
const OVERLOAD_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Paths exempt from the overall limit.
const HEALTH_PATHS: &[&str] = &["/healthz", "/readyz"];

/// Milli-tokens of `bucket` at `now`: tokens/s × TOKEN per 1e9 ns, capped
/// at `burst`.
fn refilled(bucket: &Bucket, burst: u64, rate: u64, now: Instant) -> u64 {
    let elapsed = now.saturating_duration_since(bucket.updated).as_nanos();
    let added = elapsed.saturating_mul(u128::from(rate)) / 1_000_000;
    u64::try_from(added)
        .unwrap_or(u64::MAX)
        .saturating_add(bucket.tokens)
        .min(burst)
}

/// Outcome of [`ClientTable::admit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Admitted,
    /// The client has too many requests in flight.
    Busy,
    /// The client's token bucket is empty; retry after that many seconds.
    RateLimited(u64),
    /// The table is full of clients with requests in flight.
    Full,
}

#[derive(Debug)]
struct Bucket {
    /// Milli-tokens.
    tokens: u64,
    updated: Instant,
    in_flight: u32,
}

/// Per-client concurrency and token buckets with a bounded number of
/// entries. Time is passed in, so that tests control the clock.
#[derive(Debug)]
pub struct ClientTable {
    entries: HashMap<ClientKey, Bucket>,
    capacity: usize,
    max_in_flight: u32,
    /// Bucket size in milli-tokens.
    burst: u64,
    /// Tokens per second.
    rate: u64,
    last_sweep: Instant,
    /// Last sweep because the table was full.
    last_full_sweep: Option<Instant>,
}

impl ClientTable {
    pub fn new(capacity: usize, max_in_flight: u32, burst: u32, rate: u32, now: Instant) -> Self {
        Self {
            entries: HashMap::new(),
            capacity: capacity.max(1),
            max_in_flight: max_in_flight.max(1),
            burst: u64::from(burst.max(1)) * TOKEN,
            rate: u64::from(rate.max(1)),
            last_sweep: now,
            last_full_sweep: None,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn refilled(&self, bucket: &Bucket, now: Instant) -> u64 {
        refilled(bucket, self.burst, self.rate, now)
    }

    fn sweep(&mut self, now: Instant) {
        let (burst, rate) = (self.burst, self.rate);
        // An entry without requests in flight and with a full bucket is
        // indistinguishable from a new one and can be dropped without loss.
        self.entries
            .retain(|_, bucket| bucket.in_flight > 0 || refilled(bucket, burst, rate, now) < burst);
        self.last_sweep = now;
    }

    /// Makes room for one more entry; `false` if every entry is busy.
    fn make_room(&mut self, now: Instant) -> bool {
        if self.entries.len() < self.capacity {
            return true;
        }
        let sweep_due = self
            .last_full_sweep
            .is_none_or(|at| now.saturating_duration_since(at) >= FULL_SWEEP_INTERVAL);
        if sweep_due {
            self.last_full_sweep = Some(now);
            self.sweep(now);
            if self.entries.len() < self.capacity {
                return true;
            }
        }
        let oldest = self
            .entries
            .iter()
            .filter(|(_, bucket)| bucket.in_flight == 0)
            .min_by_key(|(_, bucket)| bucket.updated)
            .map(|(key, _)| *key);
        match oldest {
            Some(key) => {
                self.entries.remove(&key);
                true
            }
            None => false,
        }
    }

    /// Admits a request of `key` at `now`: takes a token and counts it as
    /// in flight until [`release`](Self::release).
    pub fn admit(&mut self, key: ClientKey, now: Instant) -> Admission {
        if now.saturating_duration_since(self.last_sweep) >= SWEEP_INTERVAL {
            self.sweep(now);
        }
        if !self.entries.contains_key(&key) {
            if !self.make_room(now) {
                return Admission::Full;
            }
            self.entries.insert(
                key,
                Bucket {
                    tokens: self.burst,
                    updated: now,
                    in_flight: 0,
                },
            );
        }
        let (max_in_flight, rate) = (self.max_in_flight, self.rate);
        let Some(bucket) = self.entries.get(&key) else {
            return Admission::Full;
        };
        let tokens = self.refilled(bucket, now);
        let Some(bucket) = self.entries.get_mut(&key) else {
            return Admission::Full;
        };
        bucket.tokens = tokens;
        bucket.updated = now;
        if bucket.in_flight >= max_in_flight {
            return Admission::Busy;
        }
        if bucket.tokens < TOKEN {
            let missing = TOKEN - bucket.tokens;
            return Admission::RateLimited(missing.div_ceil(rate * TOKEN).max(1));
        }
        bucket.tokens -= TOKEN;
        bucket.in_flight += 1;
        Admission::Admitted
    }

    /// Ends a request admitted by [`admit`](Self::admit).
    pub fn release(&mut self, key: ClientKey) {
        if let Some(bucket) = self.entries.get_mut(&key) {
            bucket.in_flight = bucket.in_flight.saturating_sub(1);
        }
    }
}

#[derive(Debug)]
struct Failures {
    count: u32,
    since: Instant,
}

/// Failed logins per client within a fixed window, with a bounded number
/// of entries. Used by [`crate::auth::Authenticator`].
#[derive(Debug)]
pub struct FailureTable {
    entries: HashMap<ClientKey, Failures>,
    capacity: usize,
    max_failures: u32,
    window: Duration,
}

impl FailureTable {
    pub fn new(capacity: usize, max_failures: u32, window: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            capacity: capacity.max(1),
            max_failures: max_failures.max(1),
            window,
        }
    }

    /// Changes the limits; recorded failures are kept.
    pub fn configure(&mut self, capacity: usize, max_failures: u32) {
        self.capacity = capacity.max(1);
        self.max_failures = max_failures.max(1);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `key` has used up its failed attempts.
    pub fn is_blocked(&self, key: ClientKey, now: Instant) -> bool {
        self.entries.get(&key).is_some_and(|failures| {
            now.saturating_duration_since(failures.since) < self.window
                && failures.count >= self.max_failures
        })
    }

    /// Records a failed attempt of `key`. Returns `true` when this failure
    /// blocked the client.
    ///
    /// When the table is full, expired entries go first, then the oldest
    /// entry that is not blocked. Blocked entries are never evicted, so
    /// spraying new addresses cannot unblock a client. If every entry is
    /// blocked, the new key is not tracked: per-client counting fails open
    /// for it (the per-account lockout still applies) instead of letting a
    /// sprayer block new clients.
    pub fn record(&mut self, key: ClientKey, now: Instant) -> bool {
        let (window, max_failures) = (self.window, self.max_failures);
        if !self.entries.contains_key(&key) && self.entries.len() >= self.capacity {
            self.entries
                .retain(|_, f| now.saturating_duration_since(f.since) < window);
            if self.entries.len() >= self.capacity {
                let oldest = self
                    .entries
                    .iter()
                    .filter(|(_, f)| f.count < max_failures)
                    .min_by_key(|(_, f)| f.since)
                    .map(|(key, _)| *key);
                match oldest {
                    Some(oldest) => {
                        self.entries.remove(&oldest);
                    }
                    None => return false,
                }
            }
        }
        let entry = self.entries.entry(key).or_insert(Failures {
            count: 0,
            since: now,
        });
        if now.saturating_duration_since(entry.since) >= window {
            entry.count = 0;
            entry.since = now;
        }
        entry.count = entry.count.saturating_add(1);
        entry.count == self.max_failures
    }
}

/// Limits of one router, shared by its middleware.
pub struct Limiter {
    global: Arc<Semaphore>,
    clients: Mutex<ClientTable>,
    trusted_proxies: Vec<Cidr>,
    ipv6_prefix: u8,
    last_overload_warning: Mutex<Option<Instant>>,
    missing_peer_warned: AtomicBool,
    untrusted_forwarded_warned: AtomicBool,
}

impl Limiter {
    pub fn new(config: &ServerConfig) -> Arc<Self> {
        Arc::new(Self {
            global: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            clients: Mutex::new(ClientTable::new(
                config.max_tracked_clients,
                config.max_concurrent_requests_per_client,
                config.client_rate_burst,
                config.client_rate_per_second,
                Instant::now(),
            )),
            trusted_proxies: config.trusted_proxies.clone(),
            ipv6_prefix: config.client_ipv6_prefix,
            last_overload_warning: Mutex::new(None),
            missing_peer_warned: AtomicBool::new(false),
            untrusted_forwarded_warned: AtomicBool::new(false),
        })
    }

    fn peer(&self, request: &Request) -> IpAddr {
        if let Some(ConnectInfo(peer)) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
            return peer.ip();
        }
        // Without the peer address every request counts as one client:
        // limits stay in force instead of silently switching off.
        if !self.missing_peer_warned.swap(true, Ordering::Relaxed) {
            tracing::warn!("peer address unavailable; all requests share one client limit");
        }
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    }

    fn warn_overload(&self) {
        let Ok(mut last) = self.last_overload_warning.lock() else {
            return;
        };
        let now = Instant::now();
        if last.is_none_or(|at| now.saturating_duration_since(at) >= OVERLOAD_WARN_INTERVAL) {
            *last = Some(now);
            tracing::warn!("server.max_concurrent_requests reached; rejecting requests with 503");
        }
    }
}

/// Removes the in-flight count of a request when it ends.
struct ClientPermit {
    limiter: Arc<Limiter>,
    key: ClientKey,
    _global: Option<OwnedSemaphorePermit>,
}

impl Drop for ClientPermit {
    fn drop(&mut self) {
        if let Ok(mut clients) = self.limiter.clients.lock() {
            clients.release(self.key);
        }
    }
}

/// Determines the client address (see [`crate::net`]) and stores it as
/// [`ClientIp`] request extension. Runs before the trace layer, so that
/// request spans can include it.
pub async fn resolve_client(
    State(limiter): State<Arc<Limiter>>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = limiter.peer(&request);
    if request.headers().contains_key(X_FORWARDED_FOR)
        && !is_trusted(&limiter.trusted_proxies, peer)
        && !limiter
            .untrusted_forwarded_warned
            .swap(true, Ordering::Relaxed)
    {
        // Once per process: a reverse proxy that is not trusted makes all
        // users share its per-client limits.
        tracing::warn!(
            %peer,
            "ignoring X-Forwarded-For from a peer not in server.trusted_proxies; \
             if CalCraft runs behind this reverse proxy, add it to server.trusted_proxies"
        );
    }
    let ip = client_ip(peer, request.headers(), &limiter.trusted_proxies);
    let client = ClientIp {
        ip,
        key: ClientKey::new(ip, limiter.ipv6_prefix),
    };
    request.extensions_mut().insert(client);
    request.extensions_mut().insert(client.key);
    next.run(request).await
}

fn reject(status: StatusCode, retry_after: u64, message: &'static str) -> Response {
    let mut response = (status, message).into_response();
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from(retry_after));
    response
}

/// Enforces the overall and per-client limits for the request.
pub async fn enforce(
    State(limiter): State<Arc<Limiter>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(client) = request.extensions().get::<ClientIp>().copied() else {
        // `resolve_client` always runs first; refuse rather than skip limits.
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let global = if HEALTH_PATHS.contains(&request.uri().path()) {
        None
    } else {
        match Arc::clone(&limiter.global).try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                limiter.warn_overload();
                return reject(StatusCode::SERVICE_UNAVAILABLE, 1, "server busy");
            }
        }
    };
    let admission = match limiter.clients.lock() {
        Ok(mut clients) => clients.admit(client.key, Instant::now()),
        Err(_) => Admission::Full,
    };
    match admission {
        Admission::Admitted => {}
        Admission::Busy => {
            tracing::debug!(client = %client.ip, "too many concurrent requests from client");
            return reject(StatusCode::TOO_MANY_REQUESTS, 1, "too many requests");
        }
        Admission::RateLimited(retry_after) => {
            tracing::debug!(client = %client.ip, "client request rate exceeded");
            return reject(
                StatusCode::TOO_MANY_REQUESTS,
                retry_after,
                "too many requests",
            );
        }
        Admission::Full => {
            limiter.warn_overload();
            return reject(StatusCode::SERVICE_UNAVAILABLE, 1, "server busy");
        }
    }
    let _permit = ClientPermit {
        limiter: Arc::clone(&limiter),
        key: client.key,
        _global: global,
    };
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> ClientKey {
        ClientKey::new(IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + n)), 64)
    }

    #[test]
    fn token_bucket_allows_burst_then_refills() {
        let t0 = Instant::now();
        let mut table = ClientTable::new(10, 100, 3, 2, t0);
        for _ in 0..3 {
            assert_eq!(table.admit(key(1), t0), Admission::Admitted);
            table.release(key(1));
        }
        assert_eq!(table.admit(key(1), t0), Admission::RateLimited(1));
        // Other clients have their own bucket.
        assert_eq!(table.admit(key(2), t0), Admission::Admitted);
        // 2 tokens per second: one token after 500 ms, not after 499 ms.
        assert!(matches!(
            table.admit(key(1), t0 + Duration::from_millis(499)),
            Admission::RateLimited(_)
        ));
        assert_eq!(
            table.admit(key(1), t0 + Duration::from_millis(1000)),
            Admission::Admitted
        );
        table.release(key(1));
        // Never more than the burst, however long the pause.
        let later = t0 + Duration::from_secs(3600);
        for _ in 0..3 {
            assert_eq!(table.admit(key(1), later), Admission::Admitted);
            table.release(key(1));
        }
        assert!(matches!(
            table.admit(key(1), later),
            Admission::RateLimited(_)
        ));
    }

    #[test]
    fn retry_after_covers_the_missing_token() {
        let t0 = Instant::now();
        let mut table = ClientTable::new(10, 100, 1, 1, t0);
        assert_eq!(table.admit(key(1), t0), Admission::Admitted);
        assert_eq!(table.admit(key(1), t0), Admission::RateLimited(1));
    }

    #[test]
    fn concurrent_requests_per_client_are_bounded() {
        let t0 = Instant::now();
        let mut table = ClientTable::new(10, 2, 100, 100, t0);
        assert_eq!(table.admit(key(1), t0), Admission::Admitted);
        assert_eq!(table.admit(key(1), t0), Admission::Admitted);
        assert_eq!(table.admit(key(1), t0), Admission::Busy);
        assert_eq!(table.admit(key(2), t0), Admission::Admitted);
        table.release(key(1));
        assert_eq!(table.admit(key(1), t0), Admission::Admitted);
    }

    #[test]
    fn table_is_capped_and_evicts_idle_entries() {
        let t0 = Instant::now();
        let mut table = ClientTable::new(3, 10, 10, 1, t0);
        // Three clients with a request in flight fill the table.
        for n in 0..3 {
            assert_eq!(table.admit(key(n), t0), Admission::Admitted);
        }
        assert_eq!(table.admit(key(9), t0), Admission::Full);
        assert_eq!(table.len(), 3);
        // Once one is idle, the least recently used idle entry makes room,
        // even before its bucket is full again.
        table.release(key(1));
        let t1 = t0 + Duration::from_millis(10);
        assert_eq!(table.admit(key(9), t1), Admission::Admitted);
        assert_eq!(table.len(), 3);
        assert!(!table.entries.contains_key(&key(1)));

        // Spraying many addresses never grows the table beyond its cap.
        for n in 0..3 {
            table.release(key(n));
        }
        table.release(key(9));
        for n in 100..10_100 {
            let at = t1 + Duration::from_millis(u64::from(n));
            assert_eq!(table.admit(key(n), at), Admission::Admitted);
            table.release(key(n));
            assert!(table.len() <= 3);
        }
    }

    #[test]
    fn full_table_is_swept_at_most_once_per_second() {
        let t0 = Instant::now();
        let mut table = ClientTable::new(3, 10, 1, 1000, t0);
        let fill = |table: &mut ClientTable, keys: std::ops::Range<u32>, at: Instant| {
            for n in keys {
                assert_eq!(table.admit(key(n), at), Admission::Admitted);
                table.release(key(n));
            }
        };
        fill(&mut table, 0..3, t0);
        // 10 ms later every bucket is full again: the sweep empties the table.
        let t1 = t0 + Duration::from_millis(10);
        fill(&mut table, 10..11, t1);
        assert_eq!(table.len(), 1);
        fill(&mut table, 11..13, t1);
        // Full again within the second: a new client evicts one entry only,
        // although all others are idle.
        let t2 = t1 + Duration::from_millis(10);
        fill(&mut table, 20..21, t2);
        assert_eq!(table.len(), 3);
        // After a second, the next full table is swept again.
        fill(&mut table, 30..31, t1 + FULL_SWEEP_INTERVAL);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn idle_entries_are_swept_periodically() {
        let t0 = Instant::now();
        let mut table = ClientTable::new(100, 10, 2, 1, t0);
        for n in 0..50 {
            assert_eq!(table.admit(key(n), t0), Admission::Admitted);
            table.release(key(n));
        }
        // A client with a request in flight survives the sweep.
        assert_eq!(table.admit(key(1000), t0), Admission::Admitted);
        assert_eq!(table.len(), 51);
        let later = t0 + SWEEP_INTERVAL;
        assert_eq!(table.admit(key(2000), later), Admission::Admitted);
        assert_eq!(table.len(), 2);
        assert!(table.entries.contains_key(&key(1000)));
        assert!(!table.is_empty());
    }

    #[test]
    fn failure_table_blocks_within_window_and_is_capped() {
        let t0 = Instant::now();
        let window = Duration::from_secs(900);
        let mut table = FailureTable::new(2, 3, window);
        assert!(!table.is_blocked(key(1), t0));
        assert!(!table.record(key(1), t0));
        assert!(!table.record(key(1), t0));
        assert!(!table.is_blocked(key(1), t0));
        assert!(table.record(key(1), t0));
        assert!(table.is_blocked(key(1), t0));
        assert!(!table.is_blocked(key(2), t0));
        // The window expires.
        assert!(!table.is_blocked(key(1), t0 + window));
        assert!(!table.record(key(1), t0 + window));

        // Bounded: new clients evict the oldest entry.
        let t1 = t0 + window;
        table.record(key(2), t1 + Duration::from_secs(1));
        table.record(key(3), t1 + Duration::from_secs(2));
        assert_eq!(table.len(), 2);
        assert!(!table.entries.contains_key(&key(1)));
        for n in 10..1000 {
            table.record(key(n), t1 + Duration::from_secs(3));
            assert!(table.len() <= 2);
        }
        assert!(!table.is_empty());
    }

    #[test]
    fn spraying_cannot_unblock_a_client() {
        let t0 = Instant::now();
        let window = Duration::from_secs(900);
        let mut table = FailureTable::new(2, 2, window);
        table.record(key(1), t0);
        assert!(table.record(key(1), t0));
        // Thousands of new addresses only evict each other.
        for n in 10..10_000 {
            table.record(key(n), t0 + Duration::from_secs(1));
            assert!(table.is_blocked(key(1), t0 + Duration::from_secs(1)));
            assert!(table.len() <= 2);
        }
        // With every entry blocked, new clients are not tracked (fail open
        // for them) and blocked ones stay blocked.
        table.record(key(2), t0);
        assert!(table.record(key(2), t0));
        let t1 = t0 + Duration::from_secs(2);
        assert!(table.is_blocked(key(1), t1) && table.is_blocked(key(2), t1));
        for _ in 0..5 {
            assert!(!table.record(key(3), t1));
        }
        assert!(!table.is_blocked(key(3), t1));
        assert_eq!(table.len(), 2);
        // Once the window ends, there is room again.
        assert!(!table.record(key(3), t0 + window));
        assert!(!table.is_blocked(key(1), t0 + window));
    }
}
