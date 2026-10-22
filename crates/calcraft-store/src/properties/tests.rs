use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

use super::*;
use crate::{Collection, DEFAULT_COLLECTION_SLUG, User};

const ORDER_NS: &str = "http://apple.com/ns/ical/";

fn set<'a>(namespace: &'a str, name: &'a str, value: &'a str) -> PropertyChange<'a> {
    PropertyChange {
        namespace,
        name,
        value: Some(value),
    }
}

fn remove<'a>(namespace: &'a str, name: &'a str) -> PropertyChange<'a> {
    PropertyChange {
        namespace,
        name,
        value: None,
    }
}

async fn calendar(store: &Store, user: &User) -> Collection {
    store
        .collection(
            user.id,
            CollectionKind::Calendar,
            &DEFAULT_COLLECTION_SLUG.parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap()
}

async fn user(store: &Store, name: &str) -> User {
    store
        .create_user(&name.parse().unwrap(), "h", None)
        .await
        .unwrap()
}

async fn patch(store: &Store, collection: i64, changes: &[PropertyChange<'_>]) -> Result<()> {
    store
        .update_collection_with_properties(collection, None, None, None, changes)
        .await
}

fn names(props: &[DeadProperty]) -> Vec<(&str, &str, &str)> {
    props
        .iter()
        .map(|p| (p.namespace.as_str(), p.name.as_str(), p.value.as_str()))
        .collect()
}

#[tokio::test]
async fn set_replace_and_remove() {
    let store = Store::in_memory().await.unwrap();
    let alice = user(&store, "alice").await;
    let cal = calendar(&store, &alice).await;
    patch(
        &store,
        cal.id,
        &[
            set(ORDER_NS, "calendar-order", "1"),
            set("", "plain", "x"),
            set(ORDER_NS, "calendar-order", "2"),
        ],
    )
    .await
    .unwrap();
    let props = store.dead_properties(cal.id).await.unwrap();
    assert_eq!(
        names(&props),
        [("", "plain", "x"), (ORDER_NS, "calendar-order", "2")]
    );
    // Removing a property that does not exist is not an error.
    patch(
        &store,
        cal.id,
        &[remove("", "plain"), remove("urn:x", "none")],
    )
    .await
    .unwrap();
    let props = store.dead_properties(cal.id).await.unwrap();
    assert_eq!(names(&props), [(ORDER_NS, "calendar-order", "2")]);
    // The revision (getctag) moves, as for the other properties.
    let after = calendar(&store, &alice).await;
    assert_eq!(after.sync_revision, cal.sync_revision + 2);
}

#[tokio::test]
async fn properties_are_per_owner_and_collection() {
    let store = Store::in_memory().await.unwrap();
    let alice = user(&store, "alice").await;
    let bob = user(&store, "bob").await;
    let cal = calendar(&store, &alice).await;
    patch(&store, cal.id, &[set(ORDER_NS, "calendar-order", "1")])
        .await
        .unwrap();
    let work = store
        .create_collection_with_properties(
            alice.id,
            CollectionKind::Calendar,
            &"work".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
            &[set(ORDER_NS, "calendar-order", "0")],
        )
        .await
        .unwrap();
    let all = store
        .dead_properties_of_owner(alice.id, CollectionKind::Calendar)
        .await
        .unwrap();
    assert_eq!(
        all.iter()
            .map(|(id, p)| (*id, p.value.as_str()))
            .collect::<Vec<_>>(),
        [(cal.id, "1"), (work.id, "0")]
    );
    for (owner, kind) in [
        (bob.id, CollectionKind::Calendar),
        (alice.id, CollectionKind::Addressbook),
    ] {
        let none = store.dead_properties_of_owner(owner, kind).await.unwrap();
        assert!(none.is_empty());
    }
    // Deleting the collection deletes its properties.
    store.delete_collection(work.id).await.unwrap();
    assert!(store.dead_properties(work.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn limits_roll_back_everything() {
    let store = Store::in_memory().await.unwrap();
    let alice = user(&store, "alice").await;
    let cal = calendar(&store, &alice).await;
    let names: Vec<String> = (0..=MAX_DEAD_PROPERTIES).map(|i| format!("p{i}")).collect();
    let too_many: Vec<_> = names.iter().map(|n| set("urn:x", n, "v")).collect();
    let err = store
        .update_collection_with_properties(cal.id, Some(Some("Renamed")), None, None, &too_many)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Limit(_)), "{err}");
    assert!(store.dead_properties(cal.id).await.unwrap().is_empty());
    let unchanged = calendar(&store, &alice).await;
    assert_eq!(unchanged, cal);

    // Exactly at the count limit is fine.
    patch(&store, cal.id, &too_many[1..]).await.unwrap();
    patch(&store, cal.id, &[remove("urn:x", "p1")])
        .await
        .unwrap();

    // Size: nine values of 16 KiB exceed 128 KiB in total.
    let big = "v".repeat(16 * 1024);
    let book = store
        .collections(alice.id, CollectionKind::Addressbook)
        .await
        .unwrap()[0]
        .id;
    let changes: Vec<_> = names[..9].iter().map(|n| set("urn:big", n, &big)).collect();
    patch(&store, book, &changes[..7]).await.unwrap();
    let err = patch(&store, book, &changes).await.unwrap_err();
    assert!(matches!(err, StoreError::Limit(_)), "{err}");
    assert_eq!(store.dead_properties(book).await.unwrap().len(), 7);
    assert_eq!(store.dead_properties(cal.id).await.unwrap().len(), 63);

    // MKCALENDAR with too many properties creates nothing.
    let err = store
        .create_collection_with_properties(
            alice.id,
            CollectionKind::Calendar,
            &"work".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
            &too_many,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Limit(_)), "{err}");
    let work = store
        .collection(alice.id, CollectionKind::Calendar, &"work".parse().unwrap())
        .await
        .unwrap();
    assert!(work.is_none());
}

#[tokio::test]
async fn trash_keeps_properties() {
    let store = Store::in_memory().await.unwrap();
    let alice = user(&store, "alice").await;
    let cal = calendar(&store, &alice).await;
    patch(&store, cal.id, &[set(ORDER_NS, "calendar-order", "7")])
        .await
        .unwrap();
    store
        .delete_collection_to_trash(cal.id, 1_800_000_000)
        .await
        .unwrap();
    let page = store.trash_entries(alice.id, 10, 0).await.unwrap();
    let entry = &page.entries[0].id;
    store
        .restore_trash_entry(alice.id, entry, None)
        .await
        .unwrap()
        .unwrap();
    let restored = calendar(&store, &alice).await;
    assert_ne!(restored.id, cal.id);
    let props = store.dead_properties(restored.id).await.unwrap();
    assert_eq!(names(&props), [(ORDER_NS, "calendar-order", "7")]);

    // Emptied trash entries take their properties with them.
    store
        .delete_collection_to_trash(restored.id, 1_800_000_000)
        .await
        .unwrap();
    store.empty_trash(alice.id).await.unwrap();
    while store.purge_expired_trash(1_800_000_000, 100).await.unwrap() > 0 {}
    let left: i64 = sqlx::query("SELECT COUNT(*) AS n FROM trash_dead_properties")
        .fetch_one(&store.pool)
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn properties_count_towards_the_quota() {
    let store = Store::in_memory().await.unwrap();
    let alice = user(&store, "alice").await;
    let cal = calendar(&store, &alice).await;
    let book = store
        .collections(alice.id, CollectionKind::Addressbook)
        .await
        .unwrap()[0]
        .id;
    store.set_limits_for_testing(10_000, crate::MAX_TRASH_ROWS_PER_USER);
    let value = "v".repeat(6_000);
    patch(&store, cal.id, &[set("urn:x", "a", &value)])
        .await
        .unwrap();
    // A second collection does not get its own quota.
    let err = patch(&store, book, &[set("urn:x", "a", &value)])
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Limit("storage quota exceeded")),
        "{err}"
    );
    // Objects see the properties' bytes as used.
    let content = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:u\r\nSUMMARY:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        "s".repeat(4_500)
    );
    let item = crate::NewItem {
        uid: "u",
        component: "VEVENT",
        range_start: None,
        range_end: None,
        recurring: false,
        index_version: 0,
        etag: "\"e\"",
        content: content.as_bytes(),
    };
    let err = store
        .put_item(cal.id, "u.ics", &item, &crate::Precondition::None)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Limit(_)), "{err}");

    // In the trash they still count, and the entry's size includes them.
    store
        .delete_collection_to_trash(cal.id, 1_800_000_000)
        .await
        .unwrap();
    let page = store.trash_entries(alice.id, 10, 0).await.unwrap();
    assert_eq!(page.entries[0].size, 6_000 + 5 + 1);
    assert_eq!(page.bytes, 6_000 + 5 + 1);
    let err = patch(&store, book, &[set("urn:x", "a", &value)])
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Limit("storage quota exceeded")),
        "{err}"
    );

    // Above the quota, growing fails but removing still works.
    store.set_limits_for_testing(100, crate::MAX_TRASH_ROWS_PER_USER);
    patch(&store, book, &[set("urn:x", "small", "1")])
        .await
        .unwrap_err();
    store.set_limits_for_testing(10_000, crate::MAX_TRASH_ROWS_PER_USER);
    patch(&store, book, &[set("urn:x", "b", "1")])
        .await
        .unwrap();
    store.set_limits_for_testing(100, crate::MAX_TRASH_ROWS_PER_USER);
    patch(&store, book, &[remove("urn:x", "b")]).await.unwrap();
}

/// A fresh directory; removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "calcraft-props-{}-{}",
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

#[tokio::test]
async fn properties_survive_restart_and_backup() {
    let dir = TempDir::new();
    let db = dir.0.join("calcraft.db");
    let backup = dir.0.join("backup.db");
    let store = Store::open(&db).await.unwrap();
    let alice = user(&store, "alice").await;
    let cal = calendar(&store, &alice).await;
    let value = r#"<a xmlns="urn:x" k="v">t</a>"#;
    patch(&store, cal.id, &[set("urn:p", "prop", value)])
        .await
        .unwrap();
    store.backup(&backup, false).await.unwrap();
    store.close().await;

    for path in [&db, &backup] {
        let store = Store::open(path).await.unwrap();
        let props = store.dead_properties(cal.id).await.unwrap();
        assert_eq!(names(&props), [("urn:p", "prop", value)], "{path:?}");
        store.close().await;
    }
}
