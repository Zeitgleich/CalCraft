//! Trash (soft delete) with restore.
//!
//! A DELETE of an object or a collection (DAV or REST API) moves it into
//! the owner's trash instead of destroying it. For sync clients nothing
//! changes: the change log records the deletion exactly as a final delete
//! does, and a later PUT of the same name or UID works. Restoring writes a
//! normal change-log entry, so clients sync the object back in.
//!
//! Entries are only ever looked up together with their owner, by a random
//! public id; another user's id behaves like one that does not exist.
//!
//! Only the functions here insert into the trash; the low-level deletes in
//! `items.rs` ([`Store::delete_item`], [`Store::delete_collection`]) stay
//! final, so other operations built on them (e.g. MOVE) never create trash
//! entries.

use calcraft_core::{CollectionKind, Slug};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    Result, Settings, Store, StoreError,
    items::{
        MAX_COLLECTIONS_PER_USER, MAX_ITEMS_PER_COLLECTION, Precondition, check, record_change,
    },
};

/// Default of `storage.trash_retention_days`.
pub const DEFAULT_TRASH_RETENTION_DAYS: u32 = 30;
/// Rows (objects and collections) in one user's trash. Large enough for
/// the biggest possible collection; when it is full, the oldest entries are
/// dropped early to make room.
pub const MAX_TRASH_ROWS_PER_USER: i64 = MAX_ITEMS_PER_COLLECTION + 1_000;
/// Rows a single delete may remove synchronously to make room in a full
/// trash; bigger old entries are left to the background purge.
pub const MAX_EVICT_ROWS: i64 = 1_000;
/// Longest stored display text of an entry, in characters.
const MAX_SUMMARY_CHARS: usize = 200;
const SECONDS_PER_DAY: i64 = 86_400;

/// What a trash entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashEntryKind {
    /// A calendar object or vCard.
    Item,
    /// A calendar or address book together with its objects.
    Collection,
}

/// A top-level entry of a user's trash, without content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    /// Random public identifier (32 hex digits).
    pub id: String,
    pub kind: TrashEntryKind,
    /// The collection the object was deleted from, or the deleted collection.
    pub collection_kind: CollectionKind,
    pub collection_slug: String,
    /// Resource name of an object; `None` for collections.
    pub name: Option<String>,
    /// SUMMARY of an event or task, FN of a vCard, display name of a
    /// collection: untrusted text, for display only.
    pub summary: Option<String>,
    /// Bytes of content (collections: of their objects and dead properties).
    pub size: i64,
    /// Objects inside a collection entry (0 for objects).
    pub items: i64,
    /// Unix seconds.
    pub deleted_at: i64,
}

/// One page of a user's trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashPage {
    pub entries: Vec<TrashEntry>,
    /// Top-level entries in total.
    pub total: i64,
    /// Bytes in the trash; they count towards the quota.
    pub bytes: i64,
}

/// Why a trash entry cannot be restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashConflict {
    /// The collection has a new object with the same name.
    NameTaken,
    /// The collection has another object with the same UID.
    UidTaken,
    /// A collection with the same name exists again.
    CollectionExists,
    /// The object's collection is in the trash: restore it first.
    CollectionInTrash,
    /// The object's collection was deleted permanently.
    CollectionDeleted,
    /// The requested target collection does not exist (for this user and
    /// kind), or a target was given for a collection entry.
    InvalidTarget,
}

impl TrashConflict {
    /// Stable machine-readable code for the API.
    pub fn code(self) -> &'static str {
        match self {
            Self::NameTaken => "name_taken",
            Self::UidTaken => "uid_taken",
            Self::CollectionExists => "collection_exists",
            Self::CollectionInTrash => "collection_in_trash",
            Self::CollectionDeleted => "collection_deleted",
            Self::InvalidTarget => "invalid_target",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::NameTaken => "an entry with this name exists again in the collection",
            Self::UidTaken => "another entry with the same UID exists in the collection",
            Self::CollectionExists => "a collection with this name exists again",
            Self::CollectionInTrash => {
                "the collection of this entry is in the trash; restore it first"
            }
            Self::CollectionDeleted => {
                "the collection of this entry was deleted permanently; restore it into another collection"
            }
            Self::InvalidTarget => {
                "the target must be another existing collection of the same kind (objects only)"
            }
        }
    }
}

