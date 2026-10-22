//! Calendar subscriptions (webcal/ICS feeds).
//!
//! A subscription is a calendar collection with `read_only = 1` plus a row
//! in `subscriptions` (feed URL, refresh state). The server fetches the
//! feed and calls [`Store::apply_subscription_feed`], which replaces the
//! collection's objects in one transaction: only objects that really
//! changed get a new revision and a change-log entry, so sync clients
//! download just those. Every other write path refuses read-only
//! collections ([`StoreError::ReadOnly`]).

use std::collections::HashMap;

use calcraft_core::{CollectionKind, Slug};
use sqlx::{Row, sqlite::SqliteRow};

use crate::{
    Collection, Result, Store, StoreError, collection_from_row,
    items::{MAX_COLLECTIONS_PER_USER, MAX_ITEMS_PER_COLLECTION, bump_revision, record_change},
};

/// A subscription with its collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub collection: Collection,
    /// Feed URL; may contain a secret token (show only to the owner).
    pub source_url: String,
    /// Interval chosen by the owner.
    pub refresh_secs: Option<i64>,
    /// `REFRESH-INTERVAL` / `X-PUBLISHED-TTL` of the last good feed.
    pub feed_ttl_secs: Option<i64>,
    pub http_etag: Option<String>,
    pub http_last_modified: Option<String>,
    pub content_hash: Option<String>,
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_error_detail: Option<String>,
    pub failures: i64,
    pub skipped: i64,
    pub next_refresh_at: i64,
    pub created_at: i64,
    /// Objects currently stored.
    pub items: i64,
}

/// One object of a fetched feed, validated like a PUT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedItem {
    /// Stable name derived from the UID.
    pub name: String,
    pub uid: String,
    pub component: String,
    pub range_start: Option<i64>,
    pub range_end: Option<i64>,
    pub recurring: bool,
    pub index_version: i64,
    /// Content hash.
    pub etag: String,
    pub content: Vec<u8>,
}

/// State recorded with a successfully applied feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedState<'a> {
    pub feed_ttl_secs: Option<i64>,
    pub http_etag: Option<&'a str>,
    pub http_last_modified: Option<&'a str>,
    pub content_hash: &'a str,
    pub skipped: i64,
    pub now: i64,
    pub next_refresh_at: i64,
}

/// A new subscription.
#[derive(Debug, Clone, Copy)]
pub struct NewSubscription<'a> {
    pub owner_id: i64,
    pub slug: &'a Slug,
    pub display_name: Option<&'a str>,
    pub color: Option<&'a str>,
    /// Component types the calendar accepts, e.g. `VEVENT,VTODO`.
    pub components: &'a str,
    pub source_url: &'a str,
    pub refresh_secs: Option<i64>,
    /// Most subscriptions per user.
    pub max_per_user: i64,
}

/// A failed refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionFailure<'a> {
    pub code: &'a str,
    pub detail: Option<&'a str>,
    pub now: i64,
    pub next_refresh_at: i64,
}

/// What a refresh changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplaceOutcome {
    pub added: u64,
    pub updated: u64,
    pub removed: u64,
    pub unchanged: u64,
}

impl ReplaceOutcome {
    pub fn changed(&self) -> bool {
        self.added + self.updated + self.removed > 0
    }
}

/// A query over subscriptions joined with their collection; `$tail` is a
/// fixed `WHERE` clause (SQL text is never built at runtime).
macro_rules! select {
    ($tail:literal) => {
        concat!(
            "SELECT c.id, c.owner_id, c.kind, c.slug, c.display_name, c.description, \
               c.color, c.sync_revision, c.components, c.sync_floor, c.token_salt, c.read_only, \
               s.source_url, s.refresh_secs, s.feed_ttl_secs, s.http_etag, s.http_last_modified, \
               s.content_hash, s.last_attempt_at, s.last_success_at, s.last_error, \
               s.last_error_detail, s.failures, s.skipped, s.next_refresh_at, s.created_at, \
               (SELECT COUNT(*) FROM items i WHERE i.collection_id = c.id) AS items \
             FROM subscriptions s JOIN collections c ON c.id = s.collection_id ",
            $tail
        )
    };
}

fn subscription_from_row(row: &SqliteRow) -> Result<Subscription> {
    Ok(Subscription {
        collection: collection_from_row(row)?,
        source_url: row.try_get("source_url")?,
        refresh_secs: row.try_get("refresh_secs")?,
        feed_ttl_secs: row.try_get("feed_ttl_secs")?,
        http_etag: row.try_get("http_etag")?,
        http_last_modified: row.try_get("http_last_modified")?,
        content_hash: row.try_get("content_hash")?,
        last_attempt_at: row.try_get("last_attempt_at")?,
        last_success_at: row.try_get("last_success_at")?,
        last_error: row.try_get("last_error")?,
        last_error_detail: row.try_get("last_error_detail")?,
        failures: row.try_get("failures")?,
        skipped: row.try_get("skipped")?,
        next_refresh_at: row.try_get("next_refresh_at")?,
        created_at: row.try_get("created_at")?,
        items: row.try_get("items")?,
    })
}

