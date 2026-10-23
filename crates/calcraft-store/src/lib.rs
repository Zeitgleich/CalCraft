//! Storage layer of CalCraft.
//!
//! SQLite is the default backend. All queries are parameterised; no SQL is
//! ever built from user input. A PostgreSQL backend and a storage trait
//! follow once a second backend exists.

use std::{
    fmt,
    path::Path,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering},
    },
};

use calcraft_core::{CollectionKind, Slug, Username};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};

mod accounts;
mod attachments;
mod backup;
mod health;
mod items;
mod oidc;
mod properties;
mod publish;
mod scheduling;
mod sharing;
mod subscriptions;
mod test_hooks;
mod trash;

pub use accounts::{
    AppPassword, MAX_APP_PASSWORDS, RECOVERY_CODES, Session, TotpEnableOutcome, TotpState,
};
pub use attachments::{
    AttachmentMeta, MAX_REFS_PER_ITEM, NewAttachment, UNREFERENCED_GRACE_SECS, valid_managed_id,
};
pub use backup::{
    BackupInfo, DEFAULT_ACQUIRE_TIMEOUT, RestoreInfo, StoreOptions, backup_database,
    latest_migration, pre_migration_backups, restore_database,
};
pub use health::{PROBE_ACQUIRE_TIMEOUT, PROBE_BUSY_TIMEOUT, Readiness};
pub use items::{
    Change, Item, ItemIndex, ItemMeta, NewItem, Precondition, PutOutcome, QueryCandidate,
    ScheduleTag, StaleIndex, Transfer, TransferOutcome,
};
pub use items::{MAX_BYTES_PER_USER, MAX_ITEMS_PER_COLLECTION};
pub use oidc::{ApprovedBinding, OidcBinding, OidcIdentityInfo};
pub use properties::{DeadProperty, MAX_DEAD_PROPERTIES, MAX_DEAD_PROPERTY_BYTES, PropertyChange};
pub use publish::{MAX_LINKS_PER_CALENDAR, NewPublishLink, PrivateEvents, PublishLink};
pub use scheduling::{
    FreeBusyCandidate, InboxMessage, MAX_EMAIL_LEN, NewInboxMessage, SchedulingObject,
};
pub use sharing::{
    MAX_PENDING_SHARES_PER_USER, MAX_SHARES_PER_COLLECTION, SetShare, Share, ShareAccess,
    ShareView, SharedVia,
};
pub use subscriptions::{
    FeedItem, FeedState, NewSubscription, ReplaceOutcome, Subscription, SubscriptionFailure,
};
#[cfg(feature = "test-hooks")]
pub use test_hooks::SimulatedFailure;
pub use trash::{
    DEFAULT_TRASH_RETENTION_DAYS, MAX_EVICT_ROWS, MAX_TRASH_ROWS_PER_USER, RestoredEntry,
    TrashConflict, TrashEntry, TrashEntryKind, TrashPage,
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Slug of the calendar and address book created for every new user.
pub const DEFAULT_COLLECTION_SLUG: &str = "default";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(sqlx::Error),
    /// The disk (or the database size limit) is full (`SQLITE_FULL`). The
    /// failed transaction was rolled back; DAV answers 507.
    #[error("database or disk is full")]
    Full,
    /// The database stayed locked (`SQLITE_BUSY`/`SQLITE_LOCKED` after the
    /// busy timeout, or no connection within the pool timeout). Transient;
    /// DAV answers 503 with `Retry-After`.
    #[error("database is busy")]
    Busy,
    #[error("database migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("user {0} already exists")]
    UserExists(Username),
    /// Sharing: the user to share with does not exist, is disabled or is
    /// the owner.
    #[error("no such user to share with")]
    NoSuchSharee,
    /// A sharee's write: the share that granted it is gone (removed,
    /// downgraded, owner or sharee disabled) since the request resolved it.
    /// DAV answers 404, like for any path the user cannot see.
    #[error("share no longer exists")]
    ShareRevoked,
    /// The calendar user address is already used by another user.
    #[error("address already in use")]
    AddressTaken,
    /// A scheduling delivery was refused because the organizer's object
    /// changed since (a newer delivery follows).
    #[error("superseded by a newer change")]
    Superseded,
    #[error("corrupt data in database: {0}")]
    Corrupt(String),
    #[error("failed to set permissions on database file: {0}")]
    Permissions(std::io::Error),
    /// If-Match / If-None-Match did not hold.
    #[error("precondition failed")]
    PreconditionFailed,
    /// Another resource in the collection already uses this UID.
    #[error("UID already used by {0}")]
    UidConflict(String),
    #[error("collection already exists")]
    CollectionExists,
    /// COPY/MOVE: the destination collection does not exist (or is not one
    /// of the owner's collections of the source's kind).
    #[error("destination collection does not exist")]
    MissingDestination,
    /// COPY/MOVE: source and destination are the same resource.
    #[error("source and destination are the same")]
    SameResource,
    /// COPY/MOVE: the destination calendar does not accept the component type.
    #[error("component type not supported by the destination")]
    UnsupportedComponent,
    #[error("limit reached: {0}")]
    Limit(&'static str),
    /// The collection is a subscription: its objects cannot be changed.
    #[error("collection is read-only")]
    ReadOnly,
    /// A trash entry cannot be restored (see [`TrashConflict`]).
    #[error("cannot restore: {0}")]
    TrashConflict(TrashConflict),
    #[error("{0}")]
    Backup(String),
    #[error("{0}")]
    InUse(String),
    #[error(
        "the database was migrated by a newer CalCraft version (migration {version}, this \
         version knows up to {known}). Downgrading in place is not supported: run the newer \
         version again, or restore the pre-migration backup (calcraft.db.pre-migration-*) with \
         this version, see docs/backup-and-upgrade.md"
    )]
    NewerSchema { version: i64, known: i64 },
    #[error(
        "migration {0} failed earlier and left the database in an unknown state: restore the \
         pre-migration backup, see docs/backup-and-upgrade.md"
    )]
    DirtyMigration(i64),
    #[error(
        "{0} is a non-empty SQLite database without CalCraft's migration table; refusing to \
         migrate it (check storage.database)"
    )]
    NotCalcraft(String),
    #[error("backup before migrating failed, nothing was migrated: {0}")]
    PreMigrationBackup(String),
}

