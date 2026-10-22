//! Trash support: display text of deleted objects and the background purge
//! of expired entries (`storage.trash_retention_days`).

use std::time::Duration;

use calcraft_core::CollectionKind;
use calcraft_store::{Store, StoreError};

use crate::{auth::now, validate::clean_text};

/// Rows deleted per purge transaction, so that the single writer is never
/// blocked for long.
pub const PURGE_BATCH: u32 = 500;
/// Pause between purge runs.
pub const PURGE_INTERVAL: Duration = Duration::from_secs(3600);
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

/// Purges expired entries now and then every [`PURGE_INTERVAL`].
pub fn spawn_purge(store: Store) {
    tokio::spawn(async move {
        loop {
            purge_and_log(&store).await;
            tokio::time::sleep(PURGE_INTERVAL).await;
        }
    });
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
