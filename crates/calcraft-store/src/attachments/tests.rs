use calcraft_core::{CollectionKind, Slug};

use super::*;
use crate::{
    Collection, DEFAULT_COLLECTION_SLUG, NewItem, Precondition, ScheduleTag, Transfer, User,
};

const A1: &str = "0123456789abcdef0123456789abcdef";
const A2: &str = "fedcba9876543210fedcba9876543210";
const B1: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";

struct Setup {
    store: Store,
    alice: User,
    bob: User,
    alice_cal: Collection,
    alice_cal2: Collection,
    bob_cal: Collection,
}

async fn setup() -> Setup {
    let store = Store::in_memory().await.unwrap();
    let slug: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
    let alice = store
        .create_user(&"alice".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let bob = store
        .create_user(&"bob".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let calendar = |user: i64| {
        let store = store.clone();
        let slug = slug.clone();
        async move {
            store
                .collection(user, CollectionKind::Calendar, &slug)
                .await
                .unwrap()
                .unwrap()
        }
    };
    let alice_cal = calendar(alice.id).await;
    let bob_cal = calendar(bob.id).await;
    let alice_cal2 = store
        .create_collection(
            alice.id,
            CollectionKind::Calendar,
            &"second".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT,VTODO",
        )
        .await
        .unwrap();
    Setup {
        store,
        alice,
        bob,
        alice_cal,
        alice_cal2,
        bob_cal,
    }
}

async fn upload(store: &Store, owner: i64, id: &str, content: &[u8], now: i64) {
    store
        .insert_attachment(
            &NewAttachment {
                managed_id: id,
                owner_id: owner,
                filename: "report.pdf",
                media_type: "application/pdf",
                content,
                now,
            },
            i64::MAX,
        )
        .await
        .unwrap();
}

async fn put(store: &Store, collection: i64, name: &str, uid: &str, ids: &[&str]) {
    put_with(store, collection, name, uid, ids, &ScheduleTag::default())
        .await
        .unwrap();
}

async fn put_with(
    store: &Store,
    collection: i64,
    name: &str,
    uid: &str,
    ids: &[&str],
    tags: &ScheduleTag<'_>,
) -> Result<crate::PutOutcome> {
    let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    let content = format!("BEGIN:VCALENDAR {uid} {ids:?}");
    let etag = format!("\"{}\"", content.len());
    store
        .put_item_scheduled(
            collection,
            name,
            &NewItem {
                uid,
                component: "VEVENT",
                range_start: None,
                range_end: None,
                recurring: false,
                index_version: 0,
                etag: &etag,
                content: content.as_bytes(),
                attachments: &ids,
            },
            &Precondition::None,
            tags,
        )
        .await
}

async fn readable(store: &Store, id: &str, user: &User) -> bool {
    let meta = store.readable_attachment(id, user.id).await.unwrap();
    let content = store.attachment_content(id, user.id).await.unwrap();
    assert_eq!(meta.is_some(), content.is_some());
    meta.is_some()
}

async fn exists(store: &Store, owner: i64) -> i64 {
    store.attachment_usage(owner).await.unwrap().0
}

#[tokio::test]
async fn references_follow_the_content_and_collect_garbage() {
    let s = setup().await;
    upload(&s.store, s.alice.id, A1, b"one", 0).await;
    upload(&s.store, s.alice.id, A2, b"two", 0).await;
    put(&s.store, s.alice_cal.id, "e.ics", "e", &[A1, A2]).await;
    let refs = s
        .store
        .item_attachments(s.alice_cal.id, "e.ics")
        .await
        .unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].size, 3);
    assert!(readable(&s.store, A1, &s.alice).await);
    assert!(!readable(&s.store, A1, &s.bob).await);

    // Dropping A2 from the content removes the attachment, A1 stays.
    put(&s.store, s.alice_cal.id, "e.ics", "e", &[A1]).await;
    assert_eq!(exists(&s.store, s.alice.id).await, 1);
    assert!(!readable(&s.store, A2, &s.alice).await);
    // Unknown, malformed and duplicate ids are ignored.
    put(
        &s.store,
        s.alice_cal.id,
        "e.ics",
        "e",
        &[A1, A1, "nope", A2],
    )
    .await;
    assert_eq!(
        s.store
            .item_attachments(s.alice_cal.id, "e.ics")
            .await
            .unwrap()
            .len(),
        1
    );
    // Deleting the object without trash collects the attachment.
    s.store
        .delete_item(s.alice_cal.id, "e.ics", &Precondition::None)
        .await
        .unwrap();
    assert_eq!(exists(&s.store, s.alice.id).await, 0);
}

