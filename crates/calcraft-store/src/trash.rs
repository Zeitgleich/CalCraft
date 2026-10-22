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
    Result, Store, StoreError,
    items::{
        MAX_COLLECTIONS_PER_USER, MAX_ITEMS_PER_COLLECTION, Precondition, check, record_change,
    },
};

/// Default of `storage.trash_retention_days`.
pub const DEFAULT_TRASH_RETENTION_DAYS: u32 = 30;
/// Rows (objects and collections) in one user's trash. Large enough for
/// the biggest possible collection; when it is full, the oldest entries are
/// purged early to make room.
pub const MAX_TRASH_ROWS_PER_USER: i64 = MAX_ITEMS_PER_COLLECTION + 1_000;
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
    /// Bytes of content.
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
            Self::CollectionDeleted => "the collection of this entry was deleted permanently",
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

/// Makes room for `needed` more rows in the owner's trash by purging the
/// oldest entries. Returns `false` if `needed` alone exceeds the limit (the
/// caller then deletes for good instead).
async fn make_room(
    tx: &mut Transaction<'static, Sqlite>,
    owner_id: i64,
    needed: i64,
    max_rows: i64,
) -> Result<bool> {
    if needed > max_rows {
        return Ok(false);
    }
    let rows: i64 = sqlx::query("SELECT COUNT(*) AS n FROM trash WHERE owner_id = ?")
        .bind(owner_id)
        .fetch_one(&mut **tx)
        .await?
        .try_get("n")?;
    let mut excess = rows.saturating_add(needed) - max_rows;
    let mut evicted = 0_u64;
    while excess > 0 {
        let oldest = sqlx::query(
            "SELECT t.id, 1 + (SELECT COUNT(*) FROM trash c WHERE c.parent_id = t.id) AS n \
             FROM trash t WHERE t.owner_id = ? AND t.parent_id IS NULL \
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
            sqlx::query("DELETE FROM trash WHERE id = ?")
                .bind(id)
                .execute(&mut **tx)
                .await?;
            evicted += 1;
            excess -= n;
            if excess <= 0 {
                break;
            }
        }
    }
    if evicted > 0 {
        tracing::info!(
            owner_id,
            evicted,
            "trash full: purged the oldest entries early"
        );
    }
    Ok(true)
}

