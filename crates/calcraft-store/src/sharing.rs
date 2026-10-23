//! Sharing of calendars and address books with other users.
//!
//! The owner shares a collection with another user (the sharee) for
//! reading or for reading and writing. A share is pending until the sharee
//! accepts it; then the collection appears in the sharee's home under a
//! slug of their own ([`Store::shared_collection`]). Every query here
//! ignores shares whose owner or sharee is disabled, so disabling a user
//! makes their shares disappear at once (re-enabling brings them back).
//!
//! Writes by a sharee go through the `*_shared` variants of the item
//! writes ([`Store::put_item_shared`] and friends), which check inside the
//! write transaction that the share still grants write access
//! ([`ensure_share`]): a share removed while a request is in flight cannot
//! be written through any more.

use calcraft_core::{CollectionKind, Slug, Username};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{Collection, Result, Store, StoreError, collection_from_row};

/// Most shares (pending and accepted) of one collection.
pub const MAX_SHARES_PER_COLLECTION: i64 = 100;
/// Most pending invitations one user can have; further shares with them
/// are refused until they accept or decline some.
pub const MAX_PENDING_SHARES_PER_USER: i64 = 100;

/// What a sharee may do with a shared collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShareAccess {
    Read,
    ReadWrite,
}

impl ShareAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadWrite => "read-write",
        }
    }

    pub fn writable(self) -> bool {
        self == Self::ReadWrite
    }
}

impl std::str::FromStr for ShareAccess {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, ()> {
        match value {
            "read" => Ok(Self::Read),
            "read-write" => Ok(Self::ReadWrite),
            _ => Err(()),
        }
    }
}

/// How a sharee sees a collection: attached to [`Collection::shared`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedVia {
    pub share_id: i64,
    pub public_id: String,
    pub access: ShareAccess,
    pub owner: Username,
    pub owner_display_name: Option<String>,
    /// The collection's slug in the owner's home.
    pub owner_slug: Slug,
    /// The sharee's position of the collection (Apple's `calendar-order`).
    pub order: Option<i64>,
}

/// One share, as the owner, the sharee or an administrator sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub id: i64,
    pub public_id: String,
    pub collection_id: i64,
    pub kind: CollectionKind,
    pub owner_id: i64,
    pub owner: Username,
    pub owner_display_name: Option<String>,
    pub owner_slug: Slug,
    /// The owner's name of the collection.
    pub collection_display_name: Option<String>,
    pub components: String,
    pub read_only_collection: bool,
    pub sharee_id: i64,
    pub sharee: Username,
    pub sharee_display_name: Option<String>,
    pub access: ShareAccess,
    /// The slug in the sharee's home; `None` while pending.
    pub slug: Option<Slug>,
    pub created_at: i64,
    pub accepted_at: Option<i64>,
}

impl Share {
    pub fn accepted(&self) -> bool {
        self.accepted_at.is_some()
    }
}

/// Result of [`Store::set_share`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetShare {
    /// A new, pending share.
    Created(Share),
    /// The access level of an existing share changed (or stayed).
    Updated(Share),
}

impl SetShare {
    pub fn share(&self) -> &Share {
        match self {
            Self::Created(share) | Self::Updated(share) => share,
        }
    }
}

/// The sharee's own view of a shared collection; `None` keeps a value,
/// `Some(None)` clears it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShareView<'a> {
    pub display_name: Option<Option<&'a str>>,
    pub color: Option<Option<&'a str>>,
    pub order: Option<Option<i64>>,
}

/// `SELECT` of a [`Share`] from `shares s`, `collections c`, `users o`
/// (owner) and `users u` (sharee); a `WHERE` clause follows.
macro_rules! share_select {
    () => {
        "SELECT s.id, s.public_id, s.collection_id, s.kind, s.access, s.slug, \
           s.created_at, s.accepted_at, s.sharee_id, c.owner_id, c.slug AS owner_slug, \
           c.display_name AS collection_display_name, c.components, c.read_only, \
           o.username AS owner_name, o.display_name AS owner_display_name, \
           u.username AS sharee_name, u.display_name AS sharee_display_name \
         FROM shares s JOIN collections c ON c.id = s.collection_id \
           JOIN users o ON o.id = c.owner_id JOIN users u ON u.id = s.sharee_id "
    };
}

/// Both users enabled.
macro_rules! active {
    () => {
        " AND o.disabled = 0 AND u.disabled = 0 "
    };
}

