//! Sessions, app passwords and two-factor authentication.
//!
//! Secrets are never stored: only hashes of session tokens, app passwords
//! and recovery codes. Every query is scoped to a user id. Timestamps are
//! Unix seconds passed in by the caller.

use calcraft_core::Username;
use sqlx::Row;

use crate::{Result, Store, StoreError, User, user_from_row};

/// Number of recovery codes issued when 2FA is enabled.
pub const RECOVERY_CODES: usize = 10;
/// Maximum number of app passwords per user.
pub const MAX_APP_PASSWORDS: i64 = 50;
/// `last_used_at` of app passwords is updated at most this often.
const LAST_USED_GRANULARITY: i64 = 3600;

/// An app password as listed to its owner (the secret is never returned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPassword {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// Result of [`Store::enable_totp_with_step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotpEnableOutcome {
    /// 2FA is on now, with the new recovery codes.
    Enabled,
    /// It was already on (e.g. a concurrent request enabled it).
    AlreadyEnabled,
    /// The code's time step was already used (replay).
    StepUsed,
    /// No pending secret, or a different one than the code was checked
    /// against (a concurrent setup replaced it).
    NotSetUp,
}

/// Stored state of a user's TOTP configuration.
#[derive(Clone, PartialEq, Eq)]
pub struct TotpState {
    pub secret: Vec<u8>,
    pub enabled: bool,
    pub last_step: i64,
}

impl std::fmt::Debug for TotpState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TotpState")
            .field("secret", &"<redacted>")
            .field("enabled", &self.enabled)
            .field("last_step", &self.last_step)
            .finish()
    }
}

/// A valid session with its user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub user: User,
    pub expires_at: i64,
}