/// The SQLite result code (extended) of a database error.
fn sqlite_code(err: &sqlx::Error) -> Option<i64> {
    match err {
        sqlx::Error::Database(db) => db.code().and_then(|code| code.parse().ok()),
        _ => None,
    }
}

impl StoreError {
    /// The SQLite result code (extended) behind this error, for logs.
    pub fn sqlite_code(&self) -> Option<i64> {
        match self {
            Self::Database(err) => sqlite_code(err),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for StoreError {
    fn from(err: sqlx::Error) -> Self {
        // Primary result codes; extended codes keep them in the low byte.
        const SQLITE_BUSY: i64 = 5;
        const SQLITE_LOCKED: i64 = 6;
        const SQLITE_FULL: i64 = 13;
        let primary = sqlite_code(&err).map(|code| code & 0xff);
        if primary == Some(SQLITE_FULL) {
            Self::Full
        } else if matches!(primary, Some(SQLITE_BUSY | SQLITE_LOCKED))
            || matches!(err, sqlx::Error::PoolTimedOut)
        {
            Self::Busy
        } else {
            Self::Database(err)
        }
    }
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// A user account without credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub username: Username,
    pub display_name: Option<String>,
    pub disabled: bool,
    pub is_admin: bool,
    /// Two-factor authentication is active; the account password is then
    /// only accepted together with a TOTP code, never by DAV clients.
    pub totp_enabled: bool,
}

/// A user together with the password hash, for authentication only.
#[derive(Clone)]
pub struct UserCredentials {
    pub user: User,
    pub password_hash: String,
}

impl fmt::Debug for UserCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserCredentials")
            .field("user", &self.user)
            .field("password_hash", &"<redacted>")
            .finish()
    }
}

/// A calendar or address book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: i64,
    pub owner_id: i64,
    pub kind: CollectionKind,
    pub slug: Slug,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    pub sync_revision: i64,
    /// Comma-separated component types, e.g. `VEVENT,VTODO`.
    pub components: String,
    /// Oldest revision a sync token may refer to (older tombstones were pruned).
    pub sync_floor: i64,
    /// Random value bound into sync tokens.
    pub token_salt: i64,
    /// A calendar subscription: only the server's feed refresher changes
    /// its objects; DAV and API writes are refused. Also set for a
    /// collection seen through a read-only share.
    pub read_only: bool,
    /// Set when the collection is seen by a sharee through a share
    /// ([`Store::shared_collection`]): `slug`, `display_name` and `color`
    /// are then the sharee's, `id` and `owner_id` the owner's. `None` for
    /// the owner's own view.
    pub shared: Option<SharedVia>,
}

