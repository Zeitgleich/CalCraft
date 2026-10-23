//! Managed attachments (RFC 8607).
//!
//! An attachment is an immutable blob owned by the user who uploaded it.
//! Objects reference attachments through `attachment_refs` rows, which only
//! the store writes: when an object is stored, the managed ids found in its
//! content ([`crate::NewItem::attachments`]) are checked inside the write
//! transaction, and only those the object may reference get a row (see
//! [`set_item_refs`]). Read access for other users derives from these rows
//! (an attendee's copy references the organizer's attachment), never from
//! the content, so writing someone else's managed id or URL into an own
//! event grants nothing. A reference to another user's attachment records
//! the organizer's object it was delivered from (`via_item_id`) and goes
//! when that object goes or stops referencing the attachment.
//!
//! An attachment is deleted with its last reference (database trigger, see
//! migration `0012_attachments`); uploads that never got a reference are
//! removed by [`Store::purge_unreferenced_attachments`]. Its bytes count
//! towards the owner's quota for as long as it exists.

use sqlx::{Row, Sqlite, SqliteExecutor, Transaction, sqlite::SqliteRow};

use crate::{Result, Store, StoreError};

/// Most attachments one object may reference; ids beyond are ignored.
pub const MAX_REFS_PER_ITEM: usize = 100;
/// Unreferenced uploads older than this (seconds) are removed.
pub const UNREFERENCED_GRACE_SECS: i64 = 3600;

/// A new upload ([`Store::insert_attachment`]).
#[derive(Debug, Clone, Copy)]
pub struct NewAttachment<'a> {
    /// Random managed id (32 lower-case hex digits).
    pub managed_id: &'a str,
    pub owner_id: i64,
    /// Already sanitised.
    pub filename: &'a str,
    /// Already validated (`type/subtype`).
    pub media_type: &'a str,
    pub content: &'a [u8],
    /// Unix seconds.
    pub now: i64,
}

/// A stored attachment without its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentMeta {
    pub managed_id: String,
    pub owner_id: i64,
    pub filename: String,
    pub media_type: String,
    pub size: i64,
}

/// Whether `id` looks like a managed id this server issues.
pub fn valid_managed_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn meta_from_row(row: &SqliteRow) -> Result<AttachmentMeta> {
    Ok(AttachmentMeta {
        managed_id: row.try_get("managed_id")?,
        owner_id: row.try_get("owner_id")?,
        filename: row.try_get("filename")?,
        media_type: row.try_get("media_type")?,
        size: row.try_get("size")?,
    })
}

/// Bytes of the attachments a user owns (they count towards the quota).
pub(crate) async fn owner_bytes(executor: impl SqliteExecutor<'_>, owner_id: i64) -> Result<i64> {
    Ok(
        sqlx::query("SELECT COALESCE(SUM(size), 0) AS bytes FROM attachments WHERE owner_id = ?")
            .bind(owner_id)
            .fetch_one(executor)
            .await?
            .try_get("bytes")?,
    )
}

