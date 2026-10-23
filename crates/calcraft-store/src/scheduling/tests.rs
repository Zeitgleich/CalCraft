use calcraft_core::{CollectionKind, Username};

use crate::{
    DEFAULT_COLLECTION_SLUG, NewInboxMessage, NewItem, Precondition, PutOutcome, ScheduleTag,
    Store, StoreError,
};

fn name(s: &str) -> Username {
    s.parse().unwrap()
}

fn event(uid: &str) -> Vec<u8> {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART:20260101T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
    .into_bytes()
}

fn item<'a>(uid: &'a str, etag: &'a str, content: &'a [u8]) -> NewItem<'a> {
    NewItem {
        uid,
        component: "VEVENT",
        range_start: Some(0),
        range_end: Some(10),
        recurring: false,
        index_version: 0,
        etag,
        content,
        attachments: &[],
    }
}

async fn default_calendar(store: &Store, owner: i64) -> crate::Collection {
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

#[tokio::test]
async fn emails_are_unique_and_lower_case() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let bob = store.create_user(&name("bob"), "h", None).await.unwrap();
    assert!(
        store
            .set_user_email(alice.id, Some("alice@example.org"))
            .await
            .unwrap()
    );
    assert_eq!(
        store.user_email(alice.id).await.unwrap().as_deref(),
        Some("alice@example.org")
    );
    assert!(matches!(
        store
            .set_user_email(bob.id, Some("alice@example.org"))
            .await,
        Err(StoreError::AddressTaken)
    ));
    // The schema refuses addresses that are not normalised.
    assert!(
        store
            .set_user_email(bob.id, Some("Bob@example.org"))
            .await
            .is_err()
    );
    assert_eq!(
        store
            .user_by_email("alice@example.org")
            .await
            .unwrap()
            .unwrap()
            .id,
        alice.id
    );
    assert!(store.set_user_email(alice.id, None).await.unwrap());
    assert!(
        store
            .user_by_email("alice@example.org")
            .await
            .unwrap()
            .is_none()
    );
    assert!(!store.set_user_email(9999, None).await.unwrap());
    assert_eq!(store.user_emails().await.unwrap(), Vec::new());
}

#[tokio::test]
async fn schedule_tag_is_stored_and_checked() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let cal = default_calendar(&store, alice.id).await;
    let content = event("u1");
    let tags = ScheduleTag {
        set: Some("\"t1\""),
        if_match: None,
        requires: None,
    };
    let outcome = store
        .put_item_scheduled(
            cal.id,
            "a.ics",
            &item("u1", "\"e1\"", &content),
            &Precondition::None,
            &tags,
        )
        .await
        .unwrap();
    assert_eq!(outcome, PutOutcome::Created);
    let (_, tag) = store
        .item_with_schedule_tag(cal.id, "a.ics")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tag.as_deref(), Some("\"t1\""));

    // If-Schedule-Tag-Match with another tag fails, the right one succeeds.
    let wrong = ScheduleTag {
        set: Some("\"t2\""),
        if_match: Some("\"other\""),
        requires: None,
    };
    assert!(matches!(
        store
            .put_item_scheduled(
                cal.id,
                "a.ics",
                &item("u1", "\"e2\"", &content),
                &Precondition::None,
                &wrong,
            )
            .await,
        Err(StoreError::PreconditionFailed)
    ));
    let right = ScheduleTag {
        set: Some("\"t2\""),
        if_match: Some("\"t1\""),
        requires: None,
    };
    store
        .put_item_scheduled(
            cal.id,
            "a.ics",
            &item("u1", "\"e2\"", &content),
            &Precondition::None,
            &right,
        )
        .await
        .unwrap();

    let found = store
        .scheduling_object(alice.id, "u1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.collection.id, cal.id);
    assert_eq!(found.name, "a.ics");
    assert_eq!(found.schedule_tag.as_deref(), Some("\"t2\""));

    // A plain PUT clears the tag.
    store
        .put_item(
            cal.id,
            "a.ics",
            &item("u1", "\"e3\"", &content),
            &Precondition::None,
        )
        .await
        .unwrap();
    let (_, tag) = store
        .item_with_schedule_tag(cal.id, "a.ics")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tag, None);
    assert!(
        store
            .scheduling_object(alice.id, "u2")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn scheduling_objects_are_never_found_in_subscriptions() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let bob = store.create_user(&name("bob"), "h", None).await.unwrap();
    let cal = default_calendar(&store, alice.id).await;
    let content = event("u1");
    store
        .put_item(
            cal.id,
            "a.ics",
            &item("u1", "\"e\"", &content),
            &Precondition::None,
        )
        .await
        .unwrap();
    // Another user's objects are not found either.
    assert!(
        store
            .scheduling_object(bob.id, "u1")
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE collections SET read_only = 1 WHERE id = ?")
        .bind(cal.id)
        .execute(&store.writer)
        .await
        .unwrap();
    assert!(
        store
            .scheduling_object(alice.id, "u1")
            .await
            .unwrap()
            .is_none()
    );
}

fn message<'a>(
    name: &'a str,
    uid: &'a str,
    sender: &'a str,
    content: &'a [u8],
) -> NewInboxMessage<'a> {
    NewInboxMessage {
        name,
        uid,
        sender,
        etag: "\"m\"",
        content,
        now: 1,
    }
}

