//! E-mail reminders for alarms (`[reminders]`, docs/reminders.md).
//!
//! One background task runs three steps every few seconds:
//!
//! 1. **Index**: calendars whose objects changed (PUT, DELETE, MOVE, COPY,
//!    restore, imports, scheduling – anything that bumps a revision) are
//!    indexed again: for each changed object the next alarm trigger is
//!    computed with the recurrence engine within the per-object budget
//!    ([`calcraft_ical::alarm`]). Only the owner's own calendars count, no
//!    subscriptions (their alarms come from a third party).
//! 2. **Dispatch**: objects whose next trigger has come get their due
//!    alarms turned into mails, queued in the same transaction that marks
//!    them as done (no duplicates after a restart, nothing lost).
//!    Triggers more than `max_late_minutes` in the past are skipped, and
//!    objects that are new or changed only catch up on the last few
//!    minutes, so imports cannot cause a flood of old reminders.
//! 3. **Send**: the queue is delivered over SMTP with retries and
//!    back-off, within the server-wide rate.
//!
//! **Recipient**: only the calendar owner's own address (set by an
//! administrator, or `<user>@<scheduling.domain>`), never an address from
//! the calendar data: `ATTENDEE`s of an `EMAIL` alarm are ignored, so
//! nobody can make the server mail arbitrary addresses.
//!
//! Logs contain ids and counts only: no user names, addresses, subjects or
//! event data.

pub mod render;

use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use calcraft_core::mail::{Message, normalize_address};
use calcraft_dav::xml::NS_CALDAV;
use calcraft_ical::{
    Budget, Component, OBJECT_BUDGET,
    alarm::{self, Selection},
};
use calcraft_store::{
    Completion, IndexEntry, MailKind, MailLimits, NewMail, OutgoingMail, ReminderMode, Store,
    StoreError, User,
};

use crate::{
    auth::now,
    config::{Config, ReminderDefault, RemindersConfig, SchedulingConfig},
    smtp::{SmtpClient, SmtpError},
};

/// Pause between two runs of the loop.
pub const TICK: Duration = Duration::from_secs(15);
/// New or changed objects catch up on triggers of this many seconds.
pub const CHANGED_GRACE_SECS: i64 = 5 * 60;
/// How far ahead the next trigger is searched; objects without one get a
/// re-check after this time (yearly events with early alarms).
pub const HORIZON_SECS: i64 = 400 * 86_400;
/// Calendars indexed per run.
const CALENDARS_PER_RUN: u32 = 64;
/// Objects read per batch.
const INDEX_BATCH: u32 = 32;
/// Objects whose alarms are dispatched per run.
const DUE_PER_RUN: u32 = 64;
/// Mails sent per run at most (besides the per-minute rate).
const SEND_PER_RUN: u32 = 100;
/// Attempts before a mail is given up.
pub const MAX_ATTEMPTS: i64 = 10;
/// Pending mails are pruned and old rows deleted this often.
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

/// Seconds to wait before attempt `attempts + 1`.
pub fn backoff(attempts: i64) -> i64 {
    match attempts {
        ..=0 => 60,
        1 => 5 * 60,
        2 => 15 * 60,
        3 => 3600,
        _ => 3 * 3600,
    }
}

fn store_mode(mode: ReminderDefault) -> ReminderMode {
    match mode {
        ReminderDefault::Off => ReminderMode::Off,
        ReminderDefault::Email => ReminderMode::Email,
        ReminderDefault::All => ReminderMode::All,
    }
}

fn selection(mode: ReminderMode) -> Option<Selection> {
    match mode {
        ReminderMode::Off => None,
        ReminderMode::Email => Some(Selection::EmailOnly),
        ReminderMode::All => Some(Selection::All),
    }
}

