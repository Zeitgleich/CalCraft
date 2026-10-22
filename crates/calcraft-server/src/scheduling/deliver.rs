//! Delivery of iTIP messages between users of this server: the
//! recipient's calendar copy is created or updated (REQUEST, CANCEL) or the
//! organizer's copy gets the reply (REPLY), and a copy of the message goes
//! into the recipient's scheduling inbox.
//!
//! Every change of an existing object is a read–modify–write guarded by
//! its entity tag and retried on conflicts, so concurrent replies of
//! several attendees never overwrite each other.

use calcraft_ical::{Component, INDEX_VERSION, itip, parse, write::to_string};
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
    /// Quota, object limits, an invalid result or a storage error (5.1).
    NotDelivered,
    /// The recipient has no writable calendar for the component (5.3).
    NoCalendar,
    /// The recipient's object with this UID belongs to another organizer,
    /// or the organizer has no such event with this attendee (3.8 for
    /// replies, 5.1 for invitations so that nothing is revealed).
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

/// Stores `tree` as object `name` of `collection` (validated like a PUT).
async fn store_tree(
    state: &AppState,
    collection: &Collection,
    name: &str,
    tree: &Component,
    precondition: &Precondition,
    tag: Option<&str>,
) -> Result<(), StoreError> {
    let text = to_string(tree).map_err(|_| StoreError::Limit("invalid scheduling object"))?;
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
            },
        )
        .await?;
    Ok(())
}

