//! `calcraft backup` / `calcraft restore` with the real binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use calcraft_core::CollectionKind;
use calcraft_store::{DEFAULT_COLLECTION_SLUG, NewItem, Precondition, Store};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "calcraft-cli-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn calcraft(db: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_calcraft"));
    command
        .env_remove("CALCRAFT_CONFIG")
        .env("CALCRAFT_STORAGE__DATABASE", db)
        // Build switch that operators may still have exported; must not break startup.
        .env("CALCRAFT_UI_REQUIRED", "1")
        .stdin(Stdio::null());
    command
}

fn run(db: &Path, args: &[&str]) -> Output {
    calcraft(db).args(args).output().unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

async fn calendar_id(store: &Store) -> i64 {
    let owner = store.list_users().await.unwrap()[0].id;
    store
        .collection(
            owner,
            CollectionKind::Calendar,
            &DEFAULT_COLLECTION_SLUG.parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap()
        .id
}

async fn put(store: &Store, collection: i64, uid: &str, etag: &str) {
    let content = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    store
        .put_item(
            collection,
            &format!("{uid}.ics"),
            &NewItem {
                uid,
                component: "VEVENT",
                range_start: None,
                range_end: None,
                etag,
                content: content.as_bytes(),
            },
            &Precondition::None,
        )
        .await
        .unwrap();
}

async fn seed(db: &Path) -> i64 {
    let store = Store::open(db).await.unwrap();
    store
        .create_user(&"alice".parse().unwrap(), "hash", None)
        .await
        .unwrap();
    let collection = calendar_id(&store).await;
    put(&store, collection, "event", "\"v1\"").await;
    store.close().await;
    collection
}

#[tokio::test]
async fn backup_and_restore_round_trip() {
    let dir = TempDir::new("roundtrip");
    let db = dir.0.join("calcraft.db");
    let collection = seed(&db).await;
    let backup = dir.0.join("backup.db");
    let backup_arg = backup.to_str().unwrap();

    let output = run(&db, &["backup", backup_arg]);
    assert!(output.status.success(), "{}", text(&output));
    let latest = calcraft_store::latest_migration();
    assert!(
        text(&output).contains(&format!("migration {latest}")),
        "{}",
        text(&output)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&backup).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // An existing file is only replaced with --force.
    let output = run(&db, &["backup", backup_arg]);
    assert!(!output.status.success());
    assert!(text(&output).contains("--force"), "{}", text(&output));
    let output = run(&db, &["backup", "--force", backup_arg]);
    assert!(output.status.success(), "{}", text(&output));

    // Change data after the backup.
    let store = Store::open(&db).await.unwrap();
    store
        .delete_item(collection, "event.ics", &Precondition::None)
        .await
        .unwrap();
    put(&store, collection, "later", "\"v9\"").await;
    let revision_after_changes = store
        .collections(1, CollectionKind::Calendar)
        .await
        .unwrap()[0]
        .sync_revision;
    store.close().await;

    // Without a terminal, restore needs --yes.
    let output = run(&db, &["restore", backup_arg]);
    assert!(!output.status.success());
    assert!(text(&output).contains("--yes"), "{}", text(&output));

    let output = run(&db, &["restore", "--yes", backup_arg]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("previous database kept as"),
        "{}",
        text(&output)
    );

    let store = Store::open(&db).await.unwrap();
    let item = store.item(collection, "event.ics").await.unwrap().unwrap();
    assert_eq!(item.etag, "\"v1\"");
    assert!(store.item(collection, "later.ics").await.unwrap().is_none());
    let revision = store
        .collections(1, CollectionKind::Calendar)
        .await
        .unwrap()[0]
        .sync_revision;
    assert!(revision < revision_after_changes);
    store.close().await;
}

#[tokio::test]
async fn restore_is_refused_while_the_database_is_open() {
    let dir = TempDir::new("inuse");
    let db = dir.0.join("calcraft.db");
    seed(&db).await;
    let backup = dir.0.join("backup.db");
    let output = run(&db, &["backup", backup.to_str().unwrap()]);
    assert!(output.status.success(), "{}", text(&output));

    // Another process (here: this test) has the database open.
    let store = Store::open(&db).await.unwrap();
    let output = run(&db, &["restore", "--yes", backup.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(
        text(&output).contains("stop the server"),
        "{}",
        text(&output)
    );
    store.close().await;
}

#[tokio::test]
async fn restore_is_refused_while_the_server_runs() {
    let dir = TempDir::new("serve");
    let db = dir.0.join("calcraft.db");
    seed(&db).await;
    let backup = dir.0.join("backup.db");
    let output = run(&db, &["backup", backup.to_str().unwrap()]);
    assert!(output.status.success(), "{}", text(&output));

    let mut server = calcraft(&db)
        .args(["serve", "--bind", "127.0.0.1:0"])
        .env("CALCRAFT_LOGGING__FORMAT", "json")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = server.stdout.take().unwrap();
    // Read in a thread so that a hanging server cannot block the test.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.contains("listening") {
                let _ = tx.send(());
            }
        }
    });
    let started = rx.recv_timeout(Duration::from_secs(30)).is_ok();
    let output = run(&db, &["restore", "--yes", backup.to_str().unwrap()]);
    server.kill().unwrap();
    server.wait().unwrap();
    assert!(started, "server did not start");
    assert!(!output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("in use"), "{}", text(&output));
}