/// The user's own e-mail address for reminders, by the scheduling rules:
/// the address an administrator set, unless it lies in the scheduling
/// domain but is not the user's own there; else `<user>@<domain>` unless
/// another user has that as e-mail address. Never anything from calendar
/// data or from an identity provider.
pub async fn own_address(
    store: &Store,
    scheduling: &SchedulingConfig,
    user: &User,
) -> Result<Option<String>, StoreError> {
    if let Some(email) = store.user_email(user.id).await? {
        let foreign = match (&scheduling.domain, email.rsplit_once('@')) {
            (Some(domain), Some((local, host))) if host == domain => {
                local != user.username.as_str()
            }
            _ => false,
        };
        if !foreign && let Some(address) = normalize_address(&email) {
            return Ok(Some(address));
        }
    }
    if let Some(domain) = &scheduling.domain {
        let derived = format!("{}@{domain}", user.username);
        let taken = store
            .user_by_email(&derived)
            .await?
            .is_some_and(|other| other.id != user.id);
        if !taken {
            return Ok(normalize_address(&derived));
        }
    }
    Ok(None)
}

/// The VTIMEZONE of a calendar's `CALDAV:calendar-timezone`, if set.
async fn calendar_timezone(store: &Store, collection_id: i64) -> Option<Component> {
    let properties = store.dead_properties(collection_id).await.ok()?;
    let stored = properties
        .iter()
        .find(|p| p.namespace == NS_CALDAV && p.name == "calendar-timezone")?;
    // The value is stored as escaped XML text.
    let wrapped = format!("<v xmlns=\"\">{}</v>", stored.value);
    let limits = calcraft_ical::xml::XmlLimits {
        max_len: 256 * 1024,
        max_depth: 4,
        max_nodes: 16,
    };
    let node = calcraft_ical::xml::parse(wrapped.as_bytes(), &limits).ok()?;
    alarm::calendar_timezone(&node.text)
}

/// Stable identity of one reminder.
fn dedupe_key(collection_id: i64, uid: &str, instance_start: i64, trigger: i64) -> String {
    let uid = blake3::hash(uid.as_bytes()).to_hex();
    format!(
        "r:{collection_id}:{}:{instance_start}:{trigger}",
        &uid[..32]
    )
}

/// What one run did (tests, debug logs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunReport {
    pub indexed: u64,
    pub dispatched: u64,
    pub queued: u64,
    pub dropped: u64,
    pub sent: u64,
    pub retried: u64,
    pub failed: u64,
}

/// Status of the last SMTP session, for administrators (no details).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SenderStatus {
    /// Unix seconds of the last successful send.
    pub last_success: Option<i64>,
    /// Code and Unix seconds of the last failure.
    pub last_error: Option<(&'static str, i64)>,
}

/// Runtime state of reminders (`[reminders] enabled`).
pub struct Reminders {
    store: Store,
    config: RemindersConfig,
    scheduling: SchedulingConfig,
    smtp: SmtpClient,
    from_name: String,
    max_per_minute: usize,
    /// Send times within the last minute (server-wide rate).
    sent: Mutex<VecDeque<Instant>>,
    status: Mutex<SenderStatus>,
    /// Runs the loop now (test mail queued).
    wake: tokio::sync::Notify,
    /// Only one run at a time (the loop and tests).
    running: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Reminders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reminders").finish_non_exhaustive()
    }
}

impl Reminders {
    /// Reads the SMTP password file and the trust store; no network access.
    pub fn new(store: Store, config: &Config) -> anyhow::Result<Arc<Self>> {
        let smtp = SmtpClient::new(&config.smtp)?;
        Ok(Self::with_client(store, config, smtp))
    }

    /// With a given SMTP client (tests).
    pub fn with_client(store: Store, config: &Config, smtp: SmtpClient) -> Arc<Self> {
        Arc::new(Self {
            store,
            config: config.reminders.clone(),
            scheduling: config.scheduling.clone(),
            smtp,
            from_name: config.smtp.from_name.clone(),
            max_per_minute: usize::try_from(config.smtp.max_per_minute).unwrap_or(60),
            sent: Mutex::new(VecDeque::new()),
            status: Mutex::new(SenderStatus::default()),
            wake: tokio::sync::Notify::new(),
            running: tokio::sync::Mutex::new(()),
        })
    }