#[tokio::test]
async fn foreign_managed_ids_grant_nothing() {
    let s = setup().await;
    upload(&s.store, s.alice.id, A1, b"secret", 0).await;
    put(&s.store, s.alice_cal.id, "e.ics", "e", &[A1]).await;
    // Bob writes alice's managed id into an event of his own.
    put(&s.store, s.bob_cal.id, "x.ics", "x", &[A1]).await;
    assert!(
        s.store
            .item_attachments(s.bob_cal.id, "x.ics")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!readable(&s.store, A1, &s.bob).await);
    assert!(
        s.store
            .referable_attachments(s.bob_cal.id, "x.ics", s.bob.id, &[A1.to_owned()])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.store
            .referable_attachments(s.alice_cal2.id, "y.ics", s.alice.id, &[A1.to_owned()])
            .await
            .unwrap()
            .len(),
        1
    );
    // Nor does a guard that is not an object referencing it.
    let tags = ScheduleTag {
        set: Some("\"t\""),
        if_match: None,
        requires: Some((s.bob_cal.id, "x.ics", "\"none\"")),
    };
    assert!(matches!(
        put_with(&s.store, s.bob_cal.id, "y.ics", "y", &[A1], &tags).await,
        Err(StoreError::Superseded)
    ));
}

#[tokio::test]
async fn deliveries_share_the_organizer_attachment_until_removed() {
    let s = setup().await;
    upload(&s.store, s.alice.id, A1, b"agenda", 0).await;
    let organizer = ScheduleTag {
        set: Some("\"tag1\""),
        if_match: None,
        requires: None,
    };
    put_with(&s.store, s.alice_cal.id, "m.ics", "m", &[A1], &organizer)
        .await
        .unwrap();
    let delivery = ScheduleTag {
        set: Some("\"copy\""),
        if_match: None,
        requires: Some((s.alice_cal.id, "m.ics", "\"tag1\"")),
    };
    put_with(&s.store, s.bob_cal.id, "m.ics", "m", &[A1], &delivery)
        .await
        .unwrap();
    assert!(readable(&s.store, A1, &s.bob).await);
    // Bob keeps it with his own updates (PARTSTAT).
    put(&s.store, s.bob_cal.id, "m.ics", "m", &[A1]).await;
    assert!(readable(&s.store, A1, &s.bob).await);
    // Bob's copy goes into his trash: the reference to alice's attachment
    // does not follow, and a restore does not bring it back.
    s.store
        .delete_item_to_trash(s.bob_cal.id, "m.ics", &Precondition::None, 10, |_| None)
        .await
        .unwrap();
    assert!(!readable(&s.store, A1, &s.bob).await);
    let entry = s
        .store
        .trash_entries(s.bob.id, 10, 0)
        .await
        .unwrap()
        .entries[0]
        .id
        .clone();
    s.store
        .restore_trash_entry(s.bob.id, &entry, None)
        .await
        .unwrap()
        .unwrap();
    assert!(!readable(&s.store, A1, &s.bob).await);
    // The organizer removes it: the attachment is gone.
    put(&s.store, s.alice_cal.id, "m.ics", "m", &[]).await;
    assert_eq!(exists(&s.store, s.alice.id).await, 0);
}

#[tokio::test]
async fn the_last_reference_keeps_an_attachment() {
    let s = setup().await;
    upload(&s.store, s.alice.id, A1, b"agenda", 0).await;
    let organizer = ScheduleTag {
        set: Some("\"tag1\""),
        if_match: None,
        requires: None,
    };
    put_with(&s.store, s.alice_cal.id, "m.ics", "m", &[A1], &organizer)
        .await
        .unwrap();
    let delivery = ScheduleTag {
        set: Some("\"copy\""),
        if_match: None,
        requires: Some((s.alice_cal.id, "m.ics", "\"tag1\"")),
    };
    put_with(&s.store, s.bob_cal.id, "m.ics", "m", &[A1], &delivery)
        .await
        .unwrap();
    // The organizer's object goes; bob's copy still references it.
    s.store
        .delete_item(s.alice_cal.id, "m.ics", &Precondition::None)
        .await
        .unwrap();
    assert!(readable(&s.store, A1, &s.bob).await);
    s.store
        .delete_item(s.bob_cal.id, "m.ics", &Precondition::None)
        .await
        .unwrap();
    assert_eq!(exists(&s.store, s.alice.id).await, 0);
}

