use std::sync::atomic::{AtomicU32, Ordering};

use calcraft_core::CollectionKind;
use sqlx::sqlite::SqlitePoolOptions;

use super::*;
use crate::{DEFAULT_COLLECTION_SLUG, NewItem, Precondition};

/// A fresh directory per test; removed on drop (best effort on Windows,
/// where files may still be open for a moment).
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "calcraft-backup-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn raw(path: &Path) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options(path).create_if_missing(true))
        .await
        .unwrap()
}

async fn version_of(path: &Path) -> i64 {
    let pool = raw(path).await;
    let applied = pool_applied(&pool).await.unwrap().unwrap_or_default();
    pool.close().await;
    applied_version(&applied)
}

/// A database with only the migrations up to `version` applied, and one
/// user with a calendar object (if the schema has items).
async fn old_database(path: &Path, version: i64) {
    let pool = raw(path).await;
    MIGRATOR.run_to(version, &pool).await.unwrap();
    sqlx::query("INSERT INTO users (username, password_hash) VALUES ('old', 'hash')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO collections (owner_id, kind, slug, sync_revision) VALUES (1, 'calendar', 'default', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    if version >= 2 {
        sqlx::query(
            "INSERT INTO items (collection_id, name, uid, etag, component, content, revision) \
             VALUES (1, 'a.ics', 'uid-a', '\"etag-a\"', 'VEVENT', x'00', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
    }
    pool.close().await;
}

fn event(uid: &str) -> Vec<u8> {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART:20260101T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
    .into_bytes()
}

async fn put(store: &Store, collection: i64, name: &str, etag: &str) {
    let uid = name.trim_end_matches(".ics");
    let content = event(uid);
    store
        .put_item(
            collection,
            name,
            &NewItem {
                uid,
                component: "VEVENT",
                range_start: None,
                range_end: None,
                recurring: false,
                index_version: 0,
                etag,
                content: &content,
                attachments: &[],
            },
            &Precondition::None,
        )
        .await
        .unwrap();
}

async fn user_calendar(store: &Store, name: &str) -> i64 {
    let user = store
        .create_user(&name.parse().unwrap(), "hash", None)
        .await
        .unwrap();
    calendar(store, user.id).await.id
}

async fn calendar(store: &Store, owner: i64) -> crate::Collection {
    store
        .collection(
            owner,
            CollectionKind::Calendar,
            &DEFAULT_COLLECTION_SLUG.parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap()
}

#[cfg(unix)]
fn assert_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{}", path.display());
}

#[cfg(not(unix))]
fn assert_private(_path: &Path) {}

#[test]
fn timestamps_are_utc_and_sortable() {
    assert_eq!(format_utc(0), "19700101T000000Z");
    assert_eq!(format_utc(951_868_798), "20000229T235958Z");
    assert_eq!(format_utc(1_792_586_096), "20261021T123456Z");
    assert_eq!(utc_timestamp().len(), 16);
}

#[tokio::test]
async fn backup_while_writing_is_consistent() {
    let dir = TempDir::new("concurrent");
    let db = dir.join("calcraft.db");
    let store = Store::open(&db).await.unwrap();
    let collection = user_calendar(&store, "alice").await;

    let writer = {
        let store = store.clone();
        tokio::spawn(async move {
            for i in 0..200 {
                put(
                    &store,
                    collection,
                    &format!("e{i}.ics"),
                    &format!("\"{i}\""),
                )
                .await;
                if i % 3 == 0 {
                    store
                        .delete_item(collection, &format!("e{}.ics", i / 2), &Precondition::None)
                        .await
                        .unwrap();
                }
            }
        })
    };
    let mut backups = Vec::new();
    for n in 0..8 {
        let dest = dir.join(&format!("backup-{n}.db"));
        let info = store.backup(&dest, false).await.unwrap();
        assert_eq!(info.migration_version, latest_migration());
        assert_private(&dest);
        backups.push(dest);
        tokio::task::yield_now().await;
    }
    writer.await.unwrap();

    for dest in backups {
        // Every snapshot satisfies the invariants a half-applied write would break.
        let pool = raw(&dest).await;
        let row = sqlx::query(
            "SELECT c.sync_revision AS rev, \
                    (SELECT COALESCE(MAX(revision), 0) FROM changes WHERE collection_id = c.id) AS max_change, \
                    (SELECT COUNT(*) FROM items WHERE collection_id = c.id) AS items, \
                    (SELECT COUNT(*) FROM changes WHERE collection_id = c.id AND deleted = 0) AS live \
             FROM collections c WHERE c.id = ?",
        )
        .bind(collection)
        .fetch_one(&pool)
        .await
        .unwrap();
        let rev: i64 = row.get("rev");
        let max_change: i64 = row.get("max_change");
        let items: i64 = row.get("items");
        let live: i64 = row.get("live");
        assert_eq!(rev, max_change, "{}", dest.display());
        assert_eq!(items, live, "{}", dest.display());
        let check: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(check, "ok");
        pool.close().await;
    }
}

#[tokio::test]
async fn backup_refuses_to_overwrite_without_force() {
    let dir = TempDir::new("overwrite");
    let db = dir.join("calcraft.db");
    let store = Store::open(&db).await.unwrap();
    let dest = dir.join("backup.db");
    std::fs::write(&dest, b"keep me").unwrap();
    let err = store.backup(&dest, false).await.unwrap_err();
    assert!(err.to_string().contains("--force"), "{err}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"keep me");
    store.backup(&dest, true).await.unwrap();
    assert_private(&dest);
    // No temporary files are left behind.
    let names: Vec<String> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(!names.iter().any(|n| n.contains(".tmp-")), "{names:?}");
    store.close().await;
}

#[tokio::test]
async fn backup_database_does_not_migrate() {
    let dir = TempDir::new("nomigrate");
    let db = dir.join("calcraft.db");
    old_database(&db, 3).await;
    let dest = dir.join("backup.db");
    let info = backup_database(&db, &dest, false).await.unwrap();
    assert_eq!(info.migration_version, 3);
    assert_eq!(version_of(&db).await, 3);
    assert_eq!(version_of(&dest).await, 3);

    // A database that is not CalCraft's is refused.
    let other = dir.join("other.db");
    let pool = raw(&other).await;
    sqlx::query("CREATE TABLE t (x)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let err = backup_database(&other, &dir.join("o.bak"), false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a CalCraft database"), "{err}");
    assert!(
        backup_database(&dir.join("missing.db"), &dest, true)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn restore_round_trip() {
    let dir = TempDir::new("roundtrip");
    let db = dir.join("calcraft.db");
    let store = Store::open(&db).await.unwrap();
    let collection = user_calendar(&store, "alice").await;
    put(&store, collection, "a.ics", "\"a1\"").await;
    put(&store, collection, "b.ics", "\"b1\"").await;
    let owner = store.list_users().await.unwrap()[0].id;
    let before = calendar(&store, owner).await;
    let changes_before = store.changes_since(collection, 0, 100).await.unwrap();
    let backup = dir.join("backup.db");
    store.backup(&backup, false).await.unwrap();

    // Change everything after the backup.
    store
        .delete_item(collection, "a.ics", &Precondition::None)
        .await
        .unwrap();
    put(&store, collection, "b.ics", "\"b2\"").await;
    put(&store, collection, "c.ics", "\"c1\"").await;
    store
        .create_user(&"bob".parse().unwrap(), "hash", None)
        .await
        .unwrap();
    store.close().await;

    let info = restore_database(&db, &backup).await.unwrap();
    assert_eq!(info.backup.migration_version, latest_migration());
    let previous = info.previous.unwrap();
    assert!(
        previous
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("calcraft.db.before-restore-")
    );
    assert_private(&previous);
    assert_private(&db);
    assert!(backup.is_file(), "the backup itself is kept");

    let store = Store::open(&db).await.unwrap();
    let users = store.list_users().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(
        store.item(collection, "a.ics").await.unwrap().unwrap().etag,
        "\"a1\""
    );
    assert_eq!(
        store.item(collection, "b.ics").await.unwrap().unwrap().etag,
        "\"b1\""
    );
    assert!(store.item(collection, "c.ics").await.unwrap().is_none());
    let after = calendar(&store, owner).await;
    assert_eq!(
        after, before,
        "sync revision, floor and token salt are restored"
    );
    let changes_after = store.changes_since(collection, 0, 100).await.unwrap();
    assert_eq!(format!("{changes_after:?}"), format!("{changes_before:?}"));
    store.close().await;

    // The previous database is a complete copy of the state before the restore.
    let kept = Store::open(&previous).await.unwrap();
    assert_eq!(kept.list_users().await.unwrap().len(), 2);
    assert!(kept.item(collection, "c.ics").await.unwrap().is_some());
    kept.close().await;
}

#[tokio::test]
async fn restore_refuses_while_database_is_open() {
    let dir = TempDir::new("inuse");
    let db = dir.join("calcraft.db");
    let store = Store::open(&db).await.unwrap();
    let backup = dir.join("backup.db");
    store.backup(&backup, false).await.unwrap();
    let err = restore_database(&db, &backup).await.unwrap_err();
    assert!(matches!(err, StoreError::InUse(_)), "{err}");
    assert!(err.to_string().contains("stop the server"), "{err}");
    store.close().await;
    restore_database(&db, &backup).await.unwrap();
}

#[tokio::test]
async fn restore_refuses_newer_backup() {
    let dir = TempDir::new("newer");
    let db = dir.join("calcraft.db");
    let store = Store::open(&db).await.unwrap();
    user_calendar(&store, "alice").await;
    let backup = dir.join("backup.db");
    store.backup(&backup, false).await.unwrap();
    store.close().await;
    let pool = raw(&backup).await;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (9999, 'future', 1, x'00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("PRAGMA journal_mode = DELETE")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let err = restore_database(&db, &backup).await.unwrap_err();
    assert!(err.to_string().contains("newer CalCraft version"), "{err}");
    // Nothing was replaced and no copies were left behind.
    let store = Store::open(&db).await.unwrap();
    assert_eq!(store.list_users().await.unwrap().len(), 1);
    store.close().await;
    let names: Vec<String> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.contains(".tmp-") || n.contains("before-restore")),
        "{names:?}"
    );

    // A corrupt or foreign file is refused as well.
    let junk = dir.join("junk.db");
    std::fs::write(&junk, b"not a database at all, just text").unwrap();
    assert!(restore_database(&db, &junk).await.is_err());
}

#[tokio::test]
async fn opening_old_schema_writes_pre_migration_backup() {
    let dir = TempDir::new("premigration");
    let db = dir.join("calcraft.db");
    old_database(&db, 3).await;

    let store = Store::open(&db).await.unwrap();
    let backups = pre_migration_backups(&db);
    assert_eq!(backups.len(), 1, "{backups:?}");
    let name = backups[0].file_name().unwrap().to_str().unwrap().to_owned();
    assert!(
        name.starts_with("calcraft.db.pre-migration-0003-"),
        "{name}"
    );
    assert_private(&backups[0]);
    // Migrated database: data intact, latest schema.
    assert_eq!(
        store.list_users().await.unwrap()[0].username.as_str(),
        "old"
    );
    assert_eq!(
        store.item(1, "a.ics").await.unwrap().unwrap().etag,
        "\"etag-a\""
    );
    store.close().await;
    assert_eq!(version_of(&db).await, latest_migration());
    // The backup still has the old schema and the same data.
    let pool = raw(&backups[0]).await;
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    pool.close().await;
    assert_eq!(version_of(&backups[0]).await, 3);

    // Opening again migrates nothing and writes no further backup.
    Store::open(&db).await.unwrap().close().await;
    assert_eq!(pre_migration_backups(&db).len(), 1);
}

/// Upgrade test "database of release N-1 → current" for every older schema.
#[tokio::test]
async fn upgrade_from_every_previous_migration() {
    let latest = latest_migration();
    for version in known_versions().filter(|v| *v < latest) {
        let dir = TempDir::new(&format!("upgrade-{version}"));
        let db = dir.join("calcraft.db");
        old_database(&db, version).await;
        let store = Store::open(&db).await.unwrap();
        assert_eq!(store.list_users().await.unwrap().len(), 1, "from {version}");
        store.close().await;
        assert_eq!(version_of(&db).await, latest, "from {version}");
        let backups = pre_migration_backups(&db);
        assert_eq!(backups.len(), 1, "from {version}");
        assert_eq!(version_of(&backups[0]).await, version);
    }
}

/// Migration 0014 (groups): every existing share becomes a direct share
/// with its access, accepted or pending as before.
#[tokio::test]
async fn shares_survive_the_groups_migration_as_direct_shares() {
    let dir = TempDir::new("upgrade-groups");
    let db = dir.join("calcraft.db");
    old_database(&db, 13).await;
    let pool = raw(&db).await;
    sqlx::query("INSERT INTO users (username, password_hash) VALUES ('bob', 'hash')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO shares (public_id, collection_id, sharee_id, kind, access, slug, created_at, accepted_at) \
         VALUES ('0123456789abcdef0123456789abcdef', 1, 2, 'calendar', 'read-write', 'old-default', 1, 2)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let store = Store::open(&db).await.unwrap();
    let bob = store
        .user_by_name(&"bob".parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let shares = store.incoming_shares(bob.id).await.unwrap();
    assert_eq!(shares.len(), 1);
    assert!(shares[0].accepted());
    assert_eq!(shares[0].access, crate::ShareAccess::ReadWrite);
    assert_eq!(shares[0].direct, Some(crate::ShareAccess::ReadWrite));
    assert!(shares[0].groups.is_empty());
    // Removing the direct share ends it (no group grants it).
    assert!(store.remove_share(1, 1, bob.id).await.unwrap());
    assert!(store.incoming_shares(bob.id).await.unwrap().is_empty());
    store.close().await;
}

#[tokio::test]
async fn new_database_gets_no_pre_migration_backup() {
    let dir = TempDir::new("fresh");
    let db = dir.join("calcraft.db");
    Store::open(&db).await.unwrap().close().await;
    assert!(pre_migration_backups(&db).is_empty());
}

#[tokio::test]
async fn pre_migration_backup_can_be_disabled() {
    let dir = TempDir::new("disabled");
    let db = dir.join("calcraft.db");
    old_database(&db, 3).await;
    let options = StoreOptions {
        pre_migration_backup: false,
        ..StoreOptions::default()
    };
    Store::open_with(&db, &options).await.unwrap().close().await;
    assert!(pre_migration_backups(&db).is_empty());
    assert_eq!(version_of(&db).await, latest_migration());
}

#[tokio::test]
async fn only_the_newest_pre_migration_backups_are_kept() {
    let dir = TempDir::new("retention");
    let db = dir.join("calcraft.db");
    old_database(&db, 3).await;
    for day in 1..=4 {
        std::fs::write(
            dir.join(&format!(
                "calcraft.db.pre-migration-0002-2020010{day}T000000Z"
            )),
            b"old",
        )
        .unwrap();
    }
    let unrelated = dir.join("calcraft.db.pre-migration-notes.txt");
    std::fs::write(&unrelated, b"keep").unwrap();

    Store::open(&db).await.unwrap().close().await;
    let names: Vec<String> = pre_migration_backups(&db)
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names[0].starts_with("calcraft.db.pre-migration-0003-"));
    assert_eq!(names[1], "calcraft.db.pre-migration-0002-20200104T000000Z");
    assert_eq!(names[2], "calcraft.db.pre-migration-0002-20200103T000000Z");
    assert!(unrelated.is_file());
}

#[tokio::test]
async fn newer_schema_is_refused_with_rollback_hint() {
    let dir = TempDir::new("future");
    let db = dir.join("calcraft.db");
    Store::open(&db).await.unwrap().close().await;
    let pool = raw(&db).await;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (9999, 'future', 1, x'00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let err = Store::open(&db).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NewerSchema { version: 9999, .. }),
        "{err}"
    );
    let message = err.to_string();
    assert!(message.contains("pre-migration"), "{message}");
    assert!(message.contains("backup-and-upgrade.md"), "{message}");
}

#[tokio::test]
async fn failed_migration_is_reported() {
    let dir = TempDir::new("dirty");
    let db = dir.join("calcraft.db");
    Store::open(&db).await.unwrap().close().await;
    let pool = raw(&db).await;
    sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = ?")
        .bind(latest_migration())
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let err = Store::open(&db).await.unwrap_err();
    assert!(matches!(err, StoreError::DirtyMigration(_)), "{err}");
}

#[tokio::test]
async fn migration_waits_for_other_processes() {
    let dir = TempDir::new("migrate-lock");
    let db = dir.join("calcraft.db");
    old_database(&db, 3).await;
    // Another process (e.g. the old server) holds the shared lock.
    let other = DbLock::open(&db).unwrap();
    assert!(
        other
            .acquire(LockMode::Shared, Duration::ZERO)
            .await
            .unwrap()
    );
    let err = Store::open(&db).await.unwrap_err();
    assert!(matches!(err, StoreError::InUse(_)), "{err}");
    assert_eq!(version_of(&db).await, 3, "nothing was migrated");
    drop(other);
    Store::open(&db).await.unwrap().close().await;
    assert_eq!(version_of(&db).await, latest_migration());
}

#[cfg(unix)]
#[tokio::test]
async fn failed_pre_migration_backup_prevents_migration() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("backupfail");
    let db = dir.join("calcraft.db");
    old_database(&db, 3).await;
    // Create the side files while the directory is still writable.
    for file in database_files(&db) {
        crate::create_private(&file).unwrap();
    }
    drop(DbLock::open(&db).unwrap());
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o500)).unwrap();
    let writable = std::fs::write(dir.join("probe"), b"").is_ok();
    let result = Store::open(&db).await;
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    if writable {
        // Running as root: permissions are not enforced.
        return;
    }
    let err = result.unwrap_err();
    assert!(matches!(err, StoreError::PreMigrationBackup(_)), "{err}");
    assert!(err.to_string().contains("nothing was migrated"), "{err}");
    assert_eq!(version_of(&db).await, 3);
}