/// Handle to the database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Store {
    pub(crate) pool: SqlitePool,
    /// The single connection all writes use. SQLite allows one writer at a
    /// time anyway; queueing writers here (sqlx hands out connections in
    /// FIFO order) instead of in SQLite's busy handler, which polls and
    /// lets a waiter lose again and again until `busy_timeout` expires,
    /// keeps a busy server from failing writes with `SQLITE_BUSY`. Other
    /// processes (CLI backup, import) still meet the busy timeout.
    pub(crate) writer: SqlitePool,
    /// Shared process lock on `<database>.lock`; `None` in memory.
    #[allow(dead_code)]
    lock: Option<Arc<backup::DbLock>>,
    /// Database file; `None` in memory.
    path: Option<Arc<std::path::PathBuf>>,
    /// Simulated failures (feature `test-hooks`; empty otherwise).
    #[allow(dead_code)]
    simulation: test_hooks::Simulation,
    /// Runtime settings shared by all clones.
    pub(crate) settings: Arc<Settings>,
    /// Recorded write failures, for [`Store::readiness`].
    pub(crate) health: Arc<health::WriteHealth>,
}

/// Settings of a [`Store`] that are not part of the schema.
#[derive(Debug)]
pub(crate) struct Settings {
    /// Days deleted objects stay in the trash; 0 disables the trash.
    trash_retention_days: AtomicU32,
    /// Storage quota per user (objects plus trash).
    quota_bytes: AtomicI64,
    /// Rows (objects and collections) in the trash per user.
    max_trash_rows: AtomicI64,
    /// Rows one delete may remove synchronously from a full trash.
    max_evict_rows: AtomicI64,
    /// Entries were hidden for the purge; run it soon.
    purge_requested: AtomicBool,
}

impl Settings {
    pub(crate) fn new(trash_retention_days: u32) -> Arc<Self> {
        Arc::new(Self {
            trash_retention_days: AtomicU32::new(trash_retention_days),
            quota_bytes: AtomicI64::new(items::MAX_BYTES_PER_USER),
            max_trash_rows: AtomicI64::new(trash::MAX_TRASH_ROWS_PER_USER),
            max_evict_rows: AtomicI64::new(trash::MAX_EVICT_ROWS),
            purge_requested: AtomicBool::new(false),
        })
    }

    pub(crate) fn trash_retention_days(&self) -> u32 {
        self.trash_retention_days.load(Ordering::Relaxed)
    }

