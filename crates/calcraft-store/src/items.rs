//! Calendar objects and collection management.
//!
//! Every query takes the collection id, which callers only obtain after
//! resolving a collection for the authenticated owner. There is no query
//! that looks up an item without its collection.

use calcraft_core::{CollectionKind, Slug};
use sqlx::{Row, sqlite::SqliteRow};

use crate::{Collection, Result, Store, StoreError, collection_from_row};

/// Maximum number of calendars plus address books per user.
pub const MAX_COLLECTIONS_PER_USER: i64 = 100;
/// Maximum number of objects per collection.
pub const MAX_ITEMS_PER_COLLECTION: i64 = 100_000;
/// Storage quota per user for object content.
pub const MAX_BYTES_PER_USER: i64 = 512 * 1024 * 1024;
/// Tombstones kept per collection; older ones are pruned and raise the
/// collection's sync floor, so clients with older tokens resynchronise.
pub const MAX_TOMBSTONES: i64 = 10_000;

/// One entry of the change log, joined with the current object metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    pub revision: i64,
    /// `None` for a deleted object.
    pub meta: Option<ItemMeta>,
}

/// A stored calendar object including its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub etag: String,
    pub content: Vec<u8>,
}

/// A stored calendar object without its content, for listings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemMeta {
    pub name: String,
    pub etag: String,
    pub size: i64,
}

/// A validated object to store.
#[derive(Debug, Clone)]
pub struct NewItem<'a> {
    pub uid: &'a str,
    pub component: &'a str,
    pub range_start: Option<i64>,
    pub range_end: Option<i64>,
    pub etag: &'a str,
    pub content: &'a [u8],
}

/// Conditional request headers for writes (RFC 9110 §13.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    None,
    /// `If-None-Match: *` – only create.
    MustNotExist,
    /// `If-Match: *` – only overwrite.
    MustExist,
    /// `If-Match` with entity tags: the current tag must be one of them
    /// (strong comparison).
    IfMatch(Vec<String>),
    /// `If-None-Match` with entity tags: the current tag must not be one of
    /// them (weak comparison; tags are passed without `W/`).
    IfNoneMatch(Vec<String>),
}

/// A `COPY` or `MOVE` of one object between collections of one owner.
#[derive(Debug, Clone, Copy)]
pub struct Transfer<'a> {
    /// Owner of both collections; the store checks it again.
    pub owner_id: i64,
    pub source_collection: i64,
    pub source_name: &'a str,
    /// `If-Match` / `If-None-Match` of the request, evaluated on the source.
    pub source_precondition: &'a Precondition,
    pub destination_collection: i64,
    pub destination_name: &'a str,
    /// `Overwrite: T`: an existing destination is replaced.
    pub overwrite: bool,
}

/// Result of a successful [`Store::copy_item`] / [`Store::move_item`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferOutcome {
    /// `false` if an existing destination was replaced.
    pub created: bool,
    /// Entity tag of the destination (the content is unchanged).
    pub etag: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    Created,
    Updated,
}

fn check(precondition: &Precondition, existing: Option<&str>) -> Result<()> {
    let ok = match precondition {
        Precondition::None => true,
        Precondition::MustNotExist => existing.is_none(),
        Precondition::MustExist => existing.is_some(),
        Precondition::IfMatch(tags) => existing.is_some_and(|e| tags.iter().any(|t| t == e)),
        Precondition::IfNoneMatch(tags) => existing.is_none_or(|e| !tags.iter().any(|t| t == e)),
    };
    if ok {
        Ok(())
    } else {
        Err(StoreError::PreconditionFailed)
    }
}

fn meta_from_row(row: &SqliteRow) -> Result<ItemMeta> {
    Ok(ItemMeta {
        name: row.try_get("name")?,
        etag: row.try_get("etag")?,
        size: row.try_get("size")?,
    })
}

fn item_from_row(row: &SqliteRow) -> Result<Item> {
    Ok(Item {
        name: row.try_get("name")?,
        etag: row.try_get("etag")?,
        content: row.try_get("content")?,
    })
}