/// Makes the references of object `item_id` exactly the attachments among
/// `ids` it may reference. Ids it may not reference are ignored: they grant
/// nothing. References that are no longer wanted are removed, together with
/// the references delivered from them (an organizer dropping an attachment
/// ends the attendees' access at once, whether the update reaches them or
/// not), which collects attachments nobody references any more.
///
/// An object may reference
///
/// * attachments of `owner_id`, the collection's owner;
/// * on a delivery (`guard`, the organizer's object, see
///   [`crate::ScheduleTag::requires`]): attachments the organizer owns and
///   their object references itself – never attachments that object only
///   holds of a third user, so an attendee who turns their copy into an
///   own invitation cannot pass the organizer's files on;
/// * with `keep_delivered` (the attendee changes their copy of the same
///   organizer's invitation): the delivered references it already has, as
///   long as the organizer's object still references the attachment.
///
/// Every other write drops references to other users' attachments.
pub(crate) async fn set_item_refs(
    tx: &mut Transaction<'static, Sqlite>,
    item_id: i64,
    owner_id: i64,
    ids: &[String],
    guard: Option<i64>,
    keep_delivered: bool,
) -> Result<()> {
    let mut wanted: Vec<(i64, Option<i64>)> = Vec::new();
    for id in ids.iter().filter(|id| valid_managed_id(id)) {
        if wanted.len() >= MAX_REFS_PER_ITEM {
            break;
        }
        let own: Option<i64> =
            sqlx::query("SELECT id FROM attachments WHERE managed_id = ? AND owner_id = ?")
                .bind(id)
                .bind(owner_id)
                .fetch_optional(&mut **tx)
                .await?
                .map(|row| row.try_get("id"))
                .transpose()?;
        let mut found = own.map(|attachment| (attachment, None));
        if found.is_none()
            && let Some(guard) = guard
        {
            found = sqlx::query(
                "SELECT a.id FROM attachments a \
                 JOIN attachment_refs g ON g.attachment_id = a.id AND g.item_id = ?2 \
                   AND g.via_item_id IS NULL \
                 JOIN items gi ON gi.id = g.item_id \
                 JOIN collections gc ON gc.id = gi.collection_id AND gc.owner_id = a.owner_id \
                 WHERE a.managed_id = ?1",
            )
            .bind(id)
            .bind(guard)
            .fetch_optional(&mut **tx)
            .await?
            .map(|row| row.try_get::<i64, _>("id"))
            .transpose()?
            .map(|attachment| (attachment, Some(guard)));
        }
        if found.is_none() && keep_delivered {
            found = sqlx::query(
                "SELECT a.id, r.via_item_id FROM attachments a \
                 JOIN attachment_refs r ON r.attachment_id = a.id AND r.item_id = ?2 \
                   AND r.via_item_id IS NOT NULL \
                 WHERE a.managed_id = ?1 AND EXISTS (SELECT 1 FROM attachment_refs o \
                   WHERE o.item_id = r.via_item_id AND o.attachment_id = a.id \
                     AND o.via_item_id IS NULL)",
            )
            .bind(id)
            .bind(item_id)
            .fetch_optional(&mut **tx)
            .await?
            .map(|row| {
                Ok::<_, sqlx::Error>((row.try_get::<i64, _>("id")?, row.try_get("via_item_id")?))
            })
            .transpose()?;
        }
        if let Some(found) = found
            && !wanted.iter().any(|(a, _)| *a == found.0)
        {
            wanted.push(found);
        }
    }
    set_exact_refs(tx, item_id, &wanted).await
}