    pub(crate) fn quota_bytes(&self) -> i64 {
        self.quota_bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn max_trash_rows(&self) -> i64 {
        self.max_trash_rows.load(Ordering::Relaxed)
    }

    pub(crate) fn max_evict_rows(&self) -> i64 {
        self.max_evict_rows.load(Ordering::Relaxed)
    }

    pub(crate) fn request_purge(&self) {
        self.purge_requested.store(true, Ordering::Relaxed);
    }

    pub(crate) fn take_purge_request(&self) -> bool {
        self.purge_requested.swap(false, Ordering::Relaxed)
    }
}

impl Store {
    /// Opens (and creates, if missing) the database file and applies
    /// migrations with default [`StoreOptions`], see [`Store::open_with`].
    pub async fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, &StoreOptions::default()).await
    }

    /// Opens a private in-memory database, for tests.
    pub async fn in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        // A single connection, because every connection gets its own in-memory database.
        let simulation = test_hooks::Simulation::default();
        let pool = simulation
            .hook(
                SqlitePoolOptions::new()
                    .max_connections(1)
                    .acquire_timeout(DEFAULT_ACQUIRE_TIMEOUT),
            )
            .connect_with(options)
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self {
            writer: pool.clone(),
            pool,
            lock: None,
            path: None,
            simulation,
            settings: Settings::new(DEFAULT_TRASH_RETENTION_DAYS),
            health: Arc::default(),
        })
    }

    /// Days deleted objects stay in the trash; 0 means the trash is
    /// disabled and deletions are final.
    pub fn trash_retention_days(&self) -> u32 {
        self.settings.trash_retention_days()
    }

    /// Changes the trash retention (`storage.trash_retention_days`).
    pub fn set_trash_retention_days(&self, days: u32) {
        self.settings
            .trash_retention_days
            .store(days, Ordering::Relaxed);
    }

    /// Lowers the per-user quota and the trash size limit, for tests of
    /// the limits (feature `test-hooks`).
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_limits_for_testing(&self, quota_bytes: i64, max_trash_rows: i64) {
        self.settings
            .quota_bytes
            .store(quota_bytes, Ordering::Relaxed);
        self.settings
            .max_trash_rows
            .store(max_trash_rows, Ordering::Relaxed);
    }

    /// Lowers the number of rows a delete may remove synchronously from a
    /// full trash, for tests (feature `test-hooks`).
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_eviction_budget_for_testing(&self, rows: i64) {
        self.settings.max_evict_rows.store(rows, Ordering::Relaxed);
    }

    /// Checks that the database is reachable (reads only; see
    /// [`Store::readiness`] for writes).
    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Makes the database behave as if its file were read-only or the disk
    /// full, or back to normal, for failure tests (feature `test-hooks`).
    /// Every connection gets `PRAGMA query_only` / `max_page_count` when it
    /// is opened or acquired.
    #[cfg(feature = "test-hooks")]
    pub fn simulate_failure(&self, failure: SimulatedFailure) {
        self.simulation.set(failure);
    }

    /// Lowers the storage quota per user to `bytes` (0: the real quota),
    /// so quota tests need not write 512 MiB (feature `test-hooks`).
    #[cfg(feature = "test-hooks")]
    pub fn set_quota_for_tests(&self, bytes: i64) {
        let bytes = if bytes > 0 {
            bytes
        } else {
            items::MAX_BYTES_PER_USER
        };
        self.settings.quota_bytes.store(bytes, Ordering::Relaxed);
    }

    /// Creates a user with a default calendar and address book.
    pub async fn create_user(
        &self,
        username: &Username,
        password_hash: &str,
        display_name: Option<&str>,
    ) -> Result<User> {
        self.create_user_with_role(username, password_hash, display_name, false)
            .await
    }

    /// Like [`Store::create_user`], setting admin rights in the same transaction.
    pub async fn create_user_with_role(
        &self,
        username: &Username,
        password_hash: &str,
        display_name: Option<&str>,
        is_admin: bool,
    ) -> Result<User> {
        self.observe(
            async {
                // Write transactions start IMMEDIATE (see `put_item`): a deferred one that
                // reads first fails with SQLITE_BUSY instead of waiting when another
                // writer commits in between.
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                let result = sqlx::query(
                    "INSERT INTO users (username, password_hash, display_name, is_admin) VALUES (?, ?, ?, ?) RETURNING id",
                )
                .bind(username.as_str())
                .bind(password_hash)
                .bind(display_name)
                .bind(is_admin)
                .fetch_one(&mut *tx)
                .await;
                let id: i64 = match result {
                    Ok(row) => row.try_get("id")?,
                    Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
                        return Err(StoreError::UserExists(username.clone()));
                    }
                    Err(err) => return Err(err.into()),
                };
                for (kind, name) in [
                    (CollectionKind::Calendar, "Calendar"),
                    (CollectionKind::Addressbook, "Contacts"),
                ] {
                    sqlx::query(
                        "INSERT INTO collections (owner_id, kind, slug, display_name, token_salt) VALUES (?, ?, ?, ?, abs(random()))",
                    )
                    .bind(id)
                    .bind(kind.as_str())
                    .bind(DEFAULT_COLLECTION_SLUG)
                    .bind(name)
                    .execute(&mut *tx)
                    .await?;
                }
                tx.commit().await?;
                Ok(User {
                    id,
                    username: username.clone(),
                    display_name: display_name.map(str::to_owned),
                    disabled: false,
                    is_admin,
                    totp_enabled: false,
                })
            }
            .await,
        )
    }

    /// Looks up a user including the password hash.
    pub async fn user_credentials(&self, username: &Username) -> Result<Option<UserCredentials>> {
        let row = sqlx::query(
            "SELECT id, username, display_name, disabled, is_admin, totp_enabled, password_hash FROM users WHERE username = ?",
        )
        .bind(username.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(UserCredentials {
                user: user_from_row(&row)?,
                password_hash: row.try_get("password_hash")?,
            })
        })
        .transpose()
    }

    pub async fn list_users(&self) -> Result<Vec<User>> {
        let rows =
            sqlx::query("SELECT id, username, display_name, disabled, is_admin, totp_enabled FROM users ORDER BY username")
                .fetch_all(&self.pool)
                .await?;
        rows.iter().map(user_from_row).collect()
    }

    /// Replaces the password hash. Returns `false` if the user does not exist.
    pub async fn set_password_hash(
        &self,
        username: &Username,
        password_hash: &str,
    ) -> Result<bool> {
        self.observe(
            async {
                let result = sqlx::query("UPDATE users SET password_hash = ? WHERE username = ?")
                    .bind(password_hash)
                    .bind(username.as_str())
                    .execute(&self.writer)
                    .await?;
                Ok(result.rows_affected() == 1)
            }
            .await,
        )
    }

    /// Enables or disables a user. Returns `false` if the user does not exist.
    pub async fn set_disabled(&self, username: &Username, disabled: bool) -> Result<bool> {
        self.observe(
            async {
                let result = sqlx::query("UPDATE users SET disabled = ? WHERE username = ?")
                    .bind(disabled)
                    .bind(username.as_str())
                    .execute(&self.writer)
                    .await?;
                Ok(result.rows_affected() == 1)
            }
            .await,
        )
    }

    /// All collections of one kind owned by a user.
    pub async fn collections(
        &self,
        owner_id: i64,
        kind: CollectionKind,
    ) -> Result<Vec<Collection>> {
        let rows = sqlx::query(
            "SELECT id, owner_id, kind, slug, display_name, description, color, sync_revision, components, sync_floor, token_salt, read_only \
             FROM collections WHERE owner_id = ? AND kind = ? ORDER BY slug",
        )
        .bind(owner_id)
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(collection_from_row).collect()
    }

    /// A single collection of a user.
    pub async fn collection(
        &self,
        owner_id: i64,
        kind: CollectionKind,
        slug: &Slug,
    ) -> Result<Option<Collection>> {
        let row = sqlx::query(
            "SELECT id, owner_id, kind, slug, display_name, description, color, sync_revision, components, sync_floor, token_salt, read_only \
             FROM collections WHERE owner_id = ? AND kind = ? AND slug = ?",
        )
        .bind(owner_id)
        .bind(kind.as_str())
        .bind(slug.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(collection_from_row).transpose()
    }
}