impl std::fmt::Display for TrashConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// What a restore put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredEntry {
    pub kind: TrashEntryKind,
    pub collection_kind: CollectionKind,
    pub collection_slug: Slug,
    /// Resource name of a restored object.
    pub name: Option<String>,
    /// Entity tag of a restored object (unchanged by the trash).
    pub etag: Option<String>,
}

fn kind_of(value: &str) -> Result<TrashEntryKind> {
    match value {
        "item" => Ok(TrashEntryKind::Item),
        "collection" => Ok(TrashEntryKind::Collection),
        other => Err(StoreError::Corrupt(format!(
            "unknown trash entry kind {other:?}"
        ))),
    }
}

fn collection_kind_of(value: &str) -> Result<CollectionKind> {
    value
        .parse()
        .map_err(|()| StoreError::Corrupt(format!("unknown collection kind {value:?}")))
}

fn entry_from_row(row: &SqliteRow) -> Result<TrashEntry> {
    let kind: String = row.try_get("entry_kind")?;
    let collection_kind: String = row.try_get("collection_kind")?;
    Ok(TrashEntry {
        id: row.try_get("public_id")?,
        kind: kind_of(&kind)?,
        collection_kind: collection_kind_of(&collection_kind)?,
        collection_slug: row.try_get("collection_slug")?,
        name: row.try_get("name")?,
        summary: row.try_get("summary")?,
        size: row.try_get("size")?,
        items: row.try_get("items")?,
        deleted_at: row.try_get("deleted_at")?,
    })
}

fn shorten(summary: Option<String>) -> Option<String> {
    summary.map(|s| s.chars().take(MAX_SUMMARY_CHARS).collect::<String>())
}

/// Live (not purge-pending) rows of the owner's trash, children of pending
/// collection entries excluded.
/// (A macro, so that the queries stay static strings.)
macro_rules! live_rows {
    () => {
        "FROM trash c LEFT JOIN trash p ON p.id = c.parent_id \
         WHERE c.owner_id = ?1 AND c.purge_pending = 0 AND (p.id IS NULL OR p.purge_pending = 0)"
    };
}

/// Bytes of the owner's trash that count towards the quota: objects and
/// the dead properties of trashed collections.
pub(crate) async fn trash_bytes(
    executor: impl sqlx::SqliteExecutor<'_>,
    owner_id: i64,
) -> Result<i64> {
    Ok(sqlx::query(concat!(
        "SELECT COALESCE(SUM(c.size), 0) + (SELECT COALESCE(SUM(",
        crate::properties::property_bytes!("d"),
        "), 0) FROM trash_dead_properties d JOIN trash t ON t.id = d.trash_id \
           WHERE t.owner_id = ?1 AND t.purge_pending = 0) AS bytes ",
        live_rows!(),
        " AND c.entry_kind = 'item'"
    ))
    .bind(owner_id)
    .fetch_one(executor)
    .await?
    .try_get("bytes")?)
}

/// Makes room for `needed` more rows in the owner's trash by dropping the
/// oldest entries. At most `budget` rows are deleted right here; larger
/// old entries are only marked for the background purge (hidden at once,
/// removed in small batches later), so a full trash never holds the write
/// lock for long. Returns `false` if `needed` alone exceeds the limit (the
/// caller then deletes for good instead).
async fn make_room(
    tx: &mut Transaction<'static, Sqlite>,
    settings: &Settings,
    owner_id: i64,
    needed: i64,
) -> Result<bool> {
    let max_rows = settings.max_trash_rows();
    if needed > max_rows {
        return Ok(false);
    }
    let rows: i64 = sqlx::query(concat!("SELECT COUNT(*) AS n ", live_rows!()))
        .bind(owner_id)
        .fetch_one(&mut **tx)
        .await?
        .try_get("n")?;
    let mut excess = rows.saturating_add(needed) - max_rows;
    let mut budget = settings.max_evict_rows();
    let (mut deleted, mut marked) = (0_u64, 0_u64);
    while excess > 0 {
        let oldest = sqlx::query(
            "SELECT t.id, 1 + (SELECT COUNT(*) FROM trash c WHERE c.parent_id = t.id) AS n \
             FROM trash t WHERE t.owner_id = ? AND t.parent_id IS NULL AND t.purge_pending = 0 \
             ORDER BY t.deleted_at, t.id LIMIT 100",
        )
        .bind(owner_id)
        .fetch_all(&mut **tx)
        .await?;
        if oldest.is_empty() {
            break;
        }
        for row in oldest {
            let id: i64 = row.try_get("id")?;
            let n: i64 = row.try_get("n")?;
            if n <= budget {
                sqlx::query("DELETE FROM trash WHERE id = ?")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
                budget -= n;
                deleted += 1;
            } else {
                mark_for_purge(tx, id).await?;
                marked += 1;
            }
            excess -= n;
            if excess <= 0 {
                break;
            }
        }
    }
    if marked > 0 {
        settings.request_purge();
    }
    if deleted + marked > 0 {
        tracing::info!(
            owner_id,
            deleted,
            marked,
            "trash full: dropped the oldest entries early"
        );
    }
    Ok(true)
}