#[tokio::test]
async fn trash_keeps_attachments_until_purged() {
    let s = setup().await;
    upload(&s.store, s.alice.id, A1, b"keep", 0).await;
    put(&s.store, s.alice_cal.id, "e.ics", "e", &[A1]).await;
    s.store
        .delete_item_to_trash(s.alice_cal.id, "e.ics", &Precondition::None, 10, |_| None)
        .await
        .unwrap();
    assert_eq!(exists(&s.store, s.alice.id).await, 1);
    assert!(readable(&s.store, A1, &s.alice).await);
    let entry = s
        .store
        .trash_entries(s.alice.id, 10, 0)
        .await
        .unwrap()
        .entries[0]
        .id
        .clone();
    s.store
        .restore_trash_entry(s.alice.id, &entry, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        s.store
            .item_attachments(s.alice_cal.id, "e.ics")
            .await
            .unwrap()
            .len(),
        1
    );

    // A whole collection, with and back.
    s.store
        .delete_collection_to_trash(s.alice_cal.id, 20)
        .await
        .unwrap();
    assert_eq!(exists(&s.store, s.alice.id).await, 1);
    let entry = s
        .store
        .trash_entries(s.alice.id, 10, 0)
        .await
        .unwrap()
        .entries[0]
        .id
        .clone();
    s.store
        .restore_trash_entry(s.alice.id, &entry, None)
        .await
        .unwrap()
        .unwrap();
    let restored = s
        .store
        .collection(
            s.alice.id,
            CollectionKind::Calendar,
            &DEFAULT_COLLECTION_SLUG.parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        s.store
            .item_attachments(restored.id, "e.ics")
            .await
            .unwrap()
            .len(),
        1
    );

    // Into the trash again and purged: gone.
    s.store
        .delete_collection_to_trash(restored.id, 30)
        .await
        .unwrap();
    s.store.empty_trash(s.alice.id).await.unwrap();
    while s.store.purge_expired_trash(100_000_000, 100).await.unwrap() > 0 {}
    assert_eq!(exists(&s.store, s.alice.id).await, 0);
}

