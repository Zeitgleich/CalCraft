//! Delivery of iTIP messages between users of this server: the
//! recipient's calendar copy is created or updated (REQUEST, CANCEL) or the
//! organizer's copy gets the reply (REPLY), and a copy of the message goes
//! into the recipient's scheduling inbox.
//!
//! Every change of an existing object is a read–modify–write guarded by
//! its entity tag and retried (with a short random back-off) on conflicts,
//! so concurrent replies of several attendees never overwrite each other.
//! Copies derived from an organizer's object are only written while that
//! object still has the Schedule-Tag they were derived from
//! ([`Origin`]), so a slower delivery of an older version can never
//! overwrite a newer one.

use std::time::Duration;

use calcraft_ical::{Component, INDEX_VERSION, attach, itip, parse, write::to_string};
use calcraft_store::{
    Collection, NewInboxMessage, NewItem, Precondition, ScheduleTag, StoreError, User,
};

use super::{
    Scheduling, default_calendar, dtstamp, new_resource_name, new_schedule_tag, normalize,
};
use crate::{
    app::AppState,
    dav::{check_object, etag},
};

/// Attempts of a read–modify–write before giving up.
const ATTEMPTS: usize = 5;

/// Why a delivery did not happen.
#[derive(Debug)]
pub enum Failure {
    /// Quota, object or delivery limits, an invalid result, a conflicting
    /// object or a storage error (5.1).
    NotDelivered,
    /// The recipient has no writable calendar for the component (5.3).
    NoCalendar,
    /// A reply to an event the organizer does not have with this attendee
    /// (3.8). Invitations never use it: a conflicting object of the
    /// recipient is answered like a full quota (5.1), so that it is no
    /// oracle for UIDs in their calendars.
    NoAuthority,
}

impl Failure {
    pub fn status(&self) -> &'static str {
        match self {
            Self::NotDelivered => itip::status::NOT_DELIVERED,
            Self::NoCalendar => itip::status::NO_SCHEDULING,
            Self::NoAuthority => itip::status::NO_AUTHORITY,
        }
    }
}

impl From<StoreError> for Failure {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::Limit(_) | StoreError::Full => {}
            err => tracing::warn!(%err, "scheduling delivery failed"),
        }
        Self::NotDelivered
    }
}

/// The organizer's object a delivery is derived from: collection id,
/// resource name and the Schedule-Tag it was stored with.
#[derive(Debug, Clone, Copy)]
pub struct Origin<'a> {
    pub collection_id: i64,
    pub name: &'a str,
    pub tag: &'a str,
}

/// Waits a few random milliseconds before attempt `attempt + 1`, so that
/// competing writers do not collide again and again.
pub async fn backoff(attempt: usize) {
    let jitter = u64::from(crate::secrets::random_bytes::<1>()[0] % 20);
    let base = 5 * u64::try_from(attempt + 1).unwrap_or(1);
    tokio::time::sleep(Duration::from_millis(base + jitter)).await;
}

/// Serialises a tree for storage.
fn serialize(tree: &Component) -> Result<String, StoreError> {
    to_string(tree).map_err(|_| StoreError::Limit("invalid scheduling object"))
}

/// Stores `text` as object `name` of `collection` (validated like a PUT).
async fn store_text(
    state: &AppState,
    collection: &Collection,
    name: &str,
    text: &str,
    precondition: &Precondition,
    tag: Option<&str>,
    origin: Option<Origin<'_>>,
) -> Result<(), StoreError> {
    let body = text.as_bytes();
    let validated = check_object(collection, body, state.max_vcard_bytes)
        .map_err(|_| StoreError::Limit("invalid scheduling object"))?;
    let etag = etag(body);
    let item = NewItem {
        uid: &validated.uid,
        component: validated.component,
        range_start: validated.range_start,
        range_end: validated.range_end,
        recurring: validated.recurring,
        index_version: INDEX_VERSION,
        etag: &etag,
        content: body,
        // Recorded only where allowed: the organizer's own attachments, or
        // those of the organizer's object this copy is derived from.
        attachments: &validated.managed_ids,
    };
    state
        .store
        .put_item_scheduled(
            collection.id,
            name,
            &item,
            precondition,
            &ScheduleTag {
                set: tag,
                if_match: None,
                requires: origin.map(|o| (o.collection_id, o.name, o.tag)),
            },
        )
        .await?;
    Ok(())
}

