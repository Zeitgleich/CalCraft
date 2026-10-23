//! The reminder engine end to end: index, dispatch and delivery against a
//! fake SMTP relay, with the clock passed in.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU16, Ordering},
};

use calcraft_core::{CollectionKind, Username};
use calcraft_store::{
    DEFAULT_COLLECTION_SLUG, NewItem, NewSubscription, Precondition, ReminderMode, Store, User,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

use super::*;
use crate::config::{Config, ReminderDefault, SmtpTls};

/// 2026-10-26 09:00:00 UTC.
const START: i64 = 1_793_005_200;

#[derive(Default)]
struct Relay {
    /// (recipient, message) of every accepted mail.
    mails: Mutex<Vec<(String, String)>>,
    /// Reply to RCPT TO.
    rcpt: AtomicU16,
}

async fn relay() -> (u16, Arc<Relay>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let relay = Arc::new(Relay {
        rcpt: AtomicU16::new(250),
        ..Relay::default()
    });
    let shared = relay.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let relay = shared.clone();
            tokio::spawn(async move {
                let mut stream = BufReader::new(tcp);
                let _ = stream.get_mut().write_all(b"220 relay\r\n").await;
                let mut to = String::new();
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let command = line.trim_end().to_ascii_uppercase();
                    let reply = if command.starts_with("EHLO") {
                        "250-relay\r\n250 8BITMIME\r\n".to_owned()
                    } else if command.starts_with("RCPT TO:") {
                        to = line.trim_end()[8..].to_owned();
                        format!("{} rcpt\r\n", relay.rcpt.load(Ordering::Relaxed))
                    } else if command == "DATA" {
                        let _ = stream.get_mut().write_all(b"354 go\r\n").await;
                        let mut message = String::new();
                        loop {
                            let mut line = String::new();
                            if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if line == ".\r\n" {
                                break;
                            }
                            message.push_str(line.strip_prefix('.').unwrap_or(&line));
                        }
                        relay.mails.lock().unwrap().push((to.clone(), message));
                        "250 ok\r\n".to_owned()
                    } else if command == "QUIT" {
                        let _ = stream.get_mut().write_all(b"221 bye\r\n").await;
                        return;
                    } else {
                        "250 ok\r\n".to_owned()
                    };
                    if stream.get_mut().write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, relay)
}

struct Setup {
    store: Store,
    reminders: Arc<Reminders>,
    relay: Arc<Relay>,
    alice: User,
    calendar: i64,
}

fn config(port: u16) -> Config {
    let mut config = Config::default();
    config.smtp.enabled = true;
    config.smtp.host = "127.0.0.1".to_owned();
    config.smtp.port = Some(port);
    config.smtp.tls = SmtpTls::None;
    config.smtp.from = "calendar@example.org".to_owned();
    config.reminders.enabled = true;
    config.reminders.max_per_user_per_hour = 5;
    config.reminders.max_per_user_per_day = 10;
    config.reminders.base_url = Some("https://cal.example.org".to_owned());
    config
}

async fn setup_with(config: impl FnOnce(&mut Config)) -> Setup {
    let (port, relay) = relay().await;
    let store = Store::in_memory().await.unwrap();
    let name: Username = "alice".parse().unwrap();
    let alice = store.create_user(&name, "h", None).await.unwrap();
    store
        .set_user_email(alice.id, Some("alice@example.org"))
        .await
        .unwrap();
    let mut cfg = self::config(port);
    config(&mut cfg);
    let reminders = Reminders::new(store.clone(), &cfg).unwrap();
    reminders.prepare().await.unwrap();
    let calendar = store
        .collection(
            alice.id,
            CollectionKind::Calendar,
            &DEFAULT_COLLECTION_SLUG.parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;
    Setup {
        store,
        reminders,
        relay,
        alice,
        calendar,
    }
}

async fn setup() -> Setup {
    setup_with(|_| {}).await
}

fn event(uid: &str, start: &str, extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
         DTSTAMP:20260101T000000Z\r\nDTSTART:{start}\r\nDURATION:PT1H\r\nSUMMARY:Weekly sync\r\n\
         LOCATION:Room 1\r\nDESCRIPTION:secret agenda\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

const EMAIL_ALARM: &str = "BEGIN:VALARM\r\nACTION:EMAIL\r\nTRIGGER:-PT15M\r\nSUMMARY:x\r\n\
    DESCRIPTION:x\r\nATTENDEE:mailto:evil@attacker.example\r\nEND:VALARM\r\n";
const DISPLAY_ALARM: &str =
    "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nDESCRIPTION:x\r\nEND:VALARM\r\n";

async fn put(store: &Store, calendar: i64, uid: &str, content: &str) {
    let etag = format!("\"{}\"", blake3::hash(content.as_bytes()).to_hex());
    store
        .put_item(
            calendar,
            &format!("{uid}.ics"),
            &NewItem {
                uid,
                component: "VEVENT",
                range_start: None,
                range_end: None,
                recurring: false,
                index_version: 0,
                etag: &etag,
                content: content.as_bytes(),
                attachments: &[],
            },
            &Precondition::None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn email_alarm_goes_to_the_owner_only() {
    let s = setup().await;
    put(
        &s.store,
        s.calendar,
        "a",
        &event("a", "20261026T090000Z", EMAIL_ALARM),
    )
    .await;
    // Indexed an hour before: nothing due yet.
    let report = s.reminders.run(START - 3600).await.unwrap();
    assert_eq!(report.indexed, 1);
    assert_eq!(report.queued, 0);
    assert!(s.relay.mails.lock().unwrap().is_empty());
    // At the trigger (08:45).
    let report = s.reminders.run(START - 900).await.unwrap();
    assert_eq!((report.queued, report.sent), (1, 1));
    let mails = s.relay.mails.lock().unwrap().clone();
    assert_eq!(mails.len(), 1);
    let (to, message) = &mails[0];
    assert_eq!(to, "<alice@example.org>");
    assert!(!message.contains("attacker"), "{message}");
    assert!(message.contains("Subject: Reminder: Weekly sync\r\n"));
    assert!(message.contains("To: <alice@example.org>\r\n"));
    assert!(message.contains("When: Mon, 26 Oct 2026, 09:00=E2=80=9310:00 (UTC)"));
    assert!(message.contains("Location: Room 1"));
    assert!(message.contains("Open your calendar: https://cal.example.org/"));
    // No description by default.
    assert!(!message.contains("secret agenda"));
    // Nothing more, also when run again.
    let report = s.reminders.run(START - 800).await.unwrap();
    assert_eq!(report.queued, 0);
    assert_eq!(s.relay.mails.lock().unwrap().len(), 1);
    let counts = s.store.mail_counts(Some(s.alice.id), 0).await.unwrap();
    assert_eq!(counts.sent, 1);
}

#[tokio::test]
async fn display_alarms_need_mode_all() {
    let s = setup().await;
    put(
        &s.store,
        s.calendar,
        "a",
        &event("a", "20261026T090000Z", DISPLAY_ALARM),
    )
    .await;
    s.reminders.run(START - 3600).await.unwrap();
    s.reminders.run(START - 600).await.unwrap();
    assert!(s.relay.mails.lock().unwrap().is_empty());

    put(
        &s.store,
        s.calendar,
        "b",
        &event("b", "20261027T090000Z", DISPLAY_ALARM),
    )
    .await;
    s.store
        .set_reminder_settings(s.alice.id, Some(Some(ReminderMode::All)), Some(true))
        .await
        .unwrap();
    s.reminders.run(START).await.unwrap();
    let report = s.reminders.run(START + 86_400 - 600).await.unwrap();
    assert_eq!(report.sent, 1);
    let mails = s.relay.mails.lock().unwrap().clone();
    assert!(mails[0].1.contains("Tue, 27 Oct 2026"));
    // Opted in: the description is included.
    assert!(mails[0].1.contains("secret agenda"));

    // Off: nothing.
    s.store
        .set_reminder_settings(s.alice.id, Some(Some(ReminderMode::Off)), None)
        .await
        .unwrap();
    put(
        &s.store,
        s.calendar,
        "c",
        &event("c", "20261029T090000Z", EMAIL_ALARM),
    )
    .await;
    s.reminders.run(START + 2 * 86_400).await.unwrap();
    s.reminders.run(START + 3 * 86_400).await.unwrap();
    assert_eq!(s.relay.mails.lock().unwrap().len(), 1);
    assert_eq!(
        s.store.scheduled_reminders(Some(s.alice.id)).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn server_default_mode_all() {
    let s = setup_with(|c| c.reminders.default_mode = ReminderDefault::All).await;
    put(
        &s.store,
        s.calendar,
        "a",
        &event("a", "20261026T090000Z", DISPLAY_ALARM),
    )
    .await;
    s.reminders.run(START - 3600).await.unwrap();
    assert_eq!(s.reminders.run(START - 600).await.unwrap().sent, 1);
}

#[tokio::test]
async fn imports_do_not_flood_with_past_reminders() {
    let s = setup().await;
    // Imported at 10:00: alarms hours ago are never sent, one two minutes
    // ago still is.
    for (uid, start) in [
        ("old1", "20261026T070000Z"),
        ("old2", "20261026T080000Z"),
        ("recent", "20261026T101300Z"),
        ("future", "20261026T120000Z"),
    ] {
        put(&s.store, s.calendar, uid, &event(uid, start, EMAIL_ALARM)).await;
    }
    let now = START + 3600;
    let report = s.reminders.run(now).await.unwrap();
    assert_eq!(report.sent, 1);
    let report = s.reminders.run(now + 2 * 3600).await.unwrap();
    assert_eq!(report.sent, 1);
    assert_eq!(s.relay.mails.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn missed_reminders_are_dropped_after_max_late() {
    let s = setup().await;
    put(
        &s.store,
        s.calendar,
        "a",
        &event("a", "20261026T090000Z", EMAIL_ALARM),
    )
    .await;
    s.reminders.run(START - 86_400).await.unwrap();
    // The server was down from before the trigger until an hour after it.
    let report = s.reminders.run(START + 2700).await.unwrap();
    assert_eq!(report.dispatched, 1);
    assert_eq!(report.queued, 0);
    // Within max_late it is still sent.
    put(
        &s.store,
        s.calendar,
        "b",
        &event("b", "20261027T090000Z", EMAIL_ALARM),
    )
    .await;
    s.reminders.run(START).await.unwrap();
    let report = s.reminders.run(START + 86_400 - 900 + 600).await.unwrap();
    assert_eq!(report.sent, 1);
}

#[tokio::test]
async fn restarts_never_send_twice() {
    let s = setup().await;
    put(
        &s.store,
        s.calendar,
        "a",
        &event("a", "20261026T090000Z", EMAIL_ALARM),
    )
    .await;
    s.reminders.run(START - 3600).await.unwrap();
    s.reminders.run(START - 900).await.unwrap();
    // A new instance over the same database (a restart), at the same time
    // and a bit later.
    let (port, relay) = relay().await;
    let again = Reminders::new(s.store.clone(), &config(port)).unwrap();
    again.prepare().await.unwrap();
    again.run(START - 900).await.unwrap();
    again.run(START - 600).await.unwrap();
    assert!(relay.mails.lock().unwrap().is_empty());
    assert_eq!(s.relay.mails.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn recurring_events_get_a_reminder_per_instance() {
    let s = setup().await;
    put(
        &s.store,
        s.calendar,
        "daily",
        &event(
            "daily",
            "20261026T090000Z",
            &format!("RRULE:FREQ=DAILY;COUNT=3\r\nEXDATE:20261027T090000Z\r\n{EMAIL_ALARM}"),
        ),
    )
    .await;
    s.reminders.run(START - 3600).await.unwrap();
    for day in 0..4 {
        s.reminders.run(START + day * 86_400 - 900).await.unwrap();
    }
    let mails = s.relay.mails.lock().unwrap().clone();
    assert_eq!(mails.len(), 2);
    assert!(mails[0].1.contains("Mon, 26 Oct 2026"));
    assert!(mails[1].1.contains("Wed, 28 Oct 2026"));
    // The series is over.
    assert_eq!(
        s.store.scheduled_reminders(Some(s.alice.id)).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn edits_and_deletions_are_picked_up() {
    let s = setup().await;
    put(
        &s.store,
        s.calendar,
        "a",
        &event("a", "20261026T090000Z", EMAIL_ALARM),
    )
    .await;
    s.reminders.run(START - 3600).await.unwrap();
    // Moved to the next day: no reminder today, one tomorrow.
    put(
        &s.store,
        s.calendar,
        "a",
        &event("a", "20261027T090000Z", EMAIL_ALARM),
    )
    .await;
    s.reminders.run(START - 1800).await.unwrap();
    s.reminders.run(START - 900).await.unwrap();
    assert!(s.relay.mails.lock().unwrap().is_empty());
    s.reminders.run(START + 86_400 - 900).await.unwrap();
    assert_eq!(s.relay.mails.lock().unwrap().len(), 1);

    // Deleted after its reminder was queued but before it was sent: the
    // reminder is dropped.
    put(
        &s.store,
        s.calendar,
        "b",
        &event("b", "20261029T090000Z", EMAIL_ALARM),
    )
    .await;
    s.reminders.run(START + 2 * 86_400).await.unwrap();
    s.relay.rcpt.store(451, Ordering::Relaxed);
    let at = START + 3 * 86_400 - 900;
    let report = s.reminders.run(at).await.unwrap();
    assert_eq!((report.queued, report.retried), (1, 1));
    s.store.delete_collection(s.calendar).await.unwrap();
    s.relay.rcpt.store(250, Ordering::Relaxed);
    s.reminders.run(at + 600).await.unwrap();
    assert_eq!(s.relay.mails.lock().unwrap().len(), 1);
    let counts = s.store.mail_counts(Some(s.alice.id), 0).await.unwrap();
    assert_eq!(counts.cancelled, 1);
}

#[tokio::test]
async fn retries_with_backoff_and_permanent_failures() {
    let s = setup().await;
    put(
        &s.store,
        s.calendar,
        "a",
        &event("a", "20261026T120000Z", EMAIL_ALARM),
    )
    .await;
    s.reminders.run(START).await.unwrap();
    s.relay.rcpt.store(451, Ordering::Relaxed);
    let at = START + 3 * 3600 - 900;
    let report = s.reminders.run(at).await.unwrap();
    assert_eq!(report.retried, 1);
    let counts = s.store.mail_counts(Some(s.alice.id), 0).await.unwrap();
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.last_error.unwrap().0, "smtp_unavailable");
    // Not before the back-off.
    s.relay.rcpt.store(250, Ordering::Relaxed);
    assert_eq!(s.reminders.run(at + 30).await.unwrap().sent, 0);
    assert_eq!(s.reminders.run(at + backoff(0)).await.unwrap().sent, 1);

    // A permanent refusal is not retried and shows up in the status.
    put(
        &s.store,
        s.calendar,
        "b",
        &event("b", "20261027T120000Z", EMAIL_ALARM),
    )
    .await;
    s.reminders.run(at + 3600).await.unwrap();
    s.relay.rcpt.store(550, Ordering::Relaxed);
    let report = s
        .reminders
        .run(START + 86_400 + 3 * 3600 - 900)
        .await
        .unwrap();
    assert_eq!(report.failed, 1);
    let counts = s.store.mail_counts(Some(s.alice.id), 0).await.unwrap();
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.last_error.unwrap().0, "smtp_rejected");
}

#[tokio::test]
async fn unreachable_server_keeps_mails_for_later() {
    let s = setup().await;
    // A client for a port nobody listens on.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let down = Reminders::new(s.store.clone(), &config(port)).unwrap();
    put(
        &s.store,
        s.calendar,
        "a",
        &event("a", "20261026T090000Z", EMAIL_ALARM),
    )
    .await;
    down.run(START - 3600).await.unwrap();
    let report = down.run(START - 900).await.unwrap();
    assert_eq!((report.queued, report.retried), (1, 1));
    assert_eq!(
        down.sender_status().last_error.unwrap().0,
        "smtp_unavailable"
    );
    // Back up: delivered by the next attempt.
    let report = s.reminders.run(START - 900 + backoff(0)).await.unwrap();
    assert_eq!(report.sent, 1);
}

#[tokio::test]
async fn per_user_limits_drop_and_report() {
    let s = setup().await;
    for n in 0..8 {
        let uid = format!("e{n}");
        put(
            &s.store,
            s.calendar,
            &uid,
            &event(&uid, "20261026T090000Z", EMAIL_ALARM),
        )
        .await;
    }
    s.reminders.run(START - 3600).await.unwrap();
    let report = s.reminders.run(START - 900).await.unwrap();
    assert_eq!((report.queued, report.dropped), (5, 3));
    let counts = s.store.mail_counts(Some(s.alice.id), 0).await.unwrap();
    assert_eq!(counts.dropped, 3);
    assert_eq!(counts.last_error.unwrap().0, "limit");
}

#[tokio::test]
async fn no_address_no_mail_and_no_subscriptions() {
    let s = setup().await;
    let bob = s
        .store
        .create_user(&"bob".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let bob_cal = s
        .store
        .collection(
            bob.id,
            CollectionKind::Calendar,
            &DEFAULT_COLLECTION_SLUG.parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;
    put(
        &s.store,
        bob_cal,
        "a",
        &event("a", "20261026T090000Z", EMAIL_ALARM),
    )
    .await;
    // A subscription of alice's with alarms in the feed.
    let slug = "feed".parse().unwrap();
    let content = event("f", "20261026T090000Z", EMAIL_ALARM);
    s.store
        .create_subscription(
            &NewSubscription {
                owner_id: s.alice.id,
                slug: &slug,
                display_name: None,
                color: None,
                components: "VEVENT",
                source_url: "https://example.org/feed.ics",
                refresh_secs: None,
                max_per_user: 10,
            },
            &[calcraft_store::FeedItem {
                name: "f.ics".to_owned(),
                uid: "f".to_owned(),
                component: "VEVENT".to_owned(),
                range_start: None,
                range_end: None,
                recurring: false,
                index_version: 0,
                etag: "\"f\"".to_owned(),
                content: content.into_bytes(),
            }],
            &calcraft_store::FeedState {
                feed_ttl_secs: None,
                http_etag: None,
                http_last_modified: None,
                content_hash: "x",
                skipped: 0,
                now: START,
                next_refresh_at: START + 3600,
            },
        )
        .await
        .unwrap();
    s.reminders.run(START - 3600).await.unwrap();
    let report = s.reminders.run(START - 900).await.unwrap();
    // Bob's reminder has nowhere to go; the feed is not indexed at all.
    assert_eq!(report.dispatched, 1);
    assert_eq!(report.queued, 0);
    assert!(s.relay.mails.lock().unwrap().is_empty());
    assert_eq!(s.reminders.queue_test(&bob).await.unwrap(), None);
}

#[tokio::test]
async fn test_mails() {
    let s = setup().await;
    assert_eq!(s.reminders.queue_test(&s.alice).await.unwrap(), Some(true));
    let report = s.reminders.run(crate::auth::now()).await.unwrap();
    assert_eq!(report.sent, 1);
    s.reminders.send_test_now(&s.alice).await.unwrap();
    let mails = s.relay.mails.lock().unwrap().clone();
    assert_eq!(mails.len(), 2);
    assert!(mails[1].1.contains("Subject: CalCraft test message"));
}

#[tokio::test]
async fn derived_addresses_follow_the_scheduling_rules() {
    let s = setup_with(|c| c.scheduling.domain = Some("example.org".to_owned())).await;
    let bob = s
        .store
        .create_user(&"bob".parse().unwrap(), "h", None)
        .await
        .unwrap();
    let scheduling = s.reminders.scheduling().clone();
    assert_eq!(
        own_address(&s.store, &scheduling, &bob).await.unwrap(),
        Some("bob@example.org".to_owned())
    );
    // Alice's explicit address in the domain that is not her own is ignored.
    s.store
        .set_user_email(s.alice.id, Some("carol@example.org"))
        .await
        .unwrap();
    assert_eq!(
        own_address(&s.store, &scheduling, &s.alice).await.unwrap(),
        Some("alice@example.org".to_owned())
    );
}
