use calcraft_core::{CollectionKind, Slug};
use sqlx::Row;

use super::*;
use crate::{
    DEFAULT_COLLECTION_SLUG, NewAttachment, NewItem, Precondition, PutOutcome, Transfer, User,
};

struct Setup {
    store: Store,
    alice: User,
    bob: User,
    carol: User,
    cal: Collection,
}

async fn setup() -> Setup {
    let store = Store::in_memory().await.unwrap();
    let user = |name: &str| {
        let store = store.clone();
        let name = name.to_owned();
        async move {
            store
                .create_user(&name.parse().unwrap(), "h", Some("Display"))
                .await
                .unwrap()
        }
    };
    let alice = user("alice").await;
    let bob = user("bob").await;
    let carol = user("carol").await;
    let slug: Slug = DEFAULT_COLLECTION_SLUG.parse().unwrap();
    let cal = store
        .collection(alice.id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    Setup {
        store,
        alice,
        bob,
        carol,
        cal,
    }
}

fn item<'a>(uid: &'a str, content: &'a [u8], attachments: &'a [String]) -> NewItem<'a> {
    NewItem {
        uid,
        component: "VEVENT",
        range_start: None,
        range_end: None,
        recurring: false,
        index_version: 0,
        etag: "\"e\"",
        content,
        attachments,
    }
}

async fn share(s: &Setup, sharee: &User, access: ShareAccess) -> Share {
    s.store
        .set_share(s.cal.id, s.alice.id, sharee.id, access, 1)
        .await
        .unwrap()
        .unwrap()
        .share()
        .clone()
}

