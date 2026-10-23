//! Public read-only links of calendars (`/public/<token>.ics`).
//!
//! The token is a random secret the server generates; only its hash is
//! stored ([`NewPublishLink::token_hash`]), so a copy of the database does
//! not reveal working links. Links of a disabled owner do not resolve.

use sqlx::{Row, sqlite::SqliteRow};

use crate::{Collection, Result, Store, StoreError, collection_from_row};

/// Most links of one calendar.
pub const MAX_LINKS_PER_CALENDAR: i64 = 10;

/// How events with `CLASS:PRIVATE` or `CLASS:CONFIDENTIAL` are published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrivateEvents {
    /// As busy blocks without details.
    #[default]
    Busy,
    /// Not at all.
    Hide,
    /// Like every other event.
    Show,
}

impl PrivateEvents {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Hide => "hide",
            Self::Show => "show",
        }
    }
}

impl std::str::FromStr for PrivateEvents {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, ()> {
        match value {
            "busy" => Ok(Self::Busy),
            "hide" => Ok(Self::Hide),
            "show" => Ok(Self::Show),
            _ => Err(()),
        }
    }
}

/// A stored link, without its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishLink {
    pub id: i64,
    pub public_id: String,
    pub collection_id: i64,
    pub busy_only: bool,
    pub private_events: PrivateEvents,
    pub include_attendees: bool,
    pub created_at: i64,
}

/// A new link ([`Store::create_publish_link`]).
#[derive(Debug, Clone, Copy)]
pub struct NewPublishLink<'a> {
    /// Hex BLAKE3 hash of the token (64 characters).
    pub token_hash: &'a str,
    pub busy_only: bool,
    pub private_events: PrivateEvents,
    pub include_attendees: bool,
    pub now: i64,
}

macro_rules! link_columns {
    () => {
        "l.id, l.public_id, l.collection_id, l.busy_only, l.private_events, \
         l.include_attendees, l.created_at"
    };
}

fn link_from_row(row: &SqliteRow) -> Result<PublishLink> {
    let private: String = row.try_get("private_events")?;
    Ok(PublishLink {
        id: row.try_get("id")?,
        public_id: row.try_get("public_id")?,
        collection_id: row.try_get("collection_id")?,
        busy_only: row.try_get("busy_only")?,
        private_events: private
            .parse()
            .map_err(|()| StoreError::Corrupt(format!("unknown private_events {private:?}")))?,
        include_attendees: row.try_get("include_attendees")?,
        created_at: row.try_get("created_at")?,
    })
}

impl Store {
    /// Creates a link for calendar `collection_id` of `owner_id`. `None` if
    /// the calendar is not the owner's; [`StoreError::Limit`] beyond
    /// [`MAX_LINKS_PER_CALENDAR`].
    pub async fn create_publish_link(
        &self,
        collection_id: i64,
        owner_id: i64,
        link: &NewPublishLink<'_>,
    ) -> Result<Option<PublishLink>> {
        if link.token_hash.len() != 64 {
            return Err(StoreError::Corrupt("invalid token hash".to_owned()));
        }
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let owned = sqlx::query(
                    "SELECT 1 FROM collections WHERE id = ? AND owner_id = ? AND kind = 'calendar'",
                )
                .bind(collection_id)
                .bind(owner_id)
                .fetch_optional(&mut *tx)
                .await?
                .is_some();
                if !owned {
                    return Ok(None);
                }
                let count: i64 =
                    sqlx::query("SELECT COUNT(*) AS n FROM publish_links WHERE collection_id = ?")
                        .bind(collection_id)
                        .fetch_one(&mut *tx)
                        .await?
                        .try_get("n")?;
                if count >= MAX_LINKS_PER_CALENDAR {
                    return Err(StoreError::Limit("too many links for this calendar"));
                }
                let row = sqlx::query(
                    "INSERT INTO publish_links (public_id, collection_id, token_hash, busy_only, \
                       private_events, include_attendees, created_at) \
                     VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ?, ?) \
                     RETURNING id, public_id, collection_id, busy_only, private_events, \
                       include_attendees, created_at",
                )
                .bind(collection_id)
                .bind(link.token_hash)
                .bind(link.busy_only)
                .bind(link.private_events.as_str())
                .bind(link.include_attendees)
                .bind(link.now)
                .fetch_one(&mut *tx)
                .await?;
                let created = link_from_row(&row)?;
                tx.commit().await?;
                Ok(Some(created))
            }
            .await,
        )
    }

    /// The links of a calendar.
    pub async fn publish_links(&self, collection_id: i64) -> Result<Vec<PublishLink>> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            link_columns!(),
            " FROM publish_links l WHERE l.collection_id = ? ORDER BY l.created_at, l.id"
        ))
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(link_from_row).collect()
    }

    /// Revokes a link of calendar `collection_id`. Returns whether it
    /// existed.
    pub async fn delete_publish_link(&self, collection_id: i64, public_id: &str) -> Result<bool> {
        self.observe(
            async {
                let done = sqlx::query(
                    "DELETE FROM publish_links WHERE collection_id = ? AND public_id = ?",
                )
                .bind(collection_id)
                .bind(public_id)
                .execute(&self.writer)
                .await?;
                Ok(done.rows_affected() == 1)
            }
            .await,
        )
    }

    /// The link with token hash `token_hash`, its stored hash (for a
    /// constant-time comparison by the caller) and its calendar, if the
    /// owner is enabled.
    pub async fn published_calendar(
        &self,
        token_hash: &str,
    ) -> Result<Option<(PublishLink, String, Collection)>> {
        let row = sqlx::query(concat!(
            "SELECT ",
            link_columns!(),
            ", l.token_hash, c.owner_id, c.kind, c.slug, c.display_name, \
               c.description, c.color, c.sync_revision, c.components, c.sync_floor, c.token_salt, \
               c.read_only \
             FROM publish_links l JOIN collections c ON c.id = l.collection_id \
               JOIN users o ON o.id = c.owner_id \
             WHERE l.token_hash = ? AND o.disabled = 0"
        ))
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let link = link_from_row(&row)?;
        // `collection_from_row` reads `id`, which here is the link's.
        let mut collection = collection_from_row(&row)?;
        collection.id = link.collection_id;
        Ok(Some((link, row.try_get("token_hash")?, collection)))
    }
}

#[cfg(test)]
mod tests;
