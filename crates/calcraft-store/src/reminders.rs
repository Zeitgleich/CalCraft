//! E-mail reminders and the outgoing mail queue.
//!
//! * **Index** (`reminder_cursors`, `reminder_index`): per calendar the
//!   revision up to which its objects were looked at, per object the next
//!   moment its alarms need attention. Every write path already bumps
//!   `items.revision`, so changed objects are found without hooks in PUT,
//!   MOVE, COPY, restore, imports or scheduling; deleted objects leave the
//!   index through the foreign key. The server computes the alarm times
//!   (this crate does not parse iCalendar).
//! * **Queue** (`mail_outbox`): complete messages to send, with retries.
//!   A reminder is queued in the same transaction that moves its object's
//!   `scan_from` past the trigger ([`Store::complete_reminder`]), and its
//!   `dedupe_key` is unique, so no restart sends it twice or loses it.
//!   Per-user and server-wide limits are checked in that transaction.

use sqlx::Row;

use crate::{Result, Store, StoreError};

/// How long finished outbox rows are kept (limits, status).
pub const MAIL_RETENTION_SECS: i64 = 7 * 86_400;
/// Largest queued message.
pub const MAX_MAIL_BYTES: usize = 64 * 1024;
/// Dropped mails recorded per user and day; further drops are not stored.
pub const MAX_DROPPED_PER_DAY: i64 = 1000;

/// Which alarms send reminders, as stored per user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderMode {
    Off,
    /// Only `ACTION:EMAIL` alarms.
    Email,
    /// All alarms (`EMAIL`, `DISPLAY`, `AUDIO`).
    All,
}

impl ReminderMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Email => "email",
            Self::All => "all",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "email" => Some(Self::Email),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

fn mode_column(value: Option<String>) -> Result<Option<ReminderMode>> {
    value
        .map(|v| {
            ReminderMode::parse(&v)
                .ok_or_else(|| StoreError::Corrupt(format!("unknown reminder mode {v:?}")))
        })
        .transpose()
}

/// A user's reminder settings; `mode` `None` is the server default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReminderSettings {
    pub mode: Option<ReminderMode>,
    pub include_description: bool,
}

/// A calendar whose objects changed since they were last indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyCalendar {
    pub collection_id: i64,
    pub owner_id: i64,
    /// Objects with a higher revision need indexing (-1: all of them).
    pub indexed_revision: i64,
    /// The collection's current revision: the new cursor once done.
    pub revision: i64,
    pub mode: Option<ReminderMode>,
    pub owner_disabled: bool,
}

/// An object to (re)index, with its previous index state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexCandidate {
    pub item_id: i64,
    pub etag: String,
    pub content: Vec<u8>,
    /// `scan_from` of its index entry, if it has one.
    pub scan_from: Option<i64>,
}

/// The computed index state of one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry<'a> {
    pub item_id: i64,
    /// The content's ETag; nothing is written if the object changed since.
    pub etag: &'a str,
    pub scan_from: i64,
    pub next_due: Option<i64>,
}

/// An object whose alarms are due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueReminder {
    pub item_id: i64,
    pub etag: String,
    pub uid: String,
    pub content: Vec<u8>,
    pub scan_from: i64,
    pub collection_id: i64,
    pub collection_name: Option<String>,
    pub collection_slug: String,
    pub owner_id: i64,
    pub owner_disabled: bool,
    pub mode: Option<ReminderMode>,
    pub include_description: bool,
}

/// The new index state of an object whose due alarms were handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion<'a> {
    pub item_id: i64,
    /// ETag the alarms were computed from.
    pub etag: &'a str,
    /// Alarms up to this moment are done.
    pub scan_to: i64,
    pub next_due: Option<i64>,
}

/// Kind of a queued mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailKind {
    Reminder,
    Test,
}

impl MailKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reminder => "reminder",
            Self::Test => "test",
        }
    }
}

/// A mail to queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMail {
    pub user_id: i64,
    pub kind: MailKind,
    pub item_id: Option<i64>,
    pub dedupe_key: Option<String>,
    pub recipient: String,
    pub message: Vec<u8>,
    pub expires_at: i64,
}