/// Stores `tree` as object `name` of `collection`.
async fn store_tree(
    state: &AppState,
    collection: &Collection,
    name: &str,
    tree: &Component,
    precondition: &Precondition,
    tag: Option<&str>,
    origin: Option<Origin<'_>>,
) -> Result<(), StoreError> {
    let text = serialize(tree)?;
    store_text(state, collection, name, &text, precondition, tag, origin).await
}

/// Puts a message into the recipient's inbox; failures (quota) only cost
/// the notification, the calendar is already up to date.
async fn to_inbox(
    state: &AppState,
    scheduling: &Scheduling,
    recipient: &User,
    uid: &str,
    sender: &str,
    text: &str,
) {
    let name = new_resource_name();
    let content = text.as_bytes();
    let tag = etag(content);
    let message = NewInboxMessage {
        name: &name,
        uid,
        sender,
        etag: &tag,
        content,
        now: crate::auth::now(),
    };
    if let Err(err) = state
        .store
        .deliver_inbox_message(
            recipient.id,
            &message,
            i64::from(scheduling.config.inbox_max_messages),
        )
        .await
    {
        tracing::debug!(%err, user = %recipient.username, "inbox message not stored");
    }
}

/// Whether the object's ORGANIZER is one of `addresses`.
fn organized_by(tree: &Component, addresses: &[String]) -> bool {
    matches!(itip::organizer(tree), Ok(Some(o)) if addresses.contains(&normalize(&o.value)))
}

/// Charges a delivery of `bytes` to the recipient's limits (size of one
/// copy, bytes per hour).
fn admit(
    scheduling: &Scheduling,
    recipient: &User,
    copy: usize,
    total: usize,
) -> Result<(), Failure> {
    if copy > scheduling.config.max_delivered_object_bytes {
        return Err(Failure::NotDelivered);
    }
    let bytes = u64::try_from(total).unwrap_or(u64::MAX);
    if !scheduling.allow_bytes(recipient.id, bytes) {
        return Err(Failure::NotDelivered);
    }
    Ok(())
}

/// Delivers an invitation (or an update of it) to a user of this server:
/// `projection` is what the recipient sees of the organizer's object.
pub async fn request(
    state: &AppState,
    scheduling: &Scheduling,
    organizer_addresses: &[String],
    sender: &str,
    recipient: &User,
    projection: &Component,
    origin: Origin<'_>,
) -> Result<(), Failure> {
    let uid = itip::uid(projection)
        .ok_or(Failure::NotDelivered)?
        .to_owned();
    let message = serialize(&itip::message("REQUEST", projection, &dtstamp()))?;
    let mut admitted = false;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            backoff(attempt).await;
        }
        let existing = state.store.scheduling_object(recipient.id, &uid).await?;
        let (collection, name, text, guard) = match existing {
            Some(object) => {
                let old = parse(&object.content).ok();
                // Another organizer's (or the recipient's own) object with
                // this UID is never touched.
                if !old
                    .as_ref()
                    .is_some_and(|old| organized_by(old, organizer_addresses))
                {
                    return Err(Failure::NotDelivered);
                }
                let copy = itip::attendee_copy(projection, old.as_ref());
                (
                    object.collection,
                    object.name,
                    serialize(&copy)?,
                    Precondition::IfMatch(vec![object.etag]),
                )
            }
            None => {
                let component = projection
                    .components
                    .iter()
                    .find(|c| c.name != "VTIMEZONE")
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                let Some(calendar) =
                    default_calendar(&state.store, recipient.id, &component).await?
                else {
                    return Err(Failure::NoCalendar);
                };
                (
                    calendar,
                    new_resource_name(),
                    serialize(projection)?,
                    Precondition::MustNotExist,
                )
            }
        };
        if !admitted {
            admit(
                scheduling,
                recipient,
                text.len(),
                text.len() + message.len(),
            )?;
            admitted = true;
        }
        let result = store_text(
            state,
            &collection,
            &name,
            &text,
            &guard,
            Some(&new_schedule_tag()),
            Some(origin),
        )
        .await;
        match result {
            Ok(()) => {
                to_inbox(state, scheduling, recipient, &uid, sender, &message).await;
                return Ok(());
            }
            // The organizer changed the object again: that newer version
            // is delivered by its own request.
            Err(StoreError::Superseded) => return Ok(()),
            // Changed or created concurrently: read again.
            Err(StoreError::PreconditionFailed | StoreError::UidConflict(_)) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(Failure::NotDelivered)
}