#[tokio::test]
async fn pruning_never_deletes_the_backup_just_written() {
    let dir = TempDir::new("prune-new");
    let db = dir.join("calcraft.db");
    old_database(&db, 3).await;
    // A copied file with a timestamp in the future sorts before the new backup.
    let future = dir.join("calcraft.db.pre-migration-0002-20990101T000000Z");
    std::fs::write(&future, b"copied").unwrap();
    // An empty file (crash while reserving a name) is never a backup.
    let empty = dir.join("calcraft.db.pre-migration-0002-20200105T000000Z");
    std::fs::write(&empty, b"").unwrap();
    let options = StoreOptions {
        pre_migration_backups_keep: 1,
        ..StoreOptions::default()
    };
    Store::open_with(&db, &options).await.unwrap().close().await;
    let backups = pre_migration_backups(&db);
    assert_eq!(backups.len(), 1, "{backups:?}");
    let name = backups[0].file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("calcraft.db.pre-migration-0003-"),
        "{name}"
    );
    assert_eq!(version_of(&backups[0]).await, 3);
    assert!(!future.exists());
    assert!(empty.exists(), "empty files are neither listed nor pruned");
}

#[tokio::test]
async fn foreign_sqlite_file_is_not_migrated() {
    let dir = TempDir::new("foreign");
    let db = dir.join("calcraft.db");
    let pool = raw(&db).await;
    sqlx::query("CREATE TABLE notes (x)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let err = Store::open(&db).await.unwrap_err();
    assert!(matches!(err, StoreError::NotCalcraft(_)), "{err}");
    assert!(err.to_string().contains("storage.database"), "{err}");
    let pool = raw(&db).await;
    assert!(
        pool_applied(&pool).await.unwrap().is_none(),
        "nothing was migrated"
    );
    pool.close().await;
}

#[tokio::test]
async fn backup_target_must_not_be_a_database_file() {
    let dir = TempDir::new("target");
    let db = dir.join("calcraft.db");
    let store = Store::open(&db).await.unwrap();
    for suffix in ["", "-wal", "-shm", "-journal", ".lock"] {
        let target = with_suffix(&db, suffix);
        let err = store.backup(&target, true).await.unwrap_err();
        assert!(
            err.to_string().contains("one of its files"),
            "{suffix}: {err}"
        );
    }
    store.close().await;
    for suffix in ["", "-wal", ".lock"] {
        assert!(
            backup_database(&db, &with_suffix(&db, suffix), true)
                .await
                .is_err(),
            "{suffix}"
        );
    }
    // Other spellings of the same file are caught, too.
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let sneaky = dir.join("sub").join("..").join("calcraft.db-wal");
    assert!(backup_database(&db, &sneaky, true).await.is_err());
}

#[test]
fn temp_names_are_recognised_exactly() {
    for name in [
        ".calcraft.db.tmp-12-345",
        ".calcraft.db.tmp-12-345-wal",
        ".calcraft.db.pre-migration-0004-20261021T120000Z.tmp-1-2",
        ".calcraft.db.pre-migration-0004-20261021T120000Z-1.tmp-1-2-journal",
    ] {
        assert!(is_temp_name(name, "calcraft.db"), "{name}");
    }
    for name in [
        "calcraft.db.tmp-12-345",
        ".calcraft.db.tmp-12",
        ".calcraft.db.tmp-12-34x",
        ".calcraft.db.tmp-1-2-3",
        ".calcraft.db.notes.tmp-1-2",
        ".other.db.tmp-1-2",
        ".calcraft.db",
    ] {
        assert!(!is_temp_name(name, "calcraft.db"), "{name}");
    }
}

#[tokio::test]
async fn stale_temp_files_are_removed_on_open() {
    let dir = TempDir::new("stale");
    let db = dir.join("calcraft.db");
    let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
    let make = |name: &str, old: bool| {
        let path = dir.join(name);
        let file = File::create(&path).unwrap();
        if old {
            file.set_modified(two_hours_ago).unwrap();
        }
        path
    };
    let stale = make(".calcraft.db.tmp-1-2", true);
    let stale_side = make(
        ".calcraft.db.pre-migration-0003-20200101T000000Z.tmp-1-2-wal",
        true,
    );
    let fresh = make(".calcraft.db.tmp-3-4", false);
    let unrelated = make(".calcraft.db.tmp-notes", true);
    Store::open(&db).await.unwrap().close().await;
    assert!(!stale.exists());
    assert!(!stale_side.exists());
    assert!(fresh.exists(), "a backup may still be writing it");
    assert!(unrelated.exists());
}

/// Connections only exist while a lock is held: after migrating, the open
/// store holds the shared lock again, so a restore is refused.
#[tokio::test]
async fn lock_is_held_after_migrating() {
    let dir = TempDir::new("after-migrate");
    let db = dir.join("calcraft.db");
    old_database(&db, 3).await;
    let store = Store::open(&db).await.unwrap();
    let backup = pre_migration_backups(&db).remove(0);
    let err = restore_database(&db, &backup).await.unwrap_err();
    assert!(matches!(err, StoreError::InUse(_)), "{err}");
    store.list_users().await.unwrap();
    store.close().await;
    restore_database(&db, &backup).await.unwrap();
    assert_eq!(version_of(&db).await, 3);
}

#[tokio::test]
async fn backup_and_restore_include_the_trash() {
    let dir = TempDir::new("trash");
    let db = dir.join("calcraft.db");
    let store = Store::open(&db).await.unwrap();
    let collection = user_calendar(&store, "alice").await;
    put(&store, collection, "a.ics", "\"a1\"").await;
    store
        .delete_item_to_trash(collection, "a.ics", &Precondition::None, 1_000, |_| {
            Some("Dentist".to_owned())
        })
        .await
        .unwrap();
    let owner = store.list_users().await.unwrap()[0].id;
    let before = store.trash_entries(owner, 10, 0).await.unwrap();
    assert_eq!(before.total, 1);
    let backup = dir.join("backup.db");
    store.backup(&backup, false).await.unwrap();
    store.empty_trash(owner).await.unwrap();
    store.close().await;

    restore_database(&db, &backup).await.unwrap();
    let store = Store::open(&db).await.unwrap();
    let after = store.trash_entries(owner, 10, 0).await.unwrap();
    assert_eq!(after, before);
    let restored = store
        .restore_trash_entry(owner, &after.entries[0].id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.etag.as_deref(), Some("\"a1\""));
    assert_eq!(
        store
            .item(collection, "a.ics")
            .await
            .unwrap()
            .unwrap()
            .content,
        event("a")
    );
    store.close().await;
}