type Tx = sqlx::Transaction<'static, sqlx::Sqlite>;

/// Checks the per-collection object limit and the owner's quota for
/// replacing the objects of `collection_id` by `items`.
async fn check_limits(
    tx: &mut Tx,
    store: &Store,
    owner_id: i64,
    collection_id: i64,
    items: &[FeedItem],
) -> Result<()> {
    if i64::try_from(items.len()).unwrap_or(i64::MAX) > MAX_ITEMS_PER_COLLECTION {
        return Err(StoreError::Limit("too many objects in collection"));
    }
    // Objects of the owner's other collections plus the trash, as for PUT.
    let used: i64 = sqlx::query(
        "SELECT COALESCE(SUM(length(i.content)), 0) AS used FROM items i \
         JOIN collections c ON c.id = i.collection_id \
         WHERE c.owner_id = ? AND i.collection_id <> ?",
    )
    .bind(owner_id)
    .bind(collection_id)
    .fetch_one(&mut **tx)
    .await?
    .try_get("used")?;
    let used = used.saturating_add(crate::trash::trash_bytes(&mut **tx, owner_id).await?);
    let size = items.iter().fold(0i64, |sum, item| {
        sum.saturating_add(i64::try_from(item.content.len()).unwrap_or(i64::MAX))
    });
    if used.saturating_add(size) > store.settings.quota_bytes() {
        return Err(StoreError::Limit("storage quota exceeded"));
    }
    Ok(())
}

async fn upsert(tx: &mut Tx, collection_id: i64, revision: i64, item: &FeedItem) -> Result<()> {
    sqlx::query(
        "INSERT INTO items (collection_id, name, uid, etag, component, range_start, range_end, \
           recurring, index_version, content, revision) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (collection_id, name) DO UPDATE SET \
           uid = excluded.uid, etag = excluded.etag, component = excluded.component, \
           range_start = excluded.range_start, range_end = excluded.range_end, \
           recurring = excluded.recurring, index_version = excluded.index_version, \
           content = excluded.content, revision = excluded.revision, \
           modified_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .bind(collection_id)
    .bind(&item.name)
    .bind(&item.uid)
    .bind(&item.etag)
    .bind(&item.component)
    .bind(item.range_start)
    .bind(item.range_end)
    .bind(item.recurring)
    .bind(item.index_version)
    .bind(&item.content)
    .bind(revision)
    .execute(&mut **tx)
    .await?;
    record_change(tx, collection_id, revision, &item.name, false).await
}

/// Replaces the objects of `collection_id` by `items`: unchanged objects
/// (same name and entity tag) are left alone; all changes share one new
/// revision.
async fn replace_items(
    tx: &mut Tx,
    collection_id: i64,
    items: &[FeedItem],
) -> Result<ReplaceOutcome> {
    let existing: HashMap<String, String> =
        sqlx::query("SELECT name, etag FROM items WHERE collection_id = ?")
            .bind(collection_id)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|row| Ok((row.try_get("name")?, row.try_get("etag")?)))
            .collect::<Result<_>>()?;
    let wanted: HashMap<&str, &FeedItem> = items
        .iter()
        .map(|item| (item.name.as_str(), item))
        .collect();
    if wanted.len() != items.len() {
        return Err(StoreError::Corrupt(
            "duplicate object names in feed".to_owned(),
        ));
    }
    let mut outcome = ReplaceOutcome::default();
    let mut removed: Vec<&str> = existing
        .keys()
        .map(String::as_str)
        .filter(|name| !wanted.contains_key(name))
        .collect();
    removed.sort_unstable();
    let changed: Vec<&FeedItem> = items
        .iter()
        .filter(|item| existing.get(&item.name) != Some(&item.etag))
        .collect();
    outcome.unchanged = u64::try_from(items.len() - changed.len()).unwrap_or(u64::MAX);
    if removed.is_empty() && changed.is_empty() {
        return Ok(outcome);
    }
    let revision = bump_revision(tx, collection_id).await?;
    // Removed first, so that a UID can move to another name.
    for name in removed {
        sqlx::query("DELETE FROM items WHERE collection_id = ? AND name = ?")
            .bind(collection_id)
            .bind(name)
            .execute(&mut **tx)
            .await?;
        record_change(tx, collection_id, revision, name, true).await?;
        outcome.removed += 1;
    }
    for item in changed {
        if existing.contains_key(&item.name) {
            outcome.updated += 1;
        } else {
            outcome.added += 1;
        }
        upsert(tx, collection_id, revision, item).await?;
    }
    Ok(outcome)
}

