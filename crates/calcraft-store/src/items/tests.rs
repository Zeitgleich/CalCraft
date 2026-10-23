use super::*;
use crate::DEFAULT_COLLECTION_SLUG;

async fn setup() -> (Store, Collection, Collection) {
    let store = Store::in_memory().await.unwrap();
    let slug: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
    let mut calendars = Vec::new();
    for user in ["alice", "bob"] {
        let user = store
            .create_user(&user.parse().unwrap(), "h", None)
            .await
            .unwrap();
        calendars.push(
            store
                .collection(user.id, CollectionKind::Calendar, &slug)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let bob = calendars.pop().unwrap();
    let alice = calendars.pop().unwrap();
    (store, alice, bob)
}

fn item<'a>(uid: &'a str, etag: &'a str, start: Option<i64>, end: Option<i64>) -> NewItem<'a> {
    NewItem {
        uid,
        component: "VEVENT",
        range_start: start,
        range_end: end,
        recurring: false,
        index_version: 0,
        etag,
        content: b"BEGIN:VCALENDAR",
        attachments: &[],
    }
}

#[tokio::test]
async fn put_get_delete_with_preconditions() {
    let (store, cal, _) = setup().await;
    let new = item("u1", "\"e1\"", Some(0), Some(10));
    assert_eq!(
        store
            .put_item(cal.id, "a.ics", &new, &Precondition::MustNotExist)
            .await
            .unwrap(),
        PutOutcome::Created
    );
    assert!(matches!(
        store
            .put_item(cal.id, "a.ics", &new, &Precondition::MustNotExist)
            .await,
        Err(StoreError::PreconditionFailed)
    ));
    let v2 = item("u1", "\"e2\"", Some(0), Some(10));
    assert!(matches!(
        store
            .put_item(
                cal.id,
                "a.ics",
                &v2,
                &Precondition::IfMatch(vec!["\"old\"".into()])
            )
            .await,
        Err(StoreError::PreconditionFailed)
    ));
    assert_eq!(
        store
            .put_item(
                cal.id,
                "a.ics",
                &v2,
                &Precondition::IfMatch(vec!["\"e1\"".into()])
            )
            .await
            .unwrap(),
        PutOutcome::Updated
    );
    assert_eq!(
        store.item(cal.id, "a.ics").await.unwrap().unwrap().etag,
        "\"e2\""
    );

    assert!(matches!(
        store
            .delete_item(
                cal.id,
                "a.ics",
                &Precondition::IfMatch(vec!["\"e1\"".into()])
            )
            .await,
        Err(StoreError::PreconditionFailed)
    ));
    assert!(
        store
            .delete_item(cal.id, "a.ics", &Precondition::None)
            .await
            .unwrap()
    );
    assert!(
        !store
            .delete_item(cal.id, "a.ics", &Precondition::None)
            .await
            .unwrap()
    );

    let revision = store
        .collection(cal.owner_id, CollectionKind::Calendar, &cal.slug)
        .await
        .unwrap()
        .unwrap()
        .sync_revision;
    assert_eq!(revision, 3);
}

#[tokio::test]
async fn uid_lookup_is_scoped_to_the_collection() {
    let (store, alice, bob) = setup().await;
    let new = item("shared-uid", "\"e\"", None, None);
    store
        .put_item(alice.id, "a.ics", &new, &Precondition::None)
        .await
        .unwrap();
    assert_eq!(
        store
            .item_name_by_uid(alice.id, "shared-uid")
            .await
            .unwrap()
            .as_deref(),
        Some("a.ics")
    );
    assert_eq!(
        store.item_name_by_uid(bob.id, "shared-uid").await.unwrap(),
        None
    );
    assert_eq!(
        store.item_name_by_uid(alice.id, "other").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn if_none_match_with_tags() {
    let (store, cal, _) = setup().await;
    let v1 = item("u", "\"e1\"", None, None);
    store
        .put_item(cal.id, "a.ics", &v1, &Precondition::None)
        .await
        .unwrap();
    let v2 = item("u", "\"e2\"", None, None);
    assert!(matches!(
        store
            .put_item(
                cal.id,
                "a.ics",
                &v2,
                &Precondition::IfNoneMatch(vec!["\"e1\"".into()])
            )
            .await,
        Err(StoreError::PreconditionFailed)
    ));
    store
        .put_item(
            cal.id,
            "a.ics",
            &v2,
            &Precondition::IfNoneMatch(vec!["\"other\"".into()]),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .put_item(
                cal.id,
                "a.ics",
                &v1,
                &Precondition::IfMatch(vec!["\"x\"".into(), "\"e1\"".into()])
            )
            .await,
        Err(StoreError::PreconditionFailed)
    ));
    store
        .put_item(
            cal.id,
            "a.ics",
            &v1,
            &Precondition::IfMatch(vec!["\"x\"".into(), "\"e2\"".into()]),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn change_log_for_sync() {
    let (store, cal, other) = setup().await;
    let names = |changes: Vec<Change>| {
        changes
            .into_iter()
            .map(|c| (c.name, c.meta.is_some()))
            .collect::<Vec<_>>()
    };
    store
        .put_item(
            cal.id,
            "a.ics",
            &item("a", "\"1\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    store
        .put_item(
            cal.id,
            "b.ics",
            &item("b", "\"1\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    let mid = store
        .changes_since(cal.id, 0, 100)
        .await
        .unwrap()
        .last()
        .unwrap()
        .revision;
    store
        .put_item(
            cal.id,
            "a.ics",
            &item("a", "\"2\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    store
        .delete_item(cal.id, "b.ics", &Precondition::None)
        .await
        .unwrap();
    store
        .put_item(
            other.id,
            "x.ics",
            &item("x", "\"1\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();

    // Latest change per object, ordered by revision; other collections excluded.
    assert_eq!(
        names(store.changes_since(cal.id, 0, 100).await.unwrap()),
        [("a.ics".to_owned(), true), ("b.ics".to_owned(), false)]
    );
    assert_eq!(
        names(store.changes_since(cal.id, mid, 100).await.unwrap()).len(),
        2
    );
    assert_eq!(store.changes_since(cal.id, 0, 1).await.unwrap().len(), 1);
    // Repeated updates do not grow the log.
    for i in 0..20 {
        let etag = format!("\"{i}\"");
        store
            .put_item(
                cal.id,
                "a.ics",
                &item("a", &etag, None, None),
                &Precondition::None,
            )
            .await
            .unwrap();
    }
    assert_eq!(store.changes_since(cal.id, 0, 1000).await.unwrap().len(), 2);
    let by_rev = store.items_by_revision(cal.id, 10).await.unwrap();
    assert_eq!(by_rev.len(), 1);
}

#[tokio::test]
async fn uid_must_be_unique_per_collection() {
    let (store, alice, bob) = setup().await;
    store
        .put_item(
            alice.id,
            "a.ics",
            &item("same", "\"1\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    assert!(matches!(
        store.put_item(alice.id, "b.ics", &item("same", "\"2\"", None, None), &Precondition::None).await,
        Err(StoreError::UidConflict(name)) if name == "a.ics"
    ));
    // Another user's calendar is independent.
    store
        .put_item(
            bob.id,
            "b.ics",
            &item("same", "\"2\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn items_are_scoped_to_their_collection() {
    let (store, alice, bob) = setup().await;
    store
        .put_item(
            alice.id,
            "secret.ics",
            &item("u", "\"1\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    assert!(store.item(bob.id, "secret.ics").await.unwrap().is_none());
    assert!(
        store
            .query_items(bob.id, None, None, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .delete_item(bob.id, "secret.ics", &Precondition::None)
            .await
            .unwrap()
    );
    assert!(store.item(alice.id, "secret.ics").await.unwrap().is_some());
}

#[tokio::test]
async fn time_range_query() {
    let (store, cal, _) = setup().await;
    for (name, start, end) in [
        ("early.ics", Some(0), Some(100)),
        ("late.ics", Some(1000), Some(1100)),
        ("open.ics", Some(50), None),
        ("undated.ics", None, None),
    ] {
        store
            .put_item(
                cal.id,
                name,
                &item(name, "\"e\"", start, end),
                &Precondition::None,
            )
            .await
            .unwrap();
    }
    let names = |items: Vec<Item>| items.into_iter().map(|i| i.name).collect::<Vec<_>>();
    assert_eq!(
        names(
            store
                .query_items(cal.id, Some("VEVENT"), Some(200), Some(300))
                .await
                .unwrap()
        ),
        ["open.ics", "undated.ics"]
    );
    assert_eq!(
        names(
            store
                .query_items(cal.id, Some("VEVENT"), Some(1050), None)
                .await
                .unwrap()
        ),
        ["late.ics", "open.ics", "undated.ics"]
    );
    assert!(
        store
            .query_items(cal.id, Some("VTODO"), None, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn query_candidates_page_and_flag_recurrence() {
    let (store, cal, _) = setup().await;
    for (index, name) in ["a.ics", "b.ics", "c.ics"].into_iter().enumerate() {
        let mut new = item(name, "\"e\"", Some(0), Some(100));
        new.recurring = index == 1;
        store
            .put_item(cal.id, name, &new, &Precondition::None)
            .await
            .unwrap();
    }
    let page = |after: Option<&'static str>| {
        let store = store.clone();
        async move {
            store
                .query_candidates(cal.id, Some("VEVENT"), Some(50), Some(60), after, 2)
                .await
                .unwrap()
                .into_iter()
                .map(|c| (c.meta.name, c.recurring))
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        page(None).await,
        [("a.ics".to_owned(), false), ("b.ics".to_owned(), true)]
    );
    assert_eq!(page(Some("b.ics")).await, [("c.ics".to_owned(), false)]);
    assert!(page(Some("c.ics")).await.is_empty());
}

#[tokio::test]
async fn stale_index_is_updated_without_a_change() {
    let (store, cal, _) = setup().await;
    store
        .put_item(
            cal.id,
            "old.ics",
            &item("old", "\"1\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    let mut current = item("new", "\"2\"", Some(5), Some(6));
    current.index_version = 1;
    store
        .put_item(cal.id, "new.ics", &current, &Precondition::None)
        .await
        .unwrap();
    let revision = async |store: &Store| {
        store
            .collection(cal.owner_id, CollectionKind::Calendar, &cal.slug)
            .await
            .unwrap()
            .unwrap()
            .sync_revision
    };
    let before = revision(&store).await;

    let stale = store.stale_index(1, 0, 10).await.unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].etag, "\"1\"");
    let index = ItemIndex {
        range_start: Some(10),
        range_end: Some(20),
        recurring: true,
        version: 1,
    };
    // A changed object (other etag) is left alone.
    assert!(
        !store
            .set_item_index(stale[0].id, "\"x\"", &index)
            .await
            .unwrap()
    );
    assert!(
        store
            .set_item_index(stale[0].id, "\"1\"", &index)
            .await
            .unwrap()
    );
    // Idempotent: a second run changes nothing.
    assert!(
        !store
            .set_item_index(stale[0].id, "\"1\"", &index)
            .await
            .unwrap()
    );
    assert!(store.stale_index(1, 0, 10).await.unwrap().is_empty());
    let found = store
        .query_candidates(cal.id, None, Some(12), Some(13), None, 10)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].recurring);
    assert_eq!(revision(&store).await, before);
}

#[tokio::test]
async fn collections_lifecycle() {
    let (store, alice, _) = setup().await;
    let slug: Slug = "work".parse().unwrap();
    let work = store
        .create_collection(
            alice.owner_id,
            CollectionKind::Calendar,
            &slug,
            Some("Work"),
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .create_collection(
                alice.owner_id,
                CollectionKind::Calendar,
                &slug,
                None,
                None,
                None,
                "VEVENT"
            )
            .await,
        Err(StoreError::CollectionExists)
    ));
    store
        .update_collection(work.id, Some(Some("Job")), None, Some(Some("#FF0000FF")))
        .await
        .unwrap();
    let updated = store
        .collection(alice.owner_id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.display_name.as_deref(), Some("Job"));
    assert_eq!(updated.color.as_deref(), Some("#FF0000FF"));

    store
        .put_item(
            work.id,
            "x.ics",
            &item("x", "\"1\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    store.delete_collection(work.id).await.unwrap();
    assert!(
        store
            .collection(alice.owner_id, CollectionKind::Calendar, &slug)
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.item(work.id, "x.ics").await.unwrap().is_none());
}

async fn calendar(store: &Store, owner_id: i64, slug: &str, components: &str) -> Collection {
    store
        .create_collection(
            owner_id,
            CollectionKind::Calendar,
            &slug.parse().unwrap(),
            None,
            None,
            None,
            components,
        )
        .await
        .unwrap()
}

fn transfer<'a>(
    owner_id: i64,
    from: (i64, &'a str),
    to: (i64, &'a str),
    precondition: &'a Precondition,
    overwrite: bool,
) -> Transfer<'a> {
    Transfer {
        owner_id,
        source_collection: from.0,
        source_name: from.1,
        source_precondition: precondition,
        destination_collection: to.0,
        destination_name: to.1,
        overwrite,
    }
}

async fn revision(store: &Store, collection: &Collection) -> i64 {
    store
        .collection(collection.owner_id, collection.kind, &collection.slug)
        .await
        .unwrap()
        .unwrap()
        .sync_revision
}

#[tokio::test]
async fn transfers_keep_the_time_range_index() {
    let (store, cal, _) = setup().await;
    let work = calendar(&store, cal.owner_id, "work", "VEVENT,VTODO").await;
    let mut new = item("u", "\"e\"", Some(5), Some(9));
    new.recurring = true;
    new.index_version = 1;
    store
        .put_item(cal.id, "a.ics", &new, &Precondition::None)
        .await
        .unwrap();
    let none = Precondition::None;
    store
        .copy_item(&transfer(
            cal.owner_id,
            (cal.id, "a.ics"),
            (work.id, "b.ics"),
            &none,
            false,
        ))
        .await
        .unwrap()
        .unwrap();
    let found = store
        .query_candidates(work.id, None, Some(6), Some(7), None, 10)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].recurring);
    // Copies keep their index version: nothing to re-index.
    assert!(store.stale_index(1, 0, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn move_logs_removal_and_addition() {
    let (store, cal, _) = setup().await;
    let work = calendar(&store, cal.owner_id, "work", "VEVENT,VTODO").await;
    store
        .put_item(
            cal.id,
            "a.ics",
            &item("u", "\"e\"", Some(5), Some(9)),
            &Precondition::None,
        )
        .await
        .unwrap();
    let (before_cal, before_work) = (revision(&store, &cal).await, revision(&store, &work).await);
    let none = Precondition::None;
    let outcome = store
        .move_item(&transfer(
            cal.owner_id,
            (cal.id, "a.ics"),
            (work.id, "b.ics"),
            &none,
            true,
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        outcome,
        TransferOutcome {
            created: true,
            etag: "\"e\"".into()
        }
    );
    assert!(store.item(cal.id, "a.ics").await.unwrap().is_none());
    assert_eq!(
        store.item(work.id, "b.ics").await.unwrap().unwrap().etag,
        "\"e\""
    );
    // Time range and UID travel with the object.
    assert_eq!(
        store
            .query_candidates(work.id, Some("VEVENT"), Some(0), Some(6), None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .item_name_by_uid(work.id, "u")
            .await
            .unwrap()
            .as_deref(),
        Some("b.ics")
    );
    assert_eq!(revision(&store, &cal).await, before_cal + 1);
    assert_eq!(revision(&store, &work).await, before_work + 1);
    let removed = store.changes_since(cal.id, before_cal, 10).await.unwrap();
    assert_eq!(removed.len(), 1);
    assert_eq!(
        (removed[0].name.as_str(), removed[0].meta.is_none()),
        ("a.ics", true)
    );
    let added = store.changes_since(work.id, before_work, 10).await.unwrap();
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].name, "b.ics");
    assert_eq!(added[0].meta.as_ref().unwrap().etag, "\"e\"");
    // The source is gone: a second move finds nothing.
    assert_eq!(
        store
            .move_item(&transfer(
                cal.owner_id,
                (cal.id, "a.ics"),
                (work.id, "c.ics"),
                &none,
                true
            ))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn rename_within_a_collection_keeps_the_uid() {
    let (store, cal, _) = setup().await;
    store
        .put_item(
            cal.id,
            "a.ics",
            &item("u", "\"e\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    let none = Precondition::None;
    let renamed = transfer(
        cal.owner_id,
        (cal.id, "a.ics"),
        (cal.id, "b.ics"),
        &none,
        true,
    );
    // A copy would duplicate the UID in the collection.
    assert!(matches!(
        store.copy_item(&renamed).await,
        Err(StoreError::UidConflict(name)) if name == "a.ics"
    ));
    assert!(store.move_item(&renamed).await.unwrap().unwrap().created);
    assert_eq!(store.items(cal.id).await.unwrap().len(), 1);
    assert!(matches!(
        store
            .move_item(&transfer(
                cal.owner_id,
                (cal.id, "b.ics"),
                (cal.id, "b.ics"),
                &none,
                true
            ))
            .await,
        Err(StoreError::SameResource)
    ));
}

#[tokio::test]
async fn copy_and_overwrite_rules() {
    let (store, cal, _) = setup().await;
    let work = calendar(&store, cal.owner_id, "work", "VEVENT").await;
    let todos = calendar(&store, cal.owner_id, "todos", "VTODO").await;
    store
        .put_item(
            cal.id,
            "a.ics",
            &item("u", "\"e1\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    store
        .put_item(
            work.id,
            "x.ics",
            &item("other", "\"e2\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    let none = Precondition::None;
    let owner = cal.owner_id;
    // Overwrite F on an existing destination.
    assert!(matches!(
        store
            .copy_item(&transfer(
                owner,
                (cal.id, "a.ics"),
                (work.id, "x.ics"),
                &none,
                false
            ))
            .await,
        Err(StoreError::PreconditionFailed)
    ));
    // Source preconditions.
    let wrong = Precondition::IfMatch(vec!["\"nope\"".into()]);
    assert!(matches!(
        store
            .copy_item(&transfer(
                owner,
                (cal.id, "a.ics"),
                (work.id, "y.ics"),
                &wrong,
                true
            ))
            .await,
        Err(StoreError::PreconditionFailed)
    ));
    let right = Precondition::IfMatch(vec!["\"e1\"".into()]);
    let replaced = store
        .copy_item(&transfer(
            owner,
            (cal.id, "a.ics"),
            (work.id, "x.ics"),
            &right,
            true,
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(!replaced.created);
    assert_eq!(
        store.item(work.id, "x.ics").await.unwrap().unwrap().etag,
        "\"e1\""
    );
    assert!(store.item(cal.id, "a.ics").await.unwrap().is_some());
    // The UID is now used in `work`: a second copy conflicts.
    assert!(matches!(
        store.copy_item(&transfer(owner, (cal.id, "a.ics"), (work.id, "z.ics"), &none, true)).await,
        Err(StoreError::UidConflict(name)) if name == "x.ics"
    ));
    // Component set of the destination.
    assert!(matches!(
        store
            .move_item(&transfer(
                owner,
                (cal.id, "a.ics"),
                (todos.id, "a.ics"),
                &none,
                true
            ))
            .await,
        Err(StoreError::UnsupportedComponent)
    ));
    assert!(store.item(cal.id, "a.ics").await.unwrap().is_some());
}

#[tokio::test]
async fn transfers_stay_within_one_owner_and_kind() {
    let (store, alice, bob) = setup().await;
    store
        .put_item(
            alice.id,
            "a.ics",
            &item("u", "\"e\"", None, None),
            &Precondition::None,
        )
        .await
        .unwrap();
    let none = Precondition::None;
    // Into bob's calendar, even with alice as owner.
    assert!(matches!(
        store
            .move_item(&transfer(
                alice.owner_id,
                (alice.id, "a.ics"),
                (bob.id, "a.ics"),
                &none,
                true
            ))
            .await,
        Err(StoreError::MissingDestination)
    ));
    // As bob: alice's source does not exist for him.
    assert_eq!(
        store
            .move_item(&transfer(
                bob.owner_id,
                (alice.id, "a.ics"),
                (bob.id, "a.ics"),
                &none,
                true
            ))
            .await
            .unwrap(),
        None
    );
    // Into alice's address book.
    let book = store
        .collection(
            alice.owner_id,
            CollectionKind::Addressbook,
            &DEFAULT_COLLECTION_SLUG.parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .copy_item(&transfer(
                alice.owner_id,
                (alice.id, "a.ics"),
                (book.id, "a.ics"),
                &none,
                true
            ))
            .await,
        Err(StoreError::MissingDestination)
    ));
    assert!(store.item(alice.id, "a.ics").await.unwrap().is_some());
    assert!(store.items(bob.id).await.unwrap().is_empty());
    assert!(store.items(book.id).await.unwrap().is_empty());
}
