//! Backups, restore, process locking and safe migrations.
//!
//! * **Consistency:** backups use SQLite's `VACUUM INTO`, which reads the
//!   whole database inside one read transaction. In WAL mode writers keep
//!   going, and the copy is a consistent snapshot including committed pages
//!   that are still in the `-wal` file.
//! * **Atomicity:** a backup is written to a temporary file in the target
//!   directory, verified (`PRAGMA integrity_check`, migration table) and only
//!   then renamed to its final name, so a crash never leaves a truncated
//!   file under a backup name.
//! * **Running detection:** every process that opens the database holds a
//!   shared OS file lock on `<database>.lock` (`flock` on Unix, `LockFileEx`
//!   on Windows, via `std::fs::File::lock_shared`). Restore and migrations
//!   need the exclusive lock. The OS drops the lock when the process ends,
//!   even after a crash or `kill -9`, so there are no stale PID files. The
//!   lock file itself is never deleted (deleting lock files races).

use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sqlx::{
    Connection, Row, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
};

use crate::{MIGRATOR, Result, Store, StoreError, database_files, restrict_permissions};

/// How long to wait for the database lock held by another process.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// Options for [`Store::open_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreOptions {
    /// Back up an existing database before applying migrations.
    pub pre_migration_backup: bool,
    /// Number of pre-migration backups to keep (at least 1).
    pub pre_migration_backups_keep: usize,
    /// How long a query waits for a pooled connection (in particular the
    /// single write connection) before failing with [`StoreError::Busy`].
    /// Keep it below the request timeout, so that overload is answered
    /// with 503 and `Retry-After` instead of a request timeout.
    pub acquire_timeout: Duration,
    /// Days deleted objects stay in the trash; 0 disables the trash.
    pub trash_retention_days: u32,
}

/// Default of [`StoreOptions::acquire_timeout`].
pub const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            pre_migration_backup: true,
            pre_migration_backups_keep: 3,
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
            trash_retention_days: crate::DEFAULT_TRASH_RETENTION_DAYS,
        }
    }
}

/// Result of a backup or the verification of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupInfo {
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// Highest applied migration.
    pub migration_version: i64,
}

/// Result of [`restore_database`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreInfo {
    pub backup: BackupInfo,
    /// Where the replaced database was kept, if there was one.
    pub previous: Option<PathBuf>,
}

/// Highest migration this build knows.
pub fn latest_migration() -> i64 {
    known_versions().max().unwrap_or(0)
}

fn known_versions() -> impl Iterator<Item = i64> {
    MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .map(|m| m.version)
}

// ---------------------------------------------------------------------------
// Process lock
// ---------------------------------------------------------------------------

/// Advisory lock on `<database>.lock`, see the module documentation.
#[derive(Debug)]
pub(crate) struct DbLock {
    file: File,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockMode {
    Shared,
    Exclusive,
}

pub(crate) fn lock_path(database: &Path) -> PathBuf {
    with_suffix(database, ".lock")
}

impl DbLock {
    fn open(database: &Path) -> Result<Self> {
        let path = lock_path(database);
        let file = private_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| io_error(&path, "cannot open lock file", err))?;
        restrict_permissions(&path)?;
        Ok(Self { file, path })
    }

