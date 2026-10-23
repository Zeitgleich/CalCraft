//! Calendar objects and collection management.
//!
//! Every query takes the collection id, which callers only obtain after
//! resolving a collection for the authenticated owner. There is no query
//! that looks up an item without its collection.

use calcraft_core::{CollectionKind, Slug};
use sqlx::{Row, sqlite::SqliteRow};

use crate::{Collection, PropertyChange, Result, Store, StoreError, collection_from_row};

/// Maximum number of calendars plus address books per user.
pub const MAX_COLLECTIONS_PER_USER: i64 = 100;
/// Maximum number of objects per collection.
pub const MAX_ITEMS_PER_COLLECTION: i64 = 100_000;
/// Storage quota per user for object content, including the trash.
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
    /// The range only spans the hull of several instances.
    pub recurring: bool,
    /// Version of the indexing that computed the range.
    pub index_version: i64,
    pub etag: &'a str,
    pub content: &'a [u8],
    /// Managed ids (RFC 8607) the content references. The store records a
    /// reference only for those the object may reference (see
    /// `attachments::set_item_refs`) and ignores the others.
    pub attachments: &'a [String],
}

/// A candidate of a time-range query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryCandidate {
    pub meta: ItemMeta,
    /// The indexed range is only a hull: check the instances exactly.
    pub recurring: bool,
}

/// An object whose time-range index is out of date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleIndex {
    pub id: i64,
    pub etag: String,
    pub content: Vec<u8>,
}

/// New index values for [`Store::set_item_index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemIndex {
    pub range_start: Option<i64>,
    pub range_end: Option<i64>,
    pub recurring: bool,
    pub version: i64,
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
    /// A sharee's request: the shares through which they see the source
    /// and the destination. Checked in the write transaction (source
    /// readable, or writable for a MOVE; destination writable), see
    /// [`StoreError::ShareRevoked`].
    pub source_share: Option<i64>,
    pub destination_share: Option<i64>,
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

/// Schedule-Tag handling of a write ([`Store::put_item_scheduled`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScheduleTag<'a> {
    /// Schedule-Tag stored with the object; `None` clears it (the object is
    /// not a scheduling object resource).
    pub set: Option<&'a str>,
    /// `If-Schedule-Tag-Match`: the stored tag must be exactly this one.
    pub if_match: Option<&'a str>,
    /// Another object (collection id, name) that must still carry this
    /// Schedule-Tag, checked in the same transaction: a delivery derived
    /// from an organizer's object is only written while that object is
    /// unchanged, so an older delivery can never overwrite a newer one
    /// ([`StoreError::Superseded`]).
    pub requires: Option<(i64, &'a str, &'a str)>,
    /// The write is an attendee's change of their copy of an invitation
    /// (same organizer, still a scheduling object): references to the
    /// organizer's attachments that the copy got by delivery may stay, as
    /// long as the organizer's object still references them. Every other
    /// write drops references to attachments of other users.
    pub keep_delivered: bool,
}

impl Precondition {
    /// Whether the condition holds for an object with entity tag
    /// `existing` (`None`: no object).
    pub fn holds(&self, existing: Option<&str>) -> bool {
        check(self, existing).is_ok()
    }
}