/// Delivers a cancellation. A copy the recipient never touched (no answer,
/// no alarms, no change since it was delivered for `projection`) is
/// removed; otherwise it is kept, marked `STATUS:CANCELLED` (RFC 5546
/// §3.2.5), so that they see it was called off. `origin` is the organizer's
/// object if it still exists (an attendee removed from it).
pub async fn cancel(
    state: &AppState,
    scheduling: &Scheduling,
    organizer_addresses: &[String],
    sender: &str,
    recipient: &User,
    projection: &Component,
    origin: Option<Origin<'_>>,
) -> Result<(), Failure> {
    let uid = itip::uid(projection)
        .ok_or(Failure::NotDelivered)?
        .to_owned();
    let recipient_addresses = scheduling.addresses(&state.store, recipient).await?;
    let is_recipient = |v: &str| recipient_addresses.contains(&normalize(v));
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            backoff(attempt).await;
        }
        let Some(object) = state.store.scheduling_object(recipient.id, &uid).await? else {
            // Nothing to cancel (already deleted by the attendee).
            break;
        };
        let Some(old) = parse(&object.content).ok() else {
            return Err(Failure::NotDelivered);
        };
        if !organized_by(&old, organizer_addresses) {
            return Err(Failure::NotDelivered);
        }
        if itip::is_cancelled(&old) {
            break;
        }
        let guard = Precondition::IfMatch(vec![object.etag.clone()]);
        if itip::untouched(&old, projection, &is_recipient) {
            // Not into the trash: it was never the recipient's.
            match state
                .store
                .delete_item(object.collection.id, &object.name, &guard)
                .await
            {
                Ok(_) => break,
                Err(StoreError::PreconditionFailed) => continue,
                Err(err) => return Err(err.into()),
            }
        }
        // The kept copy no longer references the organizer's managed
        // attachments (RFC 8607): the cancellation ends the access.
        let mut kept = itip::cancelled(&old);
        attach::strip_managed(&mut kept);
        match store_tree(
            state,
            &object.collection,
            &object.name,
            &kept,
            &guard,
            Some(&new_schedule_tag()),
            origin,
        )
        .await
        {
            Ok(()) => break,
            Err(StoreError::Superseded) => return Ok(()),
            Err(StoreError::PreconditionFailed) => {}
            Err(err) => return Err(err.into()),
        }
    }
    let mut cancelled = itip::cancelled(projection);
    attach::strip_managed(&mut cancelled);
    let message = serialize(&itip::message("CANCEL", &cancelled, &dtstamp()))?;
    if scheduling.allow_bytes(
        recipient.id,
        u64::try_from(message.len()).unwrap_or(u64::MAX),
    ) {
        to_inbox(state, scheduling, recipient, &uid, sender, &message).await;
    }
    Ok(())
}

/// What an attendee replies with.
#[derive(Debug, Clone)]
pub enum ReplySource {
    /// The participation statuses in the attendee's copy.
    Copy(Component),
    /// The attendee deleted their copy: everything declined.
    DeclineAll,
}

/// What a reply would do to the organizer's copy.
#[derive(Debug, PartialEq, Eq)]
pub enum ReplyEffect {
    /// The attendee's status changes (or the reply is forced).
    Changes,
    /// Nothing to tell the organizer.
    Unchanged,
}

/// The organizer's copy of `uid`, if it is really theirs.
async fn organizer_copy(
    state: &AppState,
    organizer: &User,
    organizer_addresses: &[String],
    uid: &str,
) -> Result<Option<(calcraft_store::SchedulingObject, Component)>, Failure> {
    let Some(object) = state.store.scheduling_object(organizer.id, uid).await? else {
        return Ok(None);
    };
    let Ok(tree) = parse(&object.content) else {
        return Ok(None);
    };
    if !organized_by(&tree, organizer_addresses) {
        return Ok(None);
    }
    Ok(Some((object, tree)))
}

/// The organizer's copy after merging the reply, and the changed
/// components; `None` if the attendee is not invited. CPU work (parsing
/// time zones, recurrence checks) runs on the blocking pool, at most as
/// many at a time as recurrence checks of REPORTs.
async fn merge(
    state: &AppState,
    mut tree: Component,
    source: &ReplySource,
    attendee_addresses: &[String],
) -> Option<(Component, Vec<usize>)> {
    let permit = state.expansion.clone().acquire_owned().await.ok()?;
    let source = source.clone();
    let addresses = attendee_addresses.to_vec();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let is_attendee = |v: &str| addresses.contains(&normalize(v));
        itip::project(&tree, &is_attendee)?;
        let replies = match &source {
            ReplySource::Copy(copy) => itip::replies(copy, &tree, &is_attendee),
            ReplySource::DeclineAll => itip::decline_all(&tree),
        };
        let changed = itip::apply_replies(&mut tree, &is_attendee, &replies);
        Some((tree, changed))
    })
    .await
    .ok()
    .flatten()
}