impl Store {
    /// DELETE of an object: moves it into the owner's trash (or deletes it
    /// for good if the trash is disabled). The change log records the
    /// deletion exactly like [`Store::delete_item`]. `summary` extracts the
    /// display text from the stored content. Returns `false` if the object
    /// does not exist.
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
        if self.trash_retention_days() == 0 {
            return self.delete_item(collection_id, name, precondition).await;
        }
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let Some(row) = sqlx::query(
            "SELECT i.uid, i.etag, i.component, i.range_start, i.range_end, i.content, \
                    c.owner_id, c.kind, c.slug \
             FROM items i JOIN collections c ON c.id = i.collection_id \
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
        let owner_id: i64 = row.try_get("owner_id")?;
        let content: Vec<u8> = row.try_get("content")?;
        let size = i64::try_from(content.len()).unwrap_or(i64::MAX);
        if make_room(&mut tx, owner_id, 1, self.settings.max_trash_rows()).await? {
            let summary = shorten(summary(&content));
            sqlx::query(
                "INSERT INTO trash (public_id, owner_id, entry_kind, collection_kind, \
                   collection_slug, collection_id, name, uid, etag, component, range_start, \
                   range_end, content, summary, size, deleted_at) \
                 VALUES (lower(hex(randomblob(16))), ?, 'item', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(owner_id)
            .bind(row.try_get::<String, _>("kind")?)
            .bind(row.try_get::<String, _>("slug")?)
            .bind(collection_id)
            .bind(name)
            .bind(row.try_get::<String, _>("uid")?)
            .bind(&etag)
            .bind(row.try_get::<String, _>("component")?)
            .bind(row.try_get::<Option<i64>, _>("range_start")?)
            .bind(row.try_get::<Option<i64>, _>("range_end")?)
            .bind(&content)
            .bind(summary)
            .bind(size)
            .bind(now)
            .execute(&mut *tx)
            .await?;
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

    /// DELETE of a collection: moves it with all its objects into the
    /// owner's trash as one entry (or deletes it for good if the trash is
    /// disabled).
    pub async fn delete_collection_to_trash(&self, collection_id: i64, now: i64) -> Result<()> {
        if self.trash_retention_days() == 0 {
            return self.delete_collection(collection_id).await;
        }
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let Some(collection) = sqlx::query(
            "SELECT owner_id, kind, slug, display_name, description, color, components, \
                    (SELECT COUNT(*) FROM items WHERE collection_id = c.id) AS items, \
                    (SELECT COALESCE(SUM(length(content)), 0) FROM items WHERE collection_id = c.id) AS bytes \
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
        let room = make_room(
            &mut tx,
            owner_id,
            items.saturating_add(1),
            self.settings.max_trash_rows(),
        )
        .await?;
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
            sqlx::query(
                "INSERT INTO trash (owner_id, entry_kind, parent_id, collection_kind, \
                   collection_slug, name, uid, etag, component, range_start, range_end, content, \
                   size, deleted_at) \
                 SELECT ?, 'item', ?, ?, ?, name, uid, etag, component, range_start, range_end, \
                   content, length(content), ? \
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

    /// Top-level trash entries of a user, newest first.
    pub async fn trash_entries(&self, owner_id: i64, limit: u32, offset: u32) -> Result<TrashPage> {
        let rows = sqlx::query(
            "SELECT public_id, entry_kind, collection_kind, collection_slug, name, summary, size, \
                    deleted_at, (SELECT COUNT(*) FROM trash c WHERE c.parent_id = t.id) AS items \
             FROM trash t WHERE owner_id = ? AND parent_id IS NULL \
             ORDER BY deleted_at DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(owner_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let totals = sqlx::query(
            "SELECT (SELECT COUNT(*) FROM trash WHERE owner_id = ?1 AND parent_id IS NULL) AS total, \
                    (SELECT COALESCE(SUM(size), 0) FROM trash WHERE owner_id = ?1 AND entry_kind = 'item') AS bytes",
        )
        .bind(owner_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(TrashPage {
            entries: rows.iter().map(entry_from_row).collect::<Result<_>>()?,
            total: totals.try_get("total")?,
            bytes: totals.try_get("bytes")?,
        })
    }

    /// Restores a top-level trash entry of `owner_id` to its original
    /// collection and name. `Ok(None)` if there is no such entry (of this
    /// user).
    pub async fn restore_trash_entry(
        &self,
        owner_id: i64,
        public_id: &str,
    ) -> Result<Option<RestoredEntry>> {
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let Some(entry) = sqlx::query(
            "SELECT id, entry_kind, collection_kind, collection_slug, collection_id, \
                    collection_entry, name, uid, etag, component, range_start, range_end, content, \
                    display_name, description, color, components \
             FROM trash WHERE owner_id = ? AND public_id = ? AND parent_id IS NULL",
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
        let restored = match kind {
            TrashEntryKind::Item => {
                restore_item(&mut tx, owner_id, &entry, collection_kind, collection_slug).await?
            }
            TrashEntryKind::Collection => {
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

    /// Deletes a top-level trash entry of `owner_id` for good. Returns
    /// `false` if there is no such entry (of this user).
    pub async fn delete_trash_entry(&self, owner_id: i64, public_id: &str) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM trash WHERE owner_id = ? AND public_id = ? AND parent_id IS NULL",
        )
        .bind(owner_id)
        .bind(public_id)
        .execute(&self.writer)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Deletes all trash entries of `owner_id` for good; returns how many
    /// top-level entries there were.
    pub async fn empty_trash(&self, owner_id: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM trash WHERE owner_id = ? AND parent_id IS NULL")
            .bind(owner_id)
            .execute(&self.writer)
            .await?;
        Ok(result.rows_affected())
    }

    /// Deletes up to `limit` rows of expired trash entries (all entries if
    /// the trash is disabled) in one short write transaction; returns the
    /// number of rows deleted. Call repeatedly until it returns 0. Objects
    /// of an expired collection go first, so that no single statement
    /// deletes more than `limit` rows.
    pub async fn purge_expired_trash(&self, now: i64, limit: u32) -> Result<u64> {
        let days = i64::from(self.trash_retention_days());
        let cutoff = if days == 0 {
            i64::MAX
        } else {
            now.saturating_sub(days.saturating_mul(SECONDS_PER_DAY))
        };
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
}

async fn restore_item(
    tx: &mut Transaction<'static, Sqlite>,
    owner_id: i64,
    entry: &SqliteRow,
    collection_kind: CollectionKind,
    collection_slug: Slug,
) -> Result<RestoredEntry> {
    let collection_id: Option<i64> = entry.try_get("collection_id")?;
    let Some(collection_id) = collection_id else {
        let in_trash = entry
            .try_get::<Option<i64>, _>("collection_entry")?
            .is_some();
        return Err(StoreError::TrashConflict(if in_trash {
            TrashConflict::CollectionInTrash
        } else {
            TrashConflict::CollectionDeleted
        }));
    };
    // The reference is kept only while the collection exists; it must
    // still be this user's collection of the same kind.
    let live =
        sqlx::query("SELECT slug FROM collections WHERE id = ? AND owner_id = ? AND kind = ?")
            .bind(collection_id)
            .bind(owner_id)
            .bind(collection_kind.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    let Some(live) = live else {
        return Err(StoreError::TrashConflict(TrashConflict::CollectionDeleted));
    };
    let live_slug: String = live.try_get("slug")?;
    let collection_slug = live_slug.try_into().unwrap_or(collection_slug);
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
    sqlx::query(
        "INSERT INTO items (collection_id, name, uid, etag, component, range_start, range_end, content, revision) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .execute(&mut **tx)
    .await?;
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
    // Every object gets its own revision, in the order they were changed.
    let restored = sqlx::query(
        "INSERT INTO items (collection_id, name, uid, etag, component, range_start, range_end, content, revision) \
         SELECT ?, name, uid, etag, component, range_start, range_end, content, \
                ROW_NUMBER() OVER (ORDER BY id) \
         FROM trash WHERE parent_id = ?",
    )
    .bind(collection_id)
    .bind(entry_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if restored > 0 {
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