    pub fn config(&self) -> &RemindersConfig {
        &self.config
    }

    pub fn scheduling(&self) -> &SchedulingConfig {
        &self.scheduling
    }

    /// The server default mode.
    pub fn default_mode(&self) -> ReminderMode {
        store_mode(self.config.default_mode)
    }

    fn effective(&self, mode: Option<ReminderMode>) -> ReminderMode {
        mode.unwrap_or_else(|| self.default_mode())
    }

    pub fn limits(&self) -> MailLimits {
        MailLimits {
            per_user_hour: i64::from(self.config.max_per_user_per_hour),
            per_user_day: i64::from(self.config.max_per_user_per_day),
            max_pending: i64::from(self.config.max_queue),
        }
    }

    pub fn sender_status(&self) -> SenderStatus {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// To be called before [`Reminders::spawn`]: if the server default mode
    /// changed since the last start, every calendar is indexed again.
    pub async fn prepare(&self) -> Result<(), StoreError> {
        if self
            .store
            .sync_reminder_default(self.default_mode())
            .await?
        {
            tracing::info!("reminders.default_mode changed: calendars are indexed again");
        }
        Ok(())
    }

    /// Runs the loop in the background.
    pub fn spawn(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut last_prune: Option<Instant> = None;
            loop {
                if last_prune.is_none_or(|t| t.elapsed() >= PRUNE_INTERVAL) {
                    last_prune = Some(Instant::now());
                    match this.store.prune_mails(now()).await {
                        Ok((expired, deleted)) if expired + deleted > 0 => {
                            tracing::info!(expired, deleted, "pruned the mail queue");
                        }
                        Ok(_) => {}
                        Err(err) => tracing::warn!(%err, "pruning the mail queue failed"),
                    }
                }
                match this.run(now()).await {
                    Ok(report) if report != RunReport::default() => {
                        tracing::debug!(?report, "reminder run");
                    }
                    Ok(_) => {}
                    Err(err) => tracing::warn!(%err, "reminder run failed"),
                }
                tokio::select! {
                    () = tokio::time::sleep(TICK) => {}
                    () = this.wake.notified() => {}
                }
            }
        });
    }

    /// One run of all three steps at `now`.
    pub async fn run(&self, now: i64) -> Result<RunReport, StoreError> {
        let _guard = self.running.lock().await;
        let mut report = RunReport::default();
        self.index(now, &mut report).await?;
        self.dispatch(now, &mut report).await?;
        self.send(now, &mut report).await?;
        Ok(report)
    }

    async fn index(&self, now: i64, report: &mut RunReport) -> Result<(), StoreError> {
        let dirty = self
            .store
            .reminder_dirty_calendars(CALENDARS_PER_RUN)
            .await?;
        for calendar in dirty {
            let mode = self.effective(calendar.mode);
            let Some(selection) = selection(mode).filter(|_| !calendar.owner_disabled) else {
                self.store
                    .clear_reminder_index(calendar.collection_id)
                    .await?;
                self.store
                    .set_reminder_cursor(calendar.collection_id, calendar.revision)
                    .await?;
                continue;
            };
            let zone = calendar_timezone(&self.store, calendar.collection_id).await;
            let mut after = 0;
            loop {
                let batch = self
                    .store
                    .reminder_candidates(
                        calendar.collection_id,
                        calendar.indexed_revision,
                        after,
                        INDEX_BATCH,
                    )
                    .await?;
                let Some(last) = batch.last() else {
                    break;
                };
                after = last.item_id;
                let zone = zone.clone();
                let computed = tokio::task::spawn_blocking(move || {
                    batch
                        .into_iter()
                        .map(|candidate| {
                            let scan_from = candidate
                                .scan_from
                                .unwrap_or(i64::MIN)
                                .max(now - CHANGED_GRACE_SECS);
                            let next_due =
                                next_due(&candidate.content, scan_from, zone.as_ref(), selection);
                            (candidate.item_id, candidate.etag, scan_from, next_due)
                        })
                        .collect::<Vec<_>>()
                })
                .await
                .map_err(|_| StoreError::Corrupt("reminder indexing task failed".to_owned()))?;
                let entries: Vec<IndexEntry<'_>> = computed
                    .iter()
                    .map(|(item_id, etag, scan_from, next_due)| IndexEntry {
                        item_id: *item_id,
                        etag,
                        scan_from: *scan_from,
                        next_due: *next_due,
                    })
                    .collect();
                report.indexed += entries.len() as u64;
                self.store.set_reminder_index(&entries).await?;
            }
            self.store
                .set_reminder_cursor(calendar.collection_id, calendar.revision)
                .await?;
        }
        Ok(())
    }

    async fn dispatch(&self, now: i64, report: &mut RunReport) -> Result<(), StoreError> {
        let due = self.store.due_reminders(now, DUE_PER_RUN).await?;
        let mut zones: HashMap<i64, Option<Component>> = HashMap::new();
        let mut addresses: HashMap<i64, Option<String>> = HashMap::new();
        let late = i64::from(self.config.max_late_minutes) * 60;
        for reminder in due {
            report.dispatched += 1;
            let mode = self.effective(reminder.mode);
            let selection = selection(mode).filter(|_| !reminder.owner_disabled);
            let Some(selection) = selection else {
                let done = Completion {
                    item_id: reminder.item_id,
                    etag: &reminder.etag,
                    scan_to: now,
                    next_due: None,
                };
                self.store
                    .complete_reminder(&done, &[], self.limits(), now)
                    .await?;
                continue;
            };
            if let Entry::Vacant(entry) = zones.entry(reminder.collection_id) {
                entry.insert(calendar_timezone(&self.store, reminder.collection_id).await);
            }
            let zone = zones.get(&reminder.collection_id).cloned().flatten();
            if let Entry::Vacant(entry) = addresses.entry(reminder.owner_id) {
                let address = match self.store.user_by_id(reminder.owner_id).await? {
                    Some(user) => own_address(&self.store, &self.scheduling, &user).await?,
                    None => None,
                };
                entry.insert(address);
            }
            let recipient = addresses.get(&reminder.owner_id).cloned().flatten();
            let content = reminder.content.clone();
            let scan_from = reminder.scan_from;
            let calendar = reminder
                .collection_name
                .clone()
                .unwrap_or_else(|| reminder.collection_slug.clone());
            let include_description = reminder.include_description;
            let base = self.config.base().map(str::to_owned);
            let rendered = tokio::task::spawn_blocking(move || {
                let Ok(root) = calcraft_ical::parse(&content) else {
                    return (Vec::new(), None);
                };
                let mut budget = Budget::new(OBJECT_BUDGET);
                let occurrences = alarm::alarm_occurrences(
                    &root,
                    scan_from,
                    now,
                    zone.as_ref(),
                    selection,
                    &mut budget,
                )
                .unwrap_or_default();
                let mails: Vec<(i64, i64, i64, render::Content)> = occurrences
                    .iter()
                    .filter(|o| o.trigger > now - late)
                    .map(|o| {
                        let content = render::reminder(
                            &root,
                            o,
                            &calendar,
                            include_description,
                            base.as_deref(),
                        );
                        (
                            o.trigger,
                            o.instance.start,
                            o.instance.start.max(o.trigger + late),
                            content,
                        )
                    })
                    .collect();
                let mut budget = Budget::new(OBJECT_BUDGET);
                let next = alarm::next_alarm(
                    &root,
                    now,
                    HORIZON_SECS,
                    zone.as_ref(),
                    selection,
                    &mut budget,
                );
                let next = match next {
                    Ok(Some(next)) => Some(next),
                    Ok(None) => recheck(&content, now),
                    Err(_) => None,
                };
                (mails, next)
            })
            .await
            .map_err(|_| StoreError::Corrupt("reminder rendering task failed".to_owned()))?;
            let (contents, next_due) = rendered;
            let mut mails = Vec::new();
            if let Some(recipient) = &recipient {
                for (trigger, instance_start, expires_at, content) in contents {
                    let id = crate::secrets::random_token::<16>();
                    let message = Message {
                        from_name: &self.from_name,
                        from: self.smtp.from(),
                        to: recipient,
                        subject: &content.subject,
                        body: &content.body,
                        date: now,
                        id: &id,
                    };
                    let Ok(message) = message.render() else {
                        continue;
                    };
                    if message.len() > calcraft_store::MAX_MAIL_BYTES {
                        continue;
                    }
                    mails.push(NewMail {
                        user_id: reminder.owner_id,
                        kind: MailKind::Reminder,
                        item_id: Some(reminder.item_id),
                        dedupe_key: Some(dedupe_key(
                            reminder.collection_id,
                            &reminder.uid,
                            instance_start,
                            trigger,
                        )),
                        recipient: recipient.clone(),
                        message,
                        expires_at,
                    });
                }
            }
            let done = Completion {
                item_id: reminder.item_id,
                etag: &reminder.etag,
                scan_to: now,
                next_due,
            };
            if let Some(outcome) = self
                .store
                .complete_reminder(&done, &mails, self.limits(), now)
                .await?
            {
                report.queued += u64::from(outcome.queued);
                report.dropped += u64::from(outcome.dropped);
                if outcome.dropped > 0 {
                    tracing::warn!(
                        user_id = reminder.owner_id,
                        dropped = outcome.dropped,
                        "reminder mails dropped (limit or full queue)"
                    );
                }
            }
        }
        Ok(())
    }

    /// Mails the per-minute rate still allows now.
    fn rate_left(&self) -> usize {
        let mut sent = self
            .sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while sent
            .front()
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(60))
        {
            sent.pop_front();
        }
        self.max_per_minute.saturating_sub(sent.len())
    }

    fn record_send(&self) {
        self.sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(Instant::now());
    }

    fn record_status(&self, result: Result<(), SmtpError>, now: i64) {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match result {
            Ok(()) => status.last_success = Some(now),
            Err(err) => status.last_error = Some((err.code(), now)),
        }
    }

    async fn retry_or_fail(
        &self,
        mail: &OutgoingMail,
        err: SmtpError,
        now: i64,
        report: &mut RunReport,
    ) -> Result<(), StoreError> {
        if err.transient() && mail.attempts + 1 < MAX_ATTEMPTS {
            let next = now + backoff(mail.attempts);
            if next < mail.expires_at {
                report.retried += 1;
                return self.store.retry_mail(mail.id, err.code(), next).await;
            }
        }
        report.failed += 1;
        self.store
            .finish_mail(mail.id, "failed", Some(err.code()), now)
            .await
    }

    async fn send(&self, now: i64, report: &mut RunReport) -> Result<(), StoreError> {
        let limit = self.rate_left().min(SEND_PER_RUN as usize);
        if limit == 0 {
            return Ok(());
        }
        let pending = self
            .store
            .pending_mails(now, u32::try_from(limit).unwrap_or(SEND_PER_RUN))
            .await?;
        let mut connection = None;
        let mut session_error: Option<SmtpError> = None;
        for mail in &pending {
            if mail.orphaned {
                self.store
                    .finish_mail(mail.id, "cancelled", Some("deleted"), now)
                    .await?;
                continue;
            }
            if mail.expires_at <= now {
                report.failed += 1;
                self.store
                    .finish_mail(mail.id, "failed", Some("expired"), now)
                    .await?;
                continue;
            }
            if let Some(err) = session_error {
                // The server is unreachable: the rest waits as well.
                self.retry_or_fail(mail, err, now, report).await?;
                continue;
            }
            if connection
                .as_ref()
                .is_none_or(|c: &crate::smtp::Connection| !c.usable())
            {
                match self.smtp.connect().await {
                    Ok(c) => connection = Some(c),
                    Err(err) => {
                        tracing::warn!(error = %err, "connecting to the mail server failed");
                        self.record_status(Err(err), now);
                        session_error = Some(err);
                        self.retry_or_fail(mail, err, now, report).await?;
                        continue;
                    }
                }
            }
            let Some(conn) = connection.as_mut() else {
                continue;
            };
            self.record_send();
            match conn.send(&mail.recipient, &mail.message).await {
                Ok(()) => {
                    report.sent += 1;
                    self.record_status(Ok(()), now);
                    self.store.finish_mail(mail.id, "sent", None, now).await?;
                }
                Err(err) => {
                    tracing::warn!(mail_id = mail.id, error = %err, "sending a mail failed");
                    self.record_status(Err(err), now);
                    self.retry_or_fail(mail, err, now, report).await?;
                }
            }
        }
        if let Some(conn) = connection {
            conn.quit().await;
        }
        Ok(())
    }

    /// Queues a test mail to the user's own address and wakes the loop.
    /// `Ok(None)` if the user has no address.
    pub async fn queue_test(&self, user: &User) -> Result<Option<bool>, StoreError> {
        let Some(recipient) = own_address(&self.store, &self.scheduling, user).await? else {
            return Ok(None);
        };
        let content = render::test_mail(self.config.base());
        let id = crate::secrets::random_token::<16>();
        let now = now();
        let message = Message {
            from_name: &self.from_name,
            from: self.smtp.from(),
            to: &recipient,
            subject: &content.subject,
            body: &content.body,
            date: now,
            id: &id,
        }
        .render()
        .map_err(|_| StoreError::Limit("invalid address"))?;
        let outcome = self
            .store
            .queue_mails(
                &[NewMail {
                    user_id: user.id,
                    kind: MailKind::Test,
                    item_id: None,
                    dedupe_key: None,
                    recipient,
                    message,
                    expires_at: now + 3600,
                }],
                self.limits(),
                now,
            )
            .await?;
        self.wake.notify_one();
        Ok(Some(outcome.queued == 1))
    }

    /// Sends a test mail right away, bypassing the queue (`calcraft smtp
    /// test`).
    pub async fn send_test_now(&self, user: &User) -> anyhow::Result<()> {
        let recipient = own_address(&self.store, &self.scheduling, user)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the user has no e-mail address (calcraft user set-email, or scheduling.domain)"
                )
            })?;
        let content = render::test_mail(self.config.base());
        let id = crate::secrets::random_token::<16>();
        let message = Message {
            from_name: &self.from_name,
            from: self.smtp.from(),
            to: &recipient,
            subject: &content.subject,
            body: &content.body,
            date: now(),
            id: &id,
        }
        .render()
        .map_err(|err| anyhow::anyhow!("{err}"))?;
        let mut connection = self.smtp.connect().await?;
        let result = connection.send(&recipient, &message).await;
        connection.quit().await;
        Ok(result?)
    }
}