#[tokio::test]
async fn inbox_replaces_evicts_and_deletes() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let bob = store.create_user(&name("bob"), "h", None).await.unwrap();
    let content = b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".to_vec();
    let r0 = store.inbox_revision(alice.id).await.unwrap();
    store
        .deliver_inbox_message(alice.id, &message("1.ics", "u1", "mailto:b@x", &content), 3)
        .await
        .unwrap();
    // Same UID and sender: replaced.
    store
        .deliver_inbox_message(alice.id, &message("2.ics", "u1", "mailto:b@x", &content), 3)
        .await
        .unwrap();
    // Same UID, other sender: kept.
    store
        .deliver_inbox_message(alice.id, &message("3.ics", "u1", "mailto:c@x", &content), 3)
        .await
        .unwrap();
    let names: Vec<String> = store
        .inbox_messages(alice.id, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names, ["2.ics", "3.ics"]);
    assert!(store.inbox_revision(alice.id).await.unwrap() > r0);
    // Beyond the cap the oldest go.
    for i in 4..7 {
        let n = format!("{i}.ics");
        let uid = format!("u{i}");
        store
            .deliver_inbox_message(alice.id, &message(&n, &uid, "mailto:b@x", &content), 3)
            .await
            .unwrap();
    }
    let names: Vec<String> = store
        .inbox_messages(alice.id, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names, ["4.ics", "5.ics", "6.ics"]);
    // Bob does not see alice's messages.
    assert!(
        store
            .inbox_message(bob.id, "4.ics")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !store
            .delete_inbox_message(bob.id, "4.ics", &Precondition::None)
            .await
            .unwrap()
    );
    assert!(matches!(
        store
            .delete_inbox_message(
                alice.id,
                "4.ics",
                &Precondition::IfMatch(vec!["\"x\"".into()])
            )
            .await,
        Err(StoreError::PreconditionFailed)
    ));
    assert!(
        store
            .delete_inbox_message(alice.id, "4.ics", &Precondition::MustExist)
            .await
            .unwrap()
    );
    assert!(
        store
            .inbox_message(alice.id, "4.ics")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .inbox_message_meta(alice.id, "5.ics")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn inbox_counts_towards_the_quota() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    store.set_limits_for_testing(1000, 100);
    let big = vec![b'x'; 600];
    store
        .deliver_inbox_message(alice.id, &message("1.ics", "u1", "s", &big), 10)
        .await
        .unwrap();
    assert!(matches!(
        store
            .deliver_inbox_message(alice.id, &message("2.ics", "u2", "s", &big), 10)
            .await,
        Err(StoreError::Limit(_))
    ));
    // The inbox also counts for PUT.
    let cal = default_calendar(&store, alice.id).await;
    let content = vec![b'y'; 500];
    assert!(matches!(
        store
            .put_item(
                cal.id,
                "a.ics",
                &item("u1", "\"e\"", &content),
                &Precondition::None
            )
            .await,
        Err(StoreError::Limit(_))
    ));
}

#[tokio::test]
async fn freebusy_candidates_by_range() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let cal = default_calendar(&store, alice.id).await;
    let content = event("u1");
    store
        .put_item(
            cal.id,
            "a.ics",
            &item("u1", "\"e\"", &content),
            &Precondition::None,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .freebusy_candidates(cal.id, 5, 20, None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .freebusy_candidates(cal.id, 10, 20, None, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn deliveries_of_superseded_versions_are_refused() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let bob = store.create_user(&name("bob"), "h", None).await.unwrap();
    let alice_cal = default_calendar(&store, alice.id).await;
    let bob_cal = default_calendar(&store, bob.id).await;
    let content = event("u1");
    let tag = |set| ScheduleTag {
        set: Some(set),
        if_match: None,
        requires: None,
    };
    store
        .put_item_scheduled(
            alice_cal.id,
            "o.ics",
            &item("u1", "\"e1\"", &content),
            &Precondition::None,
            &tag("\"a\""),
        )
        .await
        .unwrap();
    // Derived from version "a": accepted while "a" is current.
    let derived = |from| ScheduleTag {
        set: Some("\"c\""),
        if_match: None,
        requires: Some((alice_cal.id, "o.ics", from)),
    };
    store
        .put_item_scheduled(
            bob_cal.id,
            "c.ics",
            &item("u1", "\"c1\"", &content),
            &Precondition::None,
            &derived("\"a\""),
        )
        .await
        .unwrap();
    // The organizer writes version "b"; a late delivery of "a" is refused.
    store
        .put_item_scheduled(
            alice_cal.id,
            "o.ics",
            &item("u1", "\"e2\"", &content),
            &Precondition::None,
            &tag("\"b\""),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .put_item_scheduled(
                bob_cal.id,
                "c.ics",
                &item("u1", "\"c2\"", &content),
                &Precondition::None,
                &derived("\"a\""),
            )
            .await,
        Err(StoreError::Superseded)
    ));
    let (copy, _) = store
        .item_with_schedule_tag(bob_cal.id, "c.ics")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(copy.etag, "\"c1\"");
}

#[tokio::test]
async fn invitations_can_be_refused() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    assert!(store.accepts_invitations(alice.id).await.unwrap());
    assert!(store.set_accept_invitations(alice.id, false).await.unwrap());
    assert!(!store.accepts_invitations(alice.id).await.unwrap());
    assert!(!store.accepts_invitations(9999).await.unwrap());
    assert!(!store.set_accept_invitations(9999, true).await.unwrap());
}
