//! CalDAV scheduling (RFC 6638) on the DAV side: implicit scheduling on
//! PUT and DELETE of calendar objects, the scheduling inbox (PROPFIND,
//! GET, DELETE, REPORT; never PUT) and free/busy requests POSTed to the
//! outbox. Only reached when `[scheduling] enabled`; the delivery logic
//! lives in [`crate::scheduling`].

// Several helpers return a complete HTTP response as their error; it is
// built once per request and returned right away.
#![allow(clippy::result_large_err)]

use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CONTENT_TYPE, ETAG},
    },
    response::{IntoResponse, Response},
};
use calcraft_core::CollectionKind;
use calcraft_dav::{
    DavError, Element, MAX_REPORT_BODY, Multistatus, PropName, PropValue, Propfind, Report,
    Response as DavResponse,
    xml::{NS_CALDAV, NS_CALENDARSERVER, NS_DAV, escape_text},
};
use calcraft_ical::{
    Component, INDEX_VERSION,
    itip::{self, Agent, status},
    parse,
    write::to_string,
};
use calcraft_store::{
    Collection, NewItem, Precondition, PutOutcome, ScheduleTag, StoreError, User,
};

use super::{
    Ctx, bad_request, dav_error, multistatus, not_found,
    objects::{header, ns},
    paths::{
        Resource, home_href, inbox_href, item_href, item_name_from_href, outbox_href,
        principal_href,
    },
    props::{CALENDAR_CONTENT_TYPE, respond},
    read_body, store_error,
};
use crate::{
    auth::now,
    scheduling::{
        Resolution, Scheduling, default_calendar,
        deliver::{self, ReplyEffect, ReplySource},
        freebusy, new_schedule_tag, normalize,
    },
};

/// Header carrying the Schedule-Tag (RFC 6638 §8.2).
pub const SCHEDULE_TAG: &str = "schedule-tag";
/// Conditional header for it (RFC 6638 §8.3).
const IF_SCHEDULE_TAG_MATCH: &str = "if-schedule-tag-match";
/// Most messages listed or returned per inbox request.
const MAX_INBOX_LISTING: u32 = 5_000;
/// Most recipients of one free/busy request.
const MAX_FREEBUSY_RECIPIENTS: usize = 100;
/// Largest free/busy request body.
const MAX_POST_BODY: usize = 64 * 1024;
/// Attempts of a PUT or DELETE when the object changes concurrently.
const ATTEMPTS: usize = 3;

/// `If-Schedule-Tag-Match`: `Ok(None)` without the header, `Err` if it
/// is malformed.
pub(super) fn if_schedule_tag(headers: &HeaderMap) -> Result<Option<String>, Response> {
    match header(headers, IF_SCHEDULE_TAG_MATCH) {
        None => Ok(None),
        Some(tag)
            if tag.len() >= 2 && tag.len() <= 128 && tag.starts_with('"') && tag.ends_with('"') =>
        {
            Ok(Some(tag.to_owned()))
        }
        Some(_) => {
            Err((StatusCode::BAD_REQUEST, "malformed If-Schedule-Tag-Match").into_response())
        }
    }
}

/// The role of the authenticated user in an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Their own address is the ORGANIZER and there are other attendees.
    Organizer,
    /// Somebody else organizes and they are an attendee.
    Attendee,
    /// Not a scheduling object for this user.
    None,
}

fn role(tree: &Component, is_me: &dyn Fn(&str) -> bool) -> Result<Role, Response> {
    let organizer = itip::organizer(tree).map_err(|_| {
        dav_error(
            StatusCode::FORBIDDEN,
            "<c:same-organizer-in-all-components/>",
        )
    })?;
    let Some(organizer) = organizer else {
        return Ok(Role::None);
    };
    Ok(if is_me(&organizer.value) {
        if itip::attendees(tree).any(|a| !is_me(&a.value)) {
            Role::Organizer
        } else {
            Role::None
        }
    } else if itip::attendees(tree).any(|a| is_me(&a.value)) {
        Role::Attendee
    } else {
        Role::None
    })
}

/// Result of one attempt: a response, or "the object changed, try again".
enum Attempt {
    Done(Response),
    Retry,
}

/// Stores a scheduling object built by the server. `Err(Retry)` if the
/// object changed since it was read.
async fn store_object(
    ctx: Ctx<'_>,
    collection: &Collection,
    name: &str,
    tree: &Component,
    guard: &Precondition,
    tag: &str,
) -> Result<(PutOutcome, String), Attempt> {
    let calendar = collection.kind == CollectionKind::Calendar;
    let text = to_string(tree)
        .map_err(|_| Attempt::Done(dav_error(StatusCode::FORBIDDEN, "<c:valid-calendar-data/>")))?;
    let body = text.into_bytes();
    let validated =
        super::check_object(collection, &body, ctx.state.max_vcard_bytes).map_err(|rejection| {
            let condition = match rejection {
                super::Rejection::TooLarge => "<c:max-resource-size/>",
                super::Rejection::UnsupportedComponent => "<c:supported-calendar-component/>",
                super::Rejection::Invalid(_) if calendar => "<c:valid-calendar-data/>",
                super::Rejection::Invalid(_) => "<card:valid-address-data/>",
            };
            Attempt::Done(dav_error(StatusCode::FORBIDDEN, condition))
        })?;
    let etag = super::etag(&body);
    let item = NewItem {
        uid: &validated.uid,
        component: validated.component,
        range_start: validated.range_start,
        range_end: validated.range_end,
        recurring: validated.recurring,
        index_version: INDEX_VERSION,
        etag: &etag,
        content: &body,
    };
    match ctx
        .state
        .store
        .put_item_scheduled(
            collection.id,
            name,
            &item,
            guard,
            &ScheduleTag {
                set: Some(tag),
                if_match: None,
            },
        )
        .await
    {
        Ok(outcome) => Ok((outcome, etag)),
        Err(StoreError::PreconditionFailed) => Err(Attempt::Retry),
        Err(err) => Err(Attempt::Done(uid_conflict_or(ctx, collection, err))),
    }
}

