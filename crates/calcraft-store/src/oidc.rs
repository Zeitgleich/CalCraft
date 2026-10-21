//! OpenID Connect identities bound to local users.
//!
//! An identity is the pair issuer + subject (`iss` + `sub` of the ID
//! token). E-mail addresses are never used for linking.

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
        let inserted = sqlx::query(
            "INSERT INTO oidc_identities (user_id, issuer, subject, created_at, last_login_at) \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(user_id)
        .bind(issuer)
        .bind(subject)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
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
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1;
        Ok(if updated {
            OidcBinding::Matches
        } else {
            OidcBinding::Mismatch
        })
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

    /// Allows the next SSO login of exactly this IdP identity to bind the
    /// existing account (operator approval). Replaces an earlier approval.
    pub async fn approve_oidc_link(
        &self,
        user_id: i64,
        issuer: &str,
        subject: &str,
        expires_at: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO oidc_link_approvals (user_id, issuer, subject, expires_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (user_id) DO UPDATE SET issuer = excluded.issuer, \
             subject = excluded.subject, expires_at = excluded.expires_at",
        )
        .bind(user_id)
        .bind(issuer)
        .bind(subject)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
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
        let mut tx = self.pool.begin().await?;
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

    /// Removes all SSO bindings of a user (e.g. after the IdP account was
    /// recreated). The next SSO login binds again. Returns the number removed.
    pub async fn unbind_oidc_identities(&self, user_id: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM oidc_identities WHERE user_id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn identities_bind_once_and_are_unique() {
        let store = Store::in_memory().await.unwrap();
        let alice = store
            .create_user(&"alice".parse().unwrap(), "h", None)
            .await
            .unwrap();
        let bob = store
            .create_user(&"bob".parse().unwrap(), "h", None)
            .await
            .unwrap();
        let iss = "https://idp.example";
        assert_eq!(
            store
                .bind_oidc_identity(alice.id, iss, "s1", 1)
                .await
                .unwrap(),
            OidcBinding::Bound
        );
        assert_eq!(
            store
                .bind_oidc_identity(alice.id, iss, "s1", 2)
                .await
                .unwrap(),
            OidcBinding::Matches
        );
        // Another subject for the same user, or the same subject for another user.
        assert_eq!(
            store
                .bind_oidc_identity(alice.id, iss, "s2", 3)
                .await
                .unwrap(),
            OidcBinding::Mismatch
        );
        assert_eq!(
            store
                .bind_oidc_identity(bob.id, iss, "s1", 3)
                .await
                .unwrap(),
            OidcBinding::Mismatch
        );
        // Another issuer is a separate identity.
        assert_eq!(
            store
                .bind_oidc_identity(bob.id, "https://other.example", "s1", 3)
                .await
                .unwrap(),
            OidcBinding::Bound
        );
        assert_eq!(
            store.user_id_by_oidc_identity(iss, "s1").await.unwrap(),
            Some(alice.id)
        );
        assert!(store.has_oidc_identity(alice.id, iss).await.unwrap());
        assert!(
            !store
                .has_oidc_identity(alice.id, "https://other.example")
                .await
                .unwrap()
        );

        // Approvals are bound to issuer + subject, single use and expire.
        let carol = store
            .create_user(&"carol".parse().unwrap(), "h", None)
            .await
            .unwrap();
        let bind = |sub: &'static str, now: i64| {
            let store = store.clone();
            async move {
                store
                    .bind_approved_oidc_identity(carol.id, iss, sub, now)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(bind("c1", 10).await, ApprovedBinding::NotApproved);
        store
            .approve_oidc_link(carol.id, iss, "c1", 100)
            .await
            .unwrap();
        assert_eq!(bind("other", 10).await, ApprovedBinding::NotApproved);
        // Subject already bound to alice: refused, approval kept.
        store
            .approve_oidc_link(carol.id, iss, "s1", 100)
            .await
            .unwrap();
        assert_eq!(bind("s1", 10).await, ApprovedBinding::Mismatch);
        assert_eq!(bind("s1", 10).await, ApprovedBinding::Mismatch);
        assert_eq!(bind("s1", 100).await, ApprovedBinding::NotApproved);
        store
            .approve_oidc_link(carol.id, iss, "c1", 100)
            .await
            .unwrap();
        assert_eq!(bind("c1", 10).await, ApprovedBinding::Bound);
        assert_eq!(bind("c1", 10).await, ApprovedBinding::NotApproved);
        assert_eq!(
            store.user_id_by_oidc_identity(iss, "c1").await.unwrap(),
            Some(carol.id)
        );

        assert_eq!(store.unbind_oidc_identities(alice.id).await.unwrap(), 1);
        assert_eq!(
            store.user_id_by_oidc_identity(iss, "s1").await.unwrap(),
            None
        );
    }
}
