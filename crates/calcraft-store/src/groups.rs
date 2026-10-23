//! Groups of users and sharing collections with groups (docs/sharing.md).
//!
//! Administrators create groups and manage their members. An owner shares
//! a collection with a group ([`Store::set_group_share`]); that is not an
//! access grant of its own: every member gets their own row in `shares`,
//! an invitation they accept or decline like a direct share, with their
//! own slug, name and colour. The row's `access` is the maximum of the
//! direct share and the member's group shares, recomputed by
//! [`recompute_share`] inside the transaction of every change, so that a
//! member removed from a group (or a deleted group, or a removed group
//! share) loses access at once, like a revoked share, unless a direct share
//! or another group share still grants it.
//!
//! Invitations through groups obey the per-sharee limits of direct
//! invitations (pending invitations in total and per owner, the 24-hour
//! block after a decline): a member who cannot be invited now is skipped
//! and invited on a later change of the group or the group share. A member
//! who declines a group share's invitation, or leaves the collection, is
//! never invited by that group share again (`group_share_declines`).

use calcraft_core::{GroupName, Username};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    Result, Store, StoreError,
    sharing::{Recomputed, ShareAccess, recompute_share},
};

/// Most group shares of one collection.
pub const MAX_GROUP_SHARES_PER_COLLECTION: i64 = 20;
/// Most group shares with one group (bounds the work of a membership
/// change, which revisits every share of the group).
pub const MAX_SHARES_PER_GROUP: i64 = 1000;

/// A group of users.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: i64,
    pub name: GroupName,
    pub display_name: Option<String>,
    /// Number of members (enabled or not).
    pub members: i64,
    pub created_at: i64,
}

/// A member of a group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    pub user_id: i64,
    pub username: Username,
    pub display_name: Option<String>,
    pub disabled: bool,
}

/// A collection shared with a group, as its owner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupShare {
    pub id: i64,
    pub collection_id: i64,
    pub group_id: i64,
    pub group: GroupName,
    pub group_display_name: Option<String>,
    pub access: ShareAccess,
    /// Members other than the owner (enabled or not).
    pub members: i64,
    pub created_at: i64,
}

/// Result of [`Store::set_group_share`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupShareOutcome {
    pub share: GroupShare,
    /// The group share is new (else its access changed or stayed).
    pub created: bool,
    /// New invitations of members.
    pub invited: u32,
    /// Members who could not be invited now (invitation limits or a recent
    /// decline); a later change of the group or the group share tries again.
    pub skipped: u32,
}

/// Result of [`Store::add_group_member`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberAdded {
    /// `false` if the user was a member already.
    pub added: bool,
    /// New invitations through the group's shares.
    pub invited: u32,
}

fn corrupt(err: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(err.to_string())
}

macro_rules! group_select {
    () => {
        "SELECT g.id, g.name, g.display_name, g.created_at, \
           (SELECT COUNT(*) FROM group_members m WHERE m.group_id = g.id) AS members \
         FROM user_groups g "
    };
}

fn group_from_row(row: &SqliteRow) -> Result<Group> {
    Ok(Group {
        id: row.try_get("id")?,
        name: row
            .try_get::<String, _>("name")?
            .try_into()
            .map_err(corrupt)?,
        display_name: row.try_get("display_name")?,
        members: row.try_get("members")?,
        created_at: row.try_get("created_at")?,
    })
}

macro_rules! group_share_select {
    () => {
        "SELECT gs.id, gs.collection_id, gs.group_id, gs.access, gs.created_at, \
           g.name, g.display_name, \
           (SELECT COUNT(*) FROM group_members m WHERE m.group_id = g.id \
              AND m.user_id <> c.owner_id) AS members \
         FROM group_shares gs JOIN user_groups g ON g.id = gs.group_id \
           JOIN collections c ON c.id = gs.collection_id "
    };
}

fn group_share_from_row(row: &SqliteRow) -> Result<GroupShare> {
    let access: String = row.try_get("access")?;
    Ok(GroupShare {
        id: row.try_get("id")?,
        collection_id: row.try_get("collection_id")?,
        group_id: row.try_get("group_id")?,
        group: row
            .try_get::<String, _>("name")?
            .try_into()
            .map_err(corrupt)?,
        group_display_name: row.try_get("display_name")?,
        access: access
            .parse()
            .map_err(|()| corrupt(format!("unknown share access {access:?}")))?,
        members: row.try_get("members")?,
        created_at: row.try_get("created_at")?,
    })
}

