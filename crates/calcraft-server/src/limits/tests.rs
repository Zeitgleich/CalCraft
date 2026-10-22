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
