//! Readiness: can the database be read *and* written?
//!
//! `SELECT 1` alone keeps answering while the file is read-only or the disk
//! is full, although every write fails. [`Store::readiness`] therefore also
//! runs a write probe on the writer connection, and the store remembers
//! when a real write failed for lack of space:
//!
//! - Normally the probe updates the single row of the `health` table inside
//!   a transaction it rolls back (`BEGIN IMMEDIATE`, `UPDATE`, `ROLLBACK`).
//!   That takes the write lock and fails on a read-only file and under
//!   `PRAGMA query_only`, but writes nothing: no WAL frames, no fsync, no
//!   growth.
//! - A full disk does not show on a rolled-back in-place update. Writes
//!   that fail with [`StoreError::Full`] are recorded instead, and the
//!   store stays not ready until the probe proves there is room again: it
//!   then commits a 64 KiB blob (which needs new pages) and sets it back to
//!   NULL in a second transaction (retried by the next probe if that one
//!   fails). The freed pages go to SQLite's freelist and are reused by the
//!   next probe or real write, so the file grows at most once by 64 KiB.
//!   A small real write that succeeds does not clear the state, since it may
//!   fit into existing pages while the disk is still full.
//! - A busy writer is no new information: if the probe cannot get the
//!   writer connection within [`PROBE_ACQUIRE_TIMEOUT`] (a burst of writes)
//!   or the write lock within [`PROBE_BUSY_TIMEOUT`] (another process holds
//!   it), the last known state counts (ready unless a failure is recorded).

use std::{
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use sqlx::{Connection, Row, SqliteConnection};

use crate::{Result, Store, StoreError};

/// How long the probe waits for the writer connection. Requests queue for
/// it too; a probe never waits behind them for long.
pub const PROBE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(1);

/// How long the probe waits for the write lock while another process holds
/// it (instead of the connection's 5 s `busy_timeout`).
pub const PROBE_BUSY_TIMEOUT: Duration = Duration::from_millis(500);

/// [`PROBE_BUSY_TIMEOUT`] and the connections' normal busy timeout
/// (`backup::BUSY_TIMEOUT`) as statements; PRAGMA takes no bound
/// parameters. Tests check that they match.
const SHORT_BUSY_TIMEOUT: &str = "PRAGMA busy_timeout = 500";
const NORMAL_BUSY_TIMEOUT: &str = "PRAGMA busy_timeout = 5000";

/// Size of the blob the probe commits after a full disk.
const SPACE_PROBE_BYTES: i64 = 64 * 1024;

/// Result of [`Store::readiness`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// The database can be read and written.
    Ready,
    /// Writes fail: read-only file or file system.
    NotWritable,
    /// A write failed for lack of space and there is still no room.
    Full,
    /// The database cannot be read, or the write probe failed otherwise
    /// (e.g. an I/O error).
    Unavailable,
}

impl Readiness {
    pub fn is_ready(self) -> bool {
        self == Self::Ready
    }

    /// A short reason without details, for the `/readyz` body.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Ready => "ok",
            Self::NotWritable => "database not writable",
            Self::Full => "storage full",
            Self::Unavailable => "database unavailable",
        }
    }
}

/// Kind of a recorded write failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    ReadOnly,
    Full,
}

impl FailureKind {
    fn of(err: &StoreError) -> Option<Self> {
        match err {
            StoreError::Full => Some(Self::Full),
            err if err.is_read_only() => Some(Self::ReadOnly),
            _ => None,
        }
    }

    fn readiness(self) -> Readiness {
        match self {
            Self::ReadOnly => Readiness::NotWritable,
            Self::Full => Readiness::Full,
        }
    }
}

#[derive(Debug, Default)]
struct State {
    /// The current failure and since when.
    failure: Option<(FailureKind, Instant)>,
    /// Failures of real writes recorded so far (not the probe's own).
    recorded: u64,
}

/// Write failures of one store, shared by all clones.
#[derive(Debug)]
pub(crate) struct WriteHealth {
    state: Mutex<State>,
    /// [`PROBE_ACQUIRE_TIMEOUT`] in milliseconds; lowered by tests.
    acquire_timeout_ms: AtomicU64,
}

impl Default for WriteHealth {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            acquire_timeout_ms: AtomicU64::new(
                u64::try_from(PROBE_ACQUIRE_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            ),
        }
    }
}

impl WriteHealth {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records `err` if it shows that writes fail (full or read-only).
    /// `real`: a failure of a real write, which makes cached readiness
    /// results stale (see [`Store::write_failures`]).
    pub(crate) fn record(&self, err: &StoreError, real: bool) {
        let Some(kind) = FailureKind::of(err) else {
            return;
        };
        let mut state = self.lock();
        if real {
            state.recorded += 1;
        }
        match state.failure {
            Some((current, _)) if current == kind => {}
            _ => {
                tracing::warn!(kind = ?kind, "database writes fail, instance not ready");
                state.failure = Some((kind, Instant::now()));
            }
        }
    }

    /// Clears the recorded failure (`only`: just a failure of that kind).
    pub(crate) fn clear(&self, only: Option<FailureKind>) {
        let mut state = self.lock();
        if let Some((kind, since)) = state.failure
            && only.is_none_or(|only| only == kind)
        {
            tracing::info!(
                kind = ?kind,
                after_secs = since.elapsed().as_secs(),
                "database writable again"
            );
            state.failure = None;
        }
    }

