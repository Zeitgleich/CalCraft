//! OpenID Connect identities bound to local users.
//!
//! An identity is the pair issuer + subject (`iss` + `sub` of the ID
//! token). E-mail addresses are never used for linking.

use std::collections::HashSet;

use sqlx::Row;

use crate::{Result, Store};

/// Result of [`Store::bind_oidc_identity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OidcBinding {
    /// First SSO login of this user: the identity is now bound.
    Bound,
    /// The user was already bound to this identity.
    Matches,
    /// The user is bound to a different subject of this issuer, or the
    /// subject is bound to another user.
    Mismatch,
}

/// When an identity was bound and last used; the subject is left out on
/// purpose (it is not needed for display).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OidcIdentityInfo {
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub last_login_at: i64,
}

/// Result of [`Store::bind_approved_oidc_identity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovedBinding {
    Bound,
    /// No unexpired approval for this issuer + subject.
    NotApproved,
    /// The user or the subject is already bound otherwise.
    Mismatch,
}

impl Store {
    /// The local user bound to an identity, if any.
    pub async fn user_id_by_oidc_identity(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<i64>> {
        let row =
            sqlx::query("SELECT user_id FROM oidc_identities WHERE issuer = ? AND subject = ?")
                .bind(issuer)
                .bind(subject)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|row| row.try_get("user_id"))
            .transpose()
            .map_err(Into::into)
    }

    /// Binds the identity to the user on the first SSO login and checks it
    /// on every later one. Race-free: the unique constraints decide.
    pub async fn bind_oidc_identity(
        &self,
        user_id: i64,
        issuer: &str,
        subject: &str,
        now: i64,
    ) -> Result<OidcBinding> {
        self.observe(
            async {
                let inserted = sqlx::query(
                    "INSERT INTO oidc_identities (user_id, issuer, subject, created_at, last_login_at) \
                     VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
                )
                .bind(user_id)
                .bind(issuer)
                .bind(subject)
                .bind(now)
                .bind(now)
                .execute(&self.writer)
                .await?
                .rows_affected()
                    == 1;
                if inserted {
                    return Ok(OidcBinding::Bound);
                }
                let updated = sqlx::query(
                    "UPDATE oidc_identities SET last_login_at = ? \
                     WHERE user_id = ? AND issuer = ? AND subject = ?",
                )
                .bind(now)
                .bind(user_id)
                .bind(issuer)
                .bind(subject)
                .execute(&self.writer)
                .await?
                .rows_affected()
                    == 1;
                Ok(if updated {
                    OidcBinding::Matches
                } else {
                    OidcBinding::Mismatch
                })
            }
            .await,
        )
    }

