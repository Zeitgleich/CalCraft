//! Readiness: can the database be read *and* written?
//!
//! `SELECT 1` alone keeps answering while the file is read-only or the disk
//! is full, although every write fails. [`Store::readiness`] therefore also
//! runs a write probe on the writer connection, and the store remembers
//! when a real write failed for lack of space:
//!
//! - Normally the probe updates the single row of the `health` table inside
//!   a transaction it rolls back (`BEGIN IMMEDIATE`, `UPDATE`, `ROLLBACK`).
//!   That takes the write lock and fails on a read-only file, under `PRAGMA
//!   query_only` and while another process holds the lock, but writes
//!   nothing: no WAL frames, no fsync, no growth.
//! - A full disk does not show on a rolled-back in-place update. Writes
//!   that fail with [`StoreError::Full`] are recorded instead, and the
//!   store stays not ready until the probe proves there is room again: it
//!   then commits a 64 KiB blob (which needs new pages) and sets it back to
//!   NULL. The freed pages go to SQLite's freelist and are reused by the
//!   next probe or real write, so the file grows at most once by 64 KiB.
//!   A small real write that succeeds does not clear the state, since it may
//!   fit into existing pages while the disk is still full.

use std::{
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use sqlx::{Connection, Sqlite, pool::PoolConnection};

use crate::{Result, Store, StoreError};

/// How long the probe waits for the writer connection. Requests queue for
/// it too; a probe never waits behind them for long.
pub const PROBE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(1);

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
    /// The database does not answer, stays locked or failed otherwise.
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
}

#[derive(Debug, Default)]
struct State {
    /// The current failure and since when.
    failure: Option<(FailureKind, Instant)>,
    /// Number of failures recorded so far (each new failure counts).
    recorded: u64,
}

/// Write failures of one store, shared by all clones.
#[derive(Debug, Default)]
pub(crate) struct WriteHealth {
    state: Mutex<State>,
}

impl WriteHealth {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records `err` if it shows that writes fail (full or read-only).
    pub(crate) fn record(&self, err: &StoreError) {
        let Some(kind) = FailureKind::of(err) else {
            return;
        };
        let mut state = self.lock();
        state.recorded += 1;
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
            Err(err) => self.health.record(err),
        }
        result
    }

    /// Number of write failures recorded so far. A cached readiness result
    /// is stale once this changes.
    pub fn write_failures(&self) -> u64 {
        self.health.recorded()
    }

    /// Checks that the database can be read and written, see the module
    /// docs. Never changes user data; waits at most
    /// [`PROBE_ACQUIRE_TIMEOUT`] for the writer connection.
    pub async fn readiness(&self) -> Readiness {
        if let Err(err) = self.ping().await {
            tracing::debug!(%err, sqlite_code = err.sqlite_code(), "readiness: read failed");
            return Readiness::Unavailable;
        }
        let result = if self.health.failure() == Some(FailureKind::Full) {
            self.space_probe().await
        } else {
            self.write_probe().await
        };
        match result {
            Ok(()) => {
                self.health.clear(None);
                Readiness::Ready
            }
            Err(err) => {
                tracing::debug!(%err, sqlite_code = err.sqlite_code(), "readiness: write probe failed");
                self.health.record(&err);
                match err {
                    StoreError::Full => Readiness::Full,
                    err if err.is_read_only() => Readiness::NotWritable,
                    _ => Readiness::Unavailable,
                }
            }
        }
    }

    async fn probe_connection(&self) -> Result<PoolConnection<Sqlite>> {
        match tokio::time::timeout(PROBE_ACQUIRE_TIMEOUT, self.writer.acquire()).await {
            Ok(conn) => Ok(conn?),
            Err(_) => Err(StoreError::Busy),
        }
    }

    /// Takes the write lock and changes one row, then rolls back.
    async fn write_probe(&self) -> Result<()> {
        let mut conn = self.probe_connection().await?;
        let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
        let changed = sqlx::query("UPDATE health SET probe = probe + 1 WHERE id = 1")
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.rollback().await?;
        if changed == 1 {
            Ok(())
        } else {
            Err(StoreError::Corrupt("health row missing".to_owned()))
        }
    }

    /// Commits a write that needs new pages, then frees them again.
    async fn space_probe(&self) -> Result<()> {
        let mut conn = self.probe_connection().await?;
        let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("UPDATE health SET pad = zeroblob(?) WHERE id = 1")
            .bind(SPACE_PROBE_BYTES)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        sqlx::query("UPDATE health SET pad = NULL WHERE id = 1")
            .execute(&mut *conn)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