    pub(crate) fn failure(&self) -> Option<FailureKind> {
        self.lock().failure.map(|(kind, _)| kind)
    }

    fn recorded(&self) -> u64 {
        self.lock().recorded
    }

    fn acquire_timeout(&self) -> Duration {
        Duration::from_millis(self.acquire_timeout_ms.load(Ordering::Relaxed))
    }

    #[cfg(test)]
    pub(crate) fn set_acquire_timeout(&self, timeout: Duration) {
        self.acquire_timeout_ms.store(
            u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

impl StoreError {
    /// The database file or file system is read-only (`SQLITE_READONLY`,
    /// also under `PRAGMA query_only`).
    pub fn is_read_only(&self) -> bool {
        const SQLITE_READONLY: i64 = 8;
        self.sqlite_code().map(|code| code & 0xff) == Some(SQLITE_READONLY)
    }
}

impl Store {
    /// Records the outcome of a write: a full disk or read-only database
    /// makes the store not ready, a successful write clears a recorded
    /// read-only failure (a full disk only the probe, see the module docs).
    pub(crate) fn observe<T>(&self, result: Result<T>) -> Result<T> {
        match &result {
            Ok(_) => self.health.clear(Some(FailureKind::ReadOnly)),
            Err(err) => self.health.record(err, true),
        }
        result
    }

    /// Number of failed real writes recorded so far. A cached readiness
    /// result is stale once this changes.
    pub fn write_failures(&self) -> u64 {
        self.health.recorded()
    }

    /// The readiness without a new probe: ready unless a write failure is
    /// recorded.
    pub fn assumed_readiness(&self) -> Readiness {
        self.health
            .failure()
            .map_or(Readiness::Ready, FailureKind::readiness)
    }

    /// Checks that the database can be read and written, see the module
    /// docs; [`Store::assumed_readiness`] if the writer is busy.
    pub async fn readiness(&self) -> Readiness {
        match self.probe_readiness().await {
            Some(readiness) => readiness,
            None => self.assumed_readiness(),
        }
    }

    /// Like [`Store::readiness`], but `None` if the probe could not get the
    /// writer connection or the write lock in time (no new information).
    /// Never changes user data.
    pub async fn probe_readiness(&self) -> Option<Readiness> {
        if let Err(err) = self.ping().await {
            tracing::debug!(%err, sqlite_code = err.sqlite_code(), "readiness: read failed");
            return Some(Readiness::Unavailable);
        }
        let space = self.health.failure() == Some(FailureKind::Full);
        match self.probe(space).await {
            Ok(()) => {
                self.health.clear(None);
                Some(Readiness::Ready)
            }
            Err(StoreError::Busy) => {
                tracing::debug!("readiness: writer busy, keeping the last known state");
                None
            }
            Err(err) => {
                tracing::debug!(%err, sqlite_code = err.sqlite_code(), "readiness: write probe failed");
                self.health.record(&err, false);
                Some(match FailureKind::of(&err) {
                    Some(kind) => kind.readiness(),
                    None => Readiness::Unavailable,
                })
            }
        }
    }

    /// Runs the write probe (`space`: the one that needs new pages) on the
    /// writer connection with a short busy timeout.
    async fn probe(&self, space: bool) -> Result<()> {
        let mut conn = match tokio::time::timeout(
            self.health.acquire_timeout(),
            self.writer.acquire(),
        )
        .await
        {
            Ok(conn) => conn?,
            Err(_) => return Err(StoreError::Busy),
        };
        sqlx::query(SHORT_BUSY_TIMEOUT).execute(&mut *conn).await?;
        let result = if space {
            space_probe(&mut conn).await
        } else {
            write_probe(&mut conn).await
        };
        if sqlx::query(NORMAL_BUSY_TIMEOUT)
            .execute(&mut *conn)
            .await
            .is_err()
        {
            // Never hand out the writer with the short timeout.
            conn.close_on_drop();
        }
        result
    }
}

/// Takes the write lock and changes one row, then rolls back. Removes a
/// blob left behind by an interrupted [`space_probe`].
async fn write_probe(conn: &mut SqliteConnection) -> Result<()> {
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query(
        "UPDATE health SET probe = probe + 1 WHERE id = 1 RETURNING pad IS NOT NULL AS padded",
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.rollback().await?;
    let Some(row) = row else {
        return Err(StoreError::Corrupt("health row missing".to_owned()));
    };
    if row.try_get::<bool, _>("padded")? {
        clear_pad(conn).await?;
    }
    Ok(())
}

/// Commits a write that needs new pages, then frees them again.
async fn space_probe(conn: &mut SqliteConnection) -> Result<()> {
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("UPDATE health SET pad = zeroblob(?) WHERE id = 1")
        .bind(SPACE_PROBE_BYTES)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    clear_pad(conn).await
}

async fn clear_pad(conn: &mut SqliteConnection) -> Result<()> {
    sqlx::query("UPDATE health SET pad = NULL WHERE id = 1 AND pad IS NOT NULL")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