/// A [`Collection`] seen through an accepted share of sharee `?1` in their
/// home of kind `?2` (see [`collection_from_row`]) plus the [`SharedVia`]
/// columns.
macro_rules! shared_collection_select {
    () => {
        "SELECT c.id, c.owner_id, c.kind, s.slug, \
           COALESCE(s.display_name, c.display_name) AS display_name, c.description, \
           COALESCE(s.color, c.color) AS color, c.sync_revision, c.components, c.sync_floor, \
           c.token_salt, (c.read_only OR s.access = 'read') AS read_only, \
           s.id AS share_id, s.public_id, s.access, s.sort_order, c.slug AS owner_slug, \
           o.username AS owner_name, o.display_name AS owner_display_name \
         FROM shares s JOIN collections c ON c.id = s.collection_id \
           JOIN users o ON o.id = c.owner_id JOIN users u ON u.id = s.sharee_id \
         WHERE s.sharee_id = ?1 AND s.kind = ?2 AND s.accepted_at IS NOT NULL \
           AND o.disabled = 0 AND u.disabled = 0 "
    };
}

fn corrupt(err: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(err.to_string())
}

fn access_from_row(row: &SqliteRow) -> Result<ShareAccess> {
    let access: String = row.try_get("access")?;
    access
        .parse()
        .map_err(|()| corrupt(format!("unknown share access {access:?}")))
}

fn share_from_row(row: &SqliteRow) -> Result<Share> {
    let kind: String = row.try_get("kind")?;
    let slug: Option<String> = row.try_get("slug")?;
    Ok(Share {
        id: row.try_get("id")?,
        public_id: row.try_get("public_id")?,
        collection_id: row.try_get("collection_id")?,
        kind: kind
            .parse()
            .map_err(|()| corrupt(format!("unknown collection kind {kind:?}")))?,
        owner_id: row.try_get("owner_id")?,
        owner: row
            .try_get::<String, _>("owner_name")?
            .try_into()
            .map_err(corrupt)?,
        owner_display_name: row.try_get("owner_display_name")?,
        owner_slug: row
            .try_get::<String, _>("owner_slug")?
            .try_into()
            .map_err(corrupt)?,
        collection_display_name: row.try_get("collection_display_name")?,
        components: row.try_get("components")?,
        read_only_collection: row.try_get("read_only")?,
        sharee_id: row.try_get("sharee_id")?,
        sharee: row
            .try_get::<String, _>("sharee_name")?
            .try_into()
            .map_err(corrupt)?,
        sharee_display_name: row.try_get("sharee_display_name")?,
        access: access_from_row(row)?,
        slug: slug.map(Slug::try_from).transpose().map_err(corrupt)?,
        created_at: row.try_get("created_at")?,
        accepted_at: row.try_get("accepted_at")?,
    })
}

fn shared_collection_from_row(row: &SqliteRow) -> Result<Collection> {
    let mut collection = collection_from_row(row)?;
    collection.shared = Some(SharedVia {
        share_id: row.try_get("share_id")?,
        public_id: row.try_get("public_id")?,
        access: access_from_row(row)?,
        owner: row
            .try_get::<String, _>("owner_name")?
            .try_into()
            .map_err(corrupt)?,
        owner_display_name: row.try_get("owner_display_name")?,
        owner_slug: row
            .try_get::<String, _>("owner_slug")?
            .try_into()
            .map_err(corrupt)?,
        order: row.try_get("sort_order")?,
    });
    Ok(collection)
}

/// Whether `slug` is taken in `user_id`'s home of `kind` by an accepted
/// share. Own collections are checked by their unique index.
pub(crate) async fn slug_taken_by_share(
    tx: &mut Transaction<'static, Sqlite>,
    user_id: i64,
    kind: CollectionKind,
    slug: &str,
) -> Result<bool> {
    Ok(
        sqlx::query("SELECT 1 FROM shares WHERE sharee_id = ? AND kind = ? AND slug = ?")
            .bind(user_id)
            .bind(kind.as_str())
            .bind(slug)
            .fetch_optional(&mut **tx)
            .await?
            .is_some(),
    )
}