/// Limits checked when queueing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailLimits {
    pub per_user_hour: i64,
    pub per_user_day: i64,
    /// Pending mails of all users together.
    pub max_pending: i64,
}

/// What happened to the mails of one queueing call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueOutcome {
    pub queued: u32,
    /// Recorded as dropped (a limit or the full queue).
    pub dropped: u32,
    /// Already queued earlier (same dedupe key).
    pub duplicates: u32,
}

/// A pending mail ready to be sent.
#[derive(Clone, PartialEq, Eq)]
pub struct OutgoingMail {
    pub id: i64,
    pub user_id: i64,
    pub kind: MailKind,
    /// A reminder whose object was deleted meanwhile.
    pub orphaned: bool,
    pub recipient: String,
    pub message: Vec<u8>,
    pub attempts: i64,
    pub expires_at: i64,
}

impl std::fmt::Debug for OutgoingMail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutgoingMail")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field("kind", &self.kind)
            .field("attempts", &self.attempts)
            .finish_non_exhaustive()
    }
}

/// Counts of outbox rows (created since some moment), per status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailCounts {
    pub pending: i64,
    pub sent: i64,
    pub failed: i64,
    pub dropped: i64,
    pub cancelled: i64,
    /// Error code and time of the newest failed or dropped mail.
    pub last_error: Option<(String, i64)>,
}

type Tx = sqlx::Transaction<'static, sqlx::Sqlite>;

/// Mails of a user created since `since` that count towards the limits.
async fn user_mails_since(tx: &mut Tx, user_id: i64, since: i64) -> Result<i64> {
    Ok(sqlx::query(
        "SELECT COUNT(*) AS n FROM mail_outbox \
         WHERE user_id = ? AND created_at > ? AND status IN ('pending', 'sent', 'failed')",
    )
    .bind(user_id)
    .bind(since)
    .fetch_one(&mut **tx)
    .await?
    .try_get("n")?)
}

async fn insert_mails(
    tx: &mut Tx,
    mails: &[NewMail],
    limits: MailLimits,
    now: i64,
) -> Result<QueueOutcome> {
    let mut outcome = QueueOutcome::default();
    for mail in mails {
        if mail.message.len() > MAX_MAIL_BYTES {
            return Err(StoreError::Limit("mail too large"));
        }
        if let Some(key) = &mail.dedupe_key {
            let exists = sqlx::query("SELECT 1 FROM mail_outbox WHERE dedupe_key = ?")
                .bind(key)
                .fetch_optional(&mut **tx)
                .await?
                .is_some();
            if exists {
                outcome.duplicates += 1;
                continue;
            }
        }
        let pending: i64 =
            sqlx::query("SELECT COUNT(*) AS n FROM mail_outbox WHERE status = 'pending'")
                .fetch_one(&mut **tx)
                .await?
                .try_get("n")?;
        let reason = if pending >= limits.max_pending {
            Some("queue_full")
        } else if user_mails_since(tx, mail.user_id, now - 3600).await? >= limits.per_user_hour
            || user_mails_since(tx, mail.user_id, now - 86_400).await? >= limits.per_user_day
        {
            Some("limit")
        } else {
            None
        };
        match reason {
            None => {
                sqlx::query(
                    "INSERT INTO mail_outbox (user_id, kind, item_id, dedupe_key, recipient, \
                       message, status, next_attempt_at, expires_at, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?)",
                )
                .bind(mail.user_id)
                .bind(mail.kind.as_str())
                .bind(mail.item_id)
                .bind(&mail.dedupe_key)
                .bind(&mail.recipient)
                .bind(&mail.message)
                .bind(now)
                .bind(mail.expires_at)
                .bind(now)
                .execute(&mut **tx)
                .await?;
                outcome.queued += 1;
            }
            Some(reason) => {
                outcome.dropped += 1;
                // A runaway series (an alarm every minute) must not fill the
                // table with records: beyond a bound, drops are only counted.
                let recorded: i64 = sqlx::query(
                    "SELECT COUNT(*) AS n FROM mail_outbox \
                     WHERE user_id = ? AND created_at > ? AND status = 'dropped'",
                )
                .bind(mail.user_id)
                .bind(now - 86_400)
                .fetch_one(&mut **tx)
                .await?
                .try_get("n")?;
                if recorded >= MAX_DROPPED_PER_DAY {
                    continue;
                }
                // Kept without recipient and message: the user sees that a
                // reminder was not sent, and the key is never queued again.
                sqlx::query(
                    "INSERT INTO mail_outbox (user_id, kind, item_id, dedupe_key, status, error, \
                       next_attempt_at, expires_at, created_at, done_at) \
                     VALUES (?, ?, ?, ?, 'dropped', ?, ?, ?, ?, ?)",
                )
                .bind(mail.user_id)
                .bind(mail.kind.as_str())
                .bind(mail.item_id)
                .bind(&mail.dedupe_key)
                .bind(reason)
                .bind(now)
                .bind(mail.expires_at)
                .bind(now)
                .bind(now)
                .execute(&mut **tx)
                .await?;
            }
        }
    }
    Ok(outcome)
}