/// Replaces the references of `item_id` by `wanted` (attachment row ids
/// and the object each was delivered from): new ones first, so that an
/// attachment kept by the object is never collected in between.
async fn set_exact_refs(
    tx: &mut Transaction<'static, Sqlite>,
    item_id: i64,
    wanted: &[(i64, Option<i64>)],
) -> Result<()> {
    for (attachment, via) in wanted {
        sqlx::query(
            "INSERT INTO attachment_refs (attachment_id, item_id, via_item_id) VALUES (?, ?, ?) \
             ON CONFLICT (item_id, attachment_id) WHERE item_id IS NOT NULL \
             DO UPDATE SET via_item_id = excluded.via_item_id",
        )
        .bind(attachment)
        .bind(item_id)
        .bind(via)
        .execute(&mut **tx)
        .await?;
    }
    let current: Vec<i64> =
        sqlx::query("SELECT attachment_id FROM attachment_refs WHERE item_id = ?")
            .bind(item_id)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|row| row.try_get("attachment_id"))
            .collect::<std::result::Result<_, _>>()?;
    for stale in current
        .iter()
        .filter(|id| !wanted.iter().any(|(a, _)| a == *id))
    {
        // What attendees got from this object goes first.
        sqlx::query("DELETE FROM attachment_refs WHERE via_item_id = ? AND attachment_id = ?")
            .bind(item_id)
            .bind(stale)
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM attachment_refs WHERE item_id = ? AND attachment_id = ?")
            .bind(item_id)
            .bind(stale)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// COPY: the copy `to` references the attachments of `owner_id` that `from`
/// references. Attachments of other users (an attendee's copy of an
/// invitation) are not carried over: deliveries keep only the original up
/// to date, so a copy must not keep access after a cancellation.
pub(crate) async fn copy_refs(
    tx: &mut Transaction<'static, Sqlite>,
    from: i64,
    to: i64,
    owner_id: i64,
) -> Result<()> {
    let wanted: Vec<(i64, Option<i64>)> = sqlx::query(
        "SELECT r.attachment_id FROM attachment_refs r JOIN attachments a ON a.id = r.attachment_id \
         WHERE r.item_id = ? AND a.owner_id = ?",
    )
    .bind(from)
    .bind(owner_id)
    .fetch_all(&mut **tx)
    .await?
    .iter()
    .map(|row| row.try_get("attachment_id").map(|id| (id, None)))
    .collect::<std::result::Result<_, _>>()?;
    set_exact_refs(tx, to, &wanted).await
}

/// An object goes into the trash entry `trash_id`: its references to the
/// owner's own attachments move along (a restore brings them back), those
/// to other users' attachments go with the object (access ends).
pub(crate) async fn refs_to_trash(
    tx: &mut Transaction<'static, Sqlite>,
    item_id: i64,
    trash_id: i64,
    owner_id: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE attachment_refs SET item_id = NULL, trash_id = ?1 \
         WHERE item_id = ?2 AND attachment_id IN (SELECT id FROM attachments WHERE owner_id = ?3)",
    )
    .bind(trash_id)
    .bind(item_id)
    .bind(owner_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// [`refs_to_trash`] for every object of a collection that went into the
/// trash entry `entry` (its objects are child rows with the same names).
pub(crate) async fn collection_refs_to_trash(
    tx: &mut Transaction<'static, Sqlite>,
    collection_id: i64,
    entry: i64,
    owner_id: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE attachment_refs SET trash_id = (SELECT t.id FROM trash t JOIN items i ON i.name = t.name \
             WHERE t.parent_id = ?1 AND i.id = attachment_refs.item_id), item_id = NULL \
         WHERE item_id IN (SELECT id FROM items WHERE collection_id = ?2) \
           AND attachment_id IN (SELECT id FROM attachments WHERE owner_id = ?3)",
    )
    .bind(entry)
    .bind(collection_id)
    .bind(owner_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A trash entry was restored as object `item_id`.
pub(crate) async fn refs_from_trash(
    tx: &mut Transaction<'static, Sqlite>,
    trash_id: i64,
    item_id: i64,
) -> Result<()> {
    sqlx::query("UPDATE attachment_refs SET item_id = ?, trash_id = NULL WHERE trash_id = ?")
        .bind(item_id)
        .bind(trash_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// A collection entry `entry` was restored as collection `collection_id`.
pub(crate) async fn collection_refs_from_trash(
    tx: &mut Transaction<'static, Sqlite>,
    entry: i64,
    collection_id: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE attachment_refs SET item_id = (SELECT i.id FROM items i JOIN trash t ON t.name = i.name \
             WHERE i.collection_id = ?1 AND t.id = attachment_refs.trash_id), trash_id = NULL \
         WHERE trash_id IN (SELECT id FROM trash WHERE parent_id = ?2)",
    )
    .bind(collection_id)
    .bind(entry)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl Store {
    /// Stores an upload that no object references yet; the caller then
    /// stores the object with the new `ATTACH` (and removes the upload with
    /// [`Store::delete_unreferenced_attachment`] if that fails). Fails with
    /// [`StoreError::Limit`] if the owner's quota (objects, trash,
    /// attachments) or `max_user_bytes` (attachments alone) would be
    /// exceeded.
    pub async fn insert_attachment(
        &self,
        attachment: &NewAttachment<'_>,
        max_user_bytes: i64,
    ) -> Result<()> {
        if !valid_managed_id(attachment.managed_id) {
            return Err(StoreError::Corrupt("invalid managed id".to_owned()));
        }
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let owner = attachment.owner_id;
                let size = i64::try_from(attachment.content.len()).unwrap_or(i64::MAX);
                let attachments = owner_bytes(&mut *tx, owner).await?;
                if attachments.saturating_add(size) > max_user_bytes {
                    return Err(StoreError::Limit("attachment quota exceeded"));
                }
                let objects: i64 = sqlx::query(
                    "SELECT COALESCE(SUM(length(i.content)), 0) AS used FROM items i \
                     JOIN collections c ON c.id = i.collection_id WHERE c.owner_id = ?",
                )
                .bind(owner)
                .fetch_one(&mut *tx)
                .await?
                .try_get("used")?;
                let used = objects
                    .saturating_add(attachments)
                    .saturating_add(crate::trash::trash_bytes(&mut *tx, owner).await?)
                    .saturating_add(crate::properties::owner_bytes(&mut *tx, owner).await?)
                    .saturating_add(crate::scheduling::inbox_bytes(&mut *tx, owner).await?);
                if used.saturating_add(size) > self.settings.quota_bytes() {
                    return Err(StoreError::Limit("storage quota exceeded"));
                }
                sqlx::query(
                    "INSERT INTO attachments (managed_id, owner_id, filename, media_type, size, content, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(attachment.managed_id)
                .bind(owner)
                .bind(attachment.filename)
                .bind(attachment.media_type)
                .bind(size)
                .bind(attachment.content)
                .bind(attachment.now)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(())
            }
            .await,
        )
    }

    /// Removes an upload of `owner_id` again if no object references it
    /// (the POST that uploaded it failed). Returns whether it was removed.
    pub async fn delete_unreferenced_attachment(
        &self,
        owner_id: i64,
        managed_id: &str,
    ) -> Result<bool> {
        self.observe(
            async {
                let done = sqlx::query(
                    "DELETE FROM attachments WHERE managed_id = ? AND owner_id = ? \
                       AND NOT EXISTS (SELECT 1 FROM attachment_refs r WHERE r.attachment_id = attachments.id)",
                )
                .bind(managed_id)
                .bind(owner_id)
                .execute(&self.writer)
                .await?;
                Ok(done.rows_affected() == 1)
            }
            .await,
        )
    }

    /// Removes up to `limit` uploads that no object references and that
    /// are older than [`UNREFERENCED_GRACE_SECS`] (aborted or failed
    /// uploads). Returns how many were removed.
    pub async fn purge_unreferenced_attachments(&self, now: i64, limit: u32) -> Result<u64> {
        self.observe(
            async {
                let done = sqlx::query(
                    "DELETE FROM attachments WHERE id IN (SELECT a.id FROM attachments a \
                       WHERE a.created_at < ? \
                         AND NOT EXISTS (SELECT 1 FROM attachment_refs r WHERE r.attachment_id = a.id) \
                       LIMIT ?)",
                )
                .bind(now.saturating_sub(UNREFERENCED_GRACE_SECS))
                .bind(limit)
                .execute(&self.writer)
                .await?;
                Ok(done.rows_affected())
            }
            .await,
        )
    }

    /// An attachment `reader` may download: one they own, or one a live
    /// object of theirs references (e.g. an invitation from the
    /// organizer). `None` otherwise, whether it exists or not.
    pub async fn readable_attachment(
        &self,
        managed_id: &str,
        reader: i64,
    ) -> Result<Option<AttachmentMeta>> {
        if !valid_managed_id(managed_id) {
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT a.managed_id, a.owner_id, a.filename, a.media_type, a.size FROM attachments a \
             WHERE a.managed_id = ?1 AND (a.owner_id = ?2 OR EXISTS (\
               SELECT 1 FROM attachment_refs r JOIN items i ON i.id = r.item_id \
                 JOIN collections c ON c.id = i.collection_id \
               WHERE r.attachment_id = a.id AND c.owner_id = ?2))",
        )
        .bind(managed_id)
        .bind(reader)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(meta_from_row).transpose()
    }

    /// Content of an attachment [`Store::readable_attachment`] returned
    /// (checked again: it may have been removed or the access revoked
    /// since).
    pub async fn attachment_content(
        &self,
        managed_id: &str,
        reader: i64,
    ) -> Result<Option<Vec<u8>>> {
        if !valid_managed_id(managed_id) {
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT a.content FROM attachments a \
             WHERE a.managed_id = ?1 AND (a.owner_id = ?2 OR EXISTS (\
               SELECT 1 FROM attachment_refs r JOIN items i ON i.id = r.item_id \
                 JOIN collections c ON c.id = i.collection_id \
               WHERE r.attachment_id = a.id AND c.owner_id = ?2))",
        )
        .bind(managed_id)
        .bind(reader)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row.try_get("content"))
            .transpose()
            .map_err(Into::into)
    }

    /// The attachments object `name` of collection `collection_id`
    /// references.
    pub async fn item_attachments(
        &self,
        collection_id: i64,
        name: &str,
    ) -> Result<Vec<AttachmentMeta>> {
        let rows = sqlx::query(
            "SELECT a.managed_id, a.owner_id, a.filename, a.media_type, a.size FROM attachments a \
             JOIN attachment_refs r ON r.attachment_id = a.id JOIN items i ON i.id = r.item_id \
             WHERE i.collection_id = ? AND i.name = ? ORDER BY a.id",
        )
        .bind(collection_id)
        .bind(name)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(meta_from_row).collect()
    }

    /// The attachments among `ids` that object `name` of collection
    /// `collection_id` (owned by `owner_id`) may reference: the owner's
    /// own and those the object references already. A check before a PUT;
    /// the store applies the same rule again when it writes.
    pub async fn referable_attachments(
        &self,
        collection_id: i64,
        name: &str,
        owner_id: i64,
        ids: &[String],
    ) -> Result<Vec<AttachmentMeta>> {
        let mut found = Vec::new();
        for id in ids.iter().filter(|id| valid_managed_id(id)) {
            let row = sqlx::query(
                "SELECT a.managed_id, a.owner_id, a.filename, a.media_type, a.size FROM attachments a \
                 WHERE a.managed_id = ?1 AND (a.owner_id = ?2 OR EXISTS (\
                   SELECT 1 FROM attachment_refs r JOIN items i ON i.id = r.item_id \
                   WHERE r.attachment_id = a.id AND i.collection_id = ?3 AND i.name = ?4))",
            )
            .bind(id)
            .bind(owner_id)
            .bind(collection_id)
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
            if let Some(row) = row {
                found.push(meta_from_row(&row)?);
            }
        }
        Ok(found)
    }

    /// Ends `recipient`'s access to the attachments delivered to them from
    /// object `name` of collection `collection_id` (the organizer's
    /// object): the organizer cancelled the invitation for them. Runs
    /// whether or not the cancellation itself can be delivered. Returns the
    /// number of references removed.
    pub async fn revoke_delivered_attachments(
        &self,
        collection_id: i64,
        name: &str,
        recipient: i64,
    ) -> Result<u64> {
        self.observe(
            async {
                let done = sqlx::query(
                    "DELETE FROM attachment_refs \
                     WHERE via_item_id = (SELECT id FROM items WHERE collection_id = ?1 AND name = ?2) \
                       AND item_id IN (SELECT i.id FROM items i \
                         JOIN collections c ON c.id = i.collection_id WHERE c.owner_id = ?3)",
                )
                .bind(collection_id)
                .bind(name)
                .bind(recipient)
                .execute(&self.writer)
                .await?;
                Ok(done.rows_affected())
            }
            .await,
        )
    }

    /// Number and bytes of the attachments a user owns.
    pub async fn attachment_usage(&self, owner_id: i64) -> Result<(i64, i64)> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n, COALESCE(SUM(size), 0) AS bytes FROM attachments WHERE owner_id = ?",
        )
        .bind(owner_id)
        .fetch_one(&self.pool)
        .await?;
        Ok((row.try_get("n")?, row.try_get("bytes")?))
    }
}

#[cfg(test)]
mod tests;