/// Checks a reply without changing anything (to set `SCHEDULE-STATUS` in
/// the attendee's copy before it is stored).
pub async fn check_reply(
    state: &AppState,
    organizer: &User,
    organizer_addresses: &[String],
    attendee_addresses: &[String],
    uid: &str,
    source: &ReplySource,
    forced: bool,
) -> Result<ReplyEffect, Failure> {
    let Some((_, tree)) = organizer_copy(state, organizer, organizer_addresses, uid).await? else {
        return Err(Failure::NoAuthority);
    };
    let Some((_, changed)) = merge(state, tree, source, attendee_addresses).await else {
        return Err(Failure::NoAuthority);
    };
    Ok(if changed.is_empty() && !forced {
        ReplyEffect::Unchanged
    } else {
        ReplyEffect::Changes
    })
}

/// Merges an attendee's reply into the organizer's copy (only their own
/// PARTSTAT, see [`itip::apply_replies`]) and notifies the organizer. The
/// organizer's Schedule-Tag stays (RFC 6638 §3.2.10).
#[allow(clippy::too_many_arguments)]
pub async fn reply(
    state: &AppState,
    scheduling: &Scheduling,
    organizer: &User,
    organizer_addresses: &[String],
    attendee_addresses: &[String],
    sender: &str,
    uid: &str,
    source: &ReplySource,
    forced: bool,
) -> Result<ReplyEffect, Failure> {
    let is_attendee = |v: &str| attendee_addresses.contains(&normalize(v));
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            backoff(attempt).await;
        }
        let Some((object, tree)) =
            organizer_copy(state, organizer, organizer_addresses, uid).await?
        else {
            return Err(Failure::NoAuthority);
        };
        let Some((tree, changed)) = merge(state, tree, source, attendee_addresses).await else {
            return Err(Failure::NoAuthority);
        };
        if changed.is_empty() {
            if forced {
                let all: Vec<usize> = (0..tree.components.len())
                    .filter(|&i| tree.components[i].name != "VTIMEZONE")
                    .collect();
                let message =
                    serialize(&itip::reply_message(&tree, &all, &is_attendee, &dtstamp()))?;
                to_inbox(state, scheduling, organizer, uid, sender, &message).await;
                return Ok(ReplyEffect::Changes);
            }
            return Ok(ReplyEffect::Unchanged);
        }
        match store_tree(
            state,
            &object.collection,
            &object.name,
            &tree,
            &Precondition::IfMatch(vec![object.etag.clone()]),
            object.schedule_tag.as_deref(),
            None,
        )
        .await
        {
            Ok(()) => {
                let message = serialize(&itip::reply_message(
                    &tree,
                    &changed,
                    &is_attendee,
                    &dtstamp(),
                ))?;
                to_inbox(state, scheduling, organizer, uid, sender, &message).await;
                return Ok(ReplyEffect::Changes);
            }
            Err(StoreError::PreconditionFailed) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(Failure::NotDelivered)
}

/// Sets `SCHEDULE-STATUS` on the properties `kind` (ATTENDEE or ORGANIZER)
/// matching the given addresses (`None`: every one) in an object stored
/// under Schedule-Tag `tag`, keeping the tag: the organizer's copy after
/// deliveries failed, the attendee's copy after a reply failed. Best effort:
/// if the object changed meanwhile under another tag, it is left alone.
pub async fn record_statuses(
    state: &AppState,
    collection: &Collection,
    name: &str,
    tag: &str,
    kind: &str,
    statuses: &[(Option<String>, &'static str)],
) {
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            backoff(attempt).await;
        }
        let Ok(Some((item, Some(current)))) = state
            .store
            .item_with_schedule_tag(collection.id, name)
            .await
        else {
            return;
        };
        if current != tag {
            return;
        }
        let Ok(mut tree) = parse(&item.content) else {
            return;
        };
        for (address, status) in statuses {
            itip::set_parameter(
                &mut tree,
                kind,
                &|v: &str| address.as_ref().is_none_or(|a| normalize(v) == *a),
                "SCHEDULE-STATUS",
                Some(status),
            );
        }
        match store_tree(
            state,
            collection,
            name,
            &tree,
            &Precondition::IfMatch(vec![item.etag]),
            Some(tag),
            None,
        )
        .await
        {
            Err(StoreError::PreconditionFailed) => continue,
            Err(err) => {
                tracing::warn!(%err, "could not record scheduling status");
                return;
            }
            Ok(()) => return,
        }
    }
}