impl Store {
    /// Creates a calendar or address book for a user.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_collection(
        &self,
        owner_id: i64,
        kind: CollectionKind,
        slug: &Slug,
        display_name: Option<&str>,
        description: Option<&str>,
        color: Option<&str>,
        components: &str,
    ) -> Result<Collection> {
        // Write transactions start IMMEDIATE (see `put_item`): a deferred one that
        // reads first fails with SQLITE_BUSY instead of waiting when another
        // writer commits in between.
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM collections WHERE owner_id = ?")
            .bind(owner_id)
            .fetch_one(&mut *tx)
            .await?
            .try_get("n")?;
        if count >= MAX_COLLECTIONS_PER_USER {
            return Err(StoreError::Limit("too many collections"));
        }
        let result = sqlx::query(
            "INSERT INTO collections (owner_id, kind, slug, display_name, description, color, components, token_salt) \
             VALUES (?, ?, ?, ?, ?, ?, ?, abs(random())) \
             RETURNING id, owner_id, kind, slug, display_name, description, color, sync_revision, components, sync_floor, token_salt",
        )
        .bind(owner_id)
        .bind(kind.as_str())
        .bind(slug.as_str())
        .bind(display_name)
        .bind(description)
        .bind(color)
        .bind(components)
        .fetch_one(&mut *tx)
        .await;
        let row = match result {
            Ok(row) => row,
            Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
                return Err(StoreError::CollectionExists);
            }
            Err(err) => return Err(err.into()),
        };
        let collection = collection_from_row(&row)?;
        tx.commit().await?;
        Ok(collection)
    }

    /// Deletes a collection and all its objects.
    pub async fn delete_collection(&self, collection_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM collections WHERE id = ?")
            .bind(collection_id)
            .execute(&self.writer)
            .await?;
        Ok(())
    }

    /// Updates the editable properties of a collection. `None` keeps a
    /// value, `Some(None)` clears it.
    pub async fn update_collection(
        &self,
        collection_id: i64,
        display_name: Option<Option<&str>>,
        description: Option<Option<&str>>,
        color: Option<Option<&str>>,
    ) -> Result<()> {
        // Write transactions start IMMEDIATE (see `put_item`): a deferred one that
        // reads first fails with SQLITE_BUSY instead of waiting when another
        // writer commits in between.
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        // Column names are fixed literals; values are always bound.
        for (column, value) in [
            ("display_name", display_name),
            ("description", description),
            ("color", color),
        ] {
            let Some(value) = value else { continue };
            let sql = match column {
                "display_name" => "UPDATE collections SET display_name = ? WHERE id = ?",
                "description" => "UPDATE collections SET description = ? WHERE id = ?",
                _ => "UPDATE collections SET color = ? WHERE id = ?",
            };
            sqlx::query(sql)
                .bind(value)
                .bind(collection_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE collections SET sync_revision = sync_revision + 1 WHERE id = ?")
            .bind(collection_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Lists the objects of a collection without their content.
    pub async fn items(&self, collection_id: i64) -> Result<Vec<ItemMeta>> {
        let rows = sqlx::query(
            "SELECT name, etag, length(content) AS size FROM items WHERE collection_id = ? ORDER BY name",
        )
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(meta_from_row).collect()
    }

    /// A single object of a collection.
    pub async fn item(&self, collection_id: i64, name: &str) -> Result<Option<Item>> {
        let row = sqlx::query(
            "SELECT name, etag, content FROM items WHERE collection_id = ? AND name = ?",
        )
        .bind(collection_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(item_from_row).transpose()
    }

    /// Name of the object with this UID in the collection, if any.
    pub async fn item_name_by_uid(&self, collection_id: i64, uid: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT name FROM items WHERE collection_id = ? AND uid = ?")
            .bind(collection_id)
            .bind(uid)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| row.try_get("name"))
            .transpose()
            .map_err(Into::into)
    }

    /// Metadata of a single object, without loading its content.
    pub async fn item_meta(&self, collection_id: i64, name: &str) -> Result<Option<ItemMeta>> {
        let row = sqlx::query(
            "SELECT name, etag, length(content) AS size FROM items WHERE collection_id = ? AND name = ?",
        )
        .bind(collection_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(meta_from_row).transpose()
    }

    /// Like [`Store::query_items`], but returns only metadata, so callers can
    /// check result size before loading any content.
    pub async fn query_item_meta(
        &self,
        collection_id: i64,
        component: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
        limit: u32,
    ) -> Result<Vec<ItemMeta>> {
        let rows = sqlx::query(
            "SELECT name, etag, length(content) AS size FROM items \
             WHERE collection_id = ?1 \
               AND (?2 IS NULL OR component = ?2) \
               AND (?3 IS NULL OR range_end IS NULL OR range_end > ?3) \
               AND (?4 IS NULL OR range_start IS NULL OR range_start < ?4) \
             ORDER BY name LIMIT ?5",
        )
        .bind(collection_id)
        .bind(component)
        .bind(start)
        .bind(end)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(meta_from_row).collect()
    }

    /// Objects whose conservative time span overlaps `[start, end)`.
    pub async fn query_items(
        &self,
        collection_id: i64,
        component: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
    ) -> Result<Vec<Item>> {
        let rows = sqlx::query(
            "SELECT name, etag, content FROM items \
             WHERE collection_id = ?1 \
               AND (?2 IS NULL OR component = ?2) \
               AND (?3 IS NULL OR range_end IS NULL OR range_end > ?3) \
               AND (?4 IS NULL OR range_start IS NULL OR range_start < ?4) \
             ORDER BY name",
        )
        .bind(collection_id)
        .bind(component)
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(item_from_row).collect()
    }

    /// Creates or replaces an object, enforcing preconditions and UID uniqueness.
    pub async fn put_item(
        &self,
        collection_id: i64,
        name: &str,
        item: &NewItem<'_>,
        precondition: &Precondition,
    ) -> Result<PutOutcome> {
        // IMMEDIATE takes the write lock up front, so the checks below and
        // the write are atomic and concurrent writers wait instead of failing.
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let existing: Option<String> =
            sqlx::query("SELECT etag FROM items WHERE collection_id = ? AND name = ?")
                .bind(collection_id)
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?
                .map(|row| row.try_get("etag"))
                .transpose()?;
        check(precondition, existing.as_deref())?;

        let conflict: Option<String> =
            sqlx::query("SELECT name FROM items WHERE collection_id = ? AND uid = ? AND name != ?")
                .bind(collection_id)
                .bind(item.uid)
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?
                .map(|row| row.try_get("name"))
                .transpose()?;
        if let Some(other) = conflict {
            return Err(StoreError::UidConflict(other));
        }

        if existing.is_none() {
            let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM items WHERE collection_id = ?")
                .bind(collection_id)
                .fetch_one(&mut *tx)
                .await?
                .try_get("n")?;
            if count >= MAX_ITEMS_PER_COLLECTION {
                return Err(StoreError::Limit("too many objects in collection"));
            }
        }

        let used: i64 = sqlx::query(
            "SELECT COALESCE(SUM(length(i.content)), 0) AS used FROM items i \
             JOIN collections c ON c.id = i.collection_id \
             WHERE c.owner_id = (SELECT owner_id FROM collections WHERE id = ?1) \
               AND NOT (i.collection_id = ?1 AND i.name = ?2)",
        )
        .bind(collection_id)
        .bind(name)
        .fetch_one(&mut *tx)
        .await?
        .try_get("used")?;
        let size = i64::try_from(item.content.len()).unwrap_or(i64::MAX);
        if used.saturating_add(size) > self.simulation.quota() {
            return Err(StoreError::Limit("storage quota exceeded"));
        }

        let revision: i64 = sqlx::query(
            "UPDATE collections SET sync_revision = sync_revision + 1 WHERE id = ? RETURNING sync_revision",
        )
        .bind(collection_id)
        .fetch_one(&mut *tx)
        .await?
        .try_get("sync_revision")?;

        sqlx::query(
            "INSERT INTO items (collection_id, name, uid, etag, component, range_start, range_end, content, revision) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (collection_id, name) DO UPDATE SET \
               uid = excluded.uid, etag = excluded.etag, component = excluded.component, \
               range_start = excluded.range_start, range_end = excluded.range_end, \
               content = excluded.content, revision = excluded.revision, \
               modified_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        )
        .bind(collection_id)
        .bind(name)
        .bind(item.uid)
        .bind(item.etag)
        .bind(item.component)
        .bind(item.range_start)
        .bind(item.range_end)
        .bind(item.content)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
        record_change(&mut tx, collection_id, revision, name, false).await?;
        tx.commit().await?;
        Ok(if existing.is_some() {
            PutOutcome::Updated
        } else {
            PutOutcome::Created
        })
    }

    /// Deletes an object. Returns `false` if it does not exist.
    pub async fn delete_item(
        &self,
        collection_id: i64,
        name: &str,
        precondition: &Precondition,
    ) -> Result<bool> {
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let existing: Option<String> =
            sqlx::query("SELECT etag FROM items WHERE collection_id = ? AND name = ?")
                .bind(collection_id)
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?
                .map(|row| row.try_get("etag"))
                .transpose()?;
        if existing.is_none() {
            return Ok(false);
        }
        check(precondition, existing.as_deref())?;
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

    /// Copies an object to another (or the same) collection of the same
    /// owner (WebDAV `COPY`, RFC 4918 §9.8). See [`Store::move_item`].
    pub async fn copy_item(&self, request: &Transfer<'_>) -> Result<Option<TransferOutcome>> {
        self.transfer_item(request, false).await
    }

    /// Moves an object to another (or the same) collection of the same
    /// owner (WebDAV `MOVE`, RFC 4918 §9.9) in one transaction: the
    /// destination is created or replaced, the source removed with a
    /// tombstone, and both collections get a change-log entry and a new sync
    /// revision. Returns `None` if the source does not exist.
    ///
    /// The source is removed here explicitly, not through
    /// [`Store::delete_item`]: a moved object was not deleted by the user and
    /// must never be treated like a deletion (e.g. end up in a trash).
    pub async fn move_item(&self, request: &Transfer<'_>) -> Result<Option<TransferOutcome>> {
        self.transfer_item(request, true).await
    }

    async fn transfer_item(
        &self,
        request: &Transfer<'_>,
        remove_source: bool,
    ) -> Result<Option<TransferOutcome>> {
        let Transfer {
            owner_id,
            source_collection,
            source_name,
            source_precondition,
            destination_collection,
            destination_name,
            overwrite,
        } = *request;
        let same_collection = source_collection == destination_collection;
        if same_collection && source_name == destination_name {
            return Err(StoreError::SameResource);
        }
        // IMMEDIATE: all checks and both writes are one atomic step (see
        // `put_item`). Parallel MOVEs of one object are serialised; all but
        // the first find no source.
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;

        // Both collections must belong to the owner and be of one kind. The
        // caller resolved them for the owner already; this keeps the queries
        // scoped and covers collections deleted in the meantime.
        let collection = |id: i64| {
            sqlx::query("SELECT kind, components FROM collections WHERE id = ? AND owner_id = ?")
                .bind(id)
                .bind(owner_id)
        };
        let Some(source) = collection(source_collection)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(None);
        };
        let Some(destination) = collection(destination_collection)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Err(StoreError::MissingDestination);
        };
        let kind: String = destination.try_get("kind")?;
        if source.try_get::<String, _>("kind")? != kind {
            return Err(StoreError::MissingDestination);
        }

        let Some(item) = sqlx::query(
            "SELECT uid, etag, component, range_start, range_end, content FROM items \
             WHERE collection_id = ? AND name = ?",
        )
        .bind(source_collection)
        .bind(source_name)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };
        let uid: String = item.try_get("uid")?;
        let etag: String = item.try_get("etag")?;
        let component: String = item.try_get("component")?;
        let range_start: Option<i64> = item.try_get("range_start")?;
        let range_end: Option<i64> = item.try_get("range_end")?;
        let content: Vec<u8> = item.try_get("content")?;
        check(source_precondition, Some(&etag))?;

        if kind == CollectionKind::Calendar.as_str() {
            let components: String = destination.try_get("components")?;
            if !components.split(',').any(|c| c == component) {
                return Err(StoreError::UnsupportedComponent);
            }
        }

        let existing = sqlx::query("SELECT 1 FROM items WHERE collection_id = ? AND name = ?")
            .bind(destination_collection)
            .bind(destination_name)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
        if existing && !overwrite {
            return Err(StoreError::PreconditionFailed);
        }

        // The UID must stay unique in the destination; the source itself
        // does not count when it is moved within its collection.
        let conflict: Option<String> = sqlx::query(
            "SELECT name FROM items WHERE collection_id = ?1 AND uid = ?2 AND name != ?3 \
               AND NOT (?4 AND collection_id = ?5 AND name = ?6)",
        )
        .bind(destination_collection)
        .bind(&uid)
        .bind(destination_name)
        .bind(remove_source)
        .bind(source_collection)
        .bind(source_name)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| row.try_get("name"))
        .transpose()?;
        if let Some(other) = conflict {
            return Err(StoreError::UidConflict(other));
        }

        if !existing && !(remove_source && same_collection) {
            let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM items WHERE collection_id = ?")
                .bind(destination_collection)
                .fetch_one(&mut *tx)
                .await?
                .try_get("n")?;
            if count >= MAX_ITEMS_PER_COLLECTION {
                return Err(StoreError::Limit("too many objects in collection"));
            }
        }

        // A copy adds its size to the owner's usage. A move stays within the
        // owner's collections and never increases it, so it is not counted
        // twice.
        if !remove_source {
            let used: i64 = sqlx::query(
                "SELECT COALESCE(SUM(length(i.content)), 0) AS used FROM items i \
                 JOIN collections c ON c.id = i.collection_id \
                 WHERE c.owner_id = ?1 AND NOT (i.collection_id = ?2 AND i.name = ?3)",
            )
            .bind(owner_id)
            .bind(destination_collection)
            .bind(destination_name)
            .fetch_one(&mut *tx)
            .await?
            .try_get("used")?;
            let size = i64::try_from(content.len()).unwrap_or(i64::MAX);
            if used.saturating_add(size) > self.simulation.quota() {
                return Err(StoreError::Limit("storage quota exceeded"));
            }
        }

        if remove_source {
            // Removed first: within one collection the UID must stay unique.
            sqlx::query("DELETE FROM items WHERE collection_id = ? AND name = ?")
                .bind(source_collection)
                .bind(source_name)
                .execute(&mut *tx)
                .await?;
            let revision = bump_revision(&mut tx, source_collection).await?;
            record_change(&mut tx, source_collection, revision, source_name, true).await?;
        }

        let revision = bump_revision(&mut tx, destination_collection).await?;
        sqlx::query(
            "INSERT INTO items (collection_id, name, uid, etag, component, range_start, range_end, content, revision) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (collection_id, name) DO UPDATE SET \
               uid = excluded.uid, etag = excluded.etag, component = excluded.component, \
               range_start = excluded.range_start, range_end = excluded.range_end, \
               content = excluded.content, revision = excluded.revision, \
               modified_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        )
        .bind(destination_collection)
        .bind(destination_name)
        .bind(&uid)
        .bind(&etag)
        .bind(&component)
        .bind(range_start)
        .bind(range_end)
        .bind(&content)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
        record_change(
            &mut tx,
            destination_collection,
            revision,
            destination_name,
            false,
        )
        .await?;
        tx.commit().await?;
        Ok(Some(TransferOutcome {
            created: !existing,
            etag,
        }))
    }

    /// Changes after revision `since`, oldest first, at most `limit` rows.
    /// Live objects come with their current metadata (one joined query).
    pub async fn changes_since(
        &self,
        collection_id: i64,
        since: i64,
        limit: u32,
    ) -> Result<Vec<Change>> {
        let rows = sqlx::query(
            "SELECT c.name, c.revision, c.deleted, i.etag, length(i.content) AS size \
             FROM changes c \
             LEFT JOIN items i ON i.collection_id = c.collection_id AND i.name = c.name \
             WHERE c.collection_id = ?1 AND c.revision > ?2 \
             ORDER BY c.revision LIMIT ?3",
        )
        .bind(collection_id)
        .bind(since)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let name: String = row.try_get("name")?;
                let deleted: bool = row.try_get("deleted")?;
                let etag: Option<String> = row.try_get("etag")?;
                let meta = match (deleted, etag) {
                    (false, Some(etag)) => Some(ItemMeta {
                        name: name.clone(),
                        etag,
                        size: row.try_get("size")?,
                    }),
                    _ => None,
                };
                Ok(Change {
                    name,
                    revision: row.try_get("revision")?,
                    meta,
                })
            })
            .collect()
    }

    /// Current objects ordered by the revision of their last change, at most
    /// `limit` rows; used for the initial sync and paging through it.
    pub async fn items_by_revision(
        &self,
        collection_id: i64,
        limit: u32,
    ) -> Result<Vec<(i64, ItemMeta)>> {
        let rows = sqlx::query(
            "SELECT name, etag, length(content) AS size, revision FROM items \
             WHERE collection_id = ? ORDER BY revision LIMIT ?",
        )
        .bind(collection_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| Ok((row.try_get("revision")?, meta_from_row(row)?)))
            .collect()
    }

    /// Objects of a collection without content, at most `limit`.
    pub async fn items_limited(&self, collection_id: i64, limit: u32) -> Result<Vec<ItemMeta>> {
        let rows = sqlx::query(
            "SELECT name, etag, length(content) AS size FROM items WHERE collection_id = ? ORDER BY name LIMIT ?",
        )
        .bind(collection_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(meta_from_row).collect()
    }
}

async fn bump_revision(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    collection_id: i64,
) -> Result<i64> {
    Ok(sqlx::query(
        "UPDATE collections SET sync_revision = sync_revision + 1 WHERE id = ? RETURNING sync_revision",
    )
    .bind(collection_id)
    .fetch_one(&mut **tx)
    .await?
    .try_get("sync_revision")?)
}

async fn record_change(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    collection_id: i64,
    revision: i64,
    name: &str,
    deleted: bool,
) -> Result<()> {
    // One row per object: the latest change replaces earlier ones.
    sqlx::query(
        "INSERT INTO changes (collection_id, name, revision, deleted) VALUES (?, ?, ?, ?) \
         ON CONFLICT (collection_id, name) DO UPDATE SET revision = excluded.revision, deleted = excluded.deleted",
    )
    .bind(collection_id)
    .bind(name)
    .bind(revision)
    .bind(deleted)
    .execute(&mut **tx)
    .await?;
    if !deleted {
        return Ok(());
    }
    // Bound the number of tombstones; prune the oldest and raise the floor.
    let tombstones: i64 =
        sqlx::query("SELECT COUNT(*) AS n FROM changes WHERE collection_id = ? AND deleted = 1")
            .bind(collection_id)
            .fetch_one(&mut **tx)
            .await?
            .try_get("n")?;
    if tombstones > MAX_TOMBSTONES {
        let excess = tombstones - MAX_TOMBSTONES;
        let floor: i64 = sqlx::query(
            "SELECT MAX(revision) AS floor FROM (SELECT revision FROM changes \
             WHERE collection_id = ? AND deleted = 1 ORDER BY revision LIMIT ?)",
        )
        .bind(collection_id)
        .bind(excess)
        .fetch_one(&mut **tx)
        .await?
        .try_get("floor")?;
        sqlx::query(
            "DELETE FROM changes WHERE collection_id = ? AND deleted = 1 AND revision <= ?",
        )
        .bind(collection_id)
        .bind(floor)
        .execute(&mut **tx)
        .await?;
        sqlx::query("UPDATE collections SET sync_floor = ? WHERE id = ?")
            .bind(floor)
            .bind(collection_id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_COLLECTION_SLUG;

    async fn setup() -> (Store, Collection, Collection) {
        let store = Store::in_memory().await.unwrap();
        let slug: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
        let mut calendars = Vec::new();
        for user in ["alice", "bob"] {
            let user = store
                .create_user(&user.parse().unwrap(), "h", None)
                .await
                .unwrap();
            calendars.push(
                store
                    .collection(user.id, CollectionKind::Calendar, &slug)
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let bob = calendars.pop().unwrap();
        let alice = calendars.pop().unwrap();
        (store, alice, bob)
    }

    fn item<'a>(uid: &'a str, etag: &'a str, start: Option<i64>, end: Option<i64>) -> NewItem<'a> {
        NewItem {
            uid,
            component: "VEVENT",
            range_start: start,
            range_end: end,
            etag,
            content: b"BEGIN:VCALENDAR",
        }
    }

    #[tokio::test]
    async fn put_get_delete_with_preconditions() {
        let (store, cal, _) = setup().await;
        let new = item("u1", "\"e1\"", Some(0), Some(10));
        assert_eq!(
            store
                .put_item(cal.id, "a.ics", &new, &Precondition::MustNotExist)
                .await
                .unwrap(),
            PutOutcome::Created
        );
        assert!(matches!(
            store
                .put_item(cal.id, "a.ics", &new, &Precondition::MustNotExist)
                .await,
            Err(StoreError::PreconditionFailed)
        ));
        let v2 = item("u1", "\"e2\"", Some(0), Some(10));
        assert!(matches!(
            store
                .put_item(
                    cal.id,
                    "a.ics",
                    &v2,
                    &Precondition::IfMatch(vec!["\"old\"".into()])
                )
                .await,
            Err(StoreError::PreconditionFailed)
        ));
        assert_eq!(
            store
                .put_item(
                    cal.id,
                    "a.ics",
                    &v2,
                    &Precondition::IfMatch(vec!["\"e1\"".into()])
                )
                .await
                .unwrap(),
            PutOutcome::Updated
        );
        assert_eq!(
            store.item(cal.id, "a.ics").await.unwrap().unwrap().etag,
            "\"e2\""
        );

        assert!(matches!(
            store
                .delete_item(
                    cal.id,
                    "a.ics",
                    &Precondition::IfMatch(vec!["\"e1\"".into()])
                )
                .await,
            Err(StoreError::PreconditionFailed)
        ));
        assert!(
            store
                .delete_item(cal.id, "a.ics", &Precondition::None)
                .await
                .unwrap()
        );
        assert!(
            !store
                .delete_item(cal.id, "a.ics", &Precondition::None)
                .await
                .unwrap()
        );

        let revision = store
            .collection(cal.owner_id, CollectionKind::Calendar, &cal.slug)
            .await
            .unwrap()
            .unwrap()
            .sync_revision;
        assert_eq!(revision, 3);
    }

    #[tokio::test]
    async fn uid_lookup_is_scoped_to_the_collection() {
        let (store, alice, bob) = setup().await;
        let new = item("shared-uid", "\"e\"", None, None);
        store
            .put_item(alice.id, "a.ics", &new, &Precondition::None)
            .await
            .unwrap();
        assert_eq!(
            store
                .item_name_by_uid(alice.id, "shared-uid")
                .await
                .unwrap()
                .as_deref(),
            Some("a.ics")
        );
        assert_eq!(
            store.item_name_by_uid(bob.id, "shared-uid").await.unwrap(),
            None
        );
        assert_eq!(
            store.item_name_by_uid(alice.id, "other").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn if_none_match_with_tags() {
        let (store, cal, _) = setup().await;
        let v1 = item("u", "\"e1\"", None, None);
        store
            .put_item(cal.id, "a.ics", &v1, &Precondition::None)
            .await
            .unwrap();
        let v2 = item("u", "\"e2\"", None, None);
        assert!(matches!(
            store
                .put_item(
                    cal.id,
                    "a.ics",
                    &v2,
                    &Precondition::IfNoneMatch(vec!["\"e1\"".into()])
                )
                .await,
            Err(StoreError::PreconditionFailed)
        ));
        store
            .put_item(
                cal.id,
                "a.ics",
                &v2,
                &Precondition::IfNoneMatch(vec!["\"other\"".into()]),
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .put_item(
                    cal.id,
                    "a.ics",
                    &v1,
                    &Precondition::IfMatch(vec!["\"x\"".into(), "\"e1\"".into()])
                )
                .await,
            Err(StoreError::PreconditionFailed)
        ));
        store
            .put_item(
                cal.id,
                "a.ics",
                &v1,
                &Precondition::IfMatch(vec!["\"x\"".into(), "\"e2\"".into()]),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn change_log_for_sync() {
        let (store, cal, other) = setup().await;
        let names = |changes: Vec<Change>| {
            changes
                .into_iter()
                .map(|c| (c.name, c.meta.is_some()))
                .collect::<Vec<_>>()
        };
        store
            .put_item(
                cal.id,
                "a.ics",
                &item("a", "\"1\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
        store
            .put_item(
                cal.id,
                "b.ics",
                &item("b", "\"1\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
        let mid = store
            .changes_since(cal.id, 0, 100)
            .await
            .unwrap()
            .last()
            .unwrap()
            .revision;
        store
            .put_item(
                cal.id,
                "a.ics",
                &item("a", "\"2\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
        store
            .delete_item(cal.id, "b.ics", &Precondition::None)
            .await
            .unwrap();
        store
            .put_item(
                other.id,
                "x.ics",
                &item("x", "\"1\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();

        // Latest change per object, ordered by revision; other collections excluded.
        assert_eq!(
            names(store.changes_since(cal.id, 0, 100).await.unwrap()),
            [("a.ics".to_owned(), true), ("b.ics".to_owned(), false)]
        );
        assert_eq!(
            names(store.changes_since(cal.id, mid, 100).await.unwrap()).len(),
            2
        );
        assert_eq!(store.changes_since(cal.id, 0, 1).await.unwrap().len(), 1);
        // Repeated updates do not grow the log.
        for i in 0..20 {
            let etag = format!("\"{i}\"");
            store
                .put_item(
                    cal.id,
                    "a.ics",
                    &item("a", &etag, None, None),
                    &Precondition::None,
                )
                .await
                .unwrap();
        }
        assert_eq!(store.changes_since(cal.id, 0, 1000).await.unwrap().len(), 2);
        let by_rev = store.items_by_revision(cal.id, 10).await.unwrap();
        assert_eq!(by_rev.len(), 1);
    }

    #[tokio::test]
    async fn uid_must_be_unique_per_collection() {
        let (store, alice, bob) = setup().await;
        store
            .put_item(
                alice.id,
                "a.ics",
                &item("same", "\"1\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
        assert!(matches!(
            store.put_item(alice.id, "b.ics", &item("same", "\"2\"", None, None), &Precondition::None).await,
            Err(StoreError::UidConflict(name)) if name == "a.ics"
        ));
        // Another user's calendar is independent.
        store
            .put_item(
                bob.id,
                "b.ics",
                &item("same", "\"2\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn items_are_scoped_to_their_collection() {
        let (store, alice, bob) = setup().await;
        store
            .put_item(
                alice.id,
                "secret.ics",
                &item("u", "\"1\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
        assert!(store.item(bob.id, "secret.ics").await.unwrap().is_none());
        assert!(
            store
                .query_items(bob.id, None, None, None)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !store
                .delete_item(bob.id, "secret.ics", &Precondition::None)
                .await
                .unwrap()
        );
        assert!(store.item(alice.id, "secret.ics").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn time_range_query() {
        let (store, cal, _) = setup().await;
        for (name, start, end) in [
            ("early.ics", Some(0), Some(100)),
            ("late.ics", Some(1000), Some(1100)),
            ("open.ics", Some(50), None),
            ("undated.ics", None, None),
        ] {
            store
                .put_item(
                    cal.id,
                    name,
                    &item(name, "\"e\"", start, end),
                    &Precondition::None,
                )
                .await
                .unwrap();
        }
        let names = |items: Vec<Item>| items.into_iter().map(|i| i.name).collect::<Vec<_>>();
        assert_eq!(
            names(
                store
                    .query_items(cal.id, Some("VEVENT"), Some(200), Some(300))
                    .await
                    .unwrap()
            ),
            ["open.ics", "undated.ics"]
        );
        assert_eq!(
            names(
                store
                    .query_items(cal.id, Some("VEVENT"), Some(1050), None)
                    .await
                    .unwrap()
            ),
            ["late.ics", "open.ics", "undated.ics"]
        );
        assert!(
            store
                .query_items(cal.id, Some("VTODO"), None, None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn collections_lifecycle() {
        let (store, alice, _) = setup().await;
        let slug: Slug = "work".parse().unwrap();
        let work = store
            .create_collection(
                alice.owner_id,
                CollectionKind::Calendar,
                &slug,
                Some("Work"),
                None,
                None,
                "VEVENT",
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .create_collection(
                    alice.owner_id,
                    CollectionKind::Calendar,
                    &slug,
                    None,
                    None,
                    None,
                    "VEVENT"
                )
                .await,
            Err(StoreError::CollectionExists)
        ));
        store
            .update_collection(work.id, Some(Some("Job")), None, Some(Some("#FF0000FF")))
            .await
            .unwrap();
        let updated = store
            .collection(alice.owner_id, CollectionKind::Calendar, &slug)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.display_name.as_deref(), Some("Job"));
        assert_eq!(updated.color.as_deref(), Some("#FF0000FF"));

        store
            .put_item(
                work.id,
                "x.ics",
                &item("x", "\"1\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
        store.delete_collection(work.id).await.unwrap();
        assert!(
            store
                .collection(alice.owner_id, CollectionKind::Calendar, &slug)
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.item(work.id, "x.ics").await.unwrap().is_none());
    }

    async fn calendar(store: &Store, owner_id: i64, slug: &str, components: &str) -> Collection {
        store
            .create_collection(
                owner_id,
                CollectionKind::Calendar,
                &slug.parse().unwrap(),
                None,
                None,
                None,
                components,
            )
            .await
            .unwrap()
    }

    fn transfer<'a>(
        owner_id: i64,
        from: (i64, &'a str),
        to: (i64, &'a str),
        precondition: &'a Precondition,
        overwrite: bool,
    ) -> Transfer<'a> {
        Transfer {
            owner_id,
            source_collection: from.0,
            source_name: from.1,
            source_precondition: precondition,
            destination_collection: to.0,
            destination_name: to.1,
            overwrite,
        }
    }

    async fn revision(store: &Store, collection: &Collection) -> i64 {
        store
            .collection(collection.owner_id, collection.kind, &collection.slug)
            .await
            .unwrap()
            .unwrap()
            .sync_revision
    }

    #[tokio::test]
    async fn move_logs_removal_and_addition() {
        let (store, cal, _) = setup().await;
        let work = calendar(&store, cal.owner_id, "work", "VEVENT,VTODO").await;
        store
            .put_item(
                cal.id,
                "a.ics",
                &item("u", "\"e\"", Some(5), Some(9)),
                &Precondition::None,
            )
            .await
            .unwrap();
        let (before_cal, before_work) =
            (revision(&store, &cal).await, revision(&store, &work).await);
        let none = Precondition::None;
        let outcome = store
            .move_item(&transfer(
                cal.owner_id,
                (cal.id, "a.ics"),
                (work.id, "b.ics"),
                &none,
                true,
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            outcome,
            TransferOutcome {
                created: true,
                etag: "\"e\"".into()
            }
        );
        assert!(store.item(cal.id, "a.ics").await.unwrap().is_none());
        assert_eq!(
            store.item(work.id, "b.ics").await.unwrap().unwrap().etag,
            "\"e\""
        );
        // Time range and UID travel with the object.
        assert_eq!(
            store
                .query_item_meta(work.id, Some("VEVENT"), Some(0), Some(6), 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .item_name_by_uid(work.id, "u")
                .await
                .unwrap()
                .as_deref(),
            Some("b.ics")
        );
        assert_eq!(revision(&store, &cal).await, before_cal + 1);
        assert_eq!(revision(&store, &work).await, before_work + 1);
        let removed = store.changes_since(cal.id, before_cal, 10).await.unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(
            (removed[0].name.as_str(), removed[0].meta.is_none()),
            ("a.ics", true)
        );
        let added = store.changes_since(work.id, before_work, 10).await.unwrap();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].name, "b.ics");
        assert_eq!(added[0].meta.as_ref().unwrap().etag, "\"e\"");
        // The source is gone: a second move finds nothing.
        assert_eq!(
            store
                .move_item(&transfer(
                    cal.owner_id,
                    (cal.id, "a.ics"),
                    (work.id, "c.ics"),
                    &none,
                    true
                ))
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn rename_within_a_collection_keeps_the_uid() {
        let (store, cal, _) = setup().await;
        store
            .put_item(
                cal.id,
                "a.ics",
                &item("u", "\"e\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
        let none = Precondition::None;
        let renamed = transfer(
            cal.owner_id,
            (cal.id, "a.ics"),
            (cal.id, "b.ics"),
            &none,
            true,
        );
        // A copy would duplicate the UID in the collection.
        assert!(matches!(
            store.copy_item(&renamed).await,
            Err(StoreError::UidConflict(name)) if name == "a.ics"
        ));
        assert!(store.move_item(&renamed).await.unwrap().unwrap().created);
        assert_eq!(store.items(cal.id).await.unwrap().len(), 1);
        assert!(matches!(
            store
                .move_item(&transfer(
                    cal.owner_id,
                    (cal.id, "b.ics"),
                    (cal.id, "b.ics"),
                    &none,
                    true
                ))
                .await,
            Err(StoreError::SameResource)
        ));
    }

    #[tokio::test]
    async fn copy_and_overwrite_rules() {
        let (store, cal, _) = setup().await;
        let work = calendar(&store, cal.owner_id, "work", "VEVENT").await;
        let todos = calendar(&store, cal.owner_id, "todos", "VTODO").await;
        store
            .put_item(
                cal.id,
                "a.ics",
                &item("u", "\"e1\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
        store
            .put_item(
                work.id,
                "x.ics",
                &item("other", "\"e2\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
        let none = Precondition::None;
        let owner = cal.owner_id;
        // Overwrite F on an existing destination.
        assert!(matches!(
            store
                .copy_item(&transfer(
                    owner,
                    (cal.id, "a.ics"),
                    (work.id, "x.ics"),
                    &none,
                    false
                ))
                .await,
            Err(StoreError::PreconditionFailed)
        ));
        // Source preconditions.
        let wrong = Precondition::IfMatch(vec!["\"nope\"".into()]);
        assert!(matches!(
            store
                .copy_item(&transfer(
                    owner,
                    (cal.id, "a.ics"),
                    (work.id, "y.ics"),
                    &wrong,
                    true
                ))
                .await,
            Err(StoreError::PreconditionFailed)
        ));
        let right = Precondition::IfMatch(vec!["\"e1\"".into()]);
        let replaced = store
            .copy_item(&transfer(
                owner,
                (cal.id, "a.ics"),
                (work.id, "x.ics"),
                &right,
                true,
            ))
            .await
            .unwrap()
            .unwrap();
        assert!(!replaced.created);
        assert_eq!(
            store.item(work.id, "x.ics").await.unwrap().unwrap().etag,
            "\"e1\""
        );
        assert!(store.item(cal.id, "a.ics").await.unwrap().is_some());
        // The UID is now used in `work`: a second copy conflicts.
        assert!(matches!(
            store.copy_item(&transfer(owner, (cal.id, "a.ics"), (work.id, "z.ics"), &none, true)).await,
            Err(StoreError::UidConflict(name)) if name == "x.ics"
        ));
        // Component set of the destination.
        assert!(matches!(
            store
                .move_item(&transfer(
                    owner,
                    (cal.id, "a.ics"),
                    (todos.id, "a.ics"),
                    &none,
                    true
                ))
                .await,
            Err(StoreError::UnsupportedComponent)
        ));
        assert!(store.item(cal.id, "a.ics").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn transfers_stay_within_one_owner_and_kind() {
        let (store, alice, bob) = setup().await;
        store
            .put_item(
                alice.id,
                "a.ics",
                &item("u", "\"e\"", None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
        let none = Precondition::None;
        // Into bob's calendar, even with alice as owner.
        assert!(matches!(
            store
                .move_item(&transfer(
                    alice.owner_id,
                    (alice.id, "a.ics"),
                    (bob.id, "a.ics"),
                    &none,
                    true
                ))
                .await,
            Err(StoreError::MissingDestination)
        ));
        // As bob: alice's source does not exist for him.
        assert_eq!(
            store
                .move_item(&transfer(
                    bob.owner_id,
                    (alice.id, "a.ics"),
                    (bob.id, "a.ics"),
                    &none,
                    true
                ))
                .await
                .unwrap(),
            None
        );
        // Into alice's address book.
        let book = store
            .collection(
                alice.owner_id,
                CollectionKind::Addressbook,
                &DEFAULT_COLLECTION_SLUG.parse().unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            store
                .copy_item(&transfer(
                    alice.owner_id,
                    (alice.id, "a.ics"),
                    (book.id, "a.ics"),
                    &none,
                    true
                ))
                .await,
            Err(StoreError::MissingDestination)
        ));
        assert!(store.item(alice.id, "a.ics").await.unwrap().is_some());
        assert!(store.items(bob.id).await.unwrap().is_empty());
        assert!(store.items(book.id).await.unwrap().is_empty());
    }
}