pub(crate) fn user_from_row(row: &SqliteRow) -> Result<User> {
    let username: String = row.try_get("username")?;
    Ok(User {
        id: row.try_get("id")?,
        username: username
            .try_into()
            .map_err(|err| StoreError::Corrupt(format!("{err}")))?,
        display_name: row.try_get("display_name")?,
        disabled: row.try_get("disabled")?,
        is_admin: row.try_get("is_admin")?,
        totp_enabled: row.try_get("totp_enabled")?,
    })
}

pub(crate) fn collection_from_row(row: &SqliteRow) -> Result<Collection> {
    let kind: String = row.try_get("kind")?;
    let slug: String = row.try_get("slug")?;
    Ok(Collection {
        id: row.try_get("id")?,
        owner_id: row.try_get("owner_id")?,
        kind: kind
            .parse()
            .map_err(|()| StoreError::Corrupt(format!("unknown collection kind {kind:?}")))?,
        slug: slug
            .try_into()
            .map_err(|err| StoreError::Corrupt(format!("{err}")))?,
        display_name: row.try_get("display_name")?,
        description: row.try_get("description")?,
        color: row.try_get("color")?,
        sync_revision: row.try_get("sync_revision")?,
        components: row.try_get("components")?,
        sync_floor: row.try_get("sync_floor")?,
        token_salt: row.try_get("token_salt")?,
        read_only: row.try_get("read_only")?,
        shared: None,
    })
}

/// The database file and its WAL companions.
fn database_files(path: &Path) -> [std::path::PathBuf; 3] {
    [
        path.to_path_buf(),
        backup::with_suffix(path, "-wal"),
        backup::with_suffix(path, "-shm"),
    ]
}

/// Creates an empty file with owner-only permissions if it does not exist.
#[cfg(unix)]
fn create_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(StoreError::Permissions(err)),
    }
}

/// On Windows the files inherit the ACL of their directory.
#[cfg(not(unix))]
fn create_private(_path: &Path) -> Result<()> {
    Ok(())
}

/// Makes a file readable by the owner only, if it exists.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(StoreError::Permissions(err)),
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
