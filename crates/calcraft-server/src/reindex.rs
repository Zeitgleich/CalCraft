//! Re-indexing of stored calendar objects when the time-range indexing
//! changes ([`INDEX_VERSION`]), e.g. after an upgrade that added exact
//! recurrence expansion.
//!
//! Runs in the background after startup. Until a row is re-indexed it
//! keeps its old range, which was conservative (never too narrow), so
//! queries stay correct meanwhile. The work is bounded: rows are read in
//! small batches, each object is evaluated within the per-object budget,
//! and every row is visited once. It is idempotent: rows already at the
//! current version, or changed concurrently by a PUT (which indexes them
//! itself), are skipped.

use anyhow::Context;
use calcraft_ical::{CalendarObject, INDEX_VERSION};
use calcraft_store::{ItemIndex, StaleIndex, Store};

/// Objects read per batch (at most 1 MiB each).
const BATCH: u32 = 32;

/// Matches every time range: for objects this version cannot index.
const CONSERVATIVE: ItemIndex = ItemIndex {
    range_start: None,
    range_end: None,
    recurring: false,
    version: INDEX_VERSION,
};

fn index(content: &[u8]) -> ItemIndex {
    match CalendarObject::parse(content) {
        Ok(object) => ItemIndex {
            range_start: object.range.start,
            range_end: object.range.end,
            recurring: object.recurring,
            version: INDEX_VERSION,
        },
        // Accepted by an older version but not by this one: match every
        // time range rather than none.
        Err(_) => CONSERVATIVE,
    }
}

/// [`index`], but a panic (which would be a bug) only affects this object:
/// it is indexed conservatively and the run continues, so one bad row
/// cannot block re-indexing at every start.
fn index_isolated(id: i64, content: &[u8]) -> ItemIndex {
    std::panic::catch_unwind(|| index(content)).unwrap_or_else(|_| {
        tracing::error!(
            id,
            "indexing a calendar object panicked; indexed conservatively"
        );
        CONSERVATIVE
    })
}

/// Re-indexes every calendar object with an older index version. Returns
/// the number of updated rows.
pub async fn run(store: &Store) -> anyhow::Result<usize> {
    let mut after = 0;
    let mut updated = 0;
    loop {
        let batch: Vec<StaleIndex> = store.stale_index(INDEX_VERSION, after, BATCH).await?;
        let Some(last) = batch.last() else {
            return Ok(updated);
        };
        after = last.id;
        // Parsing and expansion are CPU work: keep them off the async threads.
        let indexed = tokio::task::spawn_blocking(move || {
            batch
                .into_iter()
                .map(|item| {
                    let index = index_isolated(item.id, &item.content);
                    (item.id, item.etag, index)
                })
                .collect::<Vec<_>>()
        })
        .await
        .context("re-indexing task failed")?;
        for (id, etag, index) in indexed {
            if store.set_item_index(id, &etag, &index).await? {
                updated += 1;
            }
        }
    }
}

/// Starts [`run`] in the background and logs the outcome.
pub fn spawn(store: Store) {
    tokio::spawn(async move {
        match run(&store).await {
            Ok(0) => {}
            Ok(count) => {
                tracing::info!(count, "re-indexed calendar objects for time-range queries")
            }
            Err(err) => tracing::warn!(
                err = format!("{err:#}"),
                "re-indexing calendar objects failed; time-range queries stay conservative \
                 for the remaining objects and re-indexing is retried at the next start"
            ),
        }
    });
}
