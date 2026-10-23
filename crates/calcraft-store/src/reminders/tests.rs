use calcraft_core::{CollectionKind, Username};

use crate::{
    Completion, DEFAULT_COLLECTION_SLUG, IndexEntry, MailKind, MailLimits, NewItem, NewMail,
    Precondition, ReminderMode, Store,
};

fn name(s: &str) -> Username {
    s.parse().unwrap()
}

const LIMITS: MailLimits = MailLimits {
    per_user_hour: 3,
    per_user_day: 5,
    max_pending: 100,
};

async fn calendar(store: &Store, owner: i64) -> crate::Collection {
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

async fn put(store: &Store, collection: i64, uid: &str, etag: &str) -> i64 {
    let content = format!("BEGIN:VCALENDAR\r\nUID:{uid}\r\n{etag}\r\nEND:VCALENDAR\r\n");
    store
        .put_item(
            collection,
            &format!("{uid}.ics"),
            &NewItem {
                uid,
                component: "VEVENT",
                range_start: None,
                range_end: None,
                recurring: false,
                index_version: 0,
                etag,
                content: content.as_bytes(),
                attachments: &[],
            },
            &Precondition::None,
        )
        .await
        .unwrap();
    store
        .reminder_candidates(collection, -1, 0, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.etag == etag)
        .unwrap()
        .item_id
}

fn done(item_id: i64, etag: &str, scan_to: i64, next_due: Option<i64>) -> Completion<'_> {
    Completion {
        item_id,
        etag,
        scan_to,
        next_due,
    }
}

fn mail(user_id: i64, key: &str) -> NewMail {
    NewMail {
        user_id,
        kind: MailKind::Reminder,
        item_id: None,
        dedupe_key: Some(key.to_owned()),
        recipient: "alice@example.org".to_owned(),
        message: b"Subject: x\r\n\r\nbody\r\n".to_vec(),
        expires_at: 10_000,
    }
}

#[tokio::test]
async fn settings_round_trip_and_reset_the_cursors() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let cal = calendar(&store, alice.id).await;
    let settings = store.reminder_settings(alice.id).await.unwrap().unwrap();
    assert_eq!(settings.mode, None);
    assert!(!settings.include_description);

    store
        .set_reminder_cursor(cal.id, cal.sync_revision)
        .await
        .unwrap();
    assert!(store.reminder_dirty_calendars(10).await.unwrap().is_empty());
    assert!(
        store
            .set_reminder_settings(alice.id, Some(Some(ReminderMode::All)), Some(true))
            .await
            .unwrap()
    );
    let settings = store.reminder_settings(alice.id).await.unwrap().unwrap();
    assert_eq!(settings.mode, Some(ReminderMode::All));
    assert!(settings.include_description);
    // A new mode makes every calendar of the user dirty again.
    let dirty = store.reminder_dirty_calendars(10).await.unwrap();
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0].indexed_revision, -1);
    assert_eq!(dirty[0].mode, Some(ReminderMode::All));
    // Unknown users.
    assert!(
        !store
            .set_reminder_settings(999, None, Some(true))
            .await
            .unwrap()
    );
    assert!(store.reminder_settings(999).await.unwrap().is_none());
}

#[tokio::test]
async fn changed_objects_make_their_calendar_dirty() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let cal = calendar(&store, alice.id).await;
    let a = put(&store, cal.id, "a", "\"1\"").await;
    let dirty = store.reminder_dirty_calendars(10).await.unwrap();
    assert_eq!(dirty.len(), 1);
    let revision = dirty[0].revision;
    store.set_reminder_cursor(cal.id, revision).await.unwrap();
    assert!(store.reminder_dirty_calendars(10).await.unwrap().is_empty());
    // The cursor never moves back.
    store.set_reminder_cursor(cal.id, 0).await.unwrap();
    assert!(store.reminder_dirty_calendars(10).await.unwrap().is_empty());

    put(&store, cal.id, "b", "\"2\"").await;
    let dirty = store.reminder_dirty_calendars(10).await.unwrap();
    assert_eq!(dirty.len(), 1);
    let changed = store
        .reminder_candidates(cal.id, dirty[0].indexed_revision, 0, 10)
        .await
        .unwrap();
    assert_eq!(changed.len(), 1);
    assert_ne!(changed[0].item_id, a);

    // Address books and subscriptions are never dirty.
    let book = store
        .collections(alice.id, CollectionKind::Addressbook)
        .await
        .unwrap();
    assert!(dirty.iter().all(|d| d.collection_id != book[0].id));
}