/// The members of `group_id` other than the owner of `collection_id`.
async fn members_except_owner(
    tx: &mut Transaction<'static, Sqlite>,
    group_id: i64,
    collection_id: i64,
) -> Result<Vec<i64>> {
    sqlx::query(
        "SELECT m.user_id FROM group_members m, collections c \
         WHERE m.group_id = ? AND c.id = ? AND m.user_id <> c.owner_id ORDER BY m.user_id",
    )
    .bind(group_id)
    .bind(collection_id)
    .fetch_all(&mut **tx)
    .await?
    .iter()
    .map(|row| Ok(row.try_get("user_id")?))
    .collect()
}

/// [`Store::set_group_share`] inside a write transaction: `(group share
/// id, created, invited, skipped)`, `None` if the collection is not the
/// owner's.
pub(crate) async fn set_group_share_in(
    tx: &mut Transaction<'static, Sqlite>,
    collection_id: i64,
    owner_id: i64,
    group_id: i64,
    access: ShareAccess,
    now: i64,
) -> Result<Option<(i64, bool, u32, u32)>> {
    let Some(collection) =
        sqlx::query("SELECT read_only FROM collections WHERE id = ? AND owner_id = ?")
            .bind(collection_id)
            .bind(owner_id)
            .fetch_optional(&mut **tx)
            .await?
    else {
        return Ok(None);
    };
    if access.writable() && collection.try_get::<bool, _>("read_only")? {
        return Err(StoreError::ReadOnly);
    }
    let group_exists = sqlx::query("SELECT 1 FROM user_groups WHERE id = ?")
        .bind(group_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    if !group_exists {
        return Err(StoreError::NoSuchGroup);
    }
    let existing: Option<i64> =
        sqlx::query("SELECT id FROM group_shares WHERE collection_id = ? AND group_id = ?")
            .bind(collection_id)
            .bind(group_id)
            .fetch_optional(&mut **tx)
            .await?
            .map(|row| row.try_get("id"))
            .transpose()?;
    let (id, created) = match existing {
        Some(id) => {
            sqlx::query("UPDATE group_shares SET access = ? WHERE id = ?")
                .bind(access.as_str())
                .bind(id)
                .execute(&mut **tx)
                .await?;
            (id, false)
        }
        None => {
            let counts = sqlx::query(
                "SELECT (SELECT COUNT(*) FROM group_shares WHERE collection_id = ?) AS of_collection, \
                   (SELECT COUNT(*) FROM group_shares WHERE group_id = ?) AS of_group",
            )
            .bind(collection_id)
            .bind(group_id)
            .fetch_one(&mut **tx)
            .await?;
            if counts.try_get::<i64, _>("of_collection")? >= MAX_GROUP_SHARES_PER_COLLECTION {
                return Err(StoreError::Limit(
                    "too many group shares of this collection",
                ));
            }
            if counts.try_get::<i64, _>("of_group")? >= MAX_SHARES_PER_GROUP {
                return Err(StoreError::Limit("too many shares with this group"));
            }
            let id: i64 = sqlx::query(
                "INSERT INTO group_shares (collection_id, group_id, access, created_at) \
                 VALUES (?, ?, ?, ?) RETURNING id",
            )
            .bind(collection_id)
            .bind(group_id)
            .bind(access.as_str())
            .bind(now)
            .fetch_one(&mut **tx)
            .await?
            .try_get("id")?;
            (id, true)
        }
    };
    let (mut invited, mut skipped) = (0, 0);
    for user_id in members_except_owner(tx, group_id, collection_id).await? {
        match recompute_share(tx, collection_id, user_id, Some(now)).await? {
            Recomputed::Invited => invited += 1,
            Recomputed::Skipped => skipped += 1,
            Recomputed::Nothing | Recomputed::Kept | Recomputed::Removed => {}
        }
    }
    Ok(Some((id, created, invited, skipped)))
}

/// [`Store::remove_group_share`] inside a write transaction.
pub(crate) async fn remove_group_share_in(
    tx: &mut Transaction<'static, Sqlite>,
    collection_id: i64,
    owner_id: i64,
    group_id: i64,
) -> Result<bool> {
    let members = members_except_owner(tx, group_id, collection_id).await?;
    let done = sqlx::query(
        "DELETE FROM group_shares WHERE collection_id = ? AND group_id = ? \
           AND collection_id IN (SELECT id FROM collections WHERE owner_id = ?)",
    )
    .bind(collection_id)
    .bind(group_id)
    .bind(owner_id)
    .execute(&mut **tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    for user_id in members {
        recompute_share(tx, collection_id, user_id, None).await?;
    }
    Ok(true)
}

/// The collections shared with `group_id`.
async fn shared_collections_of_group(
    tx: &mut Transaction<'static, Sqlite>,
    group_id: i64,
) -> Result<Vec<i64>> {
    sqlx::query("SELECT collection_id FROM group_shares WHERE group_id = ? ORDER BY id")
        .bind(group_id)
        .fetch_all(&mut **tx)
        .await?
        .iter()
        .map(|row| Ok(row.try_get("collection_id")?))
        .collect()
}

impl Store {
    /// Creates a group. Fails with [`StoreError::GroupExists`] if the name
    /// is taken and [`StoreError::Limit`] if there are `max_groups`.
    pub async fn create_group(
        &self,
        name: &GroupName,
        display_name: Option<&str>,
        max_groups: u32,
        now: i64,
    ) -> Result<Group> {
        let id = self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM user_groups")
                    .fetch_one(&mut *tx)
                    .await?
                    .try_get("n")?;
                if count >= i64::from(max_groups) {
                    return Err(StoreError::Limit("too many groups"));
                }
                let result = sqlx::query(
                    "INSERT INTO user_groups (name, display_name, created_at) VALUES (?, ?, ?) \
                     RETURNING id",
                )
                .bind(name.as_str())
                .bind(display_name)
                .bind(now)
                .fetch_one(&mut *tx)
                .await;
                let id: i64 = match result {
                    Ok(row) => row.try_get("id")?,
                    Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
                        return Err(StoreError::GroupExists);
                    }
                    Err(err) => return Err(err.into()),
                };
                tx.commit().await?;
                Ok(id)
            }
            .await,
        )?;
        self.group(id)
            .await?
            .ok_or_else(|| corrupt("group vanished"))
    }

    async fn group(&self, id: i64) -> Result<Option<Group>> {
        let row = sqlx::query(concat!(group_select!(), "WHERE g.id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(group_from_row).transpose()
    }

    /// The group named `name`.
    pub async fn group_by_name(&self, name: &GroupName) -> Result<Option<Group>> {
        let row = sqlx::query(concat!(group_select!(), "WHERE g.name = ?"))
            .bind(name.as_str())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(group_from_row).transpose()
    }

    /// All groups, by name (for administrators).
    pub async fn list_groups(&self) -> Result<Vec<Group>> {
        let rows = sqlx::query(concat!(group_select!(), "ORDER BY g.name"))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(group_from_row).collect()
    }

    /// The groups `user_id` is a member of, by name.
    pub async fn groups_of_user(&self, user_id: i64) -> Result<Vec<Group>> {
        let rows = sqlx::query(concat!(
            group_select!(),
            "WHERE g.id IN (SELECT group_id FROM group_members WHERE user_id = ?) ORDER BY g.name"
        ))
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(group_from_row).collect()
    }

    /// Whether `user_id` is a member of `group_id`.
    pub async fn is_group_member(&self, group_id: i64, user_id: i64) -> Result<bool> {
        Ok(
            sqlx::query("SELECT 1 FROM group_members WHERE group_id = ? AND user_id = ?")
                .bind(group_id)
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?
                .is_some(),
        )
    }

    /// The members of a group, by username.
    pub async fn group_members(&self, group_id: i64) -> Result<Vec<GroupMember>> {
        let rows = sqlx::query(
            "SELECT u.id, u.username, u.display_name, u.disabled FROM group_members m \
               JOIN users u ON u.id = m.user_id WHERE m.group_id = ? ORDER BY u.username",
        )
        .bind(group_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(GroupMember {
                    user_id: row.try_get("id")?,
                    username: row
                        .try_get::<String, _>("username")?
                        .try_into()
                        .map_err(corrupt)?,
                    display_name: row.try_get("display_name")?,
                    disabled: row.try_get("disabled")?,
                })
            })
            .collect()
    }

    /// Deletes a group: its shares end at once for every member they gave
    /// access to (direct shares and other groups' shares stay). Returns
    /// whether it existed.
    pub async fn delete_group(&self, group_id: i64) -> Result<bool> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let collections = shared_collections_of_group(&mut tx, group_id).await?;
                let mut affected = Vec::new();
                for &collection_id in &collections {
                    for user_id in members_except_owner(&mut tx, group_id, collection_id).await? {
                        affected.push((collection_id, user_id));
                    }
                }
                // Cascades to members, group shares and their declines.
                let done = sqlx::query("DELETE FROM user_groups WHERE id = ?")
                    .bind(group_id)
                    .execute(&mut *tx)
                    .await?;
                if done.rows_affected() == 0 {
                    return Ok(false);
                }
                for (collection_id, user_id) in affected {
                    recompute_share(&mut tx, collection_id, user_id, None).await?;
                }
                tx.commit().await?;
                Ok(true)
            }
            .await,
        )
    }

    /// Adds `user_id` to `group_id`: they get an invitation for every
    /// collection shared with the group (within the invitation limits).
    /// Fails with [`StoreError::NoSuchGroup`] or
    /// [`StoreError::NoSuchSharee`] if either does not exist, and
    /// [`StoreError::Limit`] if the group has `max_members`.
    pub async fn add_group_member(
        &self,
        group_id: i64,
        user_id: i64,
        max_members: u32,
        now: i64,
    ) -> Result<MemberAdded> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let row = sqlx::query(
                    "SELECT EXISTS (SELECT 1 FROM user_groups WHERE id = ?1) AS group_exists, \
                       EXISTS (SELECT 1 FROM users WHERE id = ?2) AS user_exists, \
                       EXISTS (SELECT 1 FROM group_members WHERE group_id = ?1 AND user_id = ?2) \
                         AS member, \
                       (SELECT COUNT(*) FROM group_members WHERE group_id = ?1) AS members",
                )
                .bind(group_id)
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await?;
                if !row.try_get::<bool, _>("group_exists")? {
                    return Err(StoreError::NoSuchGroup);
                }
                if !row.try_get::<bool, _>("user_exists")? {
                    return Err(StoreError::NoSuchSharee);
                }
                if row.try_get::<bool, _>("member")? {
                    return Ok(MemberAdded {
                        added: false,
                        invited: 0,
                    });
                }
                if row.try_get::<i64, _>("members")? >= i64::from(max_members) {
                    return Err(StoreError::Limit("too many members in this group"));
                }
                sqlx::query(
                    "INSERT INTO group_members (group_id, user_id, added_at) VALUES (?, ?, ?)",
                )
                .bind(group_id)
                .bind(user_id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                let mut invited = 0;
                for collection_id in shared_collections_of_group(&mut tx, group_id).await? {
                    if recompute_share(&mut tx, collection_id, user_id, Some(now)).await?
                        == Recomputed::Invited
                    {
                        invited += 1;
                    }
                }
                tx.commit().await?;
                Ok(MemberAdded {
                    added: true,
                    invited,
                })
            }
            .await,
        )
    }

    /// Removes `user_id` from `group_id`: the group's shares end for them
    /// at once (in this transaction), unless a direct share or another
    /// group's share grants access. Returns whether they were a member.
    pub async fn remove_group_member(&self, group_id: i64, user_id: i64) -> Result<bool> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let done =
                    sqlx::query("DELETE FROM group_members WHERE group_id = ? AND user_id = ?")
                        .bind(group_id)
                        .bind(user_id)
                        .execute(&mut *tx)
                        .await?;
                if done.rows_affected() == 0 {
                    return Ok(false);
                }
                for collection_id in shared_collections_of_group(&mut tx, group_id).await? {
                    recompute_share(&mut tx, collection_id, user_id, None).await?;
                }
                tx.commit().await?;
                Ok(true)
            }
            .await,
        )
    }

    /// Shares collection `collection_id` of `owner_id` with every member
    /// of `group_id` (each gets their own invitation), or changes the
    /// access of an existing group share; also invites members who could
    /// not be invited before. Fails with [`StoreError::NoSuchGroup`],
    /// [`StoreError::ReadOnly`] for write access to a subscription and
    /// [`StoreError::Limit`] beyond [`MAX_GROUP_SHARES_PER_COLLECTION`] or
    /// [`MAX_SHARES_PER_GROUP`]; `None` if the collection is not the owner's.
    /// Whether the owner may share with the group is the caller's decision.
    pub async fn set_group_share(
        &self,
        collection_id: i64,
        owner_id: i64,
        group_id: i64,
        access: ShareAccess,
        now: i64,
    ) -> Result<Option<GroupShareOutcome>> {
        let result = self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let result =
                    set_group_share_in(&mut tx, collection_id, owner_id, group_id, access, now)
                        .await?;
                if result.is_some() {
                    tx.commit().await?;
                }
                Ok(result)
            }
            .await,
        )?;
        let Some((id, created, invited, skipped)) = result else {
            return Ok(None);
        };
        let row = sqlx::query(concat!(group_share_select!(), "WHERE gs.id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| corrupt("group share vanished"))?;
        Ok(Some(GroupShareOutcome {
            share: group_share_from_row(&row)?,
            created,
            invited,
            skipped,
        }))
    }

    /// Ends the share of collection `collection_id` (of `owner_id`) with
    /// `group_id`: members lose access at once unless a direct share or
    /// another group share grants it. Returns whether there was one.
    pub async fn remove_group_share(
        &self,
        collection_id: i64,
        owner_id: i64,
        group_id: i64,
    ) -> Result<bool> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let done =
                    remove_group_share_in(&mut tx, collection_id, owner_id, group_id).await?;
                tx.commit().await?;
                Ok(done)
            }
            .await,
        )
    }

    /// The group shares of a collection, by group name.
    pub async fn group_shares_of_collection(&self, collection_id: i64) -> Result<Vec<GroupShare>> {
        let rows = sqlx::query(concat!(
            group_share_select!(),
            "WHERE gs.collection_id = ? ORDER BY g.name"
        ))
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(group_share_from_row).collect()
    }
}

#[cfg(test)]
mod tests;