#[tokio::test]
async fn move_keeps_and_copy_shares_attachments() {
    let s = setup().await;
    upload(&s.store, s.alice.id, A1, b"m", 0).await;
    put(&s.store, s.alice_cal.id, "e.ics", "e", &[A1]).await;
    let transfer = |from: i64, name: &'static str, to: i64, to_name: &'static str| Transfer {
        owner_id: s.alice.id,
        source_collection: from,
        source_name: name,
        source_precondition: &Precondition::None,
        destination_collection: to,
        destination_name: to_name,
        overwrite: true,
    };
    s.store
        .move_item(&transfer(
            s.alice_cal.id,
            "e.ics",
            s.alice_cal2.id,
            "moved.ics",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        s.store
            .item_attachments(s.alice_cal2.id, "moved.ics")
            .await
            .unwrap()
            .len(),
        1
    );
    s.store
        .copy_item(&transfer(
            s.alice_cal2.id,
            "moved.ics",
            s.alice_cal.id,
            "copy.ics",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        s.store
            .item_attachments(s.alice_cal.id, "copy.ics")
            .await
            .unwrap()
            .len(),
        1
    );
    // Shared: counted once, kept while either references it.
    assert_eq!(s.store.attachment_usage(s.alice.id).await.unwrap(), (1, 1));
    s.store
        .delete_item(s.alice_cal2.id, "moved.ics", &Precondition::None)
        .await
        .unwrap();
    assert!(readable(&s.store, A1, &s.alice).await);
    // A move over an object replaces it and its references.
    upload(&s.store, s.alice.id, A2, b"other", 0).await;
    put(&s.store, s.alice_cal2.id, "z.ics", "z", &[A2]).await;
    s.store
        .move_item(&transfer(
            s.alice_cal.id,
            "copy.ics",
            s.alice_cal2.id,
            "z.ics",
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(!readable(&s.store, A2, &s.alice).await);
    assert!(readable(&s.store, A1, &s.alice).await);
}

#[tokio::test]
async fn copies_do_not_carry_foreign_attachments() {
    let s = setup().await;
    let bob_cal2 = s
        .store
        .create_collection(
            s.bob.id,
            CollectionKind::Calendar,
            &"second".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    upload(&s.store, s.alice.id, A1, b"agenda", 0).await;
    upload(&s.store, s.bob.id, B1, b"bob's", 0).await;
    let organizer = ScheduleTag {
        set: Some("\"tag1\""),
        if_match: None,
        requires: None,
    };
    put_with(&s.store, s.alice_cal.id, "m.ics", "m", &[A1], &organizer)
        .await
        .unwrap();
    let delivery = ScheduleTag {
        set: Some("\"copy\""),
        if_match: None,
        requires: Some((s.alice_cal.id, "m.ics", "\"tag1\"")),
    };
    put_with(&s.store, s.bob_cal.id, "m.ics", "m", &[A1, B1], &delivery)
        .await
        .unwrap();
    assert_eq!(
        s.store
            .item_attachments(s.bob_cal.id, "m.ics")
            .await
            .unwrap()
            .len(),
        2
    );
    s.store
        .copy_item(&Transfer {
            owner_id: s.bob.id,
            source_collection: s.bob_cal.id,
            source_name: "m.ics",
            source_precondition: &Precondition::None,
            destination_collection: bob_cal2.id,
            destination_name: "c.ics",
            overwrite: false,
        })
        .await
        .unwrap()
        .unwrap();
    let copied = s
        .store
        .item_attachments(bob_cal2.id, "c.ics")
        .await
        .unwrap();
    assert_eq!(copied.len(), 1);
    assert_eq!(copied[0].managed_id, B1);
}

#[tokio::test]
async fn uploads_count_towards_the_quota() {
    let s = setup().await;
    s.store
        .set_limits_for_testing(100, crate::MAX_TRASH_ROWS_PER_USER);
    let attachment = |id: &'static str, content: &'static [u8]| NewAttachment {
        managed_id: id,
        owner_id: s.alice.id,
        filename: "f",
        media_type: "text/plain",
        content,
        now: 0,
    };
    assert!(matches!(
        s.store
            .insert_attachment(&attachment(A1, &[0; 101]), i64::MAX)
            .await,
        Err(StoreError::Limit(_))
    ));
    assert!(matches!(
        s.store
            .insert_attachment(&attachment(A1, &[0; 60]), 50)
            .await,
        Err(StoreError::Limit(_))
    ));
    s.store
        .insert_attachment(&attachment(A1, &[0; 60]), i64::MAX)
        .await
        .unwrap();
    // The attachment leaves 40 bytes for objects.
    let content = vec![b'x'; 50];
    let result = s
        .store
        .put_item(
            s.alice_cal.id,
            "big.ics",
            &NewItem {
                uid: "big",
                component: "VEVENT",
                range_start: None,
                range_end: None,
                recurring: false,
                index_version: 0,
                etag: "\"big\"",
                content: &content,
                attachments: &[],
            },
            &Precondition::None,
        )
        .await;
    assert!(matches!(result, Err(StoreError::Limit(_))), "{result:?}");
    assert!(matches!(
        s.store
            .insert_attachment(&attachment(A2, &[0; 41]), i64::MAX)
            .await,
        Err(StoreError::Limit(_))
    ));
    // Malformed ids are refused before anything is written.
    assert!(
        s.store
            .insert_attachment(
                &attachment("UPPERCASE0123456789ABCDEF0123456", b"x"),
                i64::MAX
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unreferenced_uploads_are_purged_after_the_grace_period() {
    let s = setup().await;
    upload(&s.store, s.alice.id, A1, b"a", 1000).await;
    upload(&s.store, s.alice.id, A2, b"b", 1000).await;
    put(&s.store, s.alice_cal.id, "e.ics", "e", &[A2]).await;
    assert_eq!(
        s.store
            .purge_unreferenced_attachments(1000, 10)
            .await
            .unwrap(),
        0
    );
    let later = 1000 + UNREFERENCED_GRACE_SECS + 1;
    assert_eq!(
        s.store
            .purge_unreferenced_attachments(later, 10)
            .await
            .unwrap(),
        1
    );
    assert!(readable(&s.store, A2, &s.alice).await);
    assert!(!readable(&s.store, A1, &s.alice).await);
    // An explicit cleanup only removes unreferenced uploads of the owner.
    upload(&s.store, s.alice.id, A1, b"a", 1000).await;
    assert!(
        !s.store
            .delete_unreferenced_attachment(s.bob.id, A1)
            .await
            .unwrap()
    );
    assert!(
        !s.store
            .delete_unreferenced_attachment(s.alice.id, A2)
            .await
            .unwrap()
    );
    assert!(
        s.store
            .delete_unreferenced_attachment(s.alice.id, A1)
            .await
            .unwrap()
    );
}

#[test]
fn managed_id_syntax() {
    assert!(valid_managed_id(A1));
    for bad in [
        "",
        "abc",
        &A1.to_uppercase(),
        &format!("{A1}0"),
        "0123456789abcdef0123456789abcdeg",
    ] {
        assert!(!valid_managed_id(bad), "{bad}");
    }
}
