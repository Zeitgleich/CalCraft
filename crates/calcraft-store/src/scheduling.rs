//! Storage for CalDAV scheduling (RFC 6638) between users of this server:
//! calendar user addresses, the scheduling inbox, lookups of scheduling
//! objects by UID and free/busy candidates.
//!
//! Like everything else, every query is scoped to a user id that the caller
//! resolved before. Scheduling never reaches read-only calendars
//! (subscriptions): lookups and deliveries only consider writable ones.

use sqlx::Row;

use crate::{
    Collection, Item, ItemMeta, Precondition, Result, Store, StoreError, User, collection_from_row,
    items::check, user_from_row,
};

/// Longest calendar user address (RFC 5321 path limit).
pub const MAX_EMAIL_LEN: usize = 254;

/// A scheduling object resource of a user: the calendar object with a UID,
/// found in one of the user's writable calendars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulingObject {
    pub collection: Collection,
    pub name: String,
    pub etag: String,
    pub content: Vec<u8>,
    pub schedule_tag: Option<String>,
}

/// An iTIP message for a user's scheduling inbox.
#[derive(Debug, Clone, Copy)]
pub struct NewInboxMessage<'a> {
    /// Resource name in the inbox (random, chosen by the caller).
    pub name: &'a str,
    pub uid: &'a str,
    /// Normalised address of the sender; a newer message with the same UID
    /// and sender replaces the older one.
    pub sender: &'a str,
    pub etag: &'a str,
    pub content: &'a [u8],
    /// Unix seconds.
    pub now: i64,
}

/// A message in the inbox, without its content.
pub type InboxMessage = ItemMeta;

/// A VEVENT that may be busy in a free/busy query, with its indexed span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeBusyCandidate {
    pub name: String,
    pub range_start: Option<i64>,
    pub range_end: Option<i64>,
}

/// Bytes of the owner's inbox; they count towards the storage quota.
pub(crate) async fn inbox_bytes(
    executor: impl sqlx::SqliteExecutor<'_>,
    owner_id: i64,
) -> Result<i64> {
    Ok(sqlx::query(
        "SELECT COALESCE(SUM(length(content)), 0) AS bytes FROM schedule_inbox WHERE owner_id = ?",
    )
    .bind(owner_id)
    .fetch_one(executor)
    .await?
    .try_get("bytes")?)
}

impl Store {
    // --- calendar user addresses -----------------------------------------