#[tokio::test]
async fn index_entries_follow_the_object() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let cal = calendar(&store, alice.id).await;
    let a = put(&store, cal.id, "a", "\"1\"").await;
    store
        .set_reminder_index(&[IndexEntry {
            item_id: a,
            etag: "\"1\"",
            scan_from: 100,
            next_due: Some(200),
        }])
        .await
        .unwrap();
    assert!(store.due_reminders(199, 10).await.unwrap().is_empty());
    let due = store.due_reminders(200, 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].scan_from, 100);
    assert_eq!(due[0].uid, "a");
    assert_eq!(store.scheduled_reminders(Some(alice.id)).await.unwrap(), 1);

    // scan_from never moves back.
    store
        .set_reminder_index(&[IndexEntry {
            item_id: a,
            etag: "\"1\"",
            scan_from: 50,
            next_due: Some(300),
        }])
        .await
        .unwrap();
    assert_eq!(
        store.due_reminders(300, 10).await.unwrap()[0].scan_from,
        100
    );

    // A stale ETag writes nothing.
    store
        .set_reminder_index(&[IndexEntry {
            item_id: a,
            etag: "\"old\"",
            scan_from: 1000,
            next_due: Some(5),
        }])
        .await
        .unwrap();
    assert!(store.due_reminders(100, 10).await.unwrap().is_empty());

    // Deleting the object removes its entry.
    store.delete_collection(cal.id).await.unwrap();
    assert!(store.due_reminders(i64::MAX, 10).await.unwrap().is_empty());
    assert_eq!(store.scheduled_reminders(None).await.unwrap(), 0);
}