fn uid_conflict_or(ctx: Ctx<'_>, collection: &Collection, err: StoreError) -> Response {
    match err {
        StoreError::UidConflict(other) => {
            let href = item_href(collection, &ctx.user.username, &other);
            let prefix = ns(collection.kind);
            dav_error(
                StatusCode::FORBIDDEN,
                &format!(
                    "<{prefix}:no-uid-conflict><d:href>{href}</d:href></{prefix}:no-uid-conflict>"
                ),
            )
        }
        err => store_error(err),
    }
}

fn put_response(outcome: PutOutcome, etag: Option<&str>, tag: &str) -> Response {
    let status = match outcome {
        PutOutcome::Created => StatusCode::CREATED,
        PutOutcome::Updated => StatusCode::NO_CONTENT,
    };
    let mut response = status.into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(tag) {
        headers.insert(SCHEDULE_TAG, value);
    }
    // The server changed the object (SCHEDULE-STATUS & co.): no ETag
    // (RFC 4791 §5.3.4), the client reads it again.
    if let Some(etag) = etag
        && let Ok(value) = HeaderValue::from_str(etag)
    {
        headers.insert(ETAG, value);
    }
    response
}

/// PUT of a calendar object with scheduling enabled. Objects that are not
/// scheduling objects for the user are stored exactly as sent.
#[allow(clippy::too_many_arguments)]
pub(super) async fn put(
    ctx: Ctx<'_>,
    scheduling: &Scheduling,
    collection: &Collection,
    name: &str,
    body: Bytes,
    item: NewItem<'_>,
    precondition: Precondition,
    if_tag: Option<String>,
) -> Response {
    let store = &ctx.state.store;
    let user = ctx.user;
    let tree = match parse(&body) {
        Ok(tree) => tree,
        Err(_) => return dav_error(StatusCode::FORBIDDEN, "<c:valid-calendar-data/>"),
    };
    let me = match scheduling.addresses(store, user).await {
        Ok(me) => me,
        Err(err) => return store_error(err),
    };
    let is_me = |v: &str| me.contains(&normalize(v));
    let mut role = match role(&tree, &is_me) {
        Ok(role) => role,
        Err(response) => return response,
    };
    // An organizer who removes every attendee (or the ORGANIZER) still
    // cancels the invitations the server sent for this object.
    if role == Role::None {
        match store.item_with_schedule_tag(collection.id, name).await {
            Ok(Some((old, Some(_))))
                if parse(&old.content)
                    .ok()
                    .is_some_and(|old| matches!(self::role(&old, &is_me), Ok(Role::Organizer))) =>
            {
                role = Role::Organizer;
            }
            Ok(_) => {}
            Err(err) => return store_error(err),
        }
    }
    if role == Role::None {
        let tags = ScheduleTag {
            set: None,
            if_match: if_tag.as_deref(),
        };
        return match store
            .put_item_scheduled(collection.id, name, &item, &precondition, &tags)
            .await
        {
            Ok(outcome) => {
                let status = match outcome {
                    PutOutcome::Created => StatusCode::CREATED,
                    PutOutcome::Updated => StatusCode::NO_CONTENT,
                };
                (status, [(ETAG, item.etag)]).into_response()
            }
            Err(err) => uid_conflict_or(ctx, collection, err),
        };
    }
    let uid = item.uid;
    for _ in 0..ATTEMPTS {
        let old = match store.item_with_schedule_tag(collection.id, name).await {
            Ok(old) => old,
            Err(err) => return store_error(err),
        };
        if !precondition.holds(old.as_ref().map(|(item, _)| item.etag.as_str())) {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
        if let Some(tag) = &if_tag
            && old.as_ref().and_then(|(_, t)| t.as_deref()) != Some(tag.as_str())
        {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
        // One scheduling object per UID and user (RFC 6638 §3.2.4.1).
        match store.scheduling_object(user.id, uid).await {
            Ok(Some(other))
                if other.schedule_tag.is_some()
                    && (other.collection.id != collection.id || other.name != name) =>
            {
                let href = item_href(&other.collection, &user.username, &other.name);
                return dav_error(
                    StatusCode::FORBIDDEN,
                    &format!(
                        "<c:unique-scheduling-object-resource><d:href>{href}</d:href></c:unique-scheduling-object-resource>"
                    ),
                );
            }
            Ok(_) => {}
            Err(err) => return store_error(err),
        }
        let guard = match &old {
            Some((item, _)) => Precondition::IfMatch(vec![item.etag.clone()]),
            None => Precondition::MustNotExist,
        };
        let old_tree = old.as_ref().and_then(|(item, _)| parse(&item.content).ok());
        let attempt = match role {
            Role::Organizer => {
                organizer_put(
                    ctx,
                    scheduling,
                    collection,
                    name,
                    tree.clone(),
                    old_tree.as_ref(),
                    &guard,
                    &me,
                )
                .await
            }
            _ => {
                attendee_put(
                    ctx,
                    scheduling,
                    collection,
                    name,
                    tree.clone(),
                    old_tree.as_ref(),
                    &guard,
                    &me,
                )
                .await
            }
        };
        match attempt {
            Attempt::Done(response) => return response,
            Attempt::Retry => continue,
        }
    }
    StatusCode::PRECONDITION_FAILED.into_response()
}

/// A message for one attendee.
enum Action {
    Request(Component),
    Cancel(Component),
}

struct Recipient {
    /// Normalised address.
    address: String,
    action: Action,
    resolution: Resolution,
    /// Within the sender's rate limit.
    allowed: bool,
}

/// Distinct normalised attendee addresses of an object, without the user's.
fn other_attendees(tree: &Component, is_me: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for attendee in itip::attendees(tree) {
        let address = normalize(&attendee.value);
        if !is_me(&attendee.value) && !out.contains(&address) {
            out.push(address);
        }
    }
    out
}

/// The `SCHEDULE-AGENT` of the attendee with this normalised address.
fn attendee_agent(tree: &Component, address: &str) -> Option<Agent> {
    itip::attendees(tree)
        .find(|a| normalize(&a.value) == address)
        .map(itip::agent)
}

/// Works out the messages of an organizer's change: an invitation (or
/// update) for every attendee whose view changed or who is new, a
/// cancellation for every attendee removed.
async fn recipients(
    ctx: Ctx<'_>,
    scheduling: &Scheduling,
    tree: &Component,
    old: Option<&Component>,
    is_me: &(dyn Fn(&str) -> bool + Sync),
) -> Result<Vec<Recipient>, StoreError> {
    let mut addresses = other_attendees(tree, is_me);
    if let Some(old) = old {
        for address in other_attendees(old, is_me) {
            if !addresses.contains(&address) {
                addresses.push(address);
            }
        }
    }
    let mut out = Vec::new();
    for address in addresses {
        let agent = attendee_agent(tree, &address)
            .or_else(|| old.and_then(|old| attendee_agent(old, &address)));
        if agent != Some(Agent::Server) {
            continue;
        }
        let matches = |v: &str| normalize(v) == address;
        let new_view = itip::project(tree, &matches);
        let old_view = old.and_then(|old| itip::project(old, &matches));
        let forced = itip::force_send(tree, "ATTENDEE", &matches, "REQUEST");
        let action = match (new_view, old_view) {
            (Some(new), None) => Action::Request(new),
            (Some(new), Some(old)) if forced || !itip::same_for_attendee(&new, &old) => {
                Action::Request(new)
            }
            (None, Some(old)) => Action::Cancel(old),
            _ => continue,
        };
        let resolution = scheduling.resolve(&ctx.state.store, &address).await?;
        if matches!(&resolution, Resolution::Local(u) if u.id == ctx.user.id) {
            continue;
        }
        out.push(Recipient {
            address,
            action,
            resolution,
            allowed: false,
        });
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn organizer_put(
    ctx: Ctx<'_>,
    scheduling: &Scheduling,
    collection: &Collection,
    name: &str,
    mut tree: Component,
    old: Option<&Component>,
    guard: &Precondition,
    me: &[String],
) -> Attempt {
    let is_me = |v: &str| me.contains(&normalize(v));
    let max = usize::try_from(scheduling.config.max_attendees).unwrap_or(usize::MAX);
    if other_attendees(&tree, &is_me).len() > max {
        return Attempt::Done(dav_error(
            StatusCode::FORBIDDEN,
            "<c:max-attendees-per-instance/>",
        ));
    }
    let mut recipients = match recipients(ctx, scheduling, &tree, old, &is_me).await {
        Ok(recipients) => recipients,
        Err(err) => return Attempt::Done(store_error(err)),
    };
    let local = recipients
        .iter()
        .filter(|r| matches!(r.resolution, Resolution::Local(_)))
        .count();
    let mut granted = scheduling.allow(ctx.user.id, u32::try_from(local).unwrap_or(u32::MAX));
    for recipient in &mut recipients {
        if matches!(recipient.resolution, Resolution::Local(_)) && granted > 0 {
            recipient.allowed = true;
            granted -= 1;
        }
    }

    itip::strip_requests(&mut tree);
    // Clients only manage SCHEDULE-STATUS of attendees they schedule.
    itip::restore_statuses(&mut tree, old, &|p| {
        p.name == "ATTENDEE" && itip::agent(p) == Agent::Client
    });
    if let Some(old) = old {
        let local_attendee = |v: &str| {
            let address = normalize(v);
            recipients
                .iter()
                .any(|r| r.address == address && matches!(r.resolution, Resolution::Local(_)))
                || (!is_me(v)
                    && attendee_agent(old, &address) == Some(Agent::Server)
                    && !recipients.iter().any(|r| r.address == address))
        };
        let rescheduled = |v: &str| {
            let address = normalize(v);
            recipients
                .iter()
                .any(|r| r.address == address && matches!(r.action, Action::Request(_)))
        };
        itip::preserve_partstats(&mut tree, old, &local_attendee, &rescheduled);
    }
    for recipient in &recipients {
        if !matches!(recipient.action, Action::Request(_)) {
            continue;
        }
        let status = match (&recipient.resolution, recipient.allowed) {
            (Resolution::Local(_), true) => status::DELIVERED,
            (Resolution::Local(_), false) => status::NOT_DELIVERED,
            (Resolution::Unknown, _) => status::INVALID_USER,
            (Resolution::External, _) => status::NO_SCHEDULING,
        };
        let address = &recipient.address;
        itip::set_parameter(
            &mut tree,
            "ATTENDEE",
            &|v: &str| normalize(v) == *address,
            "SCHEDULE-STATUS",
            Some(status),
        );
    }

    let tag = new_schedule_tag();
    let outcome = match store_object(ctx, collection, name, &tree, guard, &tag).await {
        Ok((outcome, _)) => outcome,
        Err(attempt) => return attempt,
    };

    let sender = itip::organizer(&tree)
        .ok()
        .flatten()
        .or_else(|| old.and_then(|old| itip::organizer(old).ok().flatten()))
        .map(|o| normalize(&o.value))
        .unwrap_or_default();
    let mut failed = Vec::new();
    let mut delivered = 0usize;
    for recipient in &recipients {
        let Resolution::Local(user) = &recipient.resolution else {
            continue;
        };
        if !recipient.allowed {
            continue;
        }
        let result = match &recipient.action {
            Action::Request(view) => {
                deliver::request(ctx.state, scheduling, me, &sender, user, view).await
            }
            Action::Cancel(view) => {
                deliver::cancel(ctx.state, scheduling, me, &sender, user, view).await
            }
        };
        match result {
            Ok(()) => delivered += 1,
            Err(failure) => {
                if matches!(recipient.action, Action::Request(_)) {
                    let status = match failure {
                        deliver::Failure::NoAuthority => status::NOT_DELIVERED,
                        failure => failure.status(),
                    };
                    failed.push((recipient.address.clone(), status));
                }
            }
        }
    }
    if !failed.is_empty() {
        deliver::record_statuses(ctx.state, collection, name, &tag, &failed).await;
    }
    tracing::info!(
        user = %ctx.user.username,
        delivered,
        failed = failed.len(),
        not_local = recipients.iter().filter(|r| !matches!(r.resolution, Resolution::Local(_))).count(),
        "scheduling messages sent"
    );
    Attempt::Done(put_response(outcome, None, &tag))
}

/// The user's own participation statuses in an object (per component) and
/// the master's EXDATEs, to see whether they changed.
fn own_statuses(tree: &Component, is_me: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = tree
        .components
        .iter()
        .filter(|c| c.name != "VTIMEZONE")
        .flat_map(|c| {
            let rid = c
                .property("RECURRENCE-ID")
                .map(|p| p.value.clone())
                .unwrap_or_default();
            let mut entries: Vec<String> = itip::partstat(c, is_me)
                .map(|p| format!("{rid}={p}"))
                .into_iter()
                .collect();
            if c.property("RECURRENCE-ID").is_none() {
                entries.extend(
                    c.properties_named("EXDATE")
                        .map(|p| format!("EXDATE:{}", p.value)),
                );
            }
            entries
        })
        .collect();
    out.sort();
    out
}

#[allow(clippy::too_many_arguments)]
async fn attendee_put(
    ctx: Ctx<'_>,
    scheduling: &Scheduling,
    collection: &Collection,
    name: &str,
    mut tree: Component,
    old: Option<&Component>,
    guard: &Precondition,
    me: &[String],
) -> Attempt {
    let is_me = |v: &str| me.contains(&normalize(v));
    let Ok(Some(organizer)) = itip::organizer(&tree).map(|o| o.cloned()) else {
        return Attempt::Done(StatusCode::INTERNAL_SERVER_ERROR.into_response());
    };
    let forced = itip::force_send(&tree, "ORGANIZER", &|_| true, "REPLY");
    itip::strip_requests(&mut tree);
    itip::restore_statuses(&mut tree, old, &|_| false);
    let changed = old.is_none_or(|old| own_statuses(old, &is_me) != own_statuses(&tree, &is_me));
    let uid = itip::uid(&tree).unwrap_or_default().to_owned();

    let mut plan = None;
    if itip::agent(&organizer) == Agent::Server && (changed || forced) {
        let address = normalize(&organizer.value);
        let resolution = match scheduling.resolve(&ctx.state.store, &address).await {
            Ok(resolution) => resolution,
            Err(err) => return Attempt::Done(store_error(err)),
        };
        let status = match resolution {
            Resolution::Local(organizer_user) if organizer_user.id != ctx.user.id => {
                let addresses = match scheduling
                    .addresses(&ctx.state.store, &organizer_user)
                    .await
                {
                    Ok(addresses) => addresses,
                    Err(err) => return Attempt::Done(store_error(err)),
                };
                match deliver::check_reply(
                    ctx.state,
                    &organizer_user,
                    &addresses,
                    me,
                    &uid,
                    ReplySource::Copy(&tree),
                    forced,
                )
                .await
                {
                    Ok(ReplyEffect::Changes) if scheduling.allow(ctx.user.id, 1) == 1 => {
                        plan = Some((organizer_user, addresses));
                        Some(status::DELIVERED)
                    }
                    Ok(ReplyEffect::Changes) => Some(status::NOT_DELIVERED),
                    Ok(ReplyEffect::Unchanged) => None,
                    Err(failure) => Some(failure.status()),
                }
            }
            Resolution::Local(_) => None,
            Resolution::Unknown => Some(status::INVALID_USER),
            Resolution::External => Some(status::NO_SCHEDULING),
        };
        if let Some(status) = status {
            itip::set_parameter(
                &mut tree,
                "ORGANIZER",
                &|_| true,
                "SCHEDULE-STATUS",
                Some(status),
            );
        }
    }

    let tag = new_schedule_tag();
    let outcome = match store_object(ctx, collection, name, &tree, guard, &tag).await {
        Ok((outcome, _)) => outcome,
        Err(attempt) => return attempt,
    };
    if let Some((organizer_user, addresses)) = plan {
        let sender = me.first().cloned().unwrap_or_default();
        let result = deliver::reply(
            ctx.state,
            scheduling,
            &organizer_user,
            &addresses,
            me,
            &sender,
            &uid,
            ReplySource::Copy(&tree),
            forced,
        )
        .await;
        tracing::info!(
            user = %ctx.user.username,
            delivered = result.is_ok(),
            "scheduling reply sent"
        );
    }
    Attempt::Done(put_response(outcome, None, &tag))
}

/// DELETE of a calendar object with scheduling enabled: into the trash as
/// usual, then a cancellation to the attendees (organizer) or a decline to
/// the organizer (attendee) – only for objects the server scheduled (with
/// a Schedule-Tag), never for imported ones.
pub(super) async fn delete(
    ctx: Ctx<'_>,
    scheduling: &Scheduling,
    collection: &Collection,
    name: &str,
    precondition: Precondition,
    if_tag: Option<String>,
) -> Response {
    let store = &ctx.state.store;
    for _ in 0..ATTEMPTS {
        let (item, tag) = match store.item_with_schedule_tag(collection.id, name).await {
            Ok(Some(found)) => found,
            Ok(None) => return not_found(),
            Err(err) => return store_error(err),
        };
        if !precondition.holds(Some(&item.etag)) {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
        if let Some(expected) = &if_tag
            && tag.as_deref() != Some(expected.as_str())
        {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
        let kind = collection.kind;
        match store
            .delete_item_to_trash(
                collection.id,
                name,
                &Precondition::IfMatch(vec![item.etag.clone()]),
                now(),
                |content| crate::trash::summary(kind, content),
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => return not_found(),
            Err(StoreError::PreconditionFailed) => continue,
            Err(err) => return store_error(err),
        }
        if tag.is_some()
            && let Ok(tree) = parse(&item.content)
        {
            after_delete(ctx, scheduling, &tree).await;
        }
        return StatusCode::NO_CONTENT.into_response();
    }
    StatusCode::PRECONDITION_FAILED.into_response()
}

async fn after_delete(ctx: Ctx<'_>, scheduling: &Scheduling, tree: &Component) {
    let store = &ctx.state.store;
    let Ok(me) = scheduling.addresses(store, ctx.user).await else {
        return;
    };
    let is_me = |v: &str| me.contains(&normalize(v));
    let Ok(role) = role(tree, &is_me) else { return };
    let Ok(Some(organizer)) = itip::organizer(tree) else {
        return;
    };
    let sender = normalize(&organizer.value);
    match role {
        Role::Organizer => {
            let Ok(recipients) =
                recipients(ctx, scheduling, &Component::default(), Some(tree), &is_me).await
            else {
                return;
            };
            let local: Vec<&Recipient> = recipients
                .iter()
                .filter(|r| matches!(r.resolution, Resolution::Local(_)))
                .collect();
            let granted =
                scheduling.allow(ctx.user.id, u32::try_from(local.len()).unwrap_or(u32::MAX));
            let mut delivered = 0usize;
            for recipient in local
                .into_iter()
                .take(usize::try_from(granted).unwrap_or(0))
            {
                let (Resolution::Local(user), Action::Cancel(view)) =
                    (&recipient.resolution, &recipient.action)
                else {
                    continue;
                };
                if deliver::cancel(ctx.state, scheduling, &me, &sender, user, view)
                    .await
                    .is_ok()
                {
                    delivered += 1;
                }
            }
            tracing::info!(user = %ctx.user.username, delivered, "scheduling cancellations sent");
        }
        Role::Attendee => {
            if itip::is_cancelled(tree) || itip::agent(organizer) != Agent::Server {
                return;
            }
            let Ok(Resolution::Local(organizer_user)) = scheduling.resolve(store, &sender).await
            else {
                return;
            };
            if organizer_user.id == ctx.user.id || scheduling.allow(ctx.user.id, 1) == 0 {
                return;
            }
            let Ok(addresses) = scheduling.addresses(store, &organizer_user).await else {
                return;
            };
            let uid = itip::uid(tree).unwrap_or_default().to_owned();
            let own = me.first().cloned().unwrap_or_default();
            let result = deliver::reply(
                ctx.state,
                scheduling,
                &organizer_user,
                &addresses,
                &me,
                &own,
                &uid,
                ReplySource::DeclineAll,
                false,
            )
            .await;
            tracing::info!(user = %ctx.user.username, delivered = result.is_ok(), "scheduling reply sent");
        }
        Role::None => {}
    }
}

// --- properties ---------------------------------------------------------------

fn prop(namespace: &str, name: &str, value: PropValue) -> (PropName, PropValue) {
    (PropName::new(namespace, name), value)
}

/// Privilege elements: DAV ones and CalDAV scheduling ones (RFC 6638 §6).
fn privileges(dav: &[&'static str], caldav: &[&'static str]) -> PropValue {
    let wrap = |ns: &'static str, name: &'static str| {
        Element::new(NS_DAV, "privilege").child(Element::new(ns, name))
    };
    PropValue::Elements(
        dav.iter()
            .map(|p| wrap(NS_DAV, p))
            .chain(caldav.iter().map(|p| wrap(NS_CALDAV, p)))
            .collect(),
    )
}

/// Scheduling properties of the user's principal (RFC 6638 §2.4).
pub(super) fn principal_properties(
    user: &User,
    addresses: &[String],
) -> Vec<(PropName, PropValue)> {
    vec![
        prop(
            NS_CALDAV,
            "calendar-user-address-set",
            PropValue::Hrefs(addresses.to_vec()),
        ),
        prop(
            NS_CALDAV,
            "calendar-user-type",
            PropValue::Text("INDIVIDUAL".to_owned()),
        ),
        prop(
            NS_CALDAV,
            "schedule-inbox-URL",
            PropValue::Href(inbox_href(&user.username)),
        ),
        prop(
            NS_CALDAV,
            "schedule-outbox-URL",
            PropValue::Href(outbox_href(&user.username)),
        ),
    ]
}

fn components(names: &[&'static str]) -> PropValue {
    PropValue::Elements(
        names
            .iter()
            .map(|n| Element::new(NS_CALDAV, "comp").attr("name", *n))
            .collect(),
    )
}

/// Live properties of the inbox. `default_calendar` is the href of
/// `schedule-default-calendar-URL`, `revision` its change counter.
pub(super) fn inbox_properties(
    user: &User,
    revision: i64,
    default_calendar: Option<String>,
) -> Vec<(PropName, PropValue)> {
    let mut props = vec![
        prop(
            NS_DAV,
            "current-user-principal",
            PropValue::Href(principal_href(&user.username)),
        ),
        prop(
            NS_DAV,
            "current-user-privilege-set",
            privileges(
                &["read", "unbind", "read-current-user-privilege-set"],
                &[
                    "schedule-deliver",
                    "schedule-deliver-invite",
                    "schedule-deliver-reply",
                    "schedule-query-freebusy",
                ],
            ),
        ),
        prop(
            NS_DAV,
            "resourcetype",
            PropValue::Elements(vec![
                Element::new(NS_DAV, "collection"),
                Element::new(NS_CALDAV, "schedule-inbox"),
            ]),
        ),
        prop(
            NS_DAV,
            "owner",
            PropValue::Href(principal_href(&user.username)),
        ),
        prop(NS_DAV, "displayname", PropValue::Text("Inbox".to_owned())),
        prop(
            NS_CALENDARSERVER,
            "getctag",
            PropValue::Text(revision.to_string()),
        ),
        prop(
            NS_CALDAV,
            "supported-calendar-component-set",
            components(&["VEVENT", "VTODO"]),
        ),
        prop(
            NS_DAV,
            "supported-report-set",
            PropValue::Elements(vec![
                Element::new(NS_DAV, "supported-report").child(
                    Element::new(NS_DAV, "report")
                        .child(Element::new(NS_CALDAV, "calendar-multiget")),
                ),
                Element::new(NS_DAV, "supported-report").child(
                    Element::new(NS_DAV, "report").child(Element::new(NS_CALDAV, "calendar-query")),
                ),
            ]),
        ),
    ];
    if let Some(href) = default_calendar {
        props.push(prop(
            NS_CALDAV,
            "schedule-default-calendar-URL",
            PropValue::Href(href),
        ));
    }
    props
}

/// Live properties of the outbox.
pub(super) fn outbox_properties(user: &User) -> Vec<(PropName, PropValue)> {
    vec![
        prop(
            NS_DAV,
            "current-user-principal",
            PropValue::Href(principal_href(&user.username)),
        ),
        prop(
            NS_DAV,
            "current-user-privilege-set",
            privileges(
                &["read", "read-current-user-privilege-set"],
                &[
                    "schedule-send",
                    "schedule-send-invite",
                    "schedule-send-reply",
                    "schedule-send-freebusy",
                ],
            ),
        ),
        prop(
            NS_DAV,
            "resourcetype",
            PropValue::Elements(vec![
                Element::new(NS_DAV, "collection"),
                Element::new(NS_CALDAV, "schedule-outbox"),
            ]),
        ),
        prop(
            NS_DAV,
            "owner",
            PropValue::Href(principal_href(&user.username)),
        ),
        prop(NS_DAV, "displayname", PropValue::Text("Outbox".to_owned())),
        prop(
            NS_CALDAV,
            "supported-calendar-component-set",
            components(&["VEVENT", "VTODO", "VFREEBUSY"]),
        ),
    ]
}

/// Properties of one inbox message.
fn message_properties(
    user: &User,
    etag: &str,
    size: i64,
    content: Option<&[u8]>,
) -> Vec<(PropName, PropValue)> {
    let mut props = vec![
        prop(
            NS_DAV,
            "current-user-principal",
            PropValue::Href(principal_href(&user.username)),
        ),
        prop(
            NS_DAV,
            "current-user-privilege-set",
            privileges(&["read", "unbind", "read-current-user-privilege-set"], &[]),
        ),
        prop(NS_DAV, "resourcetype", PropValue::Elements(Vec::new())),
        prop(NS_DAV, "getetag", PropValue::Text(etag.to_owned())),
        prop(
            NS_DAV,
            "getcontenttype",
            PropValue::Text(CALENDAR_CONTENT_TYPE.to_owned()),
        ),
        prop(
            NS_DAV,
            "getcontentlength",
            PropValue::Text(size.to_string()),
        ),
    ];
    if let Some(content) = content {
        props.push(prop(
            NS_CALDAV,
            "calendar-data",
            PropValue::Text(String::from_utf8_lossy(content).into_owned()),
        ));
    }
    props
}

/// The href of the user's default calendar for invitations.
pub(super) async fn default_calendar_href(ctx: Ctx<'_>) -> Result<Option<String>, StoreError> {
    Ok(default_calendar(&ctx.state.store, ctx.user.id, "VEVENT")
        .await?
        .map(|c| {
            format!(
                "{}{}/",
                home_href(CollectionKind::Calendar, &ctx.user.username),
                c.slug
            )
        }))
}

/// PROPFIND responses of the inbox (with its messages at depth 1) or one
/// message.
pub(super) async fn propfind(
    ctx: Ctx<'_>,
    resource: &Resource,
    query: &Propfind,
    depth_one: bool,
) -> Result<Vec<DavResponse>, Response> {
    let store = &ctx.state.store;
    let user = ctx.user;
    let base = inbox_href(&user.username);
    let mut responses = Vec::new();
    match resource {
        Resource::Inbox => {
            let revision = store.inbox_revision(user.id).await.map_err(store_error)?;
            let default = default_calendar_href(ctx).await.map_err(store_error)?;
            responses.push(respond(
                &base,
                &inbox_properties(user, revision, default),
                query,
            ));
            if depth_one {
                let messages = store
                    .inbox_messages(user.id, MAX_INBOX_LISTING)
                    .await
                    .map_err(store_error)?;
                for message in messages {
                    let props = message_properties(user, &message.etag, message.size, None);
                    responses.push(respond(&format!("{base}{}", message.name), &props, query));
                }
            }
        }
        Resource::Outbox => {
            responses.push(respond(
                &outbox_href(&user.username),
                &outbox_properties(user),
                query,
            ));
        }
        Resource::InboxItem(name) => {
            let Some(meta) = store
                .inbox_message_meta(user.id, name)
                .await
                .map_err(store_error)?
            else {
                return Err(not_found());
            };
            let props = message_properties(user, &meta.etag, meta.size, None);
            responses.push(respond(&format!("{base}{name}"), &props, query));
        }
        _ => return Err(not_found()),
    }
    Ok(responses)
}

/// GET/HEAD of an inbox message.
pub(super) async fn get_message(ctx: Ctx<'_>, name: &str, with_body: bool) -> Response {
    match ctx.state.store.inbox_message(ctx.user.id, name).await {
        Ok(Some(item)) => {
            let body = if with_body {
                Body::from(item.content)
            } else {
                Body::empty()
            };
            (
                StatusCode::OK,
                [
                    (CONTENT_TYPE, CALENDAR_CONTENT_TYPE),
                    (ETAG, item.etag.as_str()),
                ],
                body,
            )
                .into_response()
        }
        Ok(None) => not_found(),
        Err(err) => store_error(err),
    }
}

/// DELETE of an inbox message: final, no trash (messages are only
/// notifications; the calendars already contain the changes).
pub(super) async fn delete_message(
    ctx: Ctx<'_>,
    name: &str,
    precondition: &Precondition,
) -> Response {
    match ctx
        .state
        .store
        .delete_inbox_message(ctx.user.id, name, precondition)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found(),
        Err(err) => store_error(err),
    }
}

/// REPORT on the inbox: calendar-multiget, and calendar-query returning
/// every message (filters are not evaluated on iTIP messages).
pub(super) async fn report(ctx: Ctx<'_>, request: Request) -> Response {
    let body = match read_body(request, MAX_REPORT_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    let report = match Report::parse(&body) {
        Ok(report) => report,
        Err(DavError::Unsupported(_)) => {
            return dav_error(StatusCode::FORBIDDEN, "<d:supported-report/>");
        }
        Err(err) => return bad_request(&err),
    };
    let store = &ctx.state.store;
    let user = ctx.user;
    let base = inbox_href(&user.username);
    let (props, names): (Propfind, Vec<Result<String, String>>) = match report {
        Report::CalendarMultiget { props, hrefs } => {
            let mut seen = Vec::new();
            let names = hrefs
                .into_iter()
                .filter(|href| {
                    let new = !seen.contains(href);
                    seen.push(href.clone());
                    new
                })
                .map(|href| item_name_from_href(&href, &base).ok_or(href))
                .collect();
            (props, names)
        }
        Report::CalendarQuery { props, .. } => {
            match store.inbox_messages(user.id, MAX_INBOX_LISTING).await {
                Ok(messages) => (props, messages.into_iter().map(|m| Ok(m.name)).collect()),
                Err(err) => return store_error(err),
            }
        }
        _ => return dav_error(StatusCode::FORBIDDEN, "<d:supported-report/>"),
    };
    let with_data = match &props {
        Propfind::AllProp => true,
        Propfind::PropName => false,
        Propfind::Prop(names) => names.iter().any(|n| n.is(NS_CALDAV, "calendar-data")),
    };
    let mut responses = Vec::new();
    for name in names {
        let name = match name {
            Ok(name) => name,
            Err(href) => {
                responses.push(DavResponse::with_status(href, 404));
                continue;
            }
        };
        let href = format!("{base}{name}");
        match store.inbox_message(user.id, &name).await {
            Ok(Some(item)) => {
                let size = i64::try_from(item.content.len()).unwrap_or(i64::MAX);
                let content = with_data.then_some(item.content.as_slice());
                let props_found = message_properties(user, &item.etag, size, content);
                responses.push(respond(&href, &props_found, &props));
            }
            Ok(None) => responses.push(DavResponse::with_status(href, 404)),
            Err(err) => return store_error(err),
        }
    }
    multistatus(&Multistatus {
        responses,
        sync_token: None,
    })
}

// --- free/busy (outbox POST) -------------------------------------------------------

/// A parsed free/busy request (RFC 6638 §5.1).
struct FreeBusyRequest {
    uid: Option<String>,
    organizer: String,
    recipients: Vec<String>,
    start: i64,
    end: i64,
}

fn parse_freebusy(body: &[u8]) -> Result<FreeBusyRequest, &'static str> {
    let calendar = parse(body).map_err(|_| "<c:valid-calendar-data/>")?;
    let method = calendar
        .property("METHOD")
        .map(|p| p.value.to_ascii_uppercase());
    if calendar.name != "VCALENDAR" || method.as_deref() != Some("REQUEST") {
        return Err("<c:valid-scheduling-message/>");
    }
    let mut freebusy = calendar.components.iter().filter(|c| c.name == "VFREEBUSY");
    let (Some(component), None) = (freebusy.next(), freebusy.next()) else {
        return Err("<c:valid-scheduling-message/>");
    };
    if calendar
        .components
        .iter()
        .any(|c| c.name != "VFREEBUSY" && c.name != "VTIMEZONE")
    {
        return Err("<c:valid-scheduling-message/>");
    }
    let time = |name: &str| -> Result<i64, &'static str> {
        let value = component
            .property(name)
            .ok_or("<c:valid-scheduling-message/>")?;
        let parsed = calcraft_ical::parse_date_or_datetime(&value.value)
            .map_err(|_| "<c:valid-scheduling-message/>")?;
        if !parsed.is_utc {
            return Err("<c:valid-scheduling-message/>");
        }
        Ok(parsed.seconds)
    };
    let start = time("DTSTART")?;
    let end = time("DTEND")?;
    if end <= start {
        return Err("<c:valid-scheduling-message/>");
    }
    let organizer = component
        .property("ORGANIZER")
        .ok_or("<c:organizer-allowed/>")?
        .value
        .clone();
    let mut recipients: Vec<String> = Vec::new();
    for attendee in component.properties_named("ATTENDEE") {
        if !recipients.contains(&attendee.value) {
            recipients.push(attendee.value.clone());
        }
    }
    if recipients.is_empty() {
        return Err("<c:recipient-specified/>");
    }
    Ok(FreeBusyRequest {
        uid: component.property("UID").map(|p| p.value.clone()),
        organizer,
        recipients,
        start,
        end,
    })
}

/// The VFREEBUSY reply for one recipient.
fn freebusy_reply(
    request: &FreeBusyRequest,
    recipient: &str,
    busy: &[(i64, i64, itip::BusyType)],
) -> String {
    let stamp = |t: i64| itip::utc_stamp(t).unwrap_or_default();
    let mut out = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//CalCraft//Scheduling//EN\r\nMETHOD:REPLY\r\nBEGIN:VFREEBUSY\r\n",
    );
    let mut push = |name: &str, params: Vec<calcraft_ical::Parameter>, value: String| {
        let property = calcraft_ical::Property {
            name: name.to_owned(),
            parameters: params,
            value,
        };
        let _ = calcraft_ical::write::write_property(&property, &mut out);
    };
    if let Some(uid) = &request.uid {
        push("UID", Vec::new(), uid.clone());
    }
    push("DTSTAMP", Vec::new(), crate::scheduling::dtstamp());
    push("DTSTART", Vec::new(), stamp(request.start));
    push("DTEND", Vec::new(), stamp(request.end));
    push("ORGANIZER", Vec::new(), request.organizer.clone());
    push("ATTENDEE", Vec::new(), recipient.to_owned());
    for kind in [itip::BusyType::Busy, itip::BusyType::Tentative] {
        let periods: Vec<String> = busy
            .iter()
            .filter(|(_, _, k)| *k == kind)
            .map(|(lo, hi, _)| format!("{}/{}", stamp(*lo), stamp(*hi)))
            .collect();
        // Folded by the writer; split to keep single lines reasonable.
        for chunk in periods.chunks(50) {
            push(
                "FREEBUSY",
                vec![calcraft_ical::Parameter {
                    name: "FBTYPE".to_owned(),
                    values: vec![kind.as_str().to_owned()],
                }],
                chunk.join(","),
            );
        }
    }
    out.push_str("END:VFREEBUSY\r\nEND:VCALENDAR\r\n");
    out
}

/// POST to the outbox: a free/busy request for users of this server
/// (RFC 6638 §5). Only the outbox owner may post, only with one of their
/// own addresses as ORGANIZER; every recipient counts towards the rate
/// limit. Unknown and disabled users get 3.7, addresses elsewhere 5.3.
pub(super) async fn post(ctx: Ctx<'_>, scheduling: &Scheduling, request: Request) -> Response {
    if let Some(content_type) = header(request.headers(), CONTENT_TYPE)
        && !content_type
            .to_ascii_lowercase()
            .starts_with("text/calendar")
    {
        return dav_error(StatusCode::FORBIDDEN, "<c:supported-calendar-data/>");
    }
    let body = match read_body(request, MAX_POST_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    let request = match parse_freebusy(&body) {
        Ok(request) => request,
        Err(condition) => return dav_error(StatusCode::FORBIDDEN, condition),
    };
    let store = &ctx.state.store;
    let me = match scheduling.addresses(store, ctx.user).await {
        Ok(me) => me,
        Err(err) => return store_error(err),
    };
    if !me.contains(&normalize(&request.organizer)) {
        return dav_error(StatusCode::FORBIDDEN, "<c:organizer-allowed/>");
    }
    if request.recipients.len() > MAX_FREEBUSY_RECIPIENTS {
        return dav_error(StatusCode::FORBIDDEN, "<c:recipient-specified/>");
    }
    let max = i64::from(scheduling.config.max_freebusy_days) * 86_400;
    if request.end - request.start > max {
        return dav_error(StatusCode::FORBIDDEN, "<c:valid-scheduling-message/>");
    }
    let mut granted = scheduling.allow(
        ctx.user.id,
        u32::try_from(request.recipients.len()).unwrap_or(u32::MAX),
    );
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="utf-8"?><c:schedule-response xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">"#,
    );
    for recipient in &request.recipients {
        let (status, data) = if !scheduling.config.free_busy {
            (status::NO_AUTHORITY.to_owned() + ";No authority", None)
        } else if granted == 0 {
            (
                status::NOT_DELIVERED.to_owned() + ";Service unavailable",
                None,
            )
        } else {
            granted -= 1;
            match scheduling.resolve(store, &normalize(recipient)).await {
                Ok(Resolution::Local(user)) => {
                    let addresses = match scheduling.addresses(store, &user).await {
                        Ok(addresses) => addresses,
                        Err(err) => return store_error(err),
                    };
                    match freebusy::busy(ctx.state, &user, &addresses, request.start, request.end)
                        .await
                    {
                        Ok(busy) => (
                            status::SUCCESS.to_owned(),
                            Some(freebusy_reply(&request, recipient, &busy)),
                        ),
                        Err(err) => return store_error(err),
                    }
                }
                Ok(Resolution::Unknown) => (
                    status::INVALID_USER.to_owned() + ";Invalid calendar user",
                    None,
                ),
                Ok(Resolution::External) => (
                    status::NO_SCHEDULING.to_owned() + ";No scheduling support for user",
                    None,
                ),
                Err(err) => return store_error(err),
            }
        };
        xml.push_str("<c:response><c:recipient><d:href>");
        xml.push_str(&escape_text(recipient));
        xml.push_str("</d:href></c:recipient><c:request-status>");
        xml.push_str(&escape_text(&status));
        xml.push_str("</c:request-status>");
        if let Some(data) = data {
            xml.push_str("<c:calendar-data>");
            xml.push_str(&escape_text(&data));
            xml.push_str("</c:calendar-data>");
        }
        xml.push_str("</c:response>");
    }
    xml.push_str("</c:schedule-response>");
    tracing::info!(
        user = %ctx.user.username,
        recipients = request.recipients.len(),
        "free/busy lookup"
    );
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}