    /// Takes the lock, retrying for up to `wait`. Returns `false` if another
    /// process holds a conflicting lock.
    async fn acquire(&self, mode: LockMode, wait: Duration) -> Result<bool> {
        let start = std::time::Instant::now();
        loop {
            let result = match mode {
                LockMode::Shared => self.file.try_lock_shared(),
                LockMode::Exclusive => self.file.try_lock(),
            };
            match result {
                Ok(()) => return Ok(true),
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(err)) => {
                    return Err(io_error(&self.path, "cannot lock", err));
                }
            }
            if start.elapsed() >= wait {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn release(&self) -> Result<()> {
        self.file
            .unlock()
            .map_err(|err| io_error(&self.path, "cannot unlock", err))
    }
}

// ---------------------------------------------------------------------------
// Migration state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Applied {
    version: i64,
    success: bool,
    checksum: Vec<u8>,
}

/// Applied migrations, or `None` if the database has no migration table
/// (new, empty database or not a CalCraft database).
async fn applied_migrations(conn: &mut SqliteConnection) -> Result<Option<Vec<Applied>>> {
    let exists = sqlx::query(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(&mut *conn)
    .await?
    .is_some();
    if !exists {
        return Ok(None);
    }
    let rows =
        sqlx::query("SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *conn)
            .await?;
    let applied = rows
        .iter()
        .map(|row| {
            Ok(Applied {
                version: row.try_get("version")?,
                success: row.try_get("success")?,
                checksum: row.try_get("checksum")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(applied))
}

async fn pool_applied(pool: &SqlitePool) -> Result<Option<Vec<Applied>>> {
    let mut conn = pool.acquire().await?;
    applied_migrations(&mut conn).await
}

fn applied_version(applied: &[Applied]) -> i64 {
    applied.iter().map(|m| m.version).max().unwrap_or(0)
}

/// Checks that this build can work with the database and returns the
/// versions of pending migrations.
fn pending_migrations(applied: &[Applied]) -> Result<Vec<i64>> {
    if let Some(dirty) = applied.iter().find(|m| !m.success) {
        return Err(StoreError::DirtyMigration(dirty.version));
    }
    let latest = latest_migration();
    for m in applied {
        let Some(known) = MIGRATOR
            .iter()
            .find(|k| k.version == m.version && !k.migration_type.is_down_migration())
        else {
            return Err(StoreError::NewerSchema {
                version: applied_version(applied),
                known: latest,
            });
        };
        if *known.checksum != *m.checksum {
            return Err(StoreError::Corrupt(format!(
                "migration {} in the database differs from the one in this build",
                m.version
            )));
        }
    }
    Ok(known_versions()
        .filter(|v| !applied.iter().any(|m| m.version == *v))
        .collect())
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

/// How long a connection waits for a lock held by another connection.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

fn connect_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT)
}

async fn connect_pool(
    path: &Path,
    max_connections: u32,
    acquire_timeout: Duration,
    simulation: &crate::test_hooks::Simulation,
) -> Result<SqlitePool> {
    let options = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(acquire_timeout);
    Ok(simulation
        .hook(options)
        // On SQLITE_FULL or an I/O error SQLite may roll a transaction back
        // by itself. sqlx's ROLLBACK on drop then fails and the connection
        // stays marked as inside a transaction, so every later `BEGIN` on it
        // fails. Such connections are closed instead of reused. Normally the
        // rollback on drop is merely still queued: the ping waits for it.
        .after_release(|conn, _| {
            Box::pin(async move {
                if !conn.is_in_transaction() {
                    return Ok(true);
                }
                conn.ping().await?;
                Ok(!conn.is_in_transaction())
            })
        })
        .connect_with(connect_options(path).create_if_missing(true))
        .await?)
}

/// `true` if the database contains any schema object (tables, indexes, …).
async fn has_schema(pool: &SqlitePool) -> Result<bool> {
    Ok(sqlx::query("SELECT 1 FROM sqlite_master LIMIT 1")
        .fetch_optional(pool)
        .await?
        .is_some())
}

/// Migration state of an opened database: pending versions, or an error if
/// this build must not touch it.
async fn check_database(pool: &SqlitePool, path: &Path) -> Result<Vec<i64>> {
    match pool_applied(pool).await? {
        Some(applied) => pending_migrations(&applied),
        None if has_schema(pool).await? => Err(StoreError::NotCalcraft(path.display().to_string())),
        None => Ok(known_versions().collect()),
    }
}

impl Store {
    /// Opens (and creates, if missing) the database file, holding the shared
    /// process lock, and applies pending migrations.
    ///
    /// Migrations need the exclusive lock, i.e. no other CalCraft process
    /// may use the database. Before migrating an existing database a backup
    /// `<database>.pre-migration-<version>-<timestamp>` is written; if that
    /// fails, nothing is migrated. A database migrated by a newer version,
    /// and a non-empty SQLite file that is not a CalCraft database, are
    /// refused.
    ///
    /// Connections exist only while this process holds a lock: to switch
    /// between shared and exclusive (which the OS cannot do atomically), all
    /// connections are closed first and the database is opened again after
    /// the new lock is held. A restore that runs in such a gap therefore
    /// never replaces the file under an open connection; the state is simply
    /// checked again on the restored file.
    pub async fn open_with(path: &Path, options: &StoreOptions) -> Result<Self> {
        // SQLite keeps recent pages (including password hashes) in the
        // -wal and -shm files and reuses existing files with their
        // permissions, so all three are created owner-only up front.
        for file in database_files(path) {
            crate::create_private(&file)?;
        }
        let lock = Arc::new(DbLock::open(path)?);
        let simulation = crate::test_hooks::Simulation::default();
        // Normally one round; a second one after migrating. More rounds only
        // happen if other processes keep restoring or migrating concurrently.
        for _ in 0..4 {
            if !lock.acquire(LockMode::Shared, LOCK_WAIT).await? {
                return Err(StoreError::InUse(format!(
                    "database {} is locked exclusively ({}): a restore or migration is running",
                    path.display(),
                    lock.path.display()
                )));
            }
            let pool = connect_pool(path, 8, options.acquire_timeout, &simulation).await?;
            let pending = match check_database(&pool, path).await {
                Ok(pending) => pending,
                Err(err) => {
                    pool.close().await;
                    lock.release()?;
                    return Err(err);
                }
            };
            if pending.is_empty() {
                remove_stale_temp_files(path);
                for file in database_files(path) {
                    restrict_permissions(&file)?;
                }
                let writer = match connect_pool(path, 1, options.acquire_timeout, &simulation).await
                {
                    Ok(writer) => writer,
                    Err(err) => {
                        pool.close().await;
                        lock.release()?;
                        return Err(err);
                    }
                };
                return Ok(Self {
                    pool,
                    writer,
                    lock: Some(lock),
                    path: Some(Arc::new(path.to_path_buf())),
                    simulation,
                    settings: crate::Settings::new(options.trash_retention_days),
                    health: Arc::default(),
                });
            }
            pool.close().await;
            lock.release()?;

            if !lock.acquire(LockMode::Exclusive, LOCK_WAIT).await? {
                return Err(StoreError::InUse(format!(
                    "database {} needs migrations, but another CalCraft process is using it \
                     (is an older server still running?). Stop it and start again",
                    path.display()
                )));
            }
            let pool = connect_pool(path, 1, options.acquire_timeout, &simulation).await?;
            let store = Self {
                writer: pool.clone(),
                pool,
                lock: None,
                path: Some(Arc::new(path.to_path_buf())),
                simulation: simulation.clone(),
                settings: crate::Settings::new(options.trash_retention_days),
                health: Arc::default(),
            };
            let result = store.migrate(path, options).await;
            store.pool.close().await;
            lock.release()?;
            result?;
        }
        Err(StoreError::InUse(format!(
            "database {} keeps changing while opening it: another process restores or migrates it",
            path.display()
        )))
    }

    /// Applies pending migrations; the caller holds the exclusive lock.
    async fn migrate(&self, path: &Path, options: &StoreOptions) -> Result<()> {
        // Another process may have migrated or restored while we waited.
        let pending = check_database(&self.pool, path).await?;
        if pending.is_empty() {
            return Ok(());
        }
        let applied = pool_applied(&self.pool).await?.unwrap_or_default();
        let from = applied_version(&applied);
        let mut written = None;
        if !applied.is_empty() && options.pre_migration_backup {
            let backup = self
                .pre_migration_backup(path, from)
                .await
                .map_err(|err| StoreError::PreMigrationBackup(err.to_string()))?;
            tracing::info!(
                backup = %backup.path.display(),
                size = backup.size,
                version = from,
                "wrote pre-migration backup"
            );
            written = Some(backup.path);
        } else if !applied.is_empty() {
            tracing::warn!("pre-migration backup disabled (storage.pre_migration_backup = false)");
        }
        tracing::info!(
            from,
            to = latest_migration(),
            pending = ?pending,
            "applying database migrations"
        );
        MIGRATOR.run(&self.pool).await?;
        tracing::info!(version = latest_migration(), "database migrations applied");
        // Only after a successful migration, and never the backup just written.
        if let Some(written) = written {
            prune_pre_migration_backups(path, options.pre_migration_backups_keep.max(1), &written);
        }
        Ok(())
    }

    async fn pre_migration_backup(&self, path: &Path, version: i64) -> Result<BackupInfo> {
        let base = with_suffix(
            path,
            &format!(".pre-migration-{version:04}-{}", utc_timestamp()),
        );
        // Written to a temporary name and renamed at the end (backup_pool),
        // so a crash never leaves an empty file under a backup name. The
        // exclusive lock rules out races for the free name.
        self.backup(&free_name(&base)?, false).await
    }

    /// Closes all connections and releases the process lock (once all
    /// clones are dropped).
    pub async fn close(self) {
        self.writer.close().await;
        self.pool.close().await;
    }

    /// Writes a consistent copy of the database to `dest` (owner-only
    /// permissions), safe while other connections write. Refuses to replace
    /// an existing file unless `force` is set.
    pub async fn backup(&self, dest: &Path, force: bool) -> Result<BackupInfo> {
        if let Some(path) = &self.path {
            refuse_database_file(path, dest)?;
        }
        backup_pool(&self.pool, dest, force).await
    }
}

/// Backs up the database file at `database` without migrating it. Safe
/// while the server runs.
pub async fn backup_database(database: &Path, dest: &Path, force: bool) -> Result<BackupInfo> {
    if !database.is_file() {
        return Err(StoreError::Backup(format!(
            "database {} does not exist",
            database.display()
        )));
    }
    refuse_database_file(database, dest)?;
    for file in database_files(database) {
        crate::create_private(&file)?;
    }
    let lock = DbLock::open(database)?;
    if !lock.acquire(LockMode::Shared, LOCK_WAIT).await? {
        return Err(StoreError::InUse(format!(
            "database {} is locked exclusively: a restore or migration is running",
            database.display()
        )));
    }
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options(database))
        .await?;
    let result = async {
        if pool_applied(&pool).await?.is_none() {
            return Err(StoreError::Backup(format!(
                "{} is not a CalCraft database",
                database.display()
            )));
        }
        backup_pool(&pool, dest, force).await
    }
    .await;
    pool.close().await;
    drop(lock);
    result
}

async fn backup_pool(pool: &SqlitePool, dest: &Path, force: bool) -> Result<BackupInfo> {
    if !force && dest.exists() {
        return Err(StoreError::Backup(format!(
            "{} already exists (use --force to replace it)",
            dest.display()
        )));
    }
    let tmp = temp_path(dest)?;
    let result = async {
        // VACUUM INTO accepts an existing empty file and keeps its permissions.
        create_new_private(&tmp).map_err(|err| io_error(&tmp, "cannot create", err))?;
        let target = tmp.to_str().ok_or_else(|| {
            StoreError::Backup(format!("path {} is not valid UTF-8", tmp.display()))
        })?;
        sqlx::query("VACUUM INTO ?")
            .bind(target)
            .execute(pool)
            .await?;
        let (size, version, _) = verify_file(&tmp).await?;
        sync_file(&tmp)?;
        if !force && dest.exists() {
            return Err(StoreError::Backup(format!(
                "{} already exists (use --force to replace it)",
                dest.display()
            )));
        }
        std::fs::rename(&tmp, dest).map_err(|err| io_error(dest, "cannot write", err))?;
        restrict_permissions(dest)?;
        sync_parent(dest);
        Ok(BackupInfo {
            path: dest.to_path_buf(),
            size,
            migration_version: version,
        })
    }
    .await;
    if result.is_err() {
        remove_with_side_files(&tmp);
    }
    result
}

/// Opens the file as a self-contained database (switching it out of WAL
/// mode) and checks integrity and the migration table.
async fn verify_file(path: &Path) -> Result<(u64, i64, Vec<Applied>)> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Delete)
        .busy_timeout(Duration::from_secs(5));
    let mut conn = SqliteConnection::connect_with(&options)
        .await
        .map_err(|err| {
            StoreError::Backup(format!("{} is not a database: {err}", path.display()))
        })?;
    let result = async {
        let rows = sqlx::query("PRAGMA integrity_check")
            .fetch_all(&mut conn)
            .await?;
        let messages: Vec<String> = rows
            .iter()
            .map(|row| row.try_get::<String, _>(0))
            .collect::<Result<_, _>>()?;
        if messages != ["ok"] {
            return Err(StoreError::Backup(format!(
                "integrity check of {} failed: {}",
                path.display(),
                messages.join("; ")
            )));
        }
        let Some(applied) = applied_migrations(&mut conn).await? else {
            return Err(StoreError::Backup(format!(
                "{} is not a CalCraft database (no migration table)",
                path.display()
            )));
        };
        Ok(applied)
    }
    .await;
    conn.close().await?;
    let applied = result?;
    let size = std::fs::metadata(path)
        .map_err(|err| io_error(path, "cannot read", err))?
        .len();
    Ok((size, applied_version(&applied), applied))
}

// ---------------------------------------------------------------------------
// Restore
// ---------------------------------------------------------------------------

/// Replaces the database at `database` with the backup at `backup`.
///
/// Requires that no CalCraft process uses the database (exclusive lock).
/// The backup is copied next to the database and verified first; it must
/// not contain migrations unknown to this build. The current database is
/// kept as `<database>.before-restore-<timestamp>`.
pub async fn restore_database(database: &Path, backup: &Path) -> Result<RestoreInfo> {
    if !backup.is_file() {
        return Err(StoreError::Backup(format!(
            "backup {} does not exist",
            backup.display()
        )));
    }
    if same_file(database, backup) {
        return Err(StoreError::Backup(
            "the backup is the database itself".to_owned(),
        ));
    }
    if with_suffix(backup, "-wal").exists() {
        return Err(StoreError::Backup(format!(
            "{} has a -wal file next to it and is not a self-contained backup; \
             create backups with `calcraft backup`",
            backup.display()
        )));
    }
    let lock = DbLock::open(database)?;
    if !lock.acquire(LockMode::Exclusive, Duration::ZERO).await? {
        return Err(StoreError::InUse(format!(
            "database {} is in use by another CalCraft process (lock {} is held): \
             stop the server (e.g. `systemctl stop calcraft`) before restoring",
            database.display(),
            lock.path.display()
        )));
    }

    let tmp = temp_path(database)?;
    let result = restore_locked(database, backup, &tmp).await;
    if result.is_err() {
        remove_with_side_files(&tmp);
    }
    drop(lock);
    result
}

async fn restore_locked(database: &Path, backup: &Path, tmp: &Path) -> Result<RestoreInfo> {
    // Work on a private copy so the original backup is never modified.
    copy_into_new(backup, tmp)?;
    let (size, version, applied) = verify_file(tmp).await?;
    match pending_migrations(&applied) {
        Ok(_) => {}
        Err(StoreError::NewerSchema { version, known }) => {
            return Err(StoreError::Backup(format!(
                "backup {} was made by a newer CalCraft version (migration {version}, this \
                 version knows up to {known}); restore it with that version",
                backup.display()
            )));
        }
        Err(err) => return Err(err),
    }
    sync_file(tmp)?;

    let mut previous = None;
    if database.is_file() {
        checkpoint(database).await;
        let base = with_suffix(database, &format!(".before-restore-{}", utc_timestamp()));
        let kept = reserve_unique(&base)?;
        if let Err(err) = keep_copy(database, &kept) {
            // E.g. disk full: abort before touching the live database.
            remove_with_side_files(&kept);
            return Err(err);
        }
        previous = Some(kept);
    }
    let kept_hint = || match &previous {
        Some(kept) => format!("; the previous database is kept as {}", kept.display()),
        None => String::new(),
    };
    // The side files go before the new file is put in place: a crash in
    // between leaves the old (checkpointed, and copied above) database
    // without its log. The other order could leave the restored database
    // next to a stale -wal file, which SQLite would replay into it.
    for side in ["-wal", "-shm", "-journal"] {
        let file = with_suffix(database, side);
        remove_if_exists(&file)
            .map_err(|err| StoreError::Backup(format!("{err}{}", kept_hint())))?;
    }
    std::fs::rename(tmp, database).map_err(|err| {
        StoreError::Backup(format!(
            "cannot replace {}: {err}{}",
            database.display(),
            kept_hint()
        ))
    })?;
    restrict_permissions(database)?;
    sync_parent(database);
    Ok(RestoreInfo {
        backup: BackupInfo {
            path: backup.to_path_buf(),
            size,
            migration_version: version,
        },
        previous,
    })
}

/// Copies the database (and a non-empty -wal file) to `kept`, which was
/// reserved as an empty owner-only file.
fn keep_copy(database: &Path, kept: &Path) -> Result<()> {
    copy_into(database, kept)?;
    let wal = with_suffix(database, "-wal");
    if std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
        let kept_wal = with_suffix(kept, "-wal");
        create_new_private(&kept_wal).map_err(|err| io_error(&kept_wal, "cannot create", err))?;
        copy_into(&wal, &kept_wal)?;
    }
    Ok(())
}

/// Moves committed WAL pages into the main file, so that a copy of the main
/// file is complete. Best effort: a damaged database is copied as it is
/// (together with its -wal file).
async fn checkpoint(database: &Path) {
    let result = async {
        let mut conn = SqliteConnection::connect_with(&connect_options(database)).await?;
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&mut conn)
            .await?;
        conn.close().await
    }
    .await;
    if let Err(err) = result {
        tracing::warn!(%err, "checkpoint of the current database failed; copying it as is");
    }
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

fn pre_migration_prefix(database: &Path) -> Option<String> {
    let name = database.file_name()?.to_str()?;
    Some(format!("{name}.pre-migration-"))
}

/// Pre-migration backups of `database`, newest first.
pub fn pre_migration_backups(database: &Path) -> Vec<PathBuf> {
    let Some(prefix) = pre_migration_prefix(database) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent_dir(database)) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            let rest = name.strip_prefix(&prefix)?;
            // <version>-<timestamp>[-<n>]; side files (-wal) are not backups.
            let mut parts = rest.split('-');
            let _version = parts.next()?.parse::<i64>().ok()?;
            let timestamp = parts.next()?;
            let n = match parts.next() {
                Some(n) => n.parse::<u32>().ok()?,
                None => 0,
            };
            if parts.next().is_some() || timestamp.len() != 16 {
                return None;
            }
            // An empty file is never a valid backup.
            if entry.metadata().ok()?.len() == 0 {
                return None;
            }
            Some((format!("{timestamp}-{n:05}"), entry.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, path)| path).collect()
}