/// Puts a message into the recipient's inbox; failures (quota) only cost
/// the notification, the calendar is already up to date.
async fn to_inbox(
    state: &AppState,
    scheduling: &Scheduling,
    recipient: &User,
    uid: &str,
    sender: &str,
    message: &Component,
) {
    let Ok(text) = to_string(message) else {
        return;
    };
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

/// Delivers an invitation (or an update of it) to a user of this server:
/// `projection` is what the recipient sees of the organizer's object.
pub async fn request(
    state: &AppState,
    scheduling: &Scheduling,
    organizer_addresses: &[String],
    sender: &str,
    recipient: &User,
    projection: &Component,
) -> Result<(), Failure> {
    let uid = itip::uid(projection)
        .ok_or(Failure::NotDelivered)?
        .to_owned();
    for _ in 0..ATTEMPTS {
        let existing = state.store.scheduling_object(recipient.id, &uid).await?;
        let result = match existing {
            Some(object) => {
                let old = parse(&object.content).ok();
                // Another organizer's (or the recipient's own) object with
                // this UID is never touched.
                if !old
                    .as_ref()
                    .is_some_and(|old| organized_by(old, organizer_addresses))
                {
                    return Err(Failure::NoAuthority);
                }
                let copy = itip::attendee_copy(projection, old.as_ref());
                store_tree(
                    state,
                    &object.collection,
                    &object.name,
                    &copy,
                    &Precondition::IfMatch(vec![object.etag.clone()]),
                    Some(&new_schedule_tag()),
                )
                .await
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
                store_tree(
                    state,
                    &calendar,
                    &new_resource_name(),
                    projection,
                    &Precondition::MustNotExist,
                    Some(&new_schedule_tag()),
                )
                .await
            }
        };
        match result {
            Ok(()) => {
                let message = itip::message("REQUEST", projection, &dtstamp());
                to_inbox(state, scheduling, recipient, &uid, sender, &message).await;
                return Ok(());
            }
            // Changed or created concurrently: read again.
            Err(StoreError::PreconditionFailed | StoreError::UidConflict(_)) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(Failure::NotDelivered)
}

/// Delivers a cancellation: the recipient's copy is kept, marked
/// `STATUS:CANCELLED` (RFC 5546 §3.2.5), so that they see it was called off.
pub async fn cancel(
    state: &AppState,
    scheduling: &Scheduling,
    organizer_addresses: &[String],
    sender: &str,
    recipient: &User,
    projection: &Component,
) -> Result<(), Failure> {
    let uid = itip::uid(projection)
        .ok_or(Failure::NotDelivered)?
        .to_owned();
    for _ in 0..ATTEMPTS {
        let Some(object) = state.store.scheduling_object(recipient.id, &uid).await? else {
            // Nothing to cancel (already deleted by the attendee).
            break;
        };
        let Some(old) = parse(&object.content).ok() else {
            return Err(Failure::NotDelivered);
        };
        if !organized_by(&old, organizer_addresses) {
            return Err(Failure::NoAuthority);
        }
        if itip::is_cancelled(&old) {
            break;
        }
        match store_tree(
            state,
            &object.collection,
            &object.name,
            &itip::cancelled(&old),
            &Precondition::IfMatch(vec![object.etag.clone()]),
            Some(&new_schedule_tag()),
        )
        .await
        {
            Ok(()) => break,
            Err(StoreError::PreconditionFailed) => {}
            Err(err) => return Err(err.into()),
        }
    }
    let message = itip::message("CANCEL", &itip::cancelled(projection), &dtstamp());
    to_inbox(state, scheduling, recipient, &uid, sender, &message).await;
    Ok(())
}

/// What an attendee replies with.
#[derive(Debug, Clone, Copy)]
pub enum ReplySource<'a> {
    /// The participation statuses in the attendee's copy.
    Copy(&'a Component),
    /// The attendee deleted their copy: everything declined.
    DeclineAll,
}

impl ReplySource<'_> {
    fn replies(
        self,
        organizer_copy: &Component,
        is_attendee: &dyn Fn(&str) -> bool,
    ) -> Vec<itip::Reply> {
        match self {
            Self::Copy(copy) => itip::replies(copy, organizer_copy, is_attendee),
            Self::DeclineAll => itip::decline_all(organizer_copy),
        }
    }
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

/// Checks a reply without changing anything (to set `SCHEDULE-STATUS` in
/// the attendee's copy before it is stored).
pub async fn check_reply(
    state: &AppState,
    organizer: &User,
    organizer_addresses: &[String],
    attendee_addresses: &[String],
    uid: &str,
    source: ReplySource<'_>,
    forced: bool,
) -> Result<ReplyEffect, Failure> {
    let is_attendee = |v: &str| attendee_addresses.contains(&normalize(v));
    let Some((_, mut tree)) = organizer_copy(state, organizer, organizer_addresses, uid).await?
    else {
        return Err(Failure::NoAuthority);
    };
    if itip::project(&tree, &is_attendee).is_none() {
        return Err(Failure::NoAuthority);
    }
    let replies = source.replies(&tree, &is_attendee);
    let changed = itip::apply_replies(&mut tree, &is_attendee, &replies);
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
    source: ReplySource<'_>,
    forced: bool,
) -> Result<ReplyEffect, Failure> {
    let is_attendee = |v: &str| attendee_addresses.contains(&normalize(v));
    for _ in 0..ATTEMPTS {
        let Some((object, mut tree)) =
            organizer_copy(state, organizer, organizer_addresses, uid).await?
        else {
            return Err(Failure::NoAuthority);
        };
        if itip::project(&tree, &is_attendee).is_none() {
            return Err(Failure::NoAuthority);
        }
        let replies = source.replies(&tree, &is_attendee);
        let changed = itip::apply_replies(&mut tree, &is_attendee, &replies);
        if changed.is_empty() {
            if forced {
                let all: Vec<usize> = (0..tree.components.len())
                    .filter(|&i| tree.components[i].name != "VTIMEZONE")
                    .collect();
                let message = itip::reply_message(&tree, &all, &is_attendee, &dtstamp());
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
        )
        .await
        {
            Ok(()) => {
                let message = itip::reply_message(&tree, &changed, &is_attendee, &dtstamp());
                to_inbox(state, scheduling, organizer, uid, sender, &message).await;
                return Ok(ReplyEffect::Changes);
            }
            Err(StoreError::PreconditionFailed) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(Failure::NotDelivered)
}

/// Sets `SCHEDULE-STATUS` of the given attendees in the organizer's copy
/// after deliveries failed, keeping its Schedule-Tag. Best effort: if the
/// object changed meanwhile under another tag, it is left alone.
pub async fn record_statuses(
    state: &AppState,
    collection: &Collection,
    name: &str,
    tag: &str,
    failed: &[(String, &'static str)],
) {
    for _ in 0..ATTEMPTS {
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
        for (address, status) in failed {
            itip::set_parameter(
                &mut tree,
                "ATTENDEE",
                &|v: &str| normalize(v) == *address,
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