impl Store {
    /// Creates a subscription with the objects of its first fetch, in one
    /// transaction: nothing is left behind if a limit is hit.
    pub async fn create_subscription(
        &self,
        new: &NewSubscription<'_>,
        items: &[FeedItem],
        state: &FeedState<'_>,
    ) -> Result<Collection> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let row = sqlx::query(
                    "SELECT (SELECT COUNT(*) FROM collections WHERE owner_id = ?1) AS collections, \
               (SELECT COUNT(*) FROM subscriptions s JOIN collections c ON c.id = s.collection_id \
                WHERE c.owner_id = ?1) AS subscriptions",
                )
                .bind(new.owner_id)
                .fetch_one(&mut *tx)
                .await?;
                if row.try_get::<i64, _>("subscriptions")? >= new.max_per_user {
                    return Err(StoreError::Limit("too many subscriptions"));
                }
                if row.try_get::<i64, _>("collections")? >= MAX_COLLECTIONS_PER_USER {
                    return Err(StoreError::Limit("too many collections"));
                }
                let result = sqlx::query(
            "INSERT INTO collections (owner_id, kind, slug, display_name, color, components, \
               token_salt, read_only) \
             VALUES (?, ?, ?, ?, ?, ?, abs(random()), 1) RETURNING id",
        )
        .bind(new.owner_id)
        .bind(CollectionKind::Calendar.as_str())
        .bind(new.slug.as_str())
        .bind(new.display_name)
        .bind(new.color)
        .bind(new.components)
        .fetch_one(&mut *tx)
        .await;
                let collection_id: i64 = match result {
                    Ok(row) => row.try_get("id")?,
                    Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
                        return Err(StoreError::CollectionExists);
                    }
                    Err(err) => return Err(err.into()),
                };
                check_limits(&mut tx, self, new.owner_id, collection_id, items).await?;
                sqlx::query(
            "INSERT INTO subscriptions (collection_id, source_url, refresh_secs, next_refresh_at, \
               created_at, last_attempt_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(collection_id)
        .bind(new.source_url)
        .bind(new.refresh_secs)
        .bind(state.next_refresh_at)
        .bind(state.now)
        .bind(state.now)
        .execute(&mut *tx)
        .await?;
                replace_items(&mut tx, collection_id, items).await?;
                record_success(&mut tx, collection_id, state).await?;
                let collection = sqlx::query(
            "SELECT id, owner_id, kind, slug, display_name, description, color, sync_revision, \
               components, sync_floor, token_salt, read_only FROM collections WHERE id = ?",
        )
        .bind(collection_id)
        .fetch_one(&mut *tx)
        .await?;
                let collection = collection_from_row(&collection)?;
                tx.commit().await?;
                Ok(collection)
            }
            .await,
        )
    }

    /// The subscriptions of a user, by slug.
    pub async fn subscriptions(&self, owner_id: i64) -> Result<Vec<Subscription>> {
        let rows = sqlx::query(select!("WHERE c.owner_id = ? ORDER BY c.slug"))
            .bind(owner_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(subscription_from_row).collect()
    }

    /// One subscription of a user.
    pub async fn subscription(&self, owner_id: i64, slug: &Slug) -> Result<Option<Subscription>> {
        let row = sqlx::query(select!(
            "WHERE c.owner_id = ? AND c.slug = ? AND c.kind = 'calendar'"
        ))
        .bind(owner_id)
        .bind(slug.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(subscription_from_row).transpose()
    }

    /// A subscription by its collection id (for the background refresher).
    pub async fn subscription_by_collection(
        &self,
        collection_id: i64,
    ) -> Result<Option<Subscription>> {
        let row = sqlx::query(select!("WHERE c.id = ?"))
            .bind(collection_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(subscription_from_row).transpose()
    }

    /// Collections of subscriptions due for a refresh at `now`, oldest
    /// first, at most `limit`. Disabled accounts are skipped.
    pub async fn due_subscriptions(&self, now: i64, limit: u32) -> Result<Vec<i64>> {
        let rows = sqlx::query(
            "SELECT s.collection_id FROM subscriptions s \
             JOIN collections c ON c.id = s.collection_id JOIN users u ON u.id = c.owner_id \
             WHERE s.next_refresh_at <= ? AND u.disabled = 0 \
             ORDER BY s.next_refresh_at LIMIT ?",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| row.try_get("collection_id").map_err(Into::into))
            .collect()
    }

    /// Starts a refresh: records the attempt and moves the next scheduled
    /// refresh to `next_refresh_at`, unless the previous attempt was less
    /// than `min_gap` seconds ago. Returns whether the refresh may run.
    pub async fn claim_subscription_refresh(
        &self,
        collection_id: i64,
        now: i64,
        min_gap: i64,
        next_refresh_at: i64,
    ) -> Result<bool> {
        self.observe(
            async {
                let result = sqlx::query(
                    "UPDATE subscriptions SET last_attempt_at = ?1, next_refresh_at = ?2 \
             WHERE collection_id = ?3 AND (last_attempt_at IS NULL OR last_attempt_at <= ?1 - ?4)",
                )
                .bind(now)
                .bind(next_refresh_at)
                .bind(collection_id)
                .bind(min_gap)
                .execute(&self.writer)
                .await?;
                Ok(result.rows_affected() == 1)
            }
            .await,
        )
    }

    /// Applies a fetched feed: replaces the objects (see [`replace_items`])
    /// and records the success. `Ok(None)` if the subscription was deleted
    /// meanwhile. On error (a limit) nothing changes.
    pub async fn apply_subscription_feed(
        &self,
        collection_id: i64,
        items: &[FeedItem],
        state: &FeedState<'_>,
    ) -> Result<Option<ReplaceOutcome>> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let Some(row) = sqlx::query(
            "SELECT c.owner_id FROM subscriptions s JOIN collections c ON c.id = s.collection_id \
             WHERE s.collection_id = ? AND c.read_only = 1",
        )
        .bind(collection_id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };
                let owner_id: i64 = row.try_get("owner_id")?;
                check_limits(&mut tx, self, owner_id, collection_id, items).await?;
                let outcome = replace_items(&mut tx, collection_id, items).await?;
                record_success(&mut tx, collection_id, state).await?;
                tx.commit().await?;
                Ok(Some(outcome))
            }
            .await,
        )
    }

    /// Records a refresh that found the feed unchanged (`304`, or the same
    /// body as last time).
    pub async fn subscription_unchanged(
        &self,
        collection_id: i64,
        now: i64,
        next_refresh_at: i64,
    ) -> Result<()> {
        self.observe(
            async {
                sqlx::query(
            "UPDATE subscriptions SET last_success_at = ?, next_refresh_at = ?, failures = 0, \
               last_error = NULL, last_error_detail = NULL WHERE collection_id = ?",
        )
        .bind(now)
        .bind(next_refresh_at)
        .bind(collection_id)
        .execute(&self.writer)
        .await?;
                Ok(())
            }
            .await,
        )
    }

    /// Records a failed refresh; the objects stay as they are.
    pub async fn subscription_failed(
        &self,
        collection_id: i64,
        failure: &SubscriptionFailure<'_>,
    ) -> Result<()> {
        self.observe(
            async {
                sqlx::query(
                    "UPDATE subscriptions SET last_error = ?, last_error_detail = ?, \
               failures = failures + 1, next_refresh_at = ? WHERE collection_id = ?",
                )
                .bind(failure.code)
                .bind(failure.detail)
                .bind(failure.next_refresh_at)
                .bind(collection_id)
                .execute(&self.writer)
                .await?;
                Ok(())
            }
            .await,
        )
    }

    /// Changes the refresh interval chosen by the owner (`Some(None)`:
    /// back to the feed's or the default) and the next refresh time.
    pub async fn update_subscription(
        &self,
        collection_id: i64,
        refresh_secs: Option<i64>,
        next_refresh_at: i64,
    ) -> Result<()> {
        self.observe(
            async {
        sqlx::query(
            "UPDATE subscriptions SET refresh_secs = ?, next_refresh_at = ? WHERE collection_id = ?",
        )
        .bind(refresh_secs)
        .bind(next_refresh_at)
        .bind(collection_id)
        .execute(&self.writer)
        .await?;
        Ok(())
            }
            .await,
        )
    }

    /// Number of subscriptions of a user.
    pub async fn count_subscriptions(&self, owner_id: i64) -> Result<i64> {
        Ok(sqlx::query(
            "SELECT COUNT(*) AS n FROM subscriptions s JOIN collections c ON c.id = s.collection_id \
             WHERE c.owner_id = ?",
        )
        .bind(owner_id)
        .fetch_one(&self.pool)
        .await?
        .try_get("n")?)
    }
}

async fn record_success(tx: &mut Tx, collection_id: i64, state: &FeedState<'_>) -> Result<()> {
    sqlx::query(
        "UPDATE subscriptions SET feed_ttl_secs = ?, http_etag = ?, http_last_modified = ?, \
           content_hash = ?, skipped = ?, last_success_at = ?, next_refresh_at = ?, failures = 0, \
           last_error = NULL, last_error_detail = NULL WHERE collection_id = ?",
    )
    .bind(state.feed_ttl_secs)
    .bind(state.http_etag)
    .bind(state.http_last_modified)
    .bind(state.content_hash)
    .bind(state.skipped)
    .bind(state.now)
    .bind(state.next_refresh_at)
    .bind(collection_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
