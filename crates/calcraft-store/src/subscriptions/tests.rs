use std::sync::atomic::Ordering;

use super::*;
use crate::{DEFAULT_COLLECTION_SLUG, NewItem, Precondition, Transfer, TrashConflict};

const URL: &str = "https://feeds.example/cal.ics?token=secret";

fn feed_item(uid: &str, body: &str) -> FeedItem {
    FeedItem {
        name: format!("{uid}.ics"),
        uid: uid.to_owned(),
        component: "VEVENT".to_owned(),
        range_start: Some(0),
        range_end: Some(10),
        recurring: false,
        index_version: 1,
        etag: format!("\"{uid}-{body}\""),
        content: format!("BEGIN:VCALENDAR {uid} {body}").into_bytes(),
    }
}

fn state(now: i64) -> FeedState<'static> {
    FeedState {
        feed_ttl_secs: Some(3600),
        http_etag: Some("\"v1\""),
        http_last_modified: None,
        content_hash: "hash",
        skipped: 0,
        now,
        next_refresh_at: now + 3600,
    }
}

async fn setup() -> (Store, i64) {
    let store = Store::in_memory().await.unwrap();
    let user = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    (store, user.id)
}

async fn subscribe(
    store: &Store,
    owner: i64,
    slug: &str,
    items: &[FeedItem],
) -> Result<Collection> {
    let slug: Slug = slug.parse().unwrap();
    store
        .create_subscription(
            &NewSubscription {
                owner_id: owner,
                slug: &slug,
                display_name: Some("Holidays"),
                color: None,
                components: "VEVENT,VTODO",
                source_url: URL,
                refresh_secs: None,
                max_per_user: 2,
            },
            items,
            &state(100),
        )
        .await
}