/// Inside a sharee's write transaction: share `share_id` must still be an
/// accepted share of `collection_id` with both users enabled and, with
/// `write`, grant write access. Otherwise [`StoreError::ShareRevoked`].
pub(crate) async fn ensure_share(
    tx: &mut Transaction<'static, Sqlite>,
    share_id: i64,
    collection_id: i64,
    write: bool,
) -> Result<()> {
    let row = sqlx::query(
        "SELECT s.access FROM shares s JOIN collections c ON c.id = s.collection_id \
           JOIN users o ON o.id = c.owner_id JOIN users u ON u.id = s.sharee_id \
         WHERE s.id = ? AND s.collection_id = ? AND s.accepted_at IS NOT NULL \
           AND o.disabled = 0 AND u.disabled = 0",
    )
    .bind(share_id)
    .bind(collection_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Err(StoreError::ShareRevoked);
    };
    if write && !access_from_row(&row)?.writable() {
        return Err(StoreError::ShareRevoked);
    }
    Ok(())
}

/// A free slug for an accepted share in the sharee's home: `<owner>-<slug>`
/// (shortened to the slug length), else with a number appended.
async fn free_slug(
    tx: &mut Transaction<'static, Sqlite>,
    sharee_id: i64,
    kind: CollectionKind,
    owner: &str,
    owner_slug: &str,
) -> Result<Slug> {
    let base: String = format!("{owner}-{owner_slug}")
        .chars()
        .take(Slug::MAX_LEN - 4)
        .collect();
    for n in 1..=999 {
        let candidate = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let Ok(slug) = candidate.parse::<Slug>() else {
            continue;
        };
        let own =
            sqlx::query("SELECT 1 FROM collections WHERE owner_id = ? AND kind = ? AND slug = ?")
                .bind(sharee_id)
                .bind(kind.as_str())
                .bind(slug.as_str())
                .fetch_optional(&mut **tx)
                .await?
                .is_some();
        if !own && !slug_taken_by_share(tx, sharee_id, kind, slug.as_str()).await? {
            return Ok(slug);
        }
    }
    Err(StoreError::Limit("no free name for the shared collection"))
}

impl Store {
    /// Marks a collection read-only like a subscription, for tests
    /// (feature `test-hooks`).
    #[cfg(feature = "test-hooks")]
    pub async fn mark_read_only_for_tests(&self, collection_id: i64) -> Result<()> {
        sqlx::query("UPDATE collections SET read_only = 1 WHERE id = ?")
            .bind(collection_id)
            .execute(&self.writer)
            .await?;
        Ok(())
    }