fn kind_column(value: &str) -> Result<MailKind> {
    match value {
        "reminder" => Ok(MailKind::Reminder),
        "test" => Ok(MailKind::Test),
        other => Err(StoreError::Corrupt(format!("unknown mail kind {other:?}"))),
    }
}

impl Store {
    /// The user's reminder settings.
    pub async fn reminder_settings(&self, user_id: i64) -> Result<Option<ReminderSettings>> {
        let row = sqlx::query("SELECT reminder_mode, reminder_description FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            Ok(ReminderSettings {
                mode: mode_column(row.try_get("reminder_mode")?)?,
                include_description: row.try_get("reminder_description")?,
            })
        })
        .transpose()
    }

    /// Changes the user's reminder settings (fields that are `Some`; a
    /// `mode` of `Some(None)` returns to the server default). The user's
    /// calendars are indexed again, so the new mode applies to all of them.
    pub async fn set_reminder_settings(
        &self,
        user_id: i64,
        mode: Option<Option<ReminderMode>>,
        include_description: Option<bool>,
    ) -> Result<bool> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let exists = sqlx::query("SELECT 1 FROM users WHERE id = ?")
                    .bind(user_id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .is_some();
                if !exists {
                    return Ok(false);
                }
                if let Some(mode) = mode {
                    sqlx::query("UPDATE users SET reminder_mode = ? WHERE id = ?")
                        .bind(mode.map(ReminderMode::as_str))
                        .bind(user_id)
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query(
                        "DELETE FROM reminder_cursors WHERE collection_id IN \
                         (SELECT id FROM collections WHERE owner_id = ?)",
                    )
                    .bind(user_id)
                    .execute(&mut *tx)
                    .await?;
                }
                if let Some(include) = include_description {
                    sqlx::query("UPDATE users SET reminder_description = ? WHERE id = ?")
                        .bind(include)
                        .bind(user_id)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?;
                Ok(true)
            }
            .await,
        )
    }

    /// Records the server default mode. If it differs from the one the
    /// index was built with, every calendar is indexed again (returns
    /// `true`).
    pub async fn sync_reminder_default(&self, mode: ReminderMode) -> Result<bool> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let stored: Option<String> =
                    sqlx::query_scalar("SELECT default_mode FROM reminder_state WHERE id = 1")
                        .fetch_optional(&mut *tx)
                        .await?;
                if stored.as_deref() == Some(mode.as_str()) {
                    return Ok(false);
                }
                sqlx::query(
                    "INSERT INTO reminder_state (id, default_mode) VALUES (1, ?) \
                     ON CONFLICT (id) DO UPDATE SET default_mode = excluded.default_mode",
                )
                .bind(mode.as_str())
                .execute(&mut *tx)
                .await?;
                let changed = stored.is_some();
                if changed {
                    sqlx::query("DELETE FROM reminder_cursors")
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?;
                Ok(changed)
            }
            .await,
        )
    }

    /// Calendars (no subscriptions, no address books) whose objects
    /// changed since they were indexed.
    pub async fn reminder_dirty_calendars(&self, limit: u32) -> Result<Vec<DirtyCalendar>> {
        let rows = sqlx::query(
            "SELECT c.id, c.owner_id, c.sync_revision, COALESCE(r.revision, -1) AS indexed, \
               u.reminder_mode, u.disabled \
             FROM collections c JOIN users u ON u.id = c.owner_id \
             LEFT JOIN reminder_cursors r ON r.collection_id = c.id \
             WHERE c.kind = 'calendar' AND c.read_only = 0 \
               AND (r.revision IS NULL OR r.revision < c.sync_revision) \
             ORDER BY c.id LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(DirtyCalendar {
                    collection_id: row.try_get("id")?,
                    owner_id: row.try_get("owner_id")?,
                    indexed_revision: row.try_get("indexed")?,
                    revision: row.try_get("sync_revision")?,
                    mode: mode_column(row.try_get("reminder_mode")?)?,
                    owner_disabled: row.try_get("disabled")?,
                })
            })
            .collect()
    }

    /// Objects of a calendar changed after `revision`, by id after
    /// `after_id`, in batches.
    pub async fn reminder_candidates(
        &self,
        collection_id: i64,
        revision: i64,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<IndexCandidate>> {
        let rows = sqlx::query(
            "SELECT i.id, i.etag, i.content, x.scan_from FROM items i \
             LEFT JOIN reminder_index x ON x.item_id = i.id \
             WHERE i.collection_id = ? AND i.revision > ? AND i.id > ? \
             ORDER BY i.id LIMIT ?",
        )
        .bind(collection_id)
        .bind(revision)
        .bind(after_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(IndexCandidate {
                    item_id: row.try_get("id")?,
                    etag: row.try_get("etag")?,
                    content: row.try_get("content")?,
                    scan_from: row.try_get("scan_from")?,
                })
            })
            .collect()
    }

    /// Stores computed index entries; entries of objects whose ETag
    /// changed meanwhile are skipped (they are indexed again). `scan_from`
    /// never moves backwards.
    pub async fn set_reminder_index(&self, entries: &[IndexEntry<'_>]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                for entry in entries {
                    sqlx::query(
                        "INSERT INTO reminder_index (item_id, etag, scan_from, next_due) \
                         SELECT id, etag, ?, ? FROM items WHERE id = ? AND etag = ? \
                         ON CONFLICT (item_id) DO UPDATE SET etag = excluded.etag, \
                           scan_from = max(scan_from, excluded.scan_from), \
                           next_due = excluded.next_due",
                    )
                    .bind(entry.scan_from)
                    .bind(entry.next_due)
                    .bind(entry.item_id)
                    .bind(entry.etag)
                    .execute(&mut *tx)
                    .await?;
                }
                tx.commit().await?;
                Ok(())
            }
            .await,
        )
    }

    /// Removes the index entries of a calendar's objects (reminders off
    /// for its owner).
    pub async fn clear_reminder_index(&self, collection_id: i64) -> Result<()> {
        self.observe(
            async {
                sqlx::query(
                    "DELETE FROM reminder_index WHERE item_id IN \
                     (SELECT id FROM items WHERE collection_id = ?)",
                )
                .bind(collection_id)
                .execute(&self.writer)
                .await?;
                Ok(())
            }
            .await,
        )
    }

    /// Records that a calendar is indexed up to `revision`.
    pub async fn set_reminder_cursor(&self, collection_id: i64, revision: i64) -> Result<()> {
        self.observe(
            async {
                sqlx::query(
                    "INSERT INTO reminder_cursors (collection_id, revision) \
                     SELECT id, ? FROM collections WHERE id = ? \
                     ON CONFLICT (collection_id) DO UPDATE SET \
                       revision = max(revision, excluded.revision)",
                )
                .bind(revision)
                .bind(collection_id)
                .execute(&self.writer)
                .await?;
                Ok(())
            }
            .await,
        )
    }

    /// Objects whose `next_due` has come, earliest first.
    pub async fn due_reminders(&self, now: i64, limit: u32) -> Result<Vec<DueReminder>> {
        let rows = sqlx::query(
            "SELECT x.item_id, x.etag, x.scan_from, i.uid, i.content, c.id AS collection_id, \
               c.display_name, c.slug, c.owner_id, u.disabled, u.reminder_mode, \
               u.reminder_description \
             FROM reminder_index x JOIN items i ON i.id = x.item_id \
             JOIN collections c ON c.id = i.collection_id JOIN users u ON u.id = c.owner_id \
             WHERE x.next_due IS NOT NULL AND x.next_due <= ? \
             ORDER BY x.next_due, x.item_id LIMIT ?",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(DueReminder {
                    item_id: row.try_get("item_id")?,
                    etag: row.try_get("etag")?,
                    uid: row.try_get("uid")?,
                    content: row.try_get("content")?,
                    scan_from: row.try_get("scan_from")?,
                    collection_id: row.try_get("collection_id")?,
                    collection_name: row.try_get("display_name")?,
                    collection_slug: row.try_get("slug")?,
                    owner_id: row.try_get("owner_id")?,
                    owner_disabled: row.try_get("disabled")?,
                    mode: mode_column(row.try_get("reminder_mode")?)?,
                    include_description: row.try_get("reminder_description")?,
                })
            })
            .collect()
    }

    /// Finishes the due alarms of one object: queues `mails` (within the
    /// limits) and moves its index entry to `scan_to` / `next_due`, in one
    /// transaction. If the object changed since it was read (other ETag),
    /// nothing is queued and the entry is left for re-indexing.
    pub async fn complete_reminder(
        &self,
        done: &Completion<'_>,
        mails: &[NewMail],
        limits: MailLimits,
        now: i64,
    ) -> Result<Option<QueueOutcome>> {
        let Completion {
            item_id,
            etag,
            scan_to,
            next_due,
        } = *done;
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let current = sqlx::query(
                    "SELECT 1 FROM reminder_index x JOIN items i ON i.id = x.item_id \
                     WHERE x.item_id = ? AND x.etag = ? AND i.etag = ?",
                )
                .bind(item_id)
                .bind(etag)
                .bind(etag)
                .fetch_optional(&mut *tx)
                .await?
                .is_some();
                if !current {
                    return Ok(None);
                }
                let outcome = insert_mails(&mut tx, mails, limits, now).await?;
                sqlx::query(
                    "UPDATE reminder_index SET scan_from = max(scan_from, ?), next_due = ? \
                     WHERE item_id = ?",
                )
                .bind(scan_to)
                .bind(next_due)
                .bind(item_id)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(Some(outcome))
            }
            .await,
        )
    }

    /// Queues mails outside the reminder index (test mails).
    pub async fn queue_mails(
        &self,
        mails: &[NewMail],
        limits: MailLimits,
        now: i64,
    ) -> Result<QueueOutcome> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let outcome = insert_mails(&mut tx, mails, limits, now).await?;
                tx.commit().await?;
                Ok(outcome)
            }
            .await,
        )
    }

    /// Pending mails whose next attempt has come, oldest first.
    pub async fn pending_mails(&self, now: i64, limit: u32) -> Result<Vec<OutgoingMail>> {
        let rows = sqlx::query(
            "SELECT id, user_id, kind, item_id, recipient, message, attempts, expires_at \
             FROM mail_outbox WHERE status = 'pending' AND next_attempt_at <= ? \
             ORDER BY next_attempt_at, id LIMIT ?",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let kind = kind_column(row.try_get("kind")?)?;
                let item_id: Option<i64> = row.try_get("item_id")?;
                Ok(OutgoingMail {
                    id: row.try_get("id")?,
                    user_id: row.try_get("user_id")?,
                    kind,
                    orphaned: kind == MailKind::Reminder && item_id.is_none(),
                    recipient: row
                        .try_get::<Option<String>, _>("recipient")?
                        .unwrap_or_default(),
                    message: row.try_get("message")?,
                    attempts: row.try_get("attempts")?,
                    expires_at: row.try_get("expires_at")?,
                })
            })
            .collect()
    }

    /// Ends a pending mail: `sent`, `failed` or `cancelled`, with an error
    /// code for the last two. Recipient and message are removed.
    pub async fn finish_mail(
        &self,
        id: i64,
        status: &'static str,
        error: Option<&str>,
        now: i64,
    ) -> Result<()> {
        debug_assert!(matches!(status, "sent" | "failed" | "cancelled"));
        self.observe(
            async {
                sqlx::query(
                    "UPDATE mail_outbox SET status = ?, error = ?, done_at = ?, \
                       recipient = NULL, message = NULL, attempts = attempts + ? \
                     WHERE id = ? AND status = 'pending'",
                )
                .bind(status)
                .bind(error)
                .bind(now)
                .bind(i64::from(status != "cancelled"))
                .bind(id)
                .execute(&self.writer)
                .await?;
                Ok(())
            }
            .await,
        )
    }

    /// A failed attempt to be retried at `next_attempt_at`.
    pub async fn retry_mail(&self, id: i64, error: &str, next_attempt_at: i64) -> Result<()> {
        self.observe(
            async {
                sqlx::query(
                    "UPDATE mail_outbox SET attempts = attempts + 1, error = ?, next_attempt_at = ? \
                     WHERE id = ? AND status = 'pending'",
                )
                .bind(error)
                .bind(next_attempt_at)
                .bind(id)
                .execute(&self.writer)
                .await?;
                Ok(())
            }
            .await,
        )
    }

    /// Gives up expired pending mails and deletes finished rows older than
    /// [`MAIL_RETENTION_SECS`]. Returns `(expired, deleted)`.
    pub async fn prune_mails(&self, now: i64) -> Result<(u64, u64)> {
        self.observe(
            async {
                let expired = sqlx::query(
                    "UPDATE mail_outbox SET status = 'failed', error = 'expired', done_at = ?, \
                       recipient = NULL, message = NULL \
                     WHERE status = 'pending' AND expires_at <= ?",
                )
                .bind(now)
                .bind(now)
                .execute(&self.writer)
                .await?
                .rows_affected();
                let deleted = sqlx::query(
                    "DELETE FROM mail_outbox WHERE status <> 'pending' AND created_at < ?",
                )
                .bind(now - MAIL_RETENTION_SECS)
                .execute(&self.writer)
                .await?
                .rows_affected();
                Ok((expired, deleted))
            }
            .await,
        )
    }

    /// Outbox counts created after `since`, of one user or (`None`) all.
    pub async fn mail_counts(&self, user_id: Option<i64>, since: i64) -> Result<MailCounts> {
        let rows = sqlx::query(
            "SELECT status, COUNT(*) AS n FROM mail_outbox \
             WHERE (? IS NULL OR user_id = ?) AND (created_at > ? OR status = 'pending') \
             GROUP BY status",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        let mut counts = MailCounts::default();
        for row in &rows {
            let n: i64 = row.try_get("n")?;
            match row.try_get::<String, _>("status")?.as_str() {
                "pending" => counts.pending = n,
                "sent" => counts.sent = n,
                "failed" => counts.failed = n,
                "dropped" => counts.dropped = n,
                "cancelled" => counts.cancelled = n,
                _ => {}
            }
        }
        let last = sqlx::query(
            "SELECT error, COALESCE(done_at, created_at) AS at FROM mail_outbox \
             WHERE (? IS NULL OR user_id = ?) AND created_at > ? AND error IS NOT NULL \
               AND status IN ('failed', 'dropped', 'pending') \
             ORDER BY at DESC, id DESC LIMIT 1",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(since)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = last {
            counts.last_error = Some((row.try_get("error")?, row.try_get("at")?));
        }
        Ok(counts)
    }

    /// Objects in the reminder index with a pending trigger.
    pub async fn scheduled_reminders(&self, user_id: Option<i64>) -> Result<i64> {
        Ok(sqlx::query(
            "SELECT COUNT(*) AS n FROM reminder_index x JOIN items i ON i.id = x.item_id \
             JOIN collections c ON c.id = i.collection_id \
             WHERE x.next_due IS NOT NULL AND (? IS NULL OR c.owner_id = ?)",
        )
        .bind(user_id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await?
        .try_get("n")?)
    }
}

#[cfg(test)]
mod tests;