    /// A user by login name, without credentials.
    pub async fn user_by_name(&self, username: &calcraft_core::Username) -> Result<Option<User>> {
        let row = sqlx::query(
            "SELECT id, username, display_name, disabled, is_admin, totp_enabled FROM users WHERE username = ?",
        )
        .bind(username.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(user_from_row).transpose()
    }

    /// The e-mail address an administrator set for the user, if any.
    pub async fn user_email(&self, user_id: i64) -> Result<Option<String>> {
        let row = sqlx::query("SELECT email FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|row| row.try_get::<Option<String>, _>("email"))
            .transpose()?
            .flatten())
    }

    /// Whether the user accepts invitations (their own setting).
    pub async fn accepts_invitations(&self, user_id: i64) -> Result<bool> {
        let row = sqlx::query("SELECT accept_invitations FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|row| row.try_get::<bool, _>("accept_invitations"))
            .transpose()?
            .unwrap_or(false))
    }

    /// Changes whether the user accepts invitations.
    pub async fn set_accept_invitations(&self, user_id: i64, accept: bool) -> Result<bool> {
        self.observe(
            async {
                let done = sqlx::query("UPDATE users SET accept_invitations = ? WHERE id = ?")
                    .bind(accept)
                    .bind(user_id)
                    .execute(&self.writer)
                    .await?;
                Ok(done.rows_affected() == 1)
            }
            .await,
        )
    }

    /// All users' e-mail addresses (`(user id, address)`), for listings.
    pub async fn user_emails(&self) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query("SELECT id, email FROM users WHERE email IS NOT NULL ORDER BY id")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("email")?)))
            .collect()
    }

    /// The user with this (normalised, lower-case) e-mail address.
    pub async fn user_by_email(&self, email: &str) -> Result<Option<User>> {
        let row = sqlx::query(
            "SELECT id, username, display_name, disabled, is_admin, totp_enabled FROM users WHERE email = ?",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(user_from_row).transpose()
    }

    /// Sets or clears a user's e-mail address. The caller validates and
    /// normalises it; the database enforces lower case and uniqueness
    /// ([`StoreError::AddressTaken`]). Returns `false` if the user does
    /// not exist.
    pub async fn set_user_email(&self, user_id: i64, email: Option<&str>) -> Result<bool> {
        if email.is_some_and(|e| e.len() > MAX_EMAIL_LEN) {
            return Err(StoreError::Limit("address too long"));
        }
        self.observe(
            async {
                let result = sqlx::query("UPDATE users SET email = ? WHERE id = ?")
                    .bind(email)
                    .bind(user_id)
                    .execute(&self.writer)
                    .await;
                match result {
                    Ok(done) => Ok(done.rows_affected() == 1),
                    Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
                        Err(StoreError::AddressTaken)
                    }
                    Err(err) => Err(err.into()),
                }
            }
            .await,
        )
    }

    // --- scheduling objects ----------------------------------------------

    /// The user's calendar object with this UID in one of their writable
    /// calendars. Should there be several (a COPY), the one that already is
    /// a scheduling object wins, then the oldest calendar.
    pub async fn scheduling_object(
        &self,
        owner_id: i64,
        uid: &str,
    ) -> Result<Option<SchedulingObject>> {
        let row = sqlx::query(
            "SELECT c.id, c.owner_id, c.kind, c.slug, c.display_name, c.description, c.color, \
               c.sync_revision, c.components, c.sync_floor, c.token_salt, c.read_only, \
               i.name AS item_name, i.etag, i.content, i.schedule_tag \
             FROM items i JOIN collections c ON c.id = i.collection_id \
             WHERE c.owner_id = ? AND c.kind = 'calendar' AND c.read_only = 0 AND i.uid = ? \
             ORDER BY (i.schedule_tag IS NULL), c.id LIMIT 1",
        )
        .bind(owner_id)
        .bind(uid)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(SchedulingObject {
                collection: collection_from_row(&row)?,
                name: row.try_get("item_name")?,
                etag: row.try_get("etag")?,
                content: row.try_get("content")?,
                schedule_tag: row.try_get("schedule_tag")?,
            })
        })
        .transpose()
    }

    /// An object together with its Schedule-Tag.
    pub async fn item_with_schedule_tag(
        &self,
        collection_id: i64,
        name: &str,
    ) -> Result<Option<(Item, Option<String>)>> {
        let row = sqlx::query(
            "SELECT name, etag, content, schedule_tag FROM items WHERE collection_id = ? AND name = ?",
        )
        .bind(collection_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok((
                Item {
                    name: row.try_get("name")?,
                    etag: row.try_get("etag")?,
                    content: row.try_get("content")?,
                },
                row.try_get("schedule_tag")?,
            ))
        })
        .transpose()
    }

    /// VEVENTs of a calendar whose indexed span overlaps `[start, end)`,
    /// in name order after `after`, at most `limit`.
    pub async fn freebusy_candidates(
        &self,
        collection_id: i64,
        start: i64,
        end: i64,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<FreeBusyCandidate>> {
        let rows = sqlx::query(
            "SELECT name, range_start, range_end FROM items \
             WHERE collection_id = ?1 AND component = 'VEVENT' \
               AND (range_end IS NULL OR range_end > ?2) \
               AND (range_start IS NULL OR range_start < ?3) \
               AND (?4 IS NULL OR name > ?4) \
             ORDER BY name LIMIT ?5",
        )
        .bind(collection_id)
        .bind(start)
        .bind(end)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(FreeBusyCandidate {
                    name: row.try_get("name")?,
                    range_start: row.try_get("range_start")?,
                    range_end: row.try_get("range_end")?,
                })
            })
            .collect()
    }

    // --- scheduling inbox ------------------------------------------------

    /// Revision of the user's inbox, for its `getctag`.
    pub async fn inbox_revision(&self, owner_id: i64) -> Result<i64> {
        let row = sqlx::query("SELECT inbox_revision FROM users WHERE id = ?")
            .bind(owner_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|row| row.try_get("inbox_revision"))
            .transpose()?
            .unwrap_or(0))
    }

    /// Messages in the user's inbox, oldest first, at most `limit`.
    pub async fn inbox_messages(&self, owner_id: i64, limit: u32) -> Result<Vec<InboxMessage>> {
        let rows = sqlx::query(
            "SELECT name, etag, length(content) AS size FROM schedule_inbox \
             WHERE owner_id = ? ORDER BY id LIMIT ?",
        )
        .bind(owner_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ItemMeta {
                    name: row.try_get("name")?,
                    etag: row.try_get("etag")?,
                    size: row.try_get("size")?,
                })
            })
            .collect()
    }

    /// One message of the user's inbox.
    pub async fn inbox_message(&self, owner_id: i64, name: &str) -> Result<Option<Item>> {
        let row = sqlx::query(
            "SELECT name, etag, content FROM schedule_inbox WHERE owner_id = ? AND name = ?",
        )
        .bind(owner_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(Item {
                name: row.try_get("name")?,
                etag: row.try_get("etag")?,
                content: row.try_get("content")?,
            })
        })
        .transpose()
    }

    /// Metadata of one message of the user's inbox.
    pub async fn inbox_message_meta(&self, owner_id: i64, name: &str) -> Result<Option<ItemMeta>> {
        let row = sqlx::query(
            "SELECT name, etag, length(content) AS size FROM schedule_inbox \
             WHERE owner_id = ? AND name = ?",
        )
        .bind(owner_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(ItemMeta {
                name: row.try_get("name")?,
                etag: row.try_get("etag")?,
                size: row.try_get("size")?,
            })
        })
        .transpose()
    }

    /// Deletes a message from the user's inbox, for good (messages never
    /// go to the trash). Returns `false` if it does not exist.
    pub async fn delete_inbox_message(
        &self,
        owner_id: i64,
        name: &str,
        precondition: &Precondition,
    ) -> Result<bool> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let existing: Option<String> =
                    sqlx::query("SELECT etag FROM schedule_inbox WHERE owner_id = ? AND name = ?")
                        .bind(owner_id)
                        .bind(name)
                        .fetch_optional(&mut *tx)
                        .await?
                        .map(|row| row.try_get("etag"))
                        .transpose()?;
                if existing.is_none() {
                    return Ok(false);
                }
                check(precondition, existing.as_deref())?;
                sqlx::query("DELETE FROM schedule_inbox WHERE owner_id = ? AND name = ?")
                    .bind(owner_id)
                    .bind(name)
                    .execute(&mut *tx)
                    .await?;
                bump_inbox(&mut tx, owner_id).await?;
                tx.commit().await?;
                Ok(true)
            }
            .await,
        )
    }

    /// Delivers a message into the user's inbox. An older message with the
    /// same UID and sender is replaced; beyond `max_messages` the oldest
    /// messages are dropped (they are notifications only, the calendars
    /// already have the changes). Fails with [`StoreError::Limit`] if the
    /// message does not fit into the user's storage quota.
    pub async fn deliver_inbox_message(
        &self,
        owner_id: i64,
        message: &NewInboxMessage<'_>,
        max_messages: i64,
    ) -> Result<()> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                sqlx::query("DELETE FROM schedule_inbox WHERE owner_id = ? AND uid = ? AND sender = ?")
                    .bind(owner_id)
                    .bind(message.uid)
                    .bind(message.sender)
                    .execute(&mut *tx)
                    .await?;
                let count: i64 =
                    sqlx::query("SELECT COUNT(*) AS n FROM schedule_inbox WHERE owner_id = ?")
                        .bind(owner_id)
                        .fetch_one(&mut *tx)
                        .await?
                        .try_get("n")?;
                let excess = count.saturating_sub(max_messages.max(1) - 1);
                if excess > 0 {
                    sqlx::query(
                        "DELETE FROM schedule_inbox WHERE id IN \
                         (SELECT id FROM schedule_inbox WHERE owner_id = ? ORDER BY id LIMIT ?)",
                    )
                    .bind(owner_id)
                    .bind(excess)
                    .execute(&mut *tx)
                    .await?;
                }
                let items: i64 = sqlx::query(
                    "SELECT COALESCE(SUM(length(i.content)), 0) AS bytes FROM items i \
                     JOIN collections c ON c.id = i.collection_id WHERE c.owner_id = ?",
                )
                .bind(owner_id)
                .fetch_one(&mut *tx)
                .await?
                .try_get("bytes")?;
                let used = items
                    .saturating_add(crate::trash::trash_bytes(&mut *tx, owner_id).await?)
                    .saturating_add(crate::properties::owner_bytes(&mut *tx, owner_id).await?)
                    .saturating_add(inbox_bytes(&mut *tx, owner_id).await?);
                let size = i64::try_from(message.content.len()).unwrap_or(i64::MAX);
                if used.saturating_add(size) > self.settings.quota_bytes() {
                    return Err(StoreError::Limit("storage quota exceeded"));
                }
                sqlx::query(
                    "INSERT INTO schedule_inbox (owner_id, name, uid, sender, etag, content, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(owner_id)
                .bind(message.name)
                .bind(message.uid)
                .bind(message.sender)
                .bind(message.etag)
                .bind(message.content)
                .bind(message.now)
                .execute(&mut *tx)
                .await?;
                bump_inbox(&mut tx, owner_id).await?;
                tx.commit().await?;
                Ok(())
            }
            .await,
        )
    }
}

async fn bump_inbox(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    owner_id: i64,
) -> Result<()> {
    sqlx::query("UPDATE users SET inbox_revision = inbox_revision + 1 WHERE id = ?")
        .bind(owner_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