/// Deletes all but the newest `keep` pre-migration backups. `written` (the
/// backup of this run) always counts as one of them, whatever its time stamp.
fn prune_pre_migration_backups(database: &Path, keep: usize, written: &Path) {
    let others = pre_migration_backups(database)
        .into_iter()
        .filter(|path| !same_file(path, written) && path != written);
    for old in others.skip(keep.saturating_sub(1)) {
        match std::fs::remove_file(&old) {
            Ok(()) => tracing::info!(path = %old.display(), "removed old pre-migration backup"),
            Err(err) => {
                tracing::warn!(path = %old.display(), %err, "cannot remove old pre-migration backup");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// File helpers
// ---------------------------------------------------------------------------

pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// A unique temporary name in the directory of `path`.
fn temp_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| StoreError::Backup(format!("invalid file name {}", path.display())))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    Ok(parent_dir(path).join(format!(".{name}.tmp-{}-{nanos}", std::process::id())))
}

/// Absolute form of a path that may not exist yet (canonical directory plus
/// file name).
fn absolute(path: &Path) -> Option<PathBuf> {
    if let Ok(path) = path.canonicalize() {
        return Some(path);
    }
    Some(
        parent_dir(path)
            .canonicalize()
            .ok()?
            .join(path.file_name()?),
    )
}

/// Refuses a backup target that is the database or one of its side files.
fn refuse_database_file(database: &Path, dest: &Path) -> Result<()> {
    let Some(dest) = absolute(dest) else {
        return Ok(());
    };
    let mut protected: Vec<PathBuf> = database_files(database).to_vec();
    protected.push(with_suffix(database, "-journal"));
    protected.push(lock_path(database));
    if protected
        .iter()
        .filter_map(|p| absolute(p))
        .any(|p| p == dest)
    {
        return Err(StoreError::Backup(format!(
            "backup target {} is the database or one of its files",
            dest.display()
        )));
    }
    Ok(())
}

/// First of `base`, `base-1`, `base-2`, … that does not exist.
fn free_name(base: &Path) -> Result<PathBuf> {
    (0..1000)
        .map(|n| {
            if n == 0 {
                base.to_path_buf()
            } else {
                with_suffix(base, &format!("-{n}"))
            }
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| {
            StoreError::Backup(format!(
                "cannot find a free file name for {}",
                base.display()
            ))
        })
}

/// Temporary files older than this are left over from a crash.
const STALE_TEMP_AGE: Duration = Duration::from_secs(3600);

/// `true` for names written by [`temp_path`] for the database or one of its
/// pre-migration backups, including SQLite side files:
/// `.<db>.tmp-<pid>-<nanos>` and `.<db>.pre-migration-<v>-<ts>.tmp-<pid>-<nanos>`.
fn is_temp_name(name: &str, database_name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix('.')
        .and_then(|n| n.strip_prefix(database_name))
    else {
        return false;
    };
    let Some((middle, tail)) = rest.rsplit_once(".tmp-") else {
        return false;
    };
    if !(middle.is_empty() || middle.starts_with(".pre-migration-")) {
        return false;
    }
    let tail = ["-wal", "-shm", "-journal"]
        .iter()
        .find_map(|side| tail.strip_suffix(side))
        .unwrap_or(tail);
    let mut parts = tail.split('-');
    let digits =
        |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    digits(parts.next()) && digits(parts.next()) && parts.next().is_none()
}

/// Removes temporary files of interrupted backups/restores older than an hour.
fn remove_stale_temp_files(database: &Path) {
    let Some(database_name) = database.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(parent_dir(database)) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !is_temp_name(&name, database_name) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > STALE_TEMP_AGE);
        if !old || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => {
                tracing::info!(path = %entry.path().display(), "removed stale temporary file")
            }
            Err(err) => {
                tracing::warn!(path = %entry.path().display(), %err, "cannot remove stale temporary file")
            }
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn private_options() -> OpenOptions {
    #[allow(unused_mut)]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn create_new_private(path: &Path) -> io::Result<File> {
    private_options().write(true).create_new(true).open(path)
}

/// Creates `base` (or `base-1`, `base-2`, …) as an empty owner-only file.
fn reserve_unique(base: &Path) -> Result<PathBuf> {
    for n in 0..1000 {
        let candidate = if n == 0 {
            base.to_path_buf()
        } else {
            with_suffix(base, &format!("-{n}"))
        };
        match create_new_private(&candidate) {
            Ok(_) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(io_error(&candidate, "cannot create", err)),
        }
    }
    Err(StoreError::Backup(format!(
        "cannot find a free file name for {}",
        base.display()
    )))
}

/// Copies `src` into the new owner-only file `dst`.
fn copy_into_new(src: &Path, dst: &Path) -> Result<()> {
    create_new_private(dst).map_err(|err| io_error(dst, "cannot create", err))?;
    copy_into(src, dst)
}

/// Copies `src` into the existing file `dst` and syncs it.
fn copy_into(src: &Path, dst: &Path) -> Result<()> {
    let mut from = File::open(src).map_err(|err| io_error(src, "cannot read", err))?;
    let mut to = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(dst)
        .map_err(|err| io_error(dst, "cannot write", err))?;
    io::copy(&mut from, &mut to).map_err(|err| io_error(dst, "cannot write", err))?;
    to.sync_all()
        .map_err(|err| io_error(dst, "cannot sync", err))?;
    restrict_permissions(dst)
}

fn sync_file(path: &Path) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.sync_all())
        .map_err(|err| io_error(path, "cannot sync", err))
}

/// Persists a rename. Directories cannot be opened for syncing on Windows.
fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Err(err) = File::open(parent_dir(path)).and_then(|d| d.sync_all()) {
        tracing::debug!(%err, "cannot sync directory");
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io_error(path, "cannot remove", err)),
    }
}

fn remove_with_side_files(path: &Path) {
    for file in database_files(path) {
        let _ = std::fs::remove_file(file);
    }
    let _ = std::fs::remove_file(with_suffix(path, "-journal"));
}

fn io_error(path: &Path, what: &str, err: io::Error) -> StoreError {
    StoreError::Backup(format!("{what} {}: {err}", path.display()))
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// Current UTC time as `YYYYMMDDTHHMMSSZ` (sorts chronologically).
pub(crate) fn utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc(secs)
}

fn format_utc(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests;
