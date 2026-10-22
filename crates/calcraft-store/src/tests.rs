use super::*;

fn name(s: &str) -> Username {
    s.parse().unwrap()
}

#[tokio::test]
async fn create_user_with_default_collections() {
    let store = Store::in_memory().await.unwrap();
    let user = store
        .create_user(&name("alice"), "hash", Some("Alice"))
        .await
        .unwrap();

    let calendars = store
        .collections(user.id, CollectionKind::Calendar)
        .await
        .unwrap();
    assert_eq!(calendars.len(), 1);
    assert_eq!(calendars[0].slug.as_str(), DEFAULT_COLLECTION_SLUG);
    let books = store
        .collections(user.id, CollectionKind::Addressbook)
        .await
        .unwrap();
    assert_eq!(books.len(), 1);

    let creds = store
        .user_credentials(&name("alice"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(creds.user, user);
    assert_eq!(creds.password_hash, "hash");
    assert!(!format!("{creds:?}").contains("hash\""));
}

#[tokio::test]
async fn duplicate_user_is_rejected() {
    let store = Store::in_memory().await.unwrap();
    store.create_user(&name("bob"), "h", None).await.unwrap();
    let err = store
        .create_user(&name("bob"), "h", None)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::UserExists(_)));
}

#[tokio::test]
async fn collections_are_isolated_per_user() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let bob = store.create_user(&name("bob"), "h", None).await.unwrap();
    let slug: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();

    let a = store
        .collection(alice.id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    let b = store
        .collection(bob.id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.owner_id, alice.id);
    assert_eq!(b.owner_id, bob.id);
}

#[tokio::test]
async fn update_password_and_disable() {
    let store = Store::in_memory().await.unwrap();
    store
        .create_user(&name("carol"), "old", None)
        .await
        .unwrap();
    assert!(
        store
            .set_password_hash(&name("carol"), "new")
            .await
            .unwrap()
    );
    assert!(store.set_disabled(&name("carol"), true).await.unwrap());
    assert!(!store.set_disabled(&name("nobody"), true).await.unwrap());

    let creds = store
        .user_credentials(&name("carol"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(creds.password_hash, "new");
    assert!(creds.user.disabled);
}

#[cfg(unix)]
#[tokio::test]
async fn database_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("calcraft-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.db");
    let store = Store::open(&path).await.unwrap();
    store.ping().await.unwrap();
    store
        .create_user(&name("walcheck"), "secret-hash", None)
        .await
        .unwrap();
    for file in database_files(&path) {
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{}", file.display());
    }
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_busy_writer_fails_with_busy_after_the_acquire_timeout() {
    let dir = std::env::temp_dir().join(format!("calcraft-busy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let options = StoreOptions {
        // Long enough to open a connection on slow CI runners (Windows),
        // short enough to keep the test quick.
        acquire_timeout: std::time::Duration::from_millis(500),
        ..StoreOptions::default()
    };
    let store = Store::open_with(&dir.join("test.db"), &options)
        .await
        .unwrap();
    // Someone holds the single write connection for too long.
    // Open a reader connection first so the read below doesn't have to.
    assert!(store.list_users().await.unwrap().is_empty());
    let held = store.writer.acquire().await.unwrap();
    let start = std::time::Instant::now();
    let err = store
        .create_user(&name("waiting"), "hash", None)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Busy), "{err:?}");
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    // Reads do not need the writer.
    assert!(store.list_users().await.unwrap().is_empty());
    drop(held);
    store
        .create_user(&name("waiting"), "hash", None)
        .await
        .unwrap();
    // Close the pools first: Windows refuses to delete open files.
    store.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}