impl Store {
    pub async fn user_by_id(&self, id: i64) -> Result<Option<User>> {
        let row = sqlx::query(
            "SELECT id, username, display_name, disabled, is_admin, totp_enabled FROM users WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(user_from_row).transpose()
    }

    /// Grants or revokes admin rights. Returns `false` if the user does not exist.
    pub async fn set_admin(&self, username: &Username, admin: bool) -> Result<bool> {
        let result = sqlx::query("UPDATE users SET is_admin = ? WHERE username = ?")
            .bind(admin)
            .bind(username.as_str())
            .execute(&self.writer)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    // --- sessions -------------------------------------------------------

    pub async fn create_session(
        &self,
        user_id: i64,
        token_hash: &str,
        now: i64,
        expires_at: i64,
    ) -> Result<()> {
        // Expired sessions are removed opportunistically.
        sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now)
            .execute(&self.writer)
            .await?;
        sqlx::query(
            "INSERT INTO sessions (user_id, token_hash, created_at, expires_at) VALUES (?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(token_hash)
        .bind(now)
        .bind(expires_at)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// The user of a valid, unexpired session.
    pub async fn session(&self, token_hash: &str, now: i64) -> Result<Option<Session>> {
        let row = sqlx::query(
            "SELECT u.id, u.username, u.display_name, u.disabled, u.is_admin, u.totp_enabled, s.expires_at \
             FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.token_hash = ? AND s.expires_at > ?",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(Session {
                user: user_from_row(&row)?,
                expires_at: row.try_get("expires_at")?,
            })
        })
        .transpose()
    }

    pub async fn delete_session(&self, token_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.writer)
            .await?;
        Ok(())
    }

    /// Ends all sessions of a user, e.g. after a password change.
    pub async fn delete_sessions_of(&self, user_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(user_id)
            .execute(&self.writer)
            .await?;
        Ok(())
    }

    // --- app passwords --------------------------------------------------

    pub async fn create_app_password(
        &self,
        user_id: i64,
        name: &str,
        secret_hash: &str,
        now: i64,
    ) -> Result<i64> {
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM app_passwords WHERE user_id = ?")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?
            .try_get("n")?;
        if count >= MAX_APP_PASSWORDS {
            return Err(StoreError::Limit("too many app passwords"));
        }
        let id: i64 = sqlx::query(
            "INSERT INTO app_passwords (user_id, name, secret_hash, created_at) VALUES (?, ?, ?, ?) RETURNING id",
        )
        .bind(user_id)
        .bind(name)
        .bind(secret_hash)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?
        .try_get("id")?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn app_passwords(&self, user_id: i64) -> Result<Vec<AppPassword>> {
        let rows = sqlx::query(
            "SELECT id, name, created_at, last_used_at FROM app_passwords WHERE user_id = ? ORDER BY id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(AppPassword {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    created_at: row.try_get("created_at")?,
                    last_used_at: row.try_get("last_used_at")?,
                })
            })
            .collect()
    }

    /// Deletes an app password of this user. Returns `false` if not found.
    pub async fn delete_app_password(&self, user_id: i64, id: i64) -> Result<bool> {
        let result = sqlx::query("DELETE FROM app_passwords WHERE id = ? AND user_id = ?")
            .bind(id)
            .bind(user_id)
            .execute(&self.writer)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Owner and secret hash of an app password, by id.
    pub async fn app_password_secret(&self, id: i64) -> Result<Option<(i64, String)>> {
        let row = sqlx::query("SELECT user_id, secret_hash FROM app_passwords WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| Ok((row.try_get("user_id")?, row.try_get("secret_hash")?)))
            .transpose()
    }

    /// Records a use, at most once per hour to avoid a write per request.
    pub async fn touch_app_password(&self, id: i64, now: i64) -> Result<()> {
        sqlx::query(
            "UPDATE app_passwords SET last_used_at = ?1 \
             WHERE id = ?2 AND (last_used_at IS NULL OR last_used_at < ?1 - ?3)",
        )
        .bind(now)
        .bind(id)
        .bind(LAST_USED_GRANULARITY)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    // --- two-factor authentication --------------------------------------

    pub async fn totp_state(&self, user_id: i64) -> Result<Option<TotpState>> {
        let row = sqlx::query(
            "SELECT totp_secret, totp_enabled, totp_last_step FROM users WHERE id = ? AND totp_secret IS NOT NULL",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(TotpState {
                secret: row.try_get("totp_secret")?,
                enabled: row.try_get("totp_enabled")?,
                last_step: row.try_get("totp_last_step")?,
            })
        })
        .transpose()
    }

    /// Stores a new, not yet enabled secret. Fails if 2FA is already enabled.
    pub async fn set_pending_totp(&self, user_id: i64, secret: &[u8]) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE users SET totp_secret = ?, totp_last_step = 0 WHERE id = ? AND totp_enabled = 0",
        )
        .bind(secret)
        .bind(user_id)
        .execute(&self.writer)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Enables 2FA and replaces the recovery codes.
    /// Enables 2FA with new recovery codes. Returns `false` (and changes
    /// nothing) if it is already enabled or no secret was set up.
    pub async fn enable_totp(&self, user_id: i64, recovery_hashes: &[String]) -> Result<bool> {
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let changed = sqlx::query(
            "UPDATE users SET totp_enabled = 1 \
             WHERE id = ? AND totp_secret IS NOT NULL AND totp_enabled = 0",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 0 {
            return Ok(false);
        }
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = ?")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        for hash in recovery_hashes {
            sqlx::query("INSERT INTO recovery_codes (user_id, code_hash) VALUES (?, ?)")
                .bind(user_id)
                .bind(hash)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Accepts the time step of a code verified against `secret` and enables
    /// 2FA with new recovery codes, in one transaction: of concurrent
    /// requests with the same code exactly one enables, the others see
    /// [`TotpEnableOutcome::AlreadyEnabled`].
    pub async fn enable_totp_with_step(
        &self,
        user_id: i64,
        secret: &[u8],
        step: i64,
        recovery_hashes: &[String],
    ) -> Result<TotpEnableOutcome> {
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT totp_enabled, totp_last_step FROM users WHERE id = ? AND totp_secret = ?",
        )
        .bind(user_id)
        .bind(secret)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(TotpEnableOutcome::NotSetUp);
        };
        if row.try_get::<bool, _>("totp_enabled")? {
            return Ok(TotpEnableOutcome::AlreadyEnabled);
        }
        if row.try_get::<i64, _>("totp_last_step")? >= step {
            return Ok(TotpEnableOutcome::StepUsed);
        }
        sqlx::query("UPDATE users SET totp_enabled = 1, totp_last_step = ? WHERE id = ?")
            .bind(step)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = ?")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        for hash in recovery_hashes {
            sqlx::query("INSERT INTO recovery_codes (user_id, code_hash) VALUES (?, ?)")
                .bind(user_id)
                .bind(hash)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(TotpEnableOutcome::Enabled)
    }

    pub async fn disable_totp(&self, user_id: i64) -> Result<()> {
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "UPDATE users SET totp_secret = NULL, totp_enabled = 0, totp_last_step = 0 WHERE id = ?",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = ?")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Accepts a TOTP time step only if it is newer than the last accepted
    /// one. Atomic, so a code cannot be used twice even concurrently.
    pub async fn accept_totp_step(&self, user_id: i64, step: i64) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE users SET totp_last_step = ?1 WHERE id = ?2 AND totp_last_step < ?1",
        )
        .bind(step)
        .bind(user_id)
        .execute(&self.writer)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Consumes a recovery code. Returns `false` if it is not valid.
    pub async fn use_recovery_code(&self, user_id: i64, code_hash: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM recovery_codes WHERE user_id = ? AND code_hash = ?")
            .bind(user_id)
            .bind(code_hash)
            .execute(&self.writer)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn recovery_codes_left(&self, user_id: i64) -> Result<i64> {
        Ok(
            sqlx::query("SELECT COUNT(*) AS n FROM recovery_codes WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(&self.pool)
                .await?
                .try_get("n")?,
        )
    }
}

#[cfg(test)]
mod tests;