pub(crate) fn check(precondition: &Precondition, existing: Option<&str>) -> Result<()> {
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
        self.create_collection_with_properties(
            owner_id,
            kind,
            slug,
            display_name,
            description,
            color,
            components,
            &[],
        )
        .await
    }

    /// [`Store::create_collection`] that also sets dead properties, in the
    /// same transaction (see [`Store::update_collection_with_properties`]).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_collection_with_properties(
        &self,
        owner_id: i64,
        kind: CollectionKind,
        slug: &Slug,
        display_name: Option<&str>,
        description: Option<&str>,
        color: Option<&str>,
        components: &str,
        properties: &[PropertyChange<'_>],
    ) -> Result<Collection> {
        self.observe(
            async {
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
                // The slug of a collection shared with the user is taken too.
                if crate::sharing::slug_taken_by_share(&mut tx, owner_id, kind, slug.as_str()).await? {
                    return Err(StoreError::CollectionExists);
                }
                let result = sqlx::query(
                    "INSERT INTO collections (owner_id, kind, slug, display_name, description, color, components, token_salt) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, abs(random())) \
                     RETURNING id, owner_id, kind, slug, display_name, description, color, sync_revision, components, sync_floor, token_salt, read_only",
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
                crate::properties::apply(
                    &mut tx,
                    collection.id,
                    properties,
                    self.settings.quota_bytes(),
                )
                .await?;
                tx.commit().await?;
                Ok(collection)
            }
            .await,
        )
    }

    /// Deletes a collection and all its objects.
    pub async fn delete_collection(&self, collection_id: i64) -> Result<()> {
        self.observe(
            async {
                sqlx::query("DELETE FROM collections WHERE id = ?")
                    .bind(collection_id)
                    .execute(&self.writer)
                    .await?;
                Ok(())
            }
            .await,
        )
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
        self.update_collection_with_properties(collection_id, display_name, description, color, &[])
            .await
    }

    /// [`Store::update_collection`] that also sets or removes dead
    /// properties, all in one transaction. Fails with [`StoreError::Limit`]
    /// (and changes nothing) if the collection would end up with more than
    /// [`MAX_DEAD_PROPERTIES`](crate::MAX_DEAD_PROPERTIES) or
    /// [`MAX_DEAD_PROPERTY_BYTES`](crate::MAX_DEAD_PROPERTY_BYTES), or the
    /// owner over the storage quota.
    ///
    /// Like the other properties they bump the collection's revision
    /// (`getctag`), which tells clients to read the properties again; no
    /// object changes are recorded, so a sync report stays empty.
    pub async fn update_collection_with_properties(
        &self,
        collection_id: i64,
        display_name: Option<Option<&str>>,
        description: Option<Option<&str>>,
        color: Option<Option<&str>>,
        properties: &[PropertyChange<'_>],
    ) -> Result<()> {
        self.observe(
            async {
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
                crate::properties::apply(
                    &mut tx,
                    collection_id,
                    properties,
                    self.settings.quota_bytes(),
                )
                .await?;
                sqlx::query(
                    "UPDATE collections SET sync_revision = sync_revision + 1 WHERE id = ?",
                )
                .bind(collection_id)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(())
            }
            .await,
        )
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
    /// check result size before loading any content. Pages through the
    /// candidates in name order: pass the last name seen as `after`.
    pub async fn query_candidates(
        &self,
        collection_id: i64,
        component: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<QueryCandidate>> {
        let rows = sqlx::query(
            "SELECT name, etag, length(content) AS size, recurring FROM items \
             WHERE collection_id = ?1 \
               AND (?2 IS NULL OR component = ?2) \
               AND (?3 IS NULL OR range_end IS NULL OR range_end > ?3) \
               AND (?4 IS NULL OR range_start IS NULL OR range_start < ?4) \
               AND (?5 IS NULL OR name > ?5) \
             ORDER BY name LIMIT ?6",
        )
        .bind(collection_id)
        .bind(component)
        .bind(start)
        .bind(end)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(QueryCandidate {
                    meta: meta_from_row(row)?,
                    recurring: row.try_get("recurring")?,
                })
            })
            .collect()
    }

    /// Up to `limit` calendar objects indexed by a version older than
    /// `version`, in id order after `after_id`.
    pub async fn stale_index(
        &self,
        version: i64,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<StaleIndex>> {
        let rows = sqlx::query(
            "SELECT id, etag, content FROM items \
             WHERE index_version < ? AND id > ? AND component <> 'VCARD' \
             ORDER BY id LIMIT ?",
        )
        .bind(version)
        .bind(after_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(StaleIndex {
                    id: row.try_get("id")?,
                    etag: row.try_get("etag")?,
                    content: row.try_get("content")?,
                })
            })
            .collect()
    }

    /// Stores new index values for an object, unless it changed since it
    /// was read (`etag`) or is already indexed by this version. Neither the
    /// object nor any sync state changes. Returns whether the row changed.
    pub async fn set_item_index(&self, id: i64, etag: &str, index: &ItemIndex) -> Result<bool> {
        self.observe(
            async {
                let done = sqlx::query(
                    "UPDATE items SET range_start = ?, range_end = ?, recurring = ?, index_version = ? \
                     WHERE id = ? AND etag = ? AND index_version < ?",
                )
                .bind(index.range_start)
                .bind(index.range_end)
                .bind(index.recurring)
                .bind(index.version)
                .bind(id)
                .bind(etag)
                .bind(index.version)
                .execute(&self.writer)
                .await?;
                Ok(done.rows_affected() == 1)
            }
            .await,
        )
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
    /// The object is not a scheduling object afterwards (no Schedule-Tag).
    pub async fn put_item(
        &self,
        collection_id: i64,
        name: &str,
        item: &NewItem<'_>,
        precondition: &Precondition,
    ) -> Result<PutOutcome> {
        self.put_item_scheduled(
            collection_id,
            name,
            item,
            precondition,
            &ScheduleTag::default(),
        )
        .await
    }

    /// [`Store::put_item`] that also sets the object's Schedule-Tag and
    /// checks `If-Schedule-Tag-Match` (RFC 6638 §3.2.10, §8.3) in the same
    /// transaction.
    pub async fn put_item_scheduled(
        &self,
        collection_id: i64,
        name: &str,
        item: &NewItem<'_>,
        precondition: &Precondition,
        schedule: &ScheduleTag<'_>,
    ) -> Result<PutOutcome> {
        self.put_item_inner(collection_id, name, item, precondition, schedule, None)
            .await
    }

    /// [`Store::put_item`] by a sharee through share `share_id`: fails with
    /// [`StoreError::ShareRevoked`] (and changes nothing) unless the share
    /// still grants write access when the write happens. The quota is the
    /// owner's.
    pub async fn put_item_shared(
        &self,
        share_id: i64,
        collection_id: i64,
        name: &str,
        item: &NewItem<'_>,
        precondition: &Precondition,
    ) -> Result<PutOutcome> {
        self.put_item_inner(
            collection_id,
            name,
            item,
            precondition,
            &ScheduleTag::default(),
            Some(share_id),
        )
        .await
    }

    async fn put_item_inner(
        &self,
        collection_id: i64,
        name: &str,
        item: &NewItem<'_>,
        precondition: &Precondition,
        schedule: &ScheduleTag<'_>,
        share: Option<i64>,
    ) -> Result<PutOutcome> {
        self.observe(
            async {
                // IMMEDIATE takes the write lock up front, so the checks below and
                // the write are atomic and concurrent writers wait instead of failing.
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                ensure_writable(&mut tx, collection_id).await?;
                if let Some(share) = share {
                    crate::sharing::ensure_share(&mut tx, share, collection_id, true).await?;
                }
                let row = sqlx::query(
                    "SELECT etag, schedule_tag FROM items WHERE collection_id = ? AND name = ?",
                )
                .bind(collection_id)
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?;
                let existing: Option<String> =
                    row.as_ref().map(|row| row.try_get("etag")).transpose()?;
                check(precondition, existing.as_deref())?;
                if let Some(expected) = schedule.if_match {
                    let current: Option<String> = row
                        .as_ref()
                        .map(|row| row.try_get("schedule_tag"))
                        .transpose()?
                        .flatten();
                    if current.as_deref() != Some(expected) {
                        return Err(StoreError::PreconditionFailed);
                    }
                }
                let mut guard_id = None;
                if let Some((guard_collection, guard_name, guard_tag)) = schedule.requires {
                    let guard = sqlx::query(
                        "SELECT id, schedule_tag FROM items WHERE collection_id = ? AND name = ?",
                    )
                    .bind(guard_collection)
                    .bind(guard_name)
                    .fetch_optional(&mut *tx)
                    .await?;
                    let current: Option<String> = guard
                        .as_ref()
                        .map(|row| row.try_get::<Option<String>, _>("schedule_tag"))
                        .transpose()?
                        .flatten();
                    if current.as_deref() != Some(guard_tag) {
                        return Err(StoreError::Superseded);
                    }
                    guard_id = guard.map(|row| row.try_get::<i64, _>("id")).transpose()?;
                }

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

                // The trash and the scheduling inbox count towards the quota:
                // deleting does not free space until the trash is emptied or
                // its entries expire.
                let row = sqlx::query(
                    "SELECT c.owner_id, (SELECT COALESCE(SUM(length(i.content)), 0) FROM items i \
                       JOIN collections o ON o.id = i.collection_id \
                       WHERE o.owner_id = c.owner_id \
                         AND NOT (i.collection_id = ?1 AND i.name = ?2)) AS used \
                     FROM collections c WHERE c.id = ?1",
                )
                .bind(collection_id)
                .bind(name)
                .fetch_one(&mut *tx)
                .await?;
                let owner_id: i64 = row.try_get("owner_id")?;
                let used = row
                    .try_get::<i64, _>("used")?
                    .saturating_add(crate::trash::trash_bytes(&mut *tx, owner_id).await?)
                    .saturating_add(crate::properties::owner_bytes(&mut *tx, owner_id).await?)
                    .saturating_add(crate::scheduling::inbox_bytes(&mut *tx, owner_id).await?)
                    .saturating_add(crate::attachments::owner_bytes(&mut *tx, owner_id).await?);
                let size = i64::try_from(item.content.len()).unwrap_or(i64::MAX);
                if used.saturating_add(size) > self.settings.quota_bytes() {
                    return Err(StoreError::Limit("storage quota exceeded"));
                }

                let revision: i64 = sqlx::query(
                    "UPDATE collections SET sync_revision = sync_revision + 1 WHERE id = ? RETURNING sync_revision",
                )
                .bind(collection_id)
                .fetch_one(&mut *tx)
                .await?
                .try_get("sync_revision")?;

                // An update keeps the row (and its id, which the attachment
                // references point to).
                let item_id: i64 = sqlx::query(
                    "INSERT INTO items (collection_id, name, uid, etag, component, range_start, range_end, \
                       recurring, index_version, content, revision, schedule_tag) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT (collection_id, name) DO UPDATE SET \
                       uid = excluded.uid, etag = excluded.etag, component = excluded.component, \
                       range_start = excluded.range_start, range_end = excluded.range_end, \
                       recurring = excluded.recurring, index_version = excluded.index_version, \
                       content = excluded.content, revision = excluded.revision, \
                       schedule_tag = excluded.schedule_tag, \
                       modified_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
                     RETURNING id",
                )
                .bind(collection_id)
                .bind(name)
                .bind(item.uid)
                .bind(item.etag)
                .bind(item.component)
                .bind(item.range_start)
                .bind(item.range_end)
                .bind(item.recurring)
                .bind(item.index_version)
                .bind(item.content)
                .bind(revision)
                .bind(schedule.set)
                .fetch_one(&mut *tx)
                .await?
                .try_get("id")?;
                crate::attachments::set_item_refs(
                    &mut tx,
                    item_id,
                    owner_id,
                    item.attachments,
                    guard_id,
                    schedule.keep_delivered && schedule.set.is_some(),
                )
                .await?;
                record_change(&mut tx, collection_id, revision, name, false).await?;
                tx.commit().await?;
                Ok(if existing.is_some() {
                    PutOutcome::Updated
                } else {
                    PutOutcome::Created
                })
            }
            .await,
        )
    }

    /// Deletes an object. Returns `false` if it does not exist.
    pub async fn delete_item(
        &self,
        collection_id: i64,
        name: &str,
        precondition: &Precondition,
    ) -> Result<bool> {
        self.delete_item_inner(collection_id, name, precondition, None)
            .await
    }

    pub(crate) async fn delete_item_inner(
        &self,
        collection_id: i64,
        name: &str,
        precondition: &Precondition,
        share: Option<i64>,
    ) -> Result<bool> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                ensure_writable(&mut tx, collection_id).await?;
                if let Some(share) = share {
                    crate::sharing::ensure_share(&mut tx, share, collection_id, true).await?;
                }
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
            .await,
        )
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
        self.observe(
            async {
                let Transfer {
                    owner_id,
                    source_share,
                    destination_share,
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
                if let Some(share) = source_share {
                    crate::sharing::ensure_share(&mut tx, share, source_collection, remove_source)
                        .await?;
                }
                if let Some(share) = destination_share {
                    crate::sharing::ensure_share(&mut tx, share, destination_collection, true)
                        .await?;
                }

                // Both collections must belong to the owner and be of one kind. The
                // caller resolved them for the owner already; this keeps the queries
                // scoped and covers collections deleted in the meantime.
                let collection = |id: i64| {
                    sqlx::query(
                        "SELECT kind, components, read_only FROM collections WHERE id = ? AND owner_id = ?",
                    )
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
                // Nothing is copied or moved into a subscription, nor moved out of one.
                if destination.try_get::<bool, _>("read_only")?
                    || (remove_source && source.try_get::<bool, _>("read_only")?)
                {
                    return Err(StoreError::ReadOnly);
                }
                let kind: String = destination.try_get("kind")?;
                if source.try_get::<String, _>("kind")? != kind {
                    return Err(StoreError::MissingDestination);
                }

                let Some(item) = sqlx::query(
                    "SELECT id, uid, etag, component, range_start, range_end, recurring, index_version, \
                       content FROM items WHERE collection_id = ? AND name = ?",
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
                let recurring: bool = item.try_get("recurring")?;
                let index_version: i64 = item.try_get("index_version")?;
                let content: Vec<u8> = item.try_get("content")?;
                let source_id: i64 = item.try_get("id")?;
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

                // A copy adds its size to the owner's usage (objects plus trash). A
                // move stays within the owner's collections and never increases it,
                // so it is not counted twice.
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
                    // The trash counts towards the quota, as for PUT.
                    let used = used
                        .saturating_add(crate::trash::trash_bytes(&mut *tx, owner_id).await?)
                        .saturating_add(crate::properties::owner_bytes(&mut *tx, owner_id).await?)
                        .saturating_add(crate::scheduling::inbox_bytes(&mut *tx, owner_id).await?)
                        // Shared with the copy, so counted once.
                        .saturating_add(crate::attachments::owner_bytes(&mut *tx, owner_id).await?);
                    let size = i64::try_from(content.len()).unwrap_or(i64::MAX);
                    if used.saturating_add(size) > self.settings.quota_bytes() {
                        return Err(StoreError::Limit("storage quota exceeded"));
                    }
                }

                if remove_source {
                    // The row itself moves, so it keeps its id and with it its
                    // attachment references. A replaced destination goes first:
                    // its references (and attachments nobody else references)
                    // go with it.
                    if existing {
                        sqlx::query("DELETE FROM items WHERE collection_id = ? AND name = ?")
                            .bind(destination_collection)
                            .bind(destination_name)
                            .execute(&mut *tx)
                            .await?;
                    }
                    let revision = bump_revision(&mut tx, source_collection).await?;
                    record_change(&mut tx, source_collection, revision, source_name, true).await?;
                    let revision = bump_revision(&mut tx, destination_collection).await?;
                    sqlx::query(
                        "UPDATE items SET collection_id = ?, name = ?, revision = ?, \
                           modified_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
                         WHERE collection_id = ? AND name = ?",
                    )
                    .bind(destination_collection)
                    .bind(destination_name)
                    .bind(revision)
                    .bind(source_collection)
                    .bind(source_name)
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
                    return Ok(Some(TransferOutcome {
                        created: !existing,
                        etag,
                    }));
                }

                let revision = bump_revision(&mut tx, destination_collection).await?;
                let copy_id: i64 = sqlx::query(
                    "INSERT INTO items (collection_id, name, uid, etag, component, range_start, range_end, \
                       recurring, index_version, content, revision, schedule_tag) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT (collection_id, name) DO UPDATE SET \
                       uid = excluded.uid, etag = excluded.etag, component = excluded.component, \
                       range_start = excluded.range_start, range_end = excluded.range_end, \
                       recurring = excluded.recurring, index_version = excluded.index_version, \
                       content = excluded.content, revision = excluded.revision, \
                       schedule_tag = excluded.schedule_tag, \
                       modified_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
                     RETURNING id",
                )
                .bind(destination_collection)
                .bind(destination_name)
                .bind(&uid)
                .bind(&etag)
                .bind(&component)
                .bind(range_start)
                .bind(range_end)
                .bind(recurring)
                .bind(index_version)
                .bind(&content)
                .bind(revision)
                // A moved scheduling object stays one (above); a copy is a
                // plain object.
                .bind(None::<String>)
                .fetch_one(&mut *tx)
                .await?
                .try_get("id")?;
                // The copy shares the owner's attachments (no extra quota).
                crate::attachments::copy_refs(&mut tx, source_id, copy_id, owner_id).await?;
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
            .await,
        )
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

/// Refuses changes to the objects of a read-only collection (a
/// subscription); only [`Store::apply_subscription_feed`] writes there.
pub(crate) async fn ensure_writable(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    collection_id: i64,
) -> Result<()> {
    let read_only = sqlx::query("SELECT read_only FROM collections WHERE id = ?")
        .bind(collection_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|row| row.try_get::<bool, _>("read_only"))
        .transpose()?;
    if read_only == Some(true) {
        return Err(StoreError::ReadOnly);
    }
    Ok(())
}

pub(crate) async fn bump_revision(
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

pub(crate) async fn record_change(
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
mod tests;
