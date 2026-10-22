use super::*;
use crate::{Collection, DEFAULT_COLLECTION_SLUG, NewItem, User};
use sqlx::Row;

const DAY: i64 = 86_400;
const NOW: i64 = 1_800_000_000;

async fn setup() -> (Store, User, Collection, User) {
    let store = Store::in_memory().await.unwrap();
    let alice = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let bob = store
        .create_user(&"bob".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let cal = calendar(&store, alice.id, DEFAULT_COLLECTION_SLUG).await;
    (store, alice, cal, bob)
}

async fn calendar(store: &Store, owner: i64, slug: &str) -> Collection {
    store
        .collection(owner, CollectionKind::Calendar, &slug.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
}

async fn put(store: &Store, collection: i64, name: &str, uid: &str, body: &str) {
    store
        .put_item(
            collection,
            name,
            &NewItem {
                uid,
                component: "VEVENT",
                range_start: None,
                range_end: None,
                etag: &format!("\"{uid}-{}\"", body.len()),
                content: body.as_bytes(),
            },
            &Precondition::None,
        )
        .await
        .unwrap();
}

async fn trash(store: &Store, collection: i64, name: &str, now: i64) -> bool {
    store
        .delete_item_to_trash(collection, name, &Precondition::None, now, |content| {
            Some(String::from_utf8_lossy(content).into_owned())
        })
        .await
        .unwrap()
}

async fn entries(store: &Store, owner: i64) -> Vec<TrashEntry> {
    store.trash_entries(owner, 100, 0).await.unwrap().entries
}

fn conflict(result: Result<Option<RestoredEntry>>) -> TrashConflict {
    match result {
        Err(StoreError::TrashConflict(conflict)) => conflict,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_moves_to_trash_and_records_a_tombstone() {
    let (store, alice, cal, _) = setup().await;
    put(&store, cal.id, "a.ics", "uid-a", "event a").await;
    let etag = store.item(cal.id, "a.ics").await.unwrap().unwrap().etag;
    assert!(trash(&store, cal.id, "a.ics", NOW).await);
    assert!(!trash(&store, cal.id, "a.ics", NOW).await, "already gone");

    assert!(store.item(cal.id, "a.ics").await.unwrap().is_none());
    let changes = store.changes_since(cal.id, 0, 10).await.unwrap();
    assert_eq!(changes.len(), 1);
    assert!(changes[0].meta.is_none(), "sync sees a deletion");

    let page = store.trash_entries(alice.id, 10, 0).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.bytes, 7);
    let entry = &page.entries[0];
    assert_eq!(entry.kind, TrashEntryKind::Item);
    assert_eq!(entry.name.as_deref(), Some("a.ics"));
    assert_eq!(entry.collection_slug, DEFAULT_COLLECTION_SLUG);
    assert_eq!(entry.summary.as_deref(), Some("event a"));
    assert_eq!(entry.deleted_at, NOW);
    assert_eq!(entry.id.len(), 32);
    assert!(entry.id.bytes().all(|b| b.is_ascii_hexdigit()));

    // The same name and UID can be used again right away.
    put(&store, cal.id, "a.ics", "uid-a", "new a").await;
    store
        .delete_item_to_trash(
            cal.id,
            "a.ics",
            &Precondition::IfMatch(vec![etag]),
            NOW,
            |_| None,
        )
        .await
        .unwrap_err();
}

#[tokio::test]
async fn restore_puts_the_object_back_with_a_change() {
    let (store, alice, cal, _) = setup().await;
    put(&store, cal.id, "a.ics", "uid-a", "event a").await;
    let before = store.item(cal.id, "a.ics").await.unwrap().unwrap();
    trash(&store, cal.id, "a.ics", NOW).await;
    let deleted_rev = store.changes_since(cal.id, 0, 10).await.unwrap()[0].revision;
    let id = entries(&store, alice.id).await[0].id.clone();

    let restored = store
        .restore_trash_entry(alice.id, &id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.name.as_deref(), Some("a.ics"));
    assert_eq!(restored.etag.as_deref(), Some(before.etag.as_str()));
    assert_eq!(store.item(cal.id, "a.ics").await.unwrap().unwrap(), before);
    let changes = store.changes_since(cal.id, deleted_rev, 10).await.unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].meta.as_ref().unwrap().etag, before.etag);
    assert!(entries(&store, alice.id).await.is_empty());
    assert!(
        store
            .restore_trash_entry(alice.id, &id, None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn restore_conflicts() {
    let (store, alice, cal, _) = setup().await;
    put(&store, cal.id, "a.ics", "uid-a", "event a").await;
    put(&store, cal.id, "b.ics", "uid-b", "event b").await;
    trash(&store, cal.id, "a.ics", NOW).await;
    trash(&store, cal.id, "b.ics", NOW + 1).await;
    let list = entries(&store, alice.id).await;
    let (b, a) = (list[0].id.clone(), list[1].id.clone());

    put(&store, cal.id, "a.ics", "uid-other", "other").await;
    assert_eq!(
        conflict(store.restore_trash_entry(alice.id, &a, None).await),
        TrashConflict::NameTaken
    );
    put(&store, cal.id, "c.ics", "uid-b", "same uid").await;
    assert_eq!(
        conflict(store.restore_trash_entry(alice.id, &b, None).await),
        TrashConflict::UidTaken
    );
    // Nothing changed.
    assert_eq!(entries(&store, alice.id).await.len(), 2);
    assert_eq!(store.items(cal.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn collections_go_to_the_trash_as_one_unit() {
    let (store, alice, _, _) = setup().await;
    let work = store
        .create_collection(
            alice.id,
            CollectionKind::Calendar,
            &"work".parse().unwrap(),
            Some("Work"),
            Some("desc"),
            Some("#112233"),
            "VEVENT",
        )
        .await
        .unwrap();
    put(&store, work.id, "a.ics", "uid-a", "event a").await;
    put(&store, work.id, "b.ics", "uid-b", "event b").await;
    put(&store, work.id, "c.ics", "uid-c", "event c").await;
    // c is deleted on its own first.
    trash(&store, work.id, "c.ics", NOW).await;
    store
        .delete_collection_to_trash(work.id, NOW + 1)
        .await
        .unwrap();
    assert!(
        store
            .collection(alice.id, CollectionKind::Calendar, &work.slug)
            .await
            .unwrap()
            .is_none()
    );
    let list = entries(&store, alice.id).await;
    assert_eq!(list.len(), 2);
    let (unit, single) = (list[0].clone(), list[1].clone());
    assert_eq!(unit.kind, TrashEntryKind::Collection);
    assert_eq!(unit.items, 2);
    assert_eq!(unit.size, 14);
    assert_eq!(unit.summary.as_deref(), Some("Work"));
    assert_eq!(
        store.trash_entries(alice.id, 10, 0).await.unwrap().bytes,
        21
    );

    // The object needs its collection back first.
    assert_eq!(
        conflict(store.restore_trash_entry(alice.id, &single.id, None).await),
        TrashConflict::CollectionInTrash
    );

    // A new collection with the same name blocks the restore.
    let again = store
        .create_collection(
            alice.id,
            CollectionKind::Calendar,
            &"work".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    assert_eq!(
        conflict(store.restore_trash_entry(alice.id, &unit.id, None).await),
        TrashConflict::CollectionExists
    );
    store.delete_collection(again.id).await.unwrap();

    let restored = store
        .restore_trash_entry(alice.id, &unit.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.kind, TrashEntryKind::Collection);
    let back = calendar(&store, alice.id, "work").await;
    assert_eq!(back.display_name.as_deref(), Some("Work"));
    assert_eq!(back.description.as_deref(), Some("desc"));
    assert_eq!(back.color.as_deref(), Some("#112233"));
    assert_eq!(back.components, "VEVENT");
    assert_ne!(
        back.token_salt, work.token_salt,
        "old sync tokens stay invalid"
    );
    let names: Vec<String> = store
        .items(back.id)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names, ["a.ics", "b.ics"]);
    let changes = store.changes_since(back.id, 0, 10).await.unwrap();
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().all(|c| c.meta.is_some()));
    assert_ne!(changes[0].revision, changes[1].revision);
    assert_eq!(back.sync_revision, 2);

    // Now the single object can follow.
    store
        .restore_trash_entry(alice.id, &single.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.items(back.id).await.unwrap().len(), 3);
    assert!(entries(&store, alice.id).await.is_empty());
}

#[tokio::test]
async fn object_of_a_permanently_deleted_collection_cannot_be_restored() {
    let (store, alice, _, _) = setup().await;
    let work = store
        .create_collection(
            alice.id,
            CollectionKind::Calendar,
            &"work".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    put(&store, work.id, "a.ics", "uid-a", "event a").await;
    trash(&store, work.id, "a.ics", NOW).await;
    store
        .delete_collection_to_trash(work.id, NOW)
        .await
        .unwrap();
    let unit = entries(&store, alice.id)
        .await
        .into_iter()
        .find(|e| e.kind == TrashEntryKind::Collection)
        .unwrap();
    assert!(store.delete_trash_entry(alice.id, &unit.id).await.unwrap());
    let single = entries(&store, alice.id).await[0].id.clone();
    assert_eq!(
        conflict(store.restore_trash_entry(alice.id, &single, None).await),
        TrashConflict::CollectionDeleted
    );
    // A new collection with the same name is a different collection.
    store
        .create_collection(
            alice.id,
            CollectionKind::Calendar,
            &"work".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    assert_eq!(
        conflict(store.restore_trash_entry(alice.id, &single, None).await),
        TrashConflict::CollectionDeleted
    );
    // It can go into another collection of the same kind instead.
    store
        .restore_trash_entry(alice.id, &single, Some(&"work".parse().unwrap()))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn objects_of_a_lost_collection_restore_into_a_target() {
    let (store, alice, cal, _) = setup().await;
    let work = store
        .create_collection(
            alice.id,
            CollectionKind::Calendar,
            &"work".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    put(&store, work.id, "a.ics", "uid-a", "event a").await;
    let modified = sqlx::query("SELECT modified_at FROM items WHERE collection_id = ?")
        .bind(work.id)
        .fetch_one(&store.pool)
        .await
        .unwrap()
        .try_get::<String, _>("modified_at")
        .unwrap();
    trash(&store, work.id, "a.ics", NOW).await;
    store.delete_collection(work.id).await.unwrap();
    let id = entries(&store, alice.id).await[0].id.clone();

    let book: Slug = "book".parse().unwrap();
    store
        .create_collection(
            alice.id,
            CollectionKind::Addressbook,
            &book,
            None,
            None,
            None,
            "VCARD",
        )
        .await
        .unwrap();
    // Wrong kind, unknown and foreign targets are refused.
    for target in ["book", "nope"] {
        assert_eq!(
            conflict(
                store
                    .restore_trash_entry(alice.id, &id, Some(&target.parse().unwrap()))
                    .await
            ),
            TrashConflict::InvalidTarget,
            "{target}"
        );
    }
    // Name and UID are checked in the target as well.
    put(&store, cal.id, "a.ics", "uid-x", "taken").await;
    assert_eq!(
        conflict(
            store
                .restore_trash_entry(alice.id, &id, Some(&cal.slug))
                .await
        ),
        TrashConflict::NameTaken
    );
    store
        .delete_item(cal.id, "a.ics", &Precondition::None)
        .await
        .unwrap();
    let restored = store
        .restore_trash_entry(alice.id, &id, Some(&cal.slug))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.collection_slug, cal.slug);
    let back =
        sqlx::query("SELECT modified_at FROM items WHERE collection_id = ? AND name = 'a.ics'")
            .bind(cal.id)
            .fetch_one(&store.pool)
            .await
            .unwrap()
            .try_get::<String, _>("modified_at")
            .unwrap();
    assert_eq!(back, modified, "modified_at is kept");
}

#[tokio::test]
async fn a_delete_into_a_full_trash_stays_bounded() {
    let (store, alice, cal, _) = setup().await;
    store.set_limits_for_testing(MAX_BYTES_PER_USER_FOR_TESTS, 6);
    store.set_eviction_budget_for_testing(2);
    let work = store
        .create_collection(
            alice.id,
            CollectionKind::Calendar,
            &"work".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    for i in 0..4 {
        put(&store, work.id, &format!("{i}.ics"), &format!("w{i}"), "x").await;
    }
    store
        .delete_collection_to_trash(work.id, NOW)
        .await
        .unwrap();
    put(&store, cal.id, "x.ics", "x", "x").await;
    trash(&store, cal.id, "x.ics", NOW + 1).await;
    let rows = || async {
        sqlx::query("SELECT COUNT(*) AS n FROM trash")
            .fetch_one(&store.pool)
            .await
            .unwrap()
            .try_get::<i64, _>("n")
            .unwrap()
    };
    assert_eq!(rows().await, 6, "the trash is full");
    assert!(!store.take_purge_request());

    // The oldest entry (5 rows) is larger than the budget: it is only
    // marked, nothing is deleted on the request path.
    put(&store, cal.id, "y.ics", "y", "y").await;
    trash(&store, cal.id, "y.ics", NOW + 2).await;
    assert_eq!(rows().await, 7);
    let names: Vec<Option<String>> = entries(&store, alice.id)
        .await
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, [Some("y.ics".into()), Some("x.ics".into())]);
    assert_eq!(store.trash_entries(alice.id, 10, 0).await.unwrap().bytes, 2);
    assert!(store.take_purge_request());
    assert!(!store.take_purge_request());

    // The purge removes it in small batches.
    assert_eq!(store.purge_expired_trash(NOW + 3, 3).await.unwrap(), 3);
    assert_eq!(store.purge_expired_trash(NOW + 3, 3).await.unwrap(), 2);
    assert_eq!(store.purge_expired_trash(NOW + 3, 3).await.unwrap(), 0);
    assert_eq!(rows().await, 2);
}

#[tokio::test]
async fn emptied_entries_disappear_at_once_and_are_purged_later() {
    let (store, alice, cal, _) = setup().await;
    put(&store, cal.id, "a.ics", "uid-a", "event a").await;
    trash(&store, cal.id, "a.ics", NOW).await;
    let id = entries(&store, alice.id).await[0].id.clone();
    assert!(store.delete_trash_entry(alice.id, &id).await.unwrap());
    assert!(!store.delete_trash_entry(alice.id, &id).await.unwrap());
    assert!(
        store
            .restore_trash_entry(alice.id, &id, None)
            .await
            .unwrap()
            .is_none()
    );
    let page = store.trash_entries(alice.id, 10, 0).await.unwrap();
    assert_eq!((page.total, page.bytes), (0, 0));
    assert!(store.take_purge_request());
    assert_eq!(store.purge_expired_trash(NOW, 100).await.unwrap(), 1);
}

#[tokio::test]
async fn entries_are_private_to_their_owner() {
    let (store, alice, cal, bob) = setup().await;
    put(&store, cal.id, "a.ics", "uid-a", "event a").await;
    trash(&store, cal.id, "a.ics", NOW).await;
    let id = entries(&store, alice.id).await[0].id.clone();
    assert!(entries(&store, bob.id).await.is_empty());
    assert!(
        store
            .restore_trash_entry(bob.id, &id, None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!store.delete_trash_entry(bob.id, &id).await.unwrap());
    assert_eq!(store.empty_trash(bob.id).await.unwrap(), 0);
    assert_eq!(entries(&store, alice.id).await.len(), 1);
    assert_eq!(store.empty_trash(alice.id).await.unwrap(), 1);
    assert!(entries(&store, alice.id).await.is_empty());
}

#[tokio::test]
async fn expired_entries_are_purged_in_bounded_batches() {
    let (store, alice, cal, _) = setup().await;
    store.set_trash_retention_days(30);
    let work = store
        .create_collection(
            alice.id,
            CollectionKind::Calendar,
            &"work".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    for i in 0..5 {
        put(&store, work.id, &format!("{i}.ics"), &format!("u{i}"), "x").await;
    }
    store
        .delete_collection_to_trash(work.id, NOW)
        .await
        .unwrap();
    put(&store, cal.id, "old.ics", "old", "old").await;
    trash(&store, cal.id, "old.ics", NOW).await;
    put(&store, cal.id, "new.ics", "new", "new").await;
    trash(&store, cal.id, "new.ics", NOW + 10 * DAY).await;

    assert_eq!(
        store
            .purge_expired_trash(NOW + 30 * DAY - 1, 100)
            .await
            .unwrap(),
        0
    );
    let later = NOW + 30 * DAY;
    assert_eq!(store.purge_expired_trash(later, 3).await.unwrap(), 3);
    assert_eq!(store.purge_expired_trash(later, 3).await.unwrap(), 3);
    assert_eq!(store.purge_expired_trash(later, 3).await.unwrap(), 1);
    assert_eq!(store.purge_expired_trash(later, 3).await.unwrap(), 0);
    let left = entries(&store, alice.id).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].name.as_deref(), Some("new.ics"));
}

#[tokio::test]
async fn disabled_trash_deletes_for_good() {
    let (store, alice, cal, _) = setup().await;
    put(&store, cal.id, "a.ics", "uid-a", "event a").await;
    trash(&store, cal.id, "a.ics", NOW).await;
    store.set_trash_retention_days(0);
    put(&store, cal.id, "b.ics", "uid-b", "event b").await;
    assert!(trash(&store, cal.id, "b.ics", NOW).await);
    assert!(
        store.changes_since(cal.id, 0, 10).await.unwrap()[1]
            .meta
            .is_none()
    );
    let work = store
        .create_collection(
            alice.id,
            CollectionKind::Addressbook,
            &"work".parse().unwrap(),
            None,
            None,
            None,
            "VCARD",
        )
        .await
        .unwrap();
    store
        .delete_collection_to_trash(work.id, NOW)
        .await
        .unwrap();
    assert_eq!(
        entries(&store, alice.id).await.len(),
        1,
        "only a is in the trash"
    );
    // Entries left from before are purged on the next run.
    assert_eq!(store.purge_expired_trash(NOW, 100).await.unwrap(), 1);
    assert!(entries(&store, alice.id).await.is_empty());
}

#[tokio::test]
async fn the_trash_counts_towards_the_quota() {
    let (store, alice, cal, _) = setup().await;
    store.set_limits_for_testing(20, MAX_TRASH_ROWS_PER_USER);
    put(&store, cal.id, "a.ics", "uid-a", "0123456789").await;
    trash(&store, cal.id, "a.ics", NOW).await;
    put(&store, cal.id, "b.ics", "uid-b", "0123456789").await;
    // Deleting does not free space ...
    trash(&store, cal.id, "b.ics", NOW).await;
    let err = store
        .put_item(
            cal.id,
            "c.ics",
            &NewItem {
                uid: "uid-c",
                component: "VEVENT",
                range_start: None,
                range_end: None,
                etag: "\"c\"",
                content: b"x",
            },
            &Precondition::None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Limit(_)), "{err:?}");
    // ... restoring needs no extra space ...
    let id = entries(&store, alice.id).await[0].id.clone();
    store
        .restore_trash_entry(alice.id, &id, None)
        .await
        .unwrap()
        .unwrap();
    // ... emptying the trash does.
    store.empty_trash(alice.id).await.unwrap();
    put(&store, cal.id, "c.ics", "uid-c", "x").await;
}

#[tokio::test]
async fn a_full_trash_drops_the_oldest_entries() {
    let (store, alice, cal, _) = setup().await;
    store.set_limits_for_testing(MAX_BYTES_PER_USER_FOR_TESTS, 3);
    for i in 0..5 {
        let name = format!("{i}.ics");
        put(&store, cal.id, &name, &format!("u{i}"), "x").await;
        trash(&store, cal.id, &name, NOW + i).await;
    }
    let names: Vec<Option<String>> = entries(&store, alice.id)
        .await
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(
        names,
        [
            Some("4.ics".into()),
            Some("3.ics".into()),
            Some("2.ics".into())
        ]
    );
    // A collection that does not fit at all is deleted for good.
    let big = store
        .create_collection(
            alice.id,
            CollectionKind::Calendar,
            &"big".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    for i in 0..3 {
        put(&store, big.id, &format!("{i}.ics"), &format!("b{i}"), "x").await;
    }
    store
        .delete_collection_to_trash(big.id, NOW + 10)
        .await
        .unwrap();
    assert!(
        store
            .collection(alice.id, CollectionKind::Calendar, &big.slug)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(entries(&store, alice.id).await.len(), 3);
}

const MAX_BYTES_PER_USER_FOR_TESTS: i64 = 1 << 20;