#[tokio::test]
async fn completing_a_reminder_is_atomic_and_idempotent() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let cal = calendar(&store, alice.id).await;
    let a = put(&store, cal.id, "a", "\"1\"").await;
    store
        .set_reminder_index(&[IndexEntry {
            item_id: a,
            etag: "\"1\"",
            scan_from: 0,
            next_due: Some(100),
        }])
        .await
        .unwrap();
    let outcome = store
        .complete_reminder(
            &done(a, "\"1\"", 100, Some(500)),
            &[mail(alice.id, "k1")],
            LIMITS,
            100,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.queued, 1);
    // The same reminder again (e.g. recomputed after a restart): not queued twice.
    let again = store
        .complete_reminder(
            &done(a, "\"1\"", 100, Some(500)),
            &[mail(alice.id, "k1")],
            LIMITS,
            100,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!((again.queued, again.duplicates), (0, 1));
    assert_eq!(store.pending_mails(100, 10).await.unwrap().len(), 1);
    assert!(store.due_reminders(499, 10).await.unwrap().is_empty());

    // The object changed meanwhile: nothing is queued.
    put(&store, cal.id, "a", "\"2\"").await;
    assert!(
        store
            .complete_reminder(
                &done(a, "\"1\"", 600, None),
                &[mail(alice.id, "k2")],
                LIMITS,
                600
            )
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(store.pending_mails(1000, 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn per_user_and_queue_limits_drop_mails() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let bob = store.create_user(&name("bob"), "h", None).await.unwrap();
    let mails: Vec<NewMail> = (0..5).map(|n| mail(alice.id, &format!("a{n}"))).collect();
    let outcome = store.queue_mails(&mails, LIMITS, 1000).await.unwrap();
    assert_eq!((outcome.queued, outcome.dropped), (3, 2));
    // Bob has his own budget.
    let outcome = store
        .queue_mails(&[mail(bob.id, "b0")], LIMITS, 1000)
        .await
        .unwrap();
    assert_eq!(outcome.queued, 1);
    // An hour later Alice may send again, up to her daily limit.
    let mails: Vec<NewMail> = (5..10).map(|n| mail(alice.id, &format!("a{n}"))).collect();
    let outcome = store
        .queue_mails(&mails, LIMITS, 1000 + 3601)
        .await
        .unwrap();
    assert_eq!((outcome.queued, outcome.dropped), (2, 3));
    let counts = store.mail_counts(Some(alice.id), 0).await.unwrap();
    assert_eq!((counts.pending, counts.dropped), (5, 5));
    assert_eq!(counts.last_error.unwrap().0, "limit");

    // The server-wide queue bound.
    let tight = MailLimits {
        max_pending: 6,
        ..LIMITS
    };
    let carol = store.create_user(&name("carol"), "h", None).await.unwrap();
    let outcome = store
        .queue_mails(&[mail(carol.id, "c0"), mail(carol.id, "c1")], tight, 9000)
        .await
        .unwrap();
    assert_eq!((outcome.queued, outcome.dropped), (0, 2));
    let counts = store.mail_counts(Some(carol.id), 0).await.unwrap();
    assert_eq!(counts.last_error.unwrap().0, "queue_full");
}

#[tokio::test]
async fn sending_retrying_and_pruning() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    store
        .queue_mails(
            &[
                mail(alice.id, "x"),
                mail(alice.id, "y"),
                mail(alice.id, "z"),
            ],
            LIMITS,
            100,
        )
        .await
        .unwrap();
    let pending = store.pending_mails(100, 10).await.unwrap();
    assert_eq!(pending.len(), 3);
    assert!(!format!("{:?}", pending[0]).contains("alice@"));
    store
        .finish_mail(pending[0].id, "sent", None, 101)
        .await
        .unwrap();
    store
        .retry_mail(pending[1].id, "smtp_unavailable", 400)
        .await
        .unwrap();
    store
        .finish_mail(pending[2].id, "failed", Some("smtp_rejected"), 102)
        .await
        .unwrap();
    // The retried mail waits for its next attempt.
    assert!(store.pending_mails(399, 10).await.unwrap().is_empty());
    let retry = store.pending_mails(400, 10).await.unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].attempts, 1);
    let counts = store.mail_counts(None, 0).await.unwrap();
    assert_eq!((counts.pending, counts.sent, counts.failed), (1, 1, 1));

    // Sent and failed rows keep neither recipient nor message.
    let leftover: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mail_outbox WHERE status <> 'pending' AND (recipient IS NOT NULL OR message IS NOT NULL)",
    )
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(leftover, 0);

    // Expired pending mails are given up; old rows are deleted.
    let (expired, deleted) = store.prune_mails(10_000).await.unwrap();
    assert_eq!((expired, deleted), (1, 0));
    let (_, deleted) = store
        .prune_mails(100 + crate::MAIL_RETENTION_SECS + 1)
        .await
        .unwrap();
    assert_eq!(deleted, 3);
}

#[tokio::test]
async fn deleted_objects_orphan_their_pending_reminders() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let cal = calendar(&store, alice.id).await;
    let a = put(&store, cal.id, "a", "\"1\"").await;
    let mut queued = mail(alice.id, "k");
    queued.item_id = Some(a);
    store.queue_mails(&[queued], LIMITS, 100).await.unwrap();
    assert!(!store.pending_mails(100, 10).await.unwrap()[0].orphaned);
    store.delete_collection(cal.id).await.unwrap();
    assert!(store.pending_mails(100, 10).await.unwrap()[0].orphaned);
}

#[tokio::test]
async fn oversized_mails_are_refused() {
    let store = Store::in_memory().await.unwrap();
    let alice = store.create_user(&name("alice"), "h", None).await.unwrap();
    let mut big = mail(alice.id, "big");
    big.message = vec![b'x'; crate::MAX_MAIL_BYTES + 1];
    assert!(store.queue_mails(&[big], LIMITS, 1).await.is_err());
}