#[tokio::test]
async fn pending_until_accepted_then_visible_with_own_slug() {
    let s = setup().await;
    let created = share(&s, &s.bob, ShareAccess::Read).await;
    assert!(!created.accepted() && created.slug.is_none());
    assert_eq!(created.public_id.len(), 32);
    assert!(
        s.store
            .shared_collections(s.bob.id, CollectionKind::Calendar)
            .await
            .unwrap()
            .is_empty()
    );
    let accepted = s
        .store
        .accept_share(s.bob.id, &created.public_id, 2)
        .await
        .unwrap()
        .unwrap();
    let slug = accepted.slug.clone().unwrap();
    assert_eq!(slug.as_str(), "alice-default");
    // Accepting twice changes nothing.
    let again = s
        .store
        .accept_share(s.bob.id, &created.public_id, 3)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again, accepted);
    let seen = s
        .store
        .shared_collection(s.bob.id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen.id, s.cal.id);
    assert_eq!(seen.owner_id, s.alice.id);
    assert_eq!(seen.slug, slug);
    assert!(seen.read_only, "a read share is read-only");
    let via = seen.shared.unwrap();
    assert_eq!(via.access, ShareAccess::Read);
    assert_eq!(via.owner.as_str(), "alice");
    assert_eq!(via.owner_slug.as_str(), "default");
    // Nobody else sees it, nor under another kind.
    assert!(
        s.store
            .shared_collection(s.carol.id, CollectionKind::Calendar, &slug)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        s.store
            .shared_collection(s.bob.id, CollectionKind::Addressbook, &slug)
            .await
            .unwrap()
            .is_none()
    );
    // Only the sharee accepts.
    assert!(
        s.store
            .accept_share(s.carol.id, &created.public_id, 2)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn refuses_owner_unknown_disabled_and_rw_subscriptions() {
    let s = setup().await;
    let refused = |r: Result<Option<SetShare>>| matches!(r, Err(StoreError::NoSuchSharee));
    assert!(refused(
        s.store
            .set_share(s.cal.id, s.alice.id, s.alice.id, ShareAccess::Read, 1)
            .await
    ));
    assert!(refused(
        s.store
            .set_share(s.cal.id, s.alice.id, 9999, ShareAccess::Read, 1)
            .await
    ));
    s.store.set_disabled(&s.carol.username, true).await.unwrap();
    assert!(refused(
        s.store
            .set_share(s.cal.id, s.alice.id, s.carol.id, ShareAccess::Read, 1)
            .await
    ));
    // Somebody else's collection is not found.
    assert!(
        s.store
            .set_share(s.cal.id, s.bob.id, s.carol.id, ShareAccess::Read, 1)
            .await
            .unwrap()
            .is_none()
    );
    // A subscription can only be shared read-only.
    sqlx::query("UPDATE collections SET read_only = 1 WHERE id = ?")
        .bind(s.cal.id)
        .execute(&s.store.writer)
        .await
        .unwrap();
    assert!(matches!(
        s.store
            .set_share(s.cal.id, s.alice.id, s.bob.id, ShareAccess::ReadWrite, 1)
            .await,
        Err(StoreError::ReadOnly)
    ));
    assert!(
        s.store
            .set_share(s.cal.id, s.alice.id, s.bob.id, ShareAccess::Read, 1)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn disabled_users_make_shares_disappear() {
    let s = setup().await;
    let created = share(&s, &s.bob, ShareAccess::ReadWrite).await;
    s.store
        .accept_share(s.bob.id, &created.public_id, 2)
        .await
        .unwrap();
    let count = |s: &Setup| {
        let store = s.store.clone();
        let (bob, cal) = (s.bob.id, s.cal.id);
        async move {
            (
                store
                    .shared_collections(bob, CollectionKind::Calendar)
                    .await
                    .unwrap()
                    .len(),
                store.shares_of_collection(cal).await.unwrap().len(),
                store.incoming_shares(bob).await.unwrap().len(),
            )
        }
    };
    assert_eq!(count(&s).await, (1, 1, 1));
    s.store.set_disabled(&s.alice.username, true).await.unwrap();
    assert_eq!(count(&s).await, (0, 0, 0));
    s.store
        .set_disabled(&s.alice.username, false)
        .await
        .unwrap();
    s.store.set_disabled(&s.bob.username, true).await.unwrap();
    assert_eq!(count(&s).await, (0, 0, 0));
}

#[tokio::test]
async fn sharee_writes_need_a_live_writable_share() {
    let s = setup().await;
    let created = share(&s, &s.bob, ShareAccess::ReadWrite).await;
    // Pending: no writes yet.
    let content = b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n";
    let new = item("u1", content, &[]);
    assert!(matches!(
        s.store
            .put_item_shared(created.id, s.cal.id, "a.ics", &new, &Precondition::None)
            .await,
        Err(StoreError::ShareRevoked)
    ));
    s.store
        .accept_share(s.bob.id, &created.public_id, 2)
        .await
        .unwrap();
    assert_eq!(
        s.store
            .put_item_shared(created.id, s.cal.id, "a.ics", &new, &Precondition::None)
            .await
            .unwrap(),
        PutOutcome::Created
    );
    // The share does not grant writes to other collections.
    let other = s
        .store
        .create_collection(
            s.alice.id,
            CollectionKind::Calendar,
            &"other".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    assert!(matches!(
        s.store
            .put_item_shared(created.id, other.id, "a.ics", &new, &Precondition::None)
            .await,
        Err(StoreError::ShareRevoked)
    ));
    // Downgraded to read: no more writes, no deletes, no moves.
    s.store
        .set_share(s.cal.id, s.alice.id, s.bob.id, ShareAccess::Read, 3)
        .await
        .unwrap();
    assert!(matches!(
        s.store
            .put_item_shared(
                created.id,
                s.cal.id,
                "b.ics",
                &item("u2", content, &[]),
                &Precondition::None
            )
            .await,
        Err(StoreError::ShareRevoked)
    ));
    assert!(matches!(
        s.store
            .delete_item_to_trash_shared(
                created.id,
                s.cal.id,
                "a.ics",
                &Precondition::None,
                4,
                |_| None
            )
            .await,
        Err(StoreError::ShareRevoked)
    ));
    let transfer = Transfer {
        owner_id: s.alice.id,
        source_share: Some(created.id),
        destination_share: Some(created.id),
        source_collection: s.cal.id,
        source_name: "a.ics",
        source_precondition: &Precondition::None,
        destination_collection: s.cal.id,
        destination_name: "c.ics",
        overwrite: false,
    };
    assert!(matches!(
        s.store.move_item(&transfer).await,
        Err(StoreError::ShareRevoked)
    ));
    // Back to read-write: the delete goes into the owner's trash.
    s.store
        .set_share(s.cal.id, s.alice.id, s.bob.id, ShareAccess::ReadWrite, 5)
        .await
        .unwrap();
    assert!(
        s.store
            .delete_item_to_trash_shared(
                created.id,
                s.cal.id,
                "a.ics",
                &Precondition::None,
                6,
                |_| None
            )
            .await
            .unwrap()
    );
    assert_eq!(
        s.store
            .trash_entries(s.alice.id, 10, 0)
            .await
            .unwrap()
            .entries
            .len(),
        1
    );
    assert!(
        s.store
            .trash_entries(s.bob.id, 10, 0)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    // Removed: nothing any more.
    assert!(
        s.store
            .remove_share(s.cal.id, s.alice.id, s.bob.id)
            .await
            .unwrap()
    );
    assert!(matches!(
        s.store
            .put_item_shared(created.id, s.cal.id, "a.ics", &new, &Precondition::None)
            .await,
        Err(StoreError::ShareRevoked)
    ));
}

#[tokio::test]
async fn sharee_writes_count_against_the_owner() {
    let s = setup().await;
    s.store.set_limits_for_testing(1000, 100);
    let created = share(&s, &s.bob, ShareAccess::ReadWrite).await;
    s.store
        .accept_share(s.bob.id, &created.public_id, 2)
        .await
        .unwrap();
    let big = vec![b'x'; 600];
    s.store
        .put_item_shared(
            created.id,
            s.cal.id,
            "a.ics",
            &item("u1", &big, &[]),
            &Precondition::None,
        )
        .await
        .unwrap();
    // The owner's quota is now nearly used up, the sharee's not at all.
    assert!(matches!(
        s.store
            .put_item(
                s.cal.id,
                "b.ics",
                &item("u2", &big, &[]),
                &Precondition::None
            )
            .await,
        Err(StoreError::Limit(_))
    ));
    let bob_cal = s
        .store
        .collection(
            s.bob.id,
            CollectionKind::Calendar,
            &DEFAULT_COLLECTION_SLUG.parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    s.store
        .put_item(
            bob_cal.id,
            "b.ics",
            &item("u2", &big, &[]),
            &Precondition::None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn sharees_read_attachments_only_while_shared() {
    let s = setup().await;
    let id = "0123456789abcdef0123456789abcdef";
    s.store
        .insert_attachment(
            &NewAttachment {
                managed_id: id,
                owner_id: s.alice.id,
                filename: "a.txt",
                media_type: "text/plain",
                content: b"x",
                now: 1,
            },
            i64::MAX,
        )
        .await
        .unwrap();
    let ids = [id.to_owned()];
    s.store
        .put_item(
            s.cal.id,
            "a.ics",
            &item("u1", b"c", &ids),
            &Precondition::None,
        )
        .await
        .unwrap();
    let readable = |user: i64| {
        let store = s.store.clone();
        async move { store.readable_attachment(id, user).await.unwrap().is_some() }
    };
    let created = share(&s, &s.bob, ShareAccess::Read).await;
    assert!(!readable(s.bob.id).await, "pending");
    s.store
        .accept_share(s.bob.id, &created.public_id, 2)
        .await
        .unwrap();
    assert!(readable(s.bob.id).await);
    assert!(
        s.store
            .attachment_content(id, s.bob.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(!readable(s.carol.id).await);
    s.store
        .leave_share(s.bob.id, &created.public_id, 3)
        .await
        .unwrap();
    assert!(!readable(s.bob.id).await);
    assert!(
        s.store
            .attachment_content(id, s.bob.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn slugs_never_collide_with_own_collections() {
    let s = setup().await;
    // Bob already has a calendar called like the share would be.
    s.store
        .create_collection(
            s.bob.id,
            CollectionKind::Calendar,
            &"alice-default".parse().unwrap(),
            None,
            None,
            None,
            "VEVENT",
        )
        .await
        .unwrap();
    let created = share(&s, &s.bob, ShareAccess::Read).await;
    let accepted = s
        .store
        .accept_share(s.bob.id, &created.public_id, 2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(accepted.slug.unwrap().as_str(), "alice-default-2");
    // And bob cannot create a collection at the share's slug.
    assert!(matches!(
        s.store
            .create_collection(
                s.bob.id,
                CollectionKind::Calendar,
                &"alice-default-2".parse().unwrap(),
                None,
                None,
                None,
                "VEVENT",
            )
            .await,
        Err(StoreError::CollectionExists)
    ));
    // The same slug in the address book home is fine.
    s.store
        .create_collection(
            s.bob.id,
            CollectionKind::Addressbook,
            &"alice-default-2".parse().unwrap(),
            None,
            None,
            None,
            "VCARD",
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn sharee_view_is_their_own() {
    let s = setup().await;
    let created = share(&s, &s.bob, ShareAccess::Read).await;
    let accepted = s
        .store
        .accept_share(s.bob.id, &created.public_id, 2)
        .await
        .unwrap()
        .unwrap();
    let slug = accepted.slug.unwrap();
    let view = ShareView {
        display_name: Some(Some("Alice's")),
        color: Some(Some("#FF0000")),
        order: Some(Some(3)),
    };
    assert!(
        s.store
            .update_share_view(s.bob.id, created.id, &view)
            .await
            .unwrap()
    );
    // Not by anybody else.
    assert!(
        !s.store
            .update_share_view(s.carol.id, created.id, &view)
            .await
            .unwrap()
    );
    let seen = s
        .store
        .shared_collection(s.bob.id, CollectionKind::Calendar, &slug)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen.display_name.as_deref(), Some("Alice's"));
    assert_eq!(seen.color.as_deref(), Some("#FF0000"));
    assert_eq!(seen.shared.unwrap().order, Some(3));
    // The owner's collection is unchanged.
    let own = s
        .store
        .collection(s.alice.id, CollectionKind::Calendar, &s.cal.slug)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(own, s.cal);
}

/// A collection of `owner` inserted directly (beyond the collection limit).
async fn raw_collection(store: &Store, owner: i64, slug: &str) -> i64 {
    sqlx::query(
        "INSERT INTO collections (owner_id, kind, slug, token_salt) VALUES (?, 'calendar', ?, 1) RETURNING id",
    )
    .bind(owner)
    .bind(slug)
    .fetch_one(&store.writer)
    .await
    .unwrap()
    .try_get("id")
    .unwrap()
}

#[tokio::test]
async fn limits_pending_invitations_per_owner_and_in_total() {
    let s = setup().await;
    // One owner can only have a few invitations pending with one user.
    for n in 0..MAX_PENDING_PER_OWNER {
        let id = raw_collection(&s.store, s.carol.id, &format!("c{n}")).await;
        s.store
            .set_share(id, s.carol.id, s.bob.id, ShareAccess::Read, 1)
            .await
            .unwrap()
            .unwrap();
    }
    let id = raw_collection(&s.store, s.carol.id, "one-more").await;
    assert!(matches!(
        s.store
            .set_share(id, s.carol.id, s.bob.id, ShareAccess::Read, 1)
            .await,
        Err(StoreError::Limit(_))
    ));
    // Others still can, up to the total.
    let total = MAX_PENDING_SHARES_PER_USER - MAX_PENDING_PER_OWNER;
    for owner in 0..total / MAX_PENDING_PER_OWNER {
        let user = s
            .store
            .create_user(&format!("owner{owner}").parse().unwrap(), "h", None)
            .await
            .unwrap();
        for n in 0..MAX_PENDING_PER_OWNER {
            let id = raw_collection(&s.store, user.id, &format!("x{n}")).await;
            s.store
                .set_share(id, user.id, s.bob.id, ShareAccess::Read, 1)
                .await
                .unwrap()
                .unwrap();
        }
    }
    assert!(matches!(
        s.store
            .set_share(s.cal.id, s.alice.id, s.bob.id, ShareAccess::Read, 1)
            .await,
        Err(StoreError::Limit(_))
    ));
    // Others can still be invited.
    assert!(
        s.store
            .set_share(s.cal.id, s.alice.id, s.carol.id, ShareAccess::Read, 1)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn declining_blocks_new_invitations_of_that_owner_for_a_day() {
    let s = setup().await;
    let created = share(&s, &s.bob, ShareAccess::Read).await;
    assert!(
        s.store
            .leave_share(s.bob.id, &created.public_id, 100)
            .await
            .unwrap()
    );
    let other = raw_collection(&s.store, s.alice.id, "other").await;
    for collection in [s.cal.id, other] {
        assert!(matches!(
            s.store
                .set_share(collection, s.alice.id, s.bob.id, ShareAccess::Read, 200)
                .await,
            Err(StoreError::ShareDeclined)
        ));
    }
    // Others may still invite bob, and alice others.
    let carol_cal = raw_collection(&s.store, s.carol.id, "c").await;
    assert!(
        s.store
            .set_share(carol_cal, s.carol.id, s.bob.id, ShareAccess::Read, 200)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        s.store
            .set_share(s.cal.id, s.alice.id, s.carol.id, ShareAccess::Read, 200)
            .await
            .unwrap()
            .is_some()
    );
    // After a day alice can invite bob again; leaving an accepted share
    // counts like declining.
    let again = s
        .store
        .set_share(
            s.cal.id,
            s.alice.id,
            s.bob.id,
            ShareAccess::Read,
            100 + DECLINE_COOLDOWN_SECS + 1,
        )
        .await
        .unwrap()
        .unwrap();
    let id = again.share().public_id.clone();
    s.store.accept_share(s.bob.id, &id, 1).await.unwrap();
    s.store.leave_share(s.bob.id, &id, 5000).await.unwrap();
    assert!(matches!(
        s.store
            .set_share(s.cal.id, s.alice.id, s.bob.id, ShareAccess::Read, 5001)
            .await,
        Err(StoreError::ShareDeclined)
    ));
    // The owner removing a share blocks nothing.
    let carol_share = s.store.incoming_shares(s.carol.id).await.unwrap();
    assert_eq!(carol_share.len(), 1);
    s.store
        .remove_share(s.cal.id, s.alice.id, s.carol.id)
        .await
        .unwrap();
    assert!(
        s.store
            .set_share(s.cal.id, s.alice.id, s.carol.id, ShareAccess::Read, 5001)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn several_shares_are_all_or_nothing() {
    let s = setup().await;
    let dave = s
        .store
        .create_user(&"dave".parse().unwrap(), "h", None)
        .await
        .unwrap();
    s.store.set_disabled(&dave.username, true).await.unwrap();
    // The last entry fails: nothing is shared.
    assert!(matches!(
        s.store
            .apply_shares(
                s.cal.id,
                s.alice.id,
                &[(s.bob.id, ShareAccess::Read), (dave.id, ShareAccess::Read)],
                &[],
                1,
            )
            .await,
        Err(StoreError::NoSuchSharee)
    ));
    assert!(
        s.store
            .shares_of_collection(s.cal.id)
            .await
            .unwrap()
            .is_empty()
    );
    s.store
        .apply_shares(
            s.cal.id,
            s.alice.id,
            &[
                (s.bob.id, ShareAccess::Read),
                (s.carol.id, ShareAccess::ReadWrite),
            ],
            &[],
            1,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        s.store.shares_of_collection(s.cal.id).await.unwrap().len(),
        2
    );
    s.store
        .apply_shares(s.cal.id, s.alice.id, &[], &[s.bob.id], 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        s.store.shares_of_collection(s.cal.id).await.unwrap().len(),
        1
    );
    // Not the owner's collection.
    assert!(
        s.store
            .apply_shares(
                s.cal.id,
                s.bob.id,
                &[(s.carol.id, ShareAccess::Read)],
                &[],
                1
            )
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn read_sharees_cannot_read_attachments_of_private_objects() {
    let s = setup().await;
    let id = "0123456789abcdef0123456789abcdef";
    s.store
        .insert_attachment(
            &NewAttachment {
                managed_id: id,
                owner_id: s.alice.id,
                filename: "a.txt",
                media_type: "text/plain",
                content: b"x",
                now: 1,
            },
            i64::MAX,
        )
        .await
        .unwrap();
    let ids = [id.to_owned()];
    let read = share(&s, &s.bob, ShareAccess::Read).await;
    s.store
        .accept_share(s.bob.id, &read.public_id, 2)
        .await
        .unwrap();
    let write = share(&s, &s.carol, ShareAccess::ReadWrite).await;
    s.store
        .accept_share(s.carol.id, &write.public_id, 2)
        .await
        .unwrap();
    let readable = |user: i64| {
        let store = s.store.clone();
        async move { store.readable_attachment(id, user).await.unwrap().is_some() }
    };
    for (content, private) in [
        ("BEGIN:VEVENT\r\nCLASS:PUBLIC\r\nEND:VEVENT\r\n", false),
        ("BEGIN:VEVENT\r\nSUMMARY:x\r\nEND:VEVENT\r\n", false),
        ("BEGIN:VEVENT\r\nCLASS:PRIVATE\r\nEND:VEVENT\r\n", true),
        ("BEGIN:VEVENT\nclass:confidential\nEND:VEVENT\n", true),
        // Folded, also with LF only, and with parameters.
        ("BEGIN:VEVENT\r\nCLA\r\n SS:PRIVATE\r\nEND:VEVENT\r\n", true),
        ("BEGIN:VEVENT\nCLA\n\tSS:PRIVATE\nEND:VEVENT\n", true),
        ("BEGIN:VEVENT\r\nCLASS;X=1:PUBLIC\r\nEND:VEVENT\r\n", true),
        // A public master with a private override.
        (
            "BEGIN:VEVENT\r\nCLASS:PUBLIC\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nCLASS:X-TEAM\r\nEND:VEVENT\r\n",
            true,
        ),
    ] {
        s.store
            .put_item(
                s.cal.id,
                "a.ics",
                &item("u1", content.as_bytes(), &ids),
                &Precondition::None,
            )
            .await
            .unwrap();
        assert_eq!(readable(s.bob.id).await, !private, "{content:?}");
        assert!(
            s.store
                .attachment_content(id, s.bob.id)
                .await
                .unwrap()
                .is_some()
                != private,
            "{content:?}"
        );
        // Read-write sharees and the owner always.
        assert!(readable(s.carol.id).await, "{content:?}");
        assert!(readable(s.alice.id).await, "{content:?}");
    }
}

#[tokio::test]
async fn sharees_never_change_scheduling_objects() {
    let s = setup().await;
    let created = share(&s, &s.bob, ShareAccess::ReadWrite).await;
    s.store
        .accept_share(s.bob.id, &created.public_id, 2)
        .await
        .unwrap();
    let content = b"c".as_slice();
    // The owner's meeting: a Schedule-Tag, even with the same bytes as a
    // plain object (same ETag).
    s.store
        .put_item(
            s.cal.id,
            "plain.ics",
            &item("p", content, &[]),
            &Precondition::None,
        )
        .await
        .unwrap();
    s.store
        .put_item_scheduled(
            s.cal.id,
            "meeting.ics",
            &item("m", content, &[]),
            &Precondition::None,
            &crate::ScheduleTag {
                set: Some("\"tag\""),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let refused = |r: Result<bool>| matches!(r, Err(StoreError::ReadOnly));
    let etag = Precondition::IfMatch(vec!["\"e\"".to_owned()]);
    assert!(matches!(
        s.store
            .put_item_shared(
                created.id,
                s.cal.id,
                "meeting.ics",
                &item("m", content, &[]),
                &etag
            )
            .await,
        Err(StoreError::ReadOnly)
    ));
    assert!(refused(
        s.store
            .delete_item_to_trash_shared(created.id, s.cal.id, "meeting.ics", &etag, 3, |_| None)
            .await
    ));
    s.store.set_trash_retention_days(0);
    assert!(refused(
        s.store
            .delete_item_to_trash_shared(created.id, s.cal.id, "meeting.ics", &etag, 3, |_| None)
            .await
    ));
    let transfer = |from: &'static str, to: &'static str| Transfer {
        owner_id: s.alice.id,
        source_share: Some(created.id),
        destination_share: Some(created.id),
        source_collection: s.cal.id,
        source_name: from,
        source_precondition: &Precondition::None,
        destination_collection: s.cal.id,
        destination_name: to,
        overwrite: true,
    };
    // Moving a meeting away, or a plain object over it: refused.
    assert!(matches!(
        s.store
            .move_item(&transfer("meeting.ics", "elsewhere.ics"))
            .await,
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(
        s.store
            .move_item(&transfer("plain.ics", "meeting.ics"))
            .await,
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(
        s.store
            .copy_item(&transfer("plain.ics", "meeting.ics"))
            .await,
        Err(StoreError::ReadOnly)
    ));
    // The meeting is unchanged.
    let (_, tag) = s
        .store
        .item_with_schedule_tag(s.cal.id, "meeting.ics")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tag.as_deref(), Some("\"tag\""));
}