/// When to look again at an object without alarms within the horizon:
/// only if it has instances beyond it (open-ended or long series).
fn recheck(content: &[u8], from: i64) -> Option<i64> {
    let object = calcraft_ical::CalendarObject::parse(content).ok()?;
    let horizon = from.saturating_add(HORIZON_SECS);
    object
        .range
        .end
        .is_none_or(|end| end > horizon - 30 * 86_400)
        .then_some(horizon)
}

/// Whether the object has any `VALARM` at all.
fn has_alarms(root: &Component) -> bool {
    root.components
        .iter()
        .any(|c| c.components.iter().any(|a| a.name == "VALARM"))
}

/// The next due moment of an object after `scan_from`; `None` without
/// (evaluable) alarms.
fn next_due(
    content: &[u8],
    scan_from: i64,
    zone: Option<&Component>,
    selection: Selection,
) -> Option<i64> {
    let root = calcraft_ical::parse(content).ok()?;
    if !has_alarms(&root) {
        return None;
    }
    let mut budget = Budget::new(OBJECT_BUDGET);
    match alarm::next_alarm(&root, scan_from, HORIZON_SECS, zone, selection, &mut budget) {
        Ok(Some(next)) => Some(next),
        // Alarms beyond the horizon: look again later.
        Ok(None) => recheck(content, scan_from),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests;
