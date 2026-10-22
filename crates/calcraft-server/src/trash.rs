//! Trash support: display text of deleted objects and the background purge
//! of expired entries (`storage.trash_retention_days`).

use std::time::Duration;

use calcraft_core::CollectionKind;
use calcraft_store::{Store, StoreError};

use crate::{auth::now, validate::clean_text};

/// Rows deleted per purge transaction, so that the single writer is never
/// blocked for long.
pub const PURGE_BATCH: u32 = 500;
/// Pause between regular purge runs.
pub const PURGE_INTERVAL: Duration = Duration::from_secs(3600);
/// How often the purge task checks for entries that were emptied, deleted
/// permanently or dropped from a full trash (they are hidden already and
/// no longer count towards the quota; this only frees the rows).
pub const PURGE_POLL: Duration = Duration::from_secs(30);
/// Longest display text, in characters.
const MAX_SUMMARY: usize = 200;

/// SUMMARY of the first event, task or journal, or FN of a vCard, as plain
/// single-line text for the trash listing. `None` if there is none or it
/// cannot be shown safely.
pub fn summary(kind: CollectionKind, content: &[u8]) -> Option<String> {
    let root = calcraft_ical::parse(content).ok()?;
    let value = match kind {
        CollectionKind::Calendar => root
            .components
            .iter()
            .filter(|c| c.name != "VTIMEZONE")
            .find_map(|c| c.property("SUMMARY"))?
            .value
            .clone(),
        CollectionKind::Addressbook => root.property("FN")?.value.clone(),
    };
    let text = calcraft_ical::write::unescape_text(&value);
    // One line, without control or invisible formatting characters.
    let line: String = text
        .split(char::is_control)
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let short: String = line.chars().take(MAX_SUMMARY).collect();
    clean_text(&short, MAX_SUMMARY).filter(|t| !t.is_empty())
}

/// Purges expired trash entries in batches until none are left. Returns
/// the number of rows deleted.
pub async fn purge_expired(store: &Store) -> Result<u64, StoreError> {
    let mut total = 0;
    loop {
        let deleted = store.purge_expired_trash(now(), PURGE_BATCH).await?;
        total += deleted;
        if deleted == 0 {
            return Ok(total);
        }
        // Let waiting writers in between batches.
        tokio::task::yield_now().await;
    }
}

async fn purge_and_log(store: &Store) {
    match purge_expired(store).await {
        Ok(0) => {}
        Ok(rows) => tracing::info!(rows, "purged expired trash entries"),
        Err(err) => {
            tracing::warn!(%err, sqlite_code = err.sqlite_code(), "purging the trash failed");
        }
    }
}

/// Purges expired entries now, then every [`PURGE_INTERVAL`], and within
/// [`PURGE_POLL`] when entries were deleted from the trash.
pub fn spawn_purge(store: Store) {
    tokio::spawn(async move {
        let mut last = tokio::time::Instant::now();
        purge_and_log(&store).await;
        loop {
            tokio::time::sleep(PURGE_POLL).await;
            if store.take_purge_request() || last.elapsed() >= PURGE_INTERVAL {
                last = tokio::time::Instant::now();
                purge_and_log(&store).await;
            }
        }
    });
}

/// Warns at startup if the trash is disabled while entries are left in it:
/// they are purged right away.
pub async fn warn_if_disabled_with_entries(store: &Store) {
    if store.trash_retention_days() != 0 {
        return;
    }
    match store.trash_entry_count().await {
        Ok(0) => {}
        Ok(entries) => tracing::warn!(
            entries,
            "storage.trash_retention_days = 0 disables the trash: the entries still in it are \
             purged now and can no longer be restored"
        ),
        Err(err) => tracing::warn!(%err, "counting trash entries failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries() {
        let event = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:X\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Lunch\\, with Bob\\nand Eve\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert_eq!(
            summary(CollectionKind::Calendar, event).as_deref(),
            Some("Lunch, with Bob and Eve")
        );
        let card = b"BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c\r\nFN:Erika Mustermann\r\nEND:VCARD\r\n";
        assert_eq!(
            summary(CollectionKind::Addressbook, card).as_deref(),
            Some("Erika Mustermann")
        );
        let hidden = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c\r\nFN:a\u{202E}b\r\nEND:VCARD\r\n";
        assert_eq!(
            summary(CollectionKind::Addressbook, hidden.as_bytes()),
            None
        );
        let long = format!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c\r\nFN:{}\r\nEND:VCARD\r\n",
            "x".repeat(500)
        );
        assert_eq!(
            summary(CollectionKind::Addressbook, long.as_bytes())
                .unwrap()
                .len(),
            200
        );
        assert_eq!(summary(CollectionKind::Calendar, b"garbage"), None);
    }
}