    async fn share_by_id(&self, share_id: i64) -> Result<Option<Share>> {
        let row = sqlx::query(concat!(share_select!(), "WHERE s.id = ?"))
            .bind(share_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(share_from_row).transpose()
    }

    /// Shares collection `collection_id` of `owner_id` with `sharee_id`, or
    /// changes the access of an existing share (which stays accepted or
    /// pending). Fails with [`StoreError::NoSuchSharee`] if the sharee does
    /// not exist, is disabled or is the owner, [`StoreError::ReadOnly`] for
    /// write access to a subscription, [`StoreError::Limit`] beyond
    /// [`MAX_SHARES_PER_COLLECTION`] or the sharee's
    /// [`MAX_PENDING_SHARES_PER_USER`], and returns `None` if the collection
    /// is not the owner's.
    pub async fn set_share(
        &self,
        collection_id: i64,
        owner_id: i64,
        sharee_id: i64,
        access: ShareAccess,
        now: i64,
    ) -> Result<Option<SetShare>> {
        let id = self
            .observe(
                async {
                    let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                    let Some(collection) = sqlx::query(
                        "SELECT kind, read_only FROM collections WHERE id = ? AND owner_id = ?",
                    )
                    .bind(collection_id)
                    .bind(owner_id)
                    .fetch_optional(&mut *tx)
                    .await?
                    else {
                        return Ok(None);
                    };
                    if sharee_id == owner_id {
                        return Err(StoreError::NoSuchSharee);
                    }
                    let sharee_ok = sqlx::query("SELECT 1 FROM users WHERE id = ? AND disabled = 0")
                        .bind(sharee_id)
                        .fetch_optional(&mut *tx)
                        .await?
                        .is_some();
                    if !sharee_ok {
                        return Err(StoreError::NoSuchSharee);
                    }
                    if access.writable() && collection.try_get::<bool, _>("read_only")? {
                        return Err(StoreError::ReadOnly);
                    }
                    let existing: Option<i64> = sqlx::query(
                        "SELECT id FROM shares WHERE collection_id = ? AND sharee_id = ?",
                    )
                    .bind(collection_id)
                    .bind(sharee_id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .map(|row| row.try_get("id"))
                    .transpose()?;
                    if let Some(id) = existing {
                        sqlx::query("UPDATE shares SET access = ? WHERE id = ?")
                            .bind(access.as_str())
                            .bind(id)
                            .execute(&mut *tx)
                            .await?;
                        tx.commit().await?;
                        return Ok(Some((id, false)));
                    }
                    let counts = sqlx::query(
                        "SELECT (SELECT COUNT(*) FROM shares WHERE collection_id = ?1) AS shares, \
                           (SELECT COUNT(*) FROM shares WHERE sharee_id = ?2 AND accepted_at IS NULL) AS pending",
                    )
                    .bind(collection_id)
                    .bind(sharee_id)
                    .fetch_one(&mut *tx)
                    .await?;
                    if counts.try_get::<i64, _>("shares")? >= MAX_SHARES_PER_COLLECTION {
                        return Err(StoreError::Limit("too many shares of this collection"));
                    }
                    if counts.try_get::<i64, _>("pending")? >= MAX_PENDING_SHARES_PER_USER {
                        return Err(StoreError::Limit("too many pending invitations for this user"));
                    }
                    let id: i64 = sqlx::query(
                        "INSERT INTO shares (public_id, collection_id, sharee_id, kind, access, created_at) \
                         VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ?) RETURNING id",
                    )
                        .bind(collection_id)
                        .bind(sharee_id)
                        .bind(collection.try_get::<String, _>("kind")?)
                        .bind(access.as_str())
                        .bind(now)
                        .fetch_one(&mut *tx)
                        .await?
                        .try_get("id")?;
                    tx.commit().await?;
                    Ok(Some((id, true)))
                }
                .await,
            )?;
        let Some((id, created)) = id else {
            return Ok(None);
        };
        let share = self
            .share_by_id(id)
            .await?
            .ok_or_else(|| corrupt("share vanished"))?;
        Ok(Some(if created {
            SetShare::Created(share)
        } else {
            SetShare::Updated(share)
        }))
    }

    /// Removes the share of collection `collection_id` (of `owner_id`) with
    /// `sharee_id`. Returns whether there was one.
    pub async fn remove_share(
        &self,
        collection_id: i64,
        owner_id: i64,
        sharee_id: i64,
    ) -> Result<bool> {
        self.observe(
            async {
                let done = sqlx::query(
                    "DELETE FROM shares WHERE collection_id = ? AND sharee_id = ? \
                       AND collection_id IN (SELECT id FROM collections WHERE owner_id = ?)",
                )
                .bind(collection_id)
                .bind(sharee_id)
                .bind(owner_id)
                .execute(&self.writer)
                .await?;
                Ok(done.rows_affected() == 1)
            }
            .await,
        )
    }

    /// The shares of a collection (pending and accepted), for its owner.
    pub async fn shares_of_collection(&self, collection_id: i64) -> Result<Vec<Share>> {
        let rows = sqlx::query(concat!(
            share_select!(),
            "WHERE s.collection_id = ?",
            active!(),
            "ORDER BY u.username"
        ))
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(share_from_row).collect()
    }

    /// All shares of an owner's collections of one kind.
    pub async fn shares_of_owner(&self, owner_id: i64, kind: CollectionKind) -> Result<Vec<Share>> {
        let rows = sqlx::query(concat!(
            share_select!(),
            "WHERE c.owner_id = ? AND s.kind = ?",
            active!(),
            "ORDER BY c.slug, u.username"
        ))
        .bind(owner_id)
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(share_from_row).collect()
    }

    /// Shares with `sharee_id`, pending and accepted.
    pub async fn incoming_shares(&self, sharee_id: i64) -> Result<Vec<Share>> {
        let rows = sqlx::query(concat!(
            share_select!(),
            "WHERE s.sharee_id = ?",
            active!(),
            "ORDER BY s.created_at, s.id"
        ))
        .bind(sharee_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(share_from_row).collect()
    }

    /// One share with `sharee_id` by its public id.
    pub async fn incoming_share(&self, sharee_id: i64, public_id: &str) -> Result<Option<Share>> {
        let row = sqlx::query(concat!(
            share_select!(),
            "WHERE s.sharee_id = ? AND s.public_id = ?",
            active!()
        ))
        .bind(sharee_id)
        .bind(public_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(share_from_row).transpose()
    }

    /// Every share, for administrators (at most `limit`).
    pub async fn all_shares(&self, limit: u32) -> Result<Vec<Share>> {
        let rows = sqlx::query(concat!(
            share_select!(),
            "ORDER BY o.username, c.slug, u.username LIMIT ?"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(share_from_row).collect()
    }

    /// The sharee accepts a share: it gets a slug in their home. Accepting
    /// an accepted share changes nothing. `None` if there is no such share.
    pub async fn accept_share(
        &self,
        sharee_id: i64,
        public_id: &str,
        now: i64,
    ) -> Result<Option<Share>> {
        let id = self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let Some(row) = sqlx::query(concat!(
                    share_select!(),
                    "WHERE s.sharee_id = ? AND s.public_id = ?",
                    active!()
                ))
                .bind(sharee_id)
                .bind(public_id)
                .fetch_optional(&mut *tx)
                .await?
                else {
                    return Ok(None);
                };
                let share = share_from_row(&row)?;
                if share.accepted() {
                    return Ok(Some(share.id));
                }
                let slug = free_slug(
                    &mut tx,
                    sharee_id,
                    share.kind,
                    share.owner.as_str(),
                    share.owner_slug.as_str(),
                )
                .await?;
                sqlx::query("UPDATE shares SET slug = ?, accepted_at = ? WHERE id = ?")
                    .bind(slug.as_str())
                    .bind(now)
                    .bind(share.id)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(Some(share.id))
            }
            .await,
        )?;
        match id {
            Some(id) => self.share_by_id(id).await,
            None => Ok(None),
        }
    }

    /// The sharee declines a pending share or unsubscribes from an
    /// accepted one: the share is removed. Returns whether there was one.
    pub async fn leave_share(&self, sharee_id: i64, public_id: &str) -> Result<bool> {
        self.observe(
            async {
                let done = sqlx::query("DELETE FROM shares WHERE sharee_id = ? AND public_id = ?")
                    .bind(sharee_id)
                    .bind(public_id)
                    .execute(&self.writer)
                    .await?;
                Ok(done.rows_affected() == 1)
            }
            .await,
        )
    }

    /// A collection shared with (and accepted by) `sharee_id`, at `slug`
    /// in their home of `kind`, as the sharee sees it.
    pub async fn shared_collection(
        &self,
        sharee_id: i64,
        kind: CollectionKind,
        slug: &Slug,
    ) -> Result<Option<Collection>> {
        let row = sqlx::query(concat!(shared_collection_select!(), "AND s.slug = ?3"))
            .bind(sharee_id)
            .bind(kind.as_str())
            .bind(slug.as_str())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(shared_collection_from_row).transpose()
    }

    /// All collections of one kind shared with (and accepted by)
    /// `sharee_id`, as the sharee sees them.
    pub async fn shared_collections(
        &self,
        sharee_id: i64,
        kind: CollectionKind,
    ) -> Result<Vec<Collection>> {
        let rows = sqlx::query(concat!(shared_collection_select!(), "ORDER BY s.slug"))
            .bind(sharee_id)
            .bind(kind.as_str())
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(shared_collection_from_row).collect()
    }

    /// Changes the sharee's own name, colour or position of an accepted
    /// share. Returns `false` if there is no such share.
    pub async fn update_share_view(
        &self,
        sharee_id: i64,
        share_id: i64,
        view: &ShareView<'_>,
    ) -> Result<bool> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let exists = sqlx::query(
                    "SELECT 1 FROM shares WHERE id = ? AND sharee_id = ? AND accepted_at IS NOT NULL",
                )
                .bind(share_id)
                .bind(sharee_id)
                .fetch_optional(&mut *tx)
                .await?
                .is_some();
                if !exists {
                    return Ok(false);
                }
                if let Some(value) = view.display_name {
                    sqlx::query("UPDATE shares SET display_name = ? WHERE id = ?")
                        .bind(value)
                        .bind(share_id)
                        .execute(&mut *tx)
                        .await?;
                }
                if let Some(value) = view.color {
                    sqlx::query("UPDATE shares SET color = ? WHERE id = ?")
                        .bind(value)
                        .bind(share_id)
                        .execute(&mut *tx)
                        .await?;
                }
                if let Some(value) = view.order {
                    sqlx::query("UPDATE shares SET sort_order = ? WHERE id = ?")
                        .bind(value)
                        .bind(share_id)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?;
                Ok(true)
            }
            .await,
        )
    }
}

#[cfg(test)]
mod tests;