#[tokio::test]
async fn create_list_and_cap() {
    let (store, owner) = setup().await;
    let items = [feed_item("a", "1"), feed_item("b", "1")];
    let collection = subscribe(&store, owner, "subs", &items).await.unwrap();
    assert!(collection.read_only);
    assert_eq!(collection.display_name.as_deref(), Some("Holidays"));
    let list = store.subscriptions(owner).await.unwrap();
    assert_eq!(list.len(), 1);
    let sub = &list[0];
    assert_eq!(sub.source_url, URL);
    assert_eq!(sub.items, 2);
    assert_eq!(sub.feed_ttl_secs, Some(3600));
    assert_eq!(sub.http_etag.as_deref(), Some("\"v1\""));
    assert_eq!(sub.last_success_at, Some(100));
    assert_eq!(sub.next_refresh_at, 3700);
    // Ordinary collections are not subscriptions.
    let default: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
    assert!(store.subscription(owner, &default).await.unwrap().is_none());

    // Same slug: the collection exists; nothing else is left behind.
    assert!(matches!(
        subscribe(&store, owner, "subs", &items).await,
        Err(StoreError::CollectionExists)
    ));
    subscribe(&store, owner, "subs2", &[]).await.unwrap();
    // Per-user cap (2 in this test).
    assert!(matches!(
        subscribe(&store, owner, "subs3", &[]).await,
        Err(StoreError::Limit("too many subscriptions"))
    ));
    assert_eq!(store.count_subscriptions(owner).await.unwrap(), 2);
    let slug3: Slug = "subs3".parse().unwrap();
    assert!(
        store
            .collection(owner, CollectionKind::Calendar, &slug3)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn refresh_changes_only_what_changed() {
    let (store, owner) = setup().await;
    let collection = subscribe(
        &store,
        owner,
        "subs",
        &[feed_item("a", "1"), feed_item("b", "1")],
    )
    .await
    .unwrap();
    let revision = |store: Store| async move {
        let slug: Slug = "subs".parse().unwrap();
        store
            .collection(owner, CollectionKind::Calendar, &slug)
            .await
            .unwrap()
            .unwrap()
            .sync_revision
    };
    let r1 = revision(store.clone()).await;

    // Identical content: no new revision, no change-log entry.
    let outcome = store
        .apply_subscription_feed(
            collection.id,
            &[feed_item("a", "1"), feed_item("b", "1")],
            &state(200),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        outcome,
        ReplaceOutcome {
            unchanged: 2,
            ..ReplaceOutcome::default()
        }
    );
    assert_eq!(revision(store.clone()).await, r1);
    assert!(
        store
            .changes_since(collection.id, r1, 100)
            .await
            .unwrap()
            .is_empty()
    );

    // b changes, a is removed, c is new: one revision for all three.
    let outcome = store
        .apply_subscription_feed(
            collection.id,
            &[feed_item("b", "2"), feed_item("c", "1")],
            &state(300),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        outcome,
        ReplaceOutcome {
            added: 1,
            updated: 1,
            removed: 1,
            unchanged: 0
        }
    );
    let r2 = revision(store.clone()).await;
    assert_eq!(r2, r1 + 1);
    let mut changes: Vec<(String, bool)> = store
        .changes_since(collection.id, r1, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.name, c.meta.is_some()))
        .collect();
    changes.sort();
    assert_eq!(
        changes,
        [
            ("a.ics".to_owned(), false),
            ("b.ics".to_owned(), true),
            ("c.ics".to_owned(), true)
        ]
    );
    let item = store.item(collection.id, "b.ics").await.unwrap().unwrap();
    assert_eq!(item.etag, "\"b-2\"");

    // Success clears an earlier failure.
    store
        .subscription_failed(
            collection.id,
            &SubscriptionFailure {
                code: "fetch_failed",
                detail: Some("HTTP 500"),
                now: 400,
                next_refresh_at: 900,
            },
        )
        .await
        .unwrap();
    let sub = store
        .subscription_by_collection(collection.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (sub.failures, sub.last_error.as_deref()),
        (1, Some("fetch_failed"))
    );
    assert_eq!(sub.items, 2, "a failure keeps the last good content");
    store
        .subscription_unchanged(collection.id, 500, 4100)
        .await
        .unwrap();
    let sub = store
        .subscription_by_collection(collection.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (sub.failures, sub.last_error, sub.last_success_at),
        (0, None, Some(500))
    );
}

#[tokio::test]
async fn limits_keep_last_good_content() {
    let (store, owner) = setup().await;
    let collection = subscribe(&store, owner, "subs", &[feed_item("a", "1")])
        .await
        .unwrap();
    store.settings.quota_bytes.store(100, Ordering::Relaxed);
    let big = FeedItem {
        content: vec![b'x'; 200],
        ..feed_item("big", "1")
    };
    assert!(matches!(
        store
            .apply_subscription_feed(collection.id, &[big.clone()], &state(200))
            .await,
        Err(StoreError::Limit("storage quota exceeded"))
    ));
    assert!(store.item(collection.id, "a.ics").await.unwrap().is_some());
    assert!(
        store
            .item(collection.id, "big.ics")
            .await
            .unwrap()
            .is_none()
    );
    // Creating counts against the quota as well.
    assert!(matches!(
        subscribe(&store, owner, "subs2", &[big]).await,
        Err(StoreError::Limit("storage quota exceeded"))
    ));
    assert_eq!(store.count_subscriptions(owner).await.unwrap(), 1);
}

#[tokio::test]
async fn claim_is_rate_limited() {
    let (store, owner) = setup().await;
    let collection = subscribe(&store, owner, "subs", &[]).await.unwrap();
    // Created at 100 (counts as an attempt).
    assert!(
        !store
            .claim_subscription_refresh(collection.id, 130, 60, 999)
            .await
            .unwrap()
    );
    assert!(
        store
            .claim_subscription_refresh(collection.id, 160, 60, 999)
            .await
            .unwrap()
    );
    assert!(
        !store
            .claim_subscription_refresh(collection.id, 161, 60, 999)
            .await
            .unwrap()
    );
    assert_eq!(
        store.due_subscriptions(998, 10).await.unwrap(),
        Vec::<i64>::new()
    );
    assert_eq!(
        store.due_subscriptions(999, 10).await.unwrap(),
        vec![collection.id]
    );
    // Disabled accounts are not refreshed.
    store
        .set_disabled(&"alice".parse().unwrap(), true)
        .await
        .unwrap();
    assert!(store.due_subscriptions(999, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn read_only_collections_refuse_every_write() {
    let (store, owner) = setup().await;
    let sub = subscribe(&store, owner, "subs", &[feed_item("a", "1")])
        .await
        .unwrap();
    let default: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
    let cal = store
        .collection(owner, CollectionKind::Calendar, &default)
        .await
        .unwrap()
        .unwrap();
    let new = NewItem {
        uid: "x",
        component: "VEVENT",
        range_start: None,
        range_end: None,
        recurring: false,
        index_version: 0,
        etag: "\"x\"",
        content: b"BEGIN:VCALENDAR",
    };
    assert!(matches!(
        store
            .put_item(sub.id, "x.ics", &new, &Precondition::None)
            .await,
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(
        store
            .delete_item(sub.id, "a.ics", &Precondition::None)
            .await,
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(
        store
            .delete_item_to_trash(sub.id, "a.ics", &Precondition::None, 1, |_| None)
            .await,
        Err(StoreError::ReadOnly)
    ));
    store
        .put_item(cal.id, "x.ics", &new, &Precondition::None)
        .await
        .unwrap();
    let transfer = |from: i64, name: &'static str, to: i64| Transfer {
        owner_id: owner,
        source_collection: from,
        source_name: name,
        source_precondition: &Precondition::None,
        destination_collection: to,
        destination_name: "moved.ics",
        overwrite: true,
    };
    assert!(matches!(
        store.copy_item(&transfer(cal.id, "x.ics", sub.id)).await,
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(
        store.move_item(&transfer(sub.id, "a.ics", cal.id)).await,
        Err(StoreError::ReadOnly)
    ));
    // Copying out of a subscription is fine (the copy is the user's own).
    store
        .copy_item(&transfer(sub.id, "a.ics", cal.id))
        .await
        .unwrap();

    // Trash entries cannot be restored into a subscription.
    store
        .delete_item_to_trash(cal.id, "x.ics", &Precondition::None, 1, |_| None)
        .await
        .unwrap();
    let entry = store
        .trash_entries(owner, 10, 0)
        .await
        .unwrap()
        .entries
        .remove(0);
    let slug: Slug = "subs".parse().unwrap();
    assert!(matches!(
        store
            .restore_trash_entry(owner, &entry.id, Some(&slug))
            .await,
        Err(StoreError::TrashConflict(TrashConflict::InvalidTarget))
    ));
    assert_eq!(store.items(sub.id).await.unwrap().len(), 1);

    // Deleting the subscription bypasses the trash.
    let before = store
        .trash_entries(owner, 10, 0)
        .await
        .unwrap()
        .entries
        .len();
    store.delete_collection_to_trash(sub.id, 2).await.unwrap();
    assert!(store.subscription(owner, &slug).await.unwrap().is_none());
    assert!(
        store
            .collection(owner, CollectionKind::Calendar, &slug)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .trash_entries(owner, 10, 0)
            .await
            .unwrap()
            .entries
            .len(),
        before
    );
    assert!(
        store
            .apply_subscription_feed(sub.id, &[], &state(5))
            .await
            .unwrap()
            .is_none()
    );
}
