use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use calcraft_core::{CollectionKind, Slug};
use sqlx::Row;

use super::*;
use crate::{DEFAULT_COLLECTION_SLUG, NewItem, Precondition, PutOutcome};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "calcraft-health-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An in-memory store with one user; returns the id of the calendar.
async fn setup() -> (Store, i64) {
    let store = Store::in_memory().await.unwrap();
    let user = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let slug: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
    let calendar = store
        .collection(user.id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    (store, calendar.id)
}

async fn put(store: &Store, collection_id: i64, name: &str) -> Result<PutOutcome> {
    let item = NewItem {
        uid: name,
        component: "VEVENT",
        range_start: None,
        range_end: None,
        recurring: false,
        index_version: 0,
        etag: "\"1\"",
        content: b"BEGIN:VCALENDAR",
    };
    store
        .put_item(collection_id, name, &item, &Precondition::None)
        .await
}

/// `probe` and the length of `pad` of the health row.
async fn health_row(store: &Store) -> (i64, Option<i64>) {
    let row = sqlx::query("SELECT probe, length(pad) AS pad FROM health WHERE id = 1")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    (row.get("probe"), row.get("pad"))
}

async fn page_count(store: &Store) -> i64 {
    sqlx::query("PRAGMA page_count")
        .fetch_one(&store.pool)
        .await
        .unwrap()
        .get(0)
}

/// The in-memory store has a single connection for reads and writes.
async fn query_only(store: &Store, on: bool) {
    let pragma = if on {
        "PRAGMA query_only = ON"
    } else {
        "PRAGMA query_only = OFF"
    };
    sqlx::query(pragma).execute(&store.writer).await.unwrap();
}

#[tokio::test]
async fn probe_rolls_back_and_reports_ready() {
    let (store, _) = setup().await;
    for _ in 0..10 {
        assert_eq!(store.readiness().await, Readiness::Ready);
    }
    assert_eq!(health_row(&store).await, (0, None));
    assert_eq!(store.write_failures(), 0);
}

#[tokio::test]
async fn read_only_database_is_not_ready_until_writable() {
    let (store, calendar) = setup().await;
    // The probe alone notices.
    query_only(&store, true).await;
    assert_eq!(store.readiness().await, Readiness::NotWritable);
    // The probe's own failure does not count as a failed real write.
    assert_eq!(store.write_failures(), 0);
    query_only(&store, false).await;
    assert_eq!(store.readiness().await, Readiness::Ready);

    // So does a failing real write, and a successful one clears it.
    query_only(&store, true).await;
    let err = put(&store, calendar, "a.ics").await.unwrap_err();
    assert!(err.is_read_only(), "{err}");
    assert_eq!(store.health.failure(), Some(FailureKind::ReadOnly));
    assert_eq!(store.write_failures(), 1);
    query_only(&store, false).await;
    put(&store, calendar, "a.ics").await.unwrap();
    assert_eq!(store.health.failure(), None);
    assert_eq!(store.readiness().await, Readiness::Ready);
}

#[tokio::test]
async fn full_disk_stays_until_the_probe_finds_room() {
    let (store, calendar) = setup().await;
    let recorded = store.write_failures();
    assert!(matches!(
        store.observe::<()>(Err(StoreError::Full)),
        Err(StoreError::Full)
    ));
    assert_eq!(store.health.failure(), Some(FailureKind::Full));
    assert_eq!(store.write_failures(), recorded + 1);
    // A small write may fit into existing pages: no proof of free space.
    put(&store, calendar, "small.ics").await.unwrap();
    assert_eq!(store.health.failure(), Some(FailureKind::Full));

    // The probe commits a blob that needs new pages, then removes it.
    assert_eq!(store.readiness().await, Readiness::Ready);
    assert_eq!(store.health.failure(), None);
    assert_eq!(health_row(&store).await, (0, None));
}

#[tokio::test]
async fn space_probe_grows_the_file_at_most_once() {
    let dir = TempDir::new("space");
    let store = Store::open(&dir.0.join("calcraft.db")).await.unwrap();
    let mut sizes = Vec::new();
    for _ in 0..20 {
        store.health.record(&StoreError::Full, false);
        assert_eq!(store.readiness().await, Readiness::Ready);
        sizes.push(page_count(&store).await);
    }
    // The freed pages are reused by every later probe.
    assert!(sizes.iter().all(|&size| size == sizes[0]), "{sizes:?}");
    assert_eq!(health_row(&store).await, (0, None));
    store.close().await;
}

#[tokio::test]
async fn other_errors_are_not_recorded() {
    let (store, _) = setup().await;
    for err in [
        StoreError::Busy,
        StoreError::Limit("quota"),
        StoreError::PreconditionFailed,
    ] {
        assert!(store.observe::<()>(Err(err)).is_err());
    }
    assert_eq!(store.health.failure(), None);
    assert_eq!(store.write_failures(), 0);
    assert_eq!(Readiness::Ready.reason(), "ok");
    assert_eq!(Readiness::Full.reason(), "storage full");
    assert_eq!(Readiness::NotWritable.reason(), "database not writable");
    assert_eq!(Readiness::Unavailable.reason(), "database unavailable");
}

#[tokio::test]
async fn busy_writer_keeps_the_last_known_state() {
    let dir = TempDir::new("busy");
    let store = Store::open(&dir.0.join("calcraft.db")).await.unwrap();
    store.health.set_acquire_timeout(Duration::from_millis(20));
    assert_eq!(store.readiness().await, Readiness::Ready);

    // A write burst holds the single writer longer than the probe waits.
    let held = store.writer.acquire().await.unwrap();
    assert_eq!(store.probe_readiness().await, None);
    assert_eq!(store.readiness().await, Readiness::Ready);
    // A recorded failure still counts while the writer is busy.
    store.health.record(&StoreError::Full, true);
    assert_eq!(store.probe_readiness().await, None);
    assert_eq!(store.readiness().await, Readiness::Full);
    drop(held);

    assert_eq!(store.readiness().await, Readiness::Ready);
    store.close().await;
}

#[tokio::test]
async fn probe_restores_the_busy_timeout() {
    assert_eq!(
        SHORT_BUSY_TIMEOUT,
        format!("PRAGMA busy_timeout = {}", PROBE_BUSY_TIMEOUT.as_millis())
    );
    assert_eq!(
        NORMAL_BUSY_TIMEOUT,
        format!(
            "PRAGMA busy_timeout = {}",
            crate::backup::BUSY_TIMEOUT.as_millis()
        )
    );
    let dir = TempDir::new("timeout");
    let store = Store::open(&dir.0.join("calcraft.db")).await.unwrap();
    for space in [false, true] {
        if space {
            store.health.record(&StoreError::Full, false);
        }
        assert_eq!(store.readiness().await, Readiness::Ready);
        let timeout: i64 = sqlx::query("PRAGMA busy_timeout")
            .fetch_one(&store.writer)
            .await
            .unwrap()
            .get(0);
        assert_eq!(timeout, 5000);
    }
    store.close().await;
}

#[tokio::test]
async fn probe_removes_a_leftover_blob() {
    let (store, _) = setup().await;
    // As if resetting the blob after a space probe had failed.
    sqlx::query("UPDATE health SET pad = zeroblob(1000) WHERE id = 1")
        .execute(&store.writer)
        .await
        .unwrap();
    assert_eq!(health_row(&store).await, (0, Some(1000)));
    assert_eq!(store.readiness().await, Readiness::Ready);
    assert_eq!(health_row(&store).await, (0, None));
}