/// Hides a top-level entry and leaves its removal to the purge.
async fn mark_for_purge(tx: &mut Transaction<'static, Sqlite>, id: i64) -> Result<()> {
    // deleted_at = 0: the purge's index on deleted_at finds it right away.
    sqlx::query("UPDATE trash SET purge_pending = 1, deleted_at = 0 WHERE id = ?")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

impl Store {
    /// DELETE of an object: moves it into the owner's trash (or deletes it
    /// for good if the trash is disabled). The change log records the
    /// deletion exactly like [`Store::delete_item`]. `summary` extracts the
    /// display text from the stored content; it runs before the write lock
    /// is taken. Returns `false` if the object does not exist.
    pub async fn delete_item_to_trash<F>(
        &self,
        collection_id: i64,
        name: &str,
        precondition: &Precondition,
        now: i64,
        summary: F,
    ) -> Result<bool>
    where
        F: FnOnce(&[u8]) -> Option<String> + Send,
    {
        self.delete_item_to_trash_inner(collection_id, name, precondition, now, summary, None)
            .await
    }

    /// [`Store::delete_item_to_trash`] by a sharee through share `share_id`
    /// (see [`Store::put_item_shared`]): the object goes into the owner's
    /// trash.
    pub async fn delete_item_to_trash_shared<F>(
        &self,
        share_id: i64,
        collection_id: i64,
        name: &str,
        precondition: &Precondition,
        now: i64,
        summary: F,
    ) -> Result<bool>
    where
        F: FnOnce(&[u8]) -> Option<String> + Send,
    {
        self.delete_item_to_trash_inner(
            collection_id,
            name,
            precondition,
            now,
            summary,
            Some(share_id),
        )
        .await
    }

    async fn delete_item_to_trash_inner<F>(
        &self,
        collection_id: i64,
        name: &str,
        precondition: &Precondition,
        now: i64,
        summary: F,
        share: Option<i64>,
    ) -> Result<bool>
    where
        F: FnOnce(&[u8]) -> Option<String> + Send,
    {
        self.observe(
            async {
                if self.trash_retention_days() == 0 {
                    return self
                        .delete_item_inner(collection_id, name, precondition, share)
                        .await;
                }
                // Parse outside the write transaction; if the object changes in
                // between, the entry simply has no display text.
                let Some(before) = self.item(collection_id, name).await? else {
                    return Ok(false);
                };
                let summary = shorten(summary(&before.content));
                drop(before.content);

                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                crate::items::ensure_writable(&mut tx, collection_id).await?;
                if let Some(share) = share {
                    crate::sharing::ensure_share(&mut tx, share, collection_id, true).await?;
                }
                let Some(row) = sqlx::query(
                    "SELECT i.id, i.etag, i.schedule_tag, c.owner_id FROM items i JOIN collections c ON c.id = i.collection_id \
                     WHERE i.collection_id = ? AND i.name = ?",
                )
                .bind(collection_id)
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?
                else {
                    return Ok(false);
                };
                let etag: String = row.try_get("etag")?;
                check(precondition, Some(&etag))?;
                if share.is_some() {
                    crate::items::scheduling_object_guard(Some(&row))?;
                }
                let summary = summary.filter(|_| etag == before.etag);
                let owner_id: i64 = row.try_get("owner_id")?;
                let item_id: i64 = row.try_get("id")?;
                if make_room(&mut tx, &self.settings, owner_id, 1).await? {
                    let entry: i64 = sqlx::query(
                        "INSERT INTO trash (public_id, owner_id, entry_kind, collection_kind, \
                           collection_slug, collection_id, name, uid, etag, component, range_start, \
                           range_end, content, modified_at, summary, size, deleted_at) \
                         SELECT lower(hex(randomblob(16))), c.owner_id, 'item', c.kind, c.slug, \
                           i.collection_id, i.name, i.uid, i.etag, i.component, i.range_start, \
                           i.range_end, i.content, i.modified_at, ?, length(i.content), ? \
                         FROM items i JOIN collections c ON c.id = i.collection_id \
                         WHERE i.collection_id = ? AND i.name = ? \
                         RETURNING id",
                    )
                    .bind(summary)
                    .bind(now)
                    .bind(collection_id)
                    .bind(name)
                    .fetch_one(&mut *tx)
                    .await?
                    .try_get("id")?;
                    crate::attachments::refs_to_trash(&mut tx, item_id, entry, owner_id).await?;
                }
                sqlx::query("DELETE FROM items WHERE collection_id = ? AND name = ?")
                    .bind(collection_id)
                    .bind(name)
                    .execute(&mut *tx)
                    .await?;
                let revision: i64 = sqlx::query(
                    "UPDATE collections SET sync_revision = sync_revision + 1 WHERE id = ? RETURNING sync_revision",
                )
                .bind(collection_id)
                .fetch_one(&mut *tx)
                .await?
                .try_get("sync_revision")?;
                record_change(&mut tx, collection_id, revision, name, true).await?;
                tx.commit().await?;
                Ok(true)
            }
            .await,
        )
    }

    /// DELETE of a collection: moves it with all its objects into the
    /// owner's trash as one entry (or deletes it for good if the trash is
    /// disabled).
    pub async fn delete_collection_to_trash(&self, collection_id: i64, now: i64) -> Result<()> {
        self.observe(
            async {
                if self.trash_retention_days() == 0 {
                    return self.delete_collection(collection_id).await;
                }
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                // A subscription is deleted for good: its objects are a copy of
                // the feed (subscribing again restores them), and a restored copy
                // would be an editable calendar without its feed.
                let read_only = sqlx::query("SELECT read_only FROM collections WHERE id = ?")
                    .bind(collection_id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .map(|row| row.try_get::<bool, _>("read_only"))
                    .transpose()?;
                if read_only == Some(true) {
                    sqlx::query("DELETE FROM collections WHERE id = ?")
                        .bind(collection_id)
                        .execute(&mut *tx)
                        .await?;
                    tx.commit().await?;
                    return Ok(());
                }
                let Some(collection) = sqlx::query(
                    "SELECT owner_id, kind, slug, display_name, description, color, components, \
                            (SELECT COUNT(*) FROM items WHERE collection_id = c.id) AS items, \
                            (SELECT COALESCE(SUM(length(content)), 0) FROM items WHERE collection_id = c.id) \
                            + (SELECT COALESCE(SUM(length(CAST(p.namespace AS BLOB)) + length(CAST(p.name AS BLOB)) \
                                 + length(CAST(p.value AS BLOB))), 0) FROM dead_properties p WHERE p.collection_id = c.id) AS bytes \
                     FROM collections c WHERE id = ?",
                )
                .bind(collection_id)
                .fetch_optional(&mut *tx)
                .await?
                else {
                    return Ok(());
                };
                let owner_id: i64 = collection.try_get("owner_id")?;
                let items: i64 = collection.try_get("items")?;
                let room = make_room(&mut tx, &self.settings, owner_id, items.saturating_add(1)).await?;
                if room {
                    let display_name: Option<String> = collection.try_get("display_name")?;
                    let entry: i64 = sqlx::query(
                        "INSERT INTO trash (public_id, owner_id, entry_kind, collection_kind, \
                           collection_slug, display_name, description, color, components, summary, \
                           size, deleted_at) \
                         VALUES (lower(hex(randomblob(16))), ?, 'collection', ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                         RETURNING id",
                    )
                    .bind(owner_id)
                    .bind(collection.try_get::<String, _>("kind")?)
                    .bind(collection.try_get::<String, _>("slug")?)
                    .bind(&display_name)
                    .bind(collection.try_get::<Option<String>, _>("description")?)
                    .bind(collection.try_get::<Option<String>, _>("color")?)
                    .bind(collection.try_get::<String, _>("components")?)
                    .bind(shorten(display_name.clone()))
                    .bind(collection.try_get::<i64, _>("bytes")?)
                    .bind(now)
                    .fetch_one(&mut *tx)
                    .await?
                    .try_get("id")?;
                    crate::properties::to_trash(&mut tx, collection_id, entry).await?;
                    sqlx::query(
                        "INSERT INTO trash (owner_id, entry_kind, parent_id, collection_kind, \
                           collection_slug, name, uid, etag, component, range_start, range_end, content, \
                           modified_at, size, deleted_at) \
                         SELECT ?, 'item', ?, ?, ?, name, uid, etag, component, range_start, range_end, \
                           content, modified_at, length(content), ? \
                         FROM items WHERE collection_id = ? ORDER BY revision",
                    )
                    .bind(owner_id)
                    .bind(entry)
                    .bind(collection.try_get::<String, _>("kind")?)
                    .bind(collection.try_get::<String, _>("slug")?)
                    .bind(now)
                    .bind(collection_id)
                    .execute(&mut *tx)
                    .await?;
                    crate::attachments::collection_refs_to_trash(
                        &mut tx,
                        collection_id,
                        entry,
                        owner_id,
                    )
                    .await?;
                    // Objects deleted from it earlier follow the collection.
                    sqlx::query(
                        "UPDATE trash SET collection_entry = ? \
                         WHERE owner_id = ? AND collection_id = ? AND parent_id IS NULL",
                    )
                    .bind(entry)
                    .bind(owner_id)
                    .bind(collection_id)
                    .execute(&mut *tx)
                    .await?;
                } else {
                    tracing::warn!(
                        owner_id,
                        items,
                        "collection too large for the trash: deleted permanently"
                    );
                }
                sqlx::query("DELETE FROM collections WHERE id = ?")
                    .bind(collection_id)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(())
            }
            .await,
        )
    }

    /// Top-level trash entries of a user, newest first.
    pub async fn trash_entries(&self, owner_id: i64, limit: u32, offset: u32) -> Result<TrashPage> {
        let rows = sqlx::query(
            "SELECT public_id, entry_kind, collection_kind, collection_slug, name, summary, size, \
                    deleted_at, (SELECT COUNT(*) FROM trash c WHERE c.parent_id = t.id) AS items \
             FROM trash t WHERE owner_id = ? AND parent_id IS NULL AND purge_pending = 0 \
             ORDER BY deleted_at DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(owner_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let total: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM trash \
             WHERE owner_id = ? AND parent_id IS NULL AND purge_pending = 0",
        )
        .bind(owner_id)
        .fetch_one(&self.pool)
        .await?
        .try_get("n")?;
        Ok(TrashPage {
            entries: rows.iter().map(entry_from_row).collect::<Result<_>>()?,
            total,
            bytes: trash_bytes(&self.pool, owner_id).await?,
        })
    }

    /// Top-level entries in all users' trash (not counting those already
    /// waiting to be purged).
    pub async fn trash_entry_count(&self) -> Result<i64> {
        Ok(sqlx::query(
            "SELECT COUNT(*) AS n FROM trash WHERE parent_id IS NULL AND purge_pending = 0",
        )
        .fetch_one(&self.pool)
        .await?
        .try_get("n")?)
    }

    /// Restores a top-level trash entry of `owner_id` to its original
    /// collection and name; an object can instead go into `target`, another
    /// collection of the same kind owned by the user (for objects whose
    /// collection was deleted permanently). `Ok(None)` if there is no such
    /// entry (of this user).
    pub async fn restore_trash_entry(
        &self,
        owner_id: i64,
        public_id: &str,
        target: Option<&Slug>,
    ) -> Result<Option<RestoredEntry>> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let Some(entry) = sqlx::query(
                    "SELECT id, entry_kind, collection_kind, collection_slug, collection_id, \
                            (SELECT purge_pending FROM trash p WHERE p.id = t.collection_entry) AS collection_entry_pending, \
                            collection_entry, name, uid, etag, component, range_start, range_end, content, \
                            modified_at, display_name, description, color, components \
                     FROM trash t WHERE owner_id = ? AND public_id = ? AND parent_id IS NULL \
                       AND purge_pending = 0",
                )
                .bind(owner_id)
                .bind(public_id)
                .fetch_optional(&mut *tx)
                .await?
                else {
                    return Ok(None);
                };
                let id: i64 = entry.try_get("id")?;
                let kind = kind_of(&entry.try_get::<String, _>("entry_kind")?)?;
                let collection_kind = collection_kind_of(&entry.try_get::<String, _>("collection_kind")?)?;
                let slug: String = entry.try_get("collection_slug")?;
                let collection_slug: Slug = slug
                    .clone()
                    .try_into()
                    .map_err(|err| StoreError::Corrupt(format!("{err}")))?;
                let restored = match (kind, target) {
                    (TrashEntryKind::Item, target) => {
                        let target = match target {
                            None => None,
                            Some(slug) => {
                                let row = sqlx::query(
                                    "SELECT id FROM collections \
                                     WHERE owner_id = ? AND kind = ? AND slug = ? AND read_only = 0",
                                )
                                .bind(owner_id)
                                .bind(collection_kind.as_str())
                                .bind(slug.as_str())
                                .fetch_optional(&mut *tx)
                                .await?
                                .ok_or(StoreError::TrashConflict(TrashConflict::InvalidTarget))?;
                                Some((row.try_get::<i64, _>("id")?, slug.clone()))
                            }
                        };
                        restore_item(
                            &mut tx,
                            owner_id,
                            &entry,
                            collection_kind,
                            collection_slug,
                            target,
                        )
                        .await?
                    }
                    (TrashEntryKind::Collection, Some(_)) => {
                        return Err(StoreError::TrashConflict(TrashConflict::InvalidTarget));
                    }
                    (TrashEntryKind::Collection, None) => {
                        restore_collection(
                            &mut tx,
                            owner_id,
                            id,
                            &entry,
                            collection_kind,
                            collection_slug,
                        )
                        .await?
                    }
                };
                sqlx::query("DELETE FROM trash WHERE id = ?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(Some(restored))
            }
            .await,
        )
    }

    /// Deletes a top-level trash entry of `owner_id` for good: it is hidden
    /// and its bytes are freed at once, the rows are removed by the purge.
    /// Returns `false` if there is no such entry (of this user).
    pub async fn delete_trash_entry(&self, owner_id: i64, public_id: &str) -> Result<bool> {
        self.observe(
            async {
                let result = sqlx::query(
                    "UPDATE trash SET purge_pending = 1, deleted_at = 0 \
                     WHERE owner_id = ? AND public_id = ? AND parent_id IS NULL AND purge_pending = 0",
                )
                .bind(owner_id)
                .bind(public_id)
                .execute(&self.writer)
                .await?;
                let found = result.rows_affected() == 1;
                if found {
                    self.settings.request_purge();
                }
                Ok(found)
            }
            .await,
        )
    }

    /// Deletes all trash entries of `owner_id` for good (like
    /// [`Store::delete_trash_entry`]); returns how many top-level entries
    /// there were.
    pub async fn empty_trash(&self, owner_id: i64) -> Result<u64> {
        self.observe(
            async {
                let result = sqlx::query(
                    "UPDATE trash SET purge_pending = 1, deleted_at = 0 \
                     WHERE owner_id = ? AND parent_id IS NULL AND purge_pending = 0",
                )
                .bind(owner_id)
                .execute(&self.writer)
                .await?;
                if result.rows_affected() > 0 {
                    self.settings.request_purge();
                }
                Ok(result.rows_affected())
            }
            .await,
        )
    }

    /// Whether entries were deleted, emptied or evicted since the last call,
    /// so the purge should run now rather than at its next interval.
    pub fn take_purge_request(&self) -> bool {
        self.settings.take_purge_request()
    }

    /// Deletes up to `limit` rows of expired or purge-pending trash entries
    /// (all entries if the trash is disabled) in one short write
    /// transaction; returns the number of rows deleted. Call repeatedly
    /// until it returns 0. Objects of a collection entry go first, so that
    /// no single statement deletes more than `limit` rows.
    pub async fn purge_expired_trash(&self, now: i64, limit: u32) -> Result<u64> {
        self.observe(
            async {
                let days = i64::from(self.trash_retention_days());
                let cutoff = if days == 0 {
                    i64::MAX
                } else {
                    now.saturating_sub(days.saturating_mul(SECONDS_PER_DAY))
                };
                // Purge-pending entries have deleted_at = 0, so the cutoff covers them.
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let children = sqlx::query(
                    "DELETE FROM trash WHERE id IN (SELECT c.id FROM trash c \
                       JOIN trash p ON p.id = c.parent_id \
                       WHERE p.parent_id IS NULL AND p.deleted_at <= ?1 LIMIT ?2)",
                )
                .bind(cutoff)
                .bind(limit)
                .execute(&mut *tx)
                .await?
                .rows_affected();
                let remaining = u64::from(limit).saturating_sub(children);
                let entries = if remaining > 0 {
                    sqlx::query(
                        "DELETE FROM trash WHERE id IN (SELECT t.id FROM trash t \
                           WHERE t.parent_id IS NULL AND t.deleted_at <= ?1 \
                             AND NOT EXISTS (SELECT 1 FROM trash c WHERE c.parent_id = t.id) LIMIT ?2)",
                    )
                    .bind(cutoff)
                    .bind(i64::try_from(remaining).unwrap_or(i64::MAX))
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
                } else {
                    0
                };
                tx.commit().await?;
                Ok(children + entries)
            }
            .await,
        )
    }
}

