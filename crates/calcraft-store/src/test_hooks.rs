//! Simulated storage failures for the failure tests (cargo feature
//! `test-hooks`, enabled only by dev-dependencies). Without the feature
//! [`Simulation`] is an empty type and adds nothing to the pool.

use sqlx::sqlite::SqlitePoolOptions;

#[cfg(feature = "test-hooks")]
use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicU8, Ordering},
};

/// Storage failures that can be simulated, see [`crate::Store::simulate_failure`].
#[cfg(feature = "test-hooks")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimulatedFailure {
    /// Normal operation.
    None,
    /// Writes fail like on a read-only file or file system (`SQLITE_READONLY`).
    ReadOnly,
    /// No room for new pages, like a full disk (`SQLITE_FULL`).
    Full,
}

/// The simulated state of one store, shared with its pool's hooks.
#[derive(Debug, Clone, Default)]
pub(crate) struct Simulation {
    /// 0: never used (hooks do nothing), else `SimulatedFailure` + 1.
    #[cfg(feature = "test-hooks")]
    state: Arc<AtomicU8>,
    /// Simulated per-user quota in bytes; 0: the real one.
    #[cfg(feature = "test-hooks")]
    quota: Arc<AtomicI64>,
}

impl Simulation {
    /// Adds the hooks to the pool options.
    #[cfg_attr(not(feature = "test-hooks"), allow(clippy::unused_self))]
    pub(crate) fn hook(&self, options: SqlitePoolOptions) -> SqlitePoolOptions {
        #[cfg(feature = "test-hooks")]
        let options = {
            let on_connect = self.state.clone();
            let on_acquire = self.state.clone();
            // Applied to new connections and on every acquire, so that no
            // connection (e.g. a replaced one) escapes the simulation.
            options
                .after_connect(move |conn, _| {
                    let state = on_connect.load(Ordering::Acquire);
                    Box::pin(async move { apply(conn, state).await })
                })
                .before_acquire(move |conn, _| {
                    let state = on_acquire.load(Ordering::Acquire);
                    Box::pin(async move { apply(conn, state).await.map(|()| true) })
                })
        };
        options
    }

    #[cfg(feature = "test-hooks")]
    pub(crate) fn set(&self, failure: SimulatedFailure) {
        let state = match failure {
            SimulatedFailure::None => 1,
            SimulatedFailure::ReadOnly => 2,
            SimulatedFailure::Full => 3,
        };
        self.state.store(state, Ordering::Release);
    }

    #[cfg(feature = "test-hooks")]
    pub(crate) fn set_quota(&self, bytes: i64) {
        self.quota.store(bytes, Ordering::Release);
    }

    /// The simulated quota, if one is set.
    #[cfg_attr(not(feature = "test-hooks"), allow(clippy::unused_self))]
    pub(crate) fn quota(&self) -> Option<i64> {
        #[cfg(feature = "test-hooks")]
        {
            let bytes = self.quota.load(Ordering::Acquire);
            if bytes > 0 {
                return Some(bytes);
            }
        }
        None
    }
}

#[cfg(feature = "test-hooks")]
async fn apply(conn: &mut sqlx::SqliteConnection, state: u8) -> Result<(), sqlx::Error> {
    const NO_PAGE_LIMIT: &str = "PRAGMA max_page_count = 1073741823";
    let pragmas = match state {
        0 => return Ok(()),
        2 => ["PRAGMA query_only = ON", NO_PAGE_LIMIT],
        // SQLite raises a smaller value to the current size: no new pages.
        3 => ["PRAGMA query_only = OFF", "PRAGMA max_page_count = 1"],
        _ => ["PRAGMA query_only = OFF", NO_PAGE_LIMIT],
    };
    for pragma in pragmas {
        sqlx::query(pragma).execute(&mut *conn).await?;
    }
    Ok(())
}
