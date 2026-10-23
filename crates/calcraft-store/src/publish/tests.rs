use calcraft_core::CollectionKind;

use super::*;
use crate::DEFAULT_COLLECTION_SLUG;

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn link(hash: &str) -> NewPublishLink<'_> {
    NewPublishLink {
        token_hash: hash,
        busy_only: true,
        private_events: PrivateEvents::Hide,
        include_attendees: false,
        now: 7,
    }
}

#[tokio::test]
async fn links_resolve_by_hash_until_revoked() {
    let store = Store::in_memory().await.unwrap();
    let alice = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let bob = store
        .create_user(&"bob".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
    let cal = store
        .collection(alice.id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    let ab = store
        .collection(alice.id, CollectionKind::Addressbook, &slug)
        .await
        .unwrap()
        .unwrap();
    // Only the owner's calendars.
    assert!(
        store
            .create_publish_link(cal.id, bob.id, &link(HASH_A))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .create_publish_link(ab.id, alice.id, &link(HASH_A))
            .await
            .unwrap()
            .is_none()
    );
    let created = store
        .create_publish_link(cal.id, alice.id, &link(HASH_A))
        .await
        .unwrap()
        .unwrap();
    assert!(created.busy_only && !created.include_attendees);
    assert_eq!(created.private_events, PrivateEvents::Hide);
    assert_eq!(created.public_id.len(), 32);
    let (found, stored, collection) = store.published_calendar(HASH_A).await.unwrap().unwrap();
    assert_eq!(found, created);
    assert_eq!(stored, HASH_A);
    assert_eq!(collection.id, cal.id);
    assert_eq!(collection.owner_id, alice.id);
    assert!(store.published_calendar(HASH_B).await.unwrap().is_none());
    assert_eq!(
        store.publish_links(cal.id).await.unwrap(),
        vec![created.clone()]
    );

    // A disabled owner's links do not resolve.
    store.set_disabled(&alice.username, true).await.unwrap();
    assert!(store.published_calendar(HASH_A).await.unwrap().is_none());
    store.set_disabled(&alice.username, false).await.unwrap();

    assert!(
        !store
            .delete_publish_link(ab.id, &created.public_id)
            .await
            .unwrap()
    );
    assert!(
        store
            .delete_publish_link(cal.id, &created.public_id)
            .await
            .unwrap()
    );
    assert!(store.published_calendar(HASH_A).await.unwrap().is_none());
}

#[tokio::test]
async fn limits_links_per_calendar() {
    let store = Store::in_memory().await.unwrap();
    let alice = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let cal = store
        .collection(
            alice.id,
            CollectionKind::Calendar,
            &DEFAULT_COLLECTION_SLUG.parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    for n in 0..MAX_LINKS_PER_CALENDAR {
        let hash = format!("{n:064x}");
        store
            .create_publish_link(cal.id, alice.id, &link(&hash))
            .await
            .unwrap()
            .unwrap();
    }
    assert!(matches!(
        store
            .create_publish_link(cal.id, alice.id, &link(HASH_B))
            .await,
        Err(StoreError::Limit(_))
    ));
    assert!(matches!(
        store
            .create_publish_link(cal.id, alice.id, &link("short"))
            .await,
        Err(StoreError::Corrupt(_))
    ));
    // Deleting the calendar deletes its links.
    store.delete_collection(cal.id).await.unwrap();
    assert!(store.publish_links(cal.id).await.unwrap().is_empty());
}