async fn restore_item(
    tx: &mut Transaction<'static, Sqlite>,
    owner_id: i64,
    entry: &SqliteRow,
    collection_kind: CollectionKind,
    collection_slug: Slug,
    target: Option<(i64, Slug)>,
) -> Result<RestoredEntry> {
    let collection_id: Option<i64> = match &target {
        Some((id, _)) => Some(*id),
        None => entry.try_get("collection_id")?,
    };
    let Some(collection_id) = collection_id else {
        // A collection entry waiting to be purged counts as gone.
        let in_trash = entry
            .try_get::<Option<i64>, _>("collection_entry")?
            .is_some()
            && entry.try_get::<Option<bool>, _>("collection_entry_pending")? == Some(false);
        return Err(StoreError::TrashConflict(if in_trash {
            TrashConflict::CollectionInTrash
        } else {
            TrashConflict::CollectionDeleted
        }));
    };
    // The reference is kept only while the collection exists; it must
    // still be this user's collection of the same kind.
    let live = sqlx::query(
        "SELECT slug, read_only FROM collections WHERE id = ? AND owner_id = ? AND kind = ?",
    )
    .bind(collection_id)
    .bind(owner_id)
    .bind(collection_kind.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let Some(live) = live else {
        return Err(StoreError::TrashConflict(TrashConflict::CollectionDeleted));
    };
    // Subscriptions never take restored objects.
    if live.try_get::<bool, _>("read_only")? {
        return Err(StoreError::TrashConflict(TrashConflict::InvalidTarget));
    }
    let live_slug: String = live.try_get("slug")?;
    let collection_slug = match target {
        Some((_, slug)) => slug,
        None => live_slug.try_into().unwrap_or(collection_slug),
    };
    let name: String = entry.try_get("name")?;
    let uid: String = entry.try_get("uid")?;
    let taken = sqlx::query("SELECT 1 FROM items WHERE collection_id = ? AND name = ?")
        .bind(collection_id)
        .bind(&name)
        .fetch_optional(&mut **tx)
        .await?;
    if taken.is_some() {
        return Err(StoreError::TrashConflict(TrashConflict::NameTaken));
    }
    let uid_taken = sqlx::query("SELECT 1 FROM items WHERE collection_id = ? AND uid = ?")
        .bind(collection_id)
        .bind(&uid)
        .fetch_optional(&mut **tx)
        .await?;
    if uid_taken.is_some() {
        return Err(StoreError::TrashConflict(TrashConflict::UidTaken));
    }
    let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM items WHERE collection_id = ?")
        .bind(collection_id)
        .fetch_one(&mut **tx)
        .await?
        .try_get("n")?;
    if count >= MAX_ITEMS_PER_COLLECTION {
        return Err(StoreError::Limit("too many objects in collection"));
    }
    // The quota is unchanged: the bytes move from the trash back into the
    // collection.
    let revision: i64 = sqlx::query(
        "UPDATE collections SET sync_revision = sync_revision + 1 WHERE id = ? RETURNING sync_revision",
    )
    .bind(collection_id)
    .fetch_one(&mut **tx)
    .await?
    .try_get("sync_revision")?;
    let etag: String = entry.try_get("etag")?;
    let item_id: i64 = sqlx::query(
        "INSERT INTO items (collection_id, name, uid, etag, component, range_start, range_end, content, revision, modified_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, COALESCE(?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))) \
         RETURNING id",
    )
    .bind(collection_id)
    .bind(&name)
    .bind(&uid)
    .bind(&etag)
    .bind(entry.try_get::<String, _>("component")?)
    .bind(entry.try_get::<Option<i64>, _>("range_start")?)
    .bind(entry.try_get::<Option<i64>, _>("range_end")?)
    .bind(entry.try_get::<Vec<u8>, _>("content")?)
    .bind(revision)
    .bind(entry.try_get::<Option<String>, _>("modified_at")?)
    .fetch_one(&mut **tx)
    .await?
    .try_get("id")?;
    crate::attachments::refs_from_trash(tx, entry.try_get("id")?, item_id).await?;
    record_change(tx, collection_id, revision, &name, false).await?;
    Ok(RestoredEntry {
        kind: TrashEntryKind::Item,
        collection_kind,
        collection_slug,
        name: Some(name),
        etag: Some(etag),
    })
}

async fn restore_collection(
    tx: &mut Transaction<'static, Sqlite>,
    owner_id: i64,
    entry_id: i64,
    entry: &SqliteRow,
    collection_kind: CollectionKind,
    collection_slug: Slug,
) -> Result<RestoredEntry> {
    let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM collections WHERE owner_id = ?")
        .bind(owner_id)
        .fetch_one(&mut **tx)
        .await?
        .try_get("n")?;
    if count >= MAX_COLLECTIONS_PER_USER {
        return Err(StoreError::Limit("too many collections"));
    }
    // The slug may now be taken by a collection shared with the owner.
    if crate::sharing::slug_taken_by_share(tx, owner_id, collection_kind, collection_slug.as_str())
        .await?
    {
        return Err(StoreError::TrashConflict(TrashConflict::CollectionExists));
    }
    // A new token salt: sync tokens of the deleted collection stay invalid.
    let result = sqlx::query(
        "INSERT INTO collections (owner_id, kind, slug, display_name, description, color, components, token_salt) \
         VALUES (?, ?, ?, ?, ?, ?, ?, abs(random())) RETURNING id",
    )
    .bind(owner_id)
    .bind(collection_kind.as_str())
    .bind(collection_slug.as_str())
    .bind(entry.try_get::<Option<String>, _>("display_name")?)
    .bind(entry.try_get::<Option<String>, _>("description")?)
    .bind(entry.try_get::<Option<String>, _>("color")?)
    .bind(entry.try_get::<Option<String>, _>("components")?.unwrap_or_else(|| {
        match collection_kind {
            CollectionKind::Calendar => "VEVENT,VTODO",
            CollectionKind::Addressbook => "VCARD",
        }
        .to_owned()
    }))
    .fetch_one(&mut **tx)
    .await;
    let collection_id: i64 = match result {
        Ok(row) => row.try_get("id")?,
        Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
            return Err(StoreError::TrashConflict(TrashConflict::CollectionExists));
        }
        Err(err) => return Err(err.into()),
    };
    crate::properties::from_trash(tx, entry_id, collection_id).await?;
    // Every object gets its own revision, in the order they were changed.
    let restored = sqlx::query(
        "INSERT INTO items (collection_id, name, uid, etag, component, range_start, range_end, content, revision, modified_at) \
         SELECT ?, name, uid, etag, component, range_start, range_end, content, \
                ROW_NUMBER() OVER (ORDER BY id), \
                COALESCE(modified_at, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')) \
         FROM trash WHERE parent_id = ?",
    )
    .bind(collection_id)
    .bind(entry_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if restored > 0 {
        crate::attachments::collection_refs_from_trash(tx, entry_id, collection_id).await?;
        sqlx::query(
            "INSERT INTO changes (collection_id, name, revision, deleted) \
             SELECT collection_id, name, revision, 0 FROM items WHERE collection_id = ?",
        )
        .bind(collection_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query("UPDATE collections SET sync_revision = ? WHERE id = ?")
            .bind(i64::try_from(restored).unwrap_or(i64::MAX))
            .bind(collection_id)
            .execute(&mut **tx)
            .await?;
    }
    // Objects deleted from the collection before it was deleted can be
    // restored into it again.
    sqlx::query(
        "UPDATE trash SET collection_id = ?, collection_entry = NULL WHERE collection_entry = ?",
    )
    .bind(collection_id)
    .bind(entry_id)
    .execute(&mut **tx)
    .await?;
    Ok(RestoredEntry {
        kind: TrashEntryKind::Collection,
        collection_kind,
        collection_slug,
        name: None,
        etag: None,
    })
}

#[cfg(test)]
mod tests;