    /// Whether the user is already bound to an identity of this issuer.
    pub async fn has_oidc_identity(&self, user_id: i64, issuer: &str) -> Result<bool> {
        let row = sqlx::query("SELECT 1 FROM oidc_identities WHERE user_id = ? AND issuer = ?")
            .bind(user_id)
            .bind(issuer)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// The identity of this issuer bound to the user, without the subject.
    pub async fn oidc_identity_info(
        &self,
        user_id: i64,
        issuer: &str,
    ) -> Result<Option<OidcIdentityInfo>> {
        let row = sqlx::query(
            "SELECT created_at, last_login_at FROM oidc_identities WHERE user_id = ? AND issuer = ?",
        )
        .bind(user_id)
        .bind(issuer)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(OidcIdentityInfo {
                created_at: row.try_get("created_at")?,
                last_login_at: row.try_get("last_login_at")?,
            })
        })
        .transpose()
    }

    /// Ids of all users bound to at least one SSO identity (any issuer),
    /// for the user list of the administration.
    pub async fn oidc_linked_user_ids(&self) -> Result<HashSet<i64>> {
        let rows = sqlx::query("SELECT DISTINCT user_id FROM oidc_identities")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| row.try_get("user_id").map_err(Into::into))
            .collect()
    }

    /// Allows the next SSO login of exactly this IdP identity to bind the
    /// existing account (operator approval). Replaces an earlier approval.
    pub async fn approve_oidc_link(
        &self,
        user_id: i64,
        issuer: &str,
        subject: &str,
        expires_at: i64,
    ) -> Result<()> {
        self.observe(
            async {
                sqlx::query(
                    "INSERT INTO oidc_link_approvals (user_id, issuer, subject, expires_at) VALUES (?, ?, ?, ?) \
                     ON CONFLICT (user_id) DO UPDATE SET issuer = excluded.issuer, \
                     subject = excluded.subject, expires_at = excluded.expires_at",
                )
                .bind(user_id)
                .bind(issuer)
                .bind(subject)
                .bind(expires_at)
                .execute(&self.writer)
                .await?;
                Ok(())
            }
            .await,
        )
    }

    /// Binds an existing, not yet bound account if an unexpired approval
    /// for exactly this issuer + subject exists. Consuming the approval and
    /// creating the binding happen in one transaction: an approval for
    /// another subject is left untouched, and a failed binding keeps it.
    pub async fn bind_approved_oidc_identity(
        &self,
        user_id: i64,
        issuer: &str,
        subject: &str,
        now: i64,
    ) -> Result<ApprovedBinding> {
        self.observe(
            async {
                // Write transactions start IMMEDIATE (see `put_item`): a deferred one that
                // reads first fails with SQLITE_BUSY instead of waiting when another
                // writer commits in between.
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let consumed = sqlx::query(
                    "DELETE FROM oidc_link_approvals \
                     WHERE user_id = ? AND issuer = ? AND subject = ? AND expires_at > ?",
                )
                .bind(user_id)
                .bind(issuer)
                .bind(subject)
                .bind(now)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                    == 1;
                if !consumed {
                    return Ok(ApprovedBinding::NotApproved);
                }
                let inserted = sqlx::query(
                    "INSERT INTO oidc_identities (user_id, issuer, subject, created_at, last_login_at) \
                     VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
                )
                .bind(user_id)
                .bind(issuer)
                .bind(subject)
                .bind(now)
                .bind(now)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                    == 1;
                if !inserted {
                    // Rolled back on drop: the approval stays.
                    return Ok(ApprovedBinding::Mismatch);
                }
                tx.commit().await?;
                Ok(ApprovedBinding::Bound)
            }
            .await,
        )
    }

    /// Records an SSO login of an identity that is already bound. Never
    /// creates a binding: returns `false` if exactly this user + issuer +
    /// subject is not (or no longer) bound, e.g. because it was unlinked
    /// after the login checked it. New bindings only come from
    /// [`Self::bind_approved_oidc_identity`] or, for accounts just created
    /// by SSO, [`Self::bind_oidc_identity`].
    pub async fn touch_oidc_identity(
        &self,
        user_id: i64,
        issuer: &str,
        subject: &str,
        now: i64,
    ) -> Result<bool> {
        self.observe(
            async {
                let updated = sqlx::query(
                    "UPDATE oidc_identities SET last_login_at = ? \
                     WHERE user_id = ? AND issuer = ? AND subject = ?",
                )
                .bind(now)
                .bind(user_id)
                .bind(issuer)
                .bind(subject)
                .execute(&self.writer)
                .await?
                .rows_affected();
                Ok(updated == 1)
            }
            .await,
        )
    }

    /// Creates a web session for an SSO login, but only if the identity is
    /// still bound to the user. The check is part of the same statement, so
    /// a concurrent [`Self::unbind_oidc_identities`] either comes first (no
    /// session) or removes the new session as well. Returns `false` if the
    /// identity is not bound.
    pub async fn create_oidc_session(
        &self,
        user_id: i64,
        issuer: &str,
        subject: &str,
        token_hash: &str,
        now: i64,
        expires_at: i64,
    ) -> Result<bool> {
        self.observe(
            async {
                let inserted = sqlx::query(
                    "INSERT INTO sessions (user_id, token_hash, created_at, expires_at) \
                     SELECT ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM oidc_identities \
                     WHERE user_id = ? AND issuer = ? AND subject = ?)",
                )
                .bind(user_id)
                .bind(token_hash)
                .bind(now)
                .bind(expires_at)
                .bind(user_id)
                .bind(issuer)
                .bind(subject)
                .execute(&self.writer)
                .await?
                .rows_affected();
                Ok(inserted == 1)
            }
            .await,
        )
    }

    /// Removes all SSO bindings of a user (e.g. after the IdP account was
    /// recreated) and ends the user's web sessions, in one transaction.
    /// Linking the account again needs an operator approval. Returns the
    /// number of bindings removed.
    pub async fn unbind_oidc_identities(&self, user_id: i64) -> Result<u64> {
        self.observe(
            async {
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let removed = sqlx::query("DELETE FROM oidc_identities WHERE user_id = ?")
                    .bind(user_id)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
                sqlx::query("DELETE FROM sessions WHERE user_id = ?")
                    .bind(user_id)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(removed)
            }
            .await,
        )
    }
}

#[cfg(test)]
mod tests;
