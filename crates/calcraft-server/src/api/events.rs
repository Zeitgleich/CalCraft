//! Calendar views of the web UI: the user's calendars, the instances of
//! their events in a time range, and events to show, create, change, move
//! and delete.
//!
//! Reads resolve calendars like DAV does (own, subscriptions, accepted
//! shares) and show read-only sharees the same busy stubs of private
//! events. Every change is a DAV request run through the DAV handlers
//! ([`crate::dav::rest`]): there is no second write path. Changes need the
//! `If-Match` header with the ETag the UI last saw; a changed object
//! answers 412 (`precondition_failed`) and the UI reloads it.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json,
    body::to_bytes,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{
        HeaderMap, StatusCode,
        header::{ETAG, IF_MATCH},
    },
    response::{IntoResponse, Response},
};
use calcraft_core::{
    CollectionKind, Slug,
    api::{
        CalendarSource, CalendarView, EventDetail, EventInput, EventInstance, EventList,
        EventSaved, EventTimes, MoveEvent,
    },
};
use calcraft_ical::{Budget, Component, InstanceTime, OBJECT_BUDGET, instances, parse, write};
use calcraft_store::{Collection, Item, User};
use serde::Deserialize;

use super::{ApiError, ApiJson, SessionUser};
use crate::{
    app::AppState,
    auth::now,
    dav::rest::{self, Expect},
    events::{self, EditError},
    redact,
    secrets::random_token,
};

/// Longest time range of one list request.
pub const MAX_RANGE: i64 = 400 * 86_400;
/// Most instances in one list response.
pub const MAX_INSTANCES: usize = 5_000;
/// Most objects evaluated for one list response.
const MAX_OBJECTS: usize = 5_000;
/// Candidates read per storage query.
const PAGE: u32 = 500;
/// Total work for the expansion of one list response (see
/// [`calcraft_ical::Budget`]), and its wall-clock limit.
const LIST_WORK: u64 = 4_000_000;
const LIST_DEADLINE: Duration = Duration::from_secs(5);
/// Largest DAV error body read when translating a response.
const MAX_DAV_BODY: usize = 16 * 1024;

fn edit_error(err: EditError) -> ApiError {
    match err {
        EditError::Invalid(message) => ApiError::BadRequest(message),
        EditError::NotFound => ApiError::NotFound,
    }
}

fn slug(value: &str) -> Result<Slug, ApiError> {
    value.parse().map_err(|_| ApiError::NotFound)
}

/// A calendar of the user's home (own, subscription or accepted share)
/// that holds events.
async fn calendar(state: &AppState, user: &User, slug_value: &str) -> Result<Collection, ApiError> {
    let slug = slug(slug_value)?;
    rest::calendar(state, user, &slug)
        .await?
        .filter(holds_events)
        .ok_or(ApiError::NotFound)
}

fn holds_events(collection: &Collection) -> bool {
    collection.kind == CollectionKind::Calendar
        && collection.components.split(',').any(|c| c == "VEVENT")
}

fn view(collection: &Collection) -> CalendarView {
    let source = match (&collection.shared, collection.read_only) {
        (Some(_), _) => CalendarSource::Shared,
        (None, true) => CalendarSource::Subscription,
        (None, false) => CalendarSource::Own,
    };
    CalendarView {
        slug: collection.slug.to_string(),
        display_name: collection.display_name.clone(),
        color: collection.color.clone(),
        source,
        writable: !collection.read_only,
        owner: collection.shared.as_ref().map(|via| via.owner.to_string()),
        private_hidden: rest::hides_private(collection),
    }
}

/// All calendars of the user's home that hold events.
async fn home(state: &AppState, user: &User) -> Result<Vec<Collection>, ApiError> {
    let properties = state
        .store
        .dead_properties_of_owner(user.id, CollectionKind::Calendar)
        .await?;
    let mut own: Vec<(Option<i64>, Collection)> = state
        .store
        .collections(user.id, CollectionKind::Calendar)
        .await?
        .into_iter()
        .filter(holds_events)
        .map(|collection| {
            let id = collection.id;
            let order = crate::dav::calendar_order(
                properties.iter().filter(|(c, _)| *c == id).map(|(_, p)| p),
            );
            (order, collection)
        })
        .collect();
    own.sort_by_cached_key(|(order, c)| {
        let name = c.display_name.as_deref().unwrap_or(c.slug.as_str());
        (order.is_none(), *order, name.to_lowercase())
    });
    let mut calendars: Vec<Collection> = own.into_iter().map(|(_, c)| c).collect();
    if state.sharing {
        let mut shared: Vec<Collection> = state
            .store
            .shared_collections(user.id, CollectionKind::Calendar)
            .await?
            .into_iter()
            .filter(holds_events)
            .collect();
        shared.sort_by_cached_key(|c| {
            let order = c.shared.as_ref().and_then(|via| via.order);
            let name = c.display_name.as_deref().unwrap_or(c.slug.as_str());
            (order.is_none(), order, name.to_lowercase())
        });
        calendars.extend(shared);
    }
    Ok(calendars)
}

pub async fn calendars(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<Json<Vec<CalendarView>>, ApiError> {
    let calendars = home(&state, &session.user).await?;
    Ok(Json(calendars.iter().map(view).collect()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    /// Unix seconds.
    start: i64,
    end: i64,
    /// Only this calendar.
    #[serde(default)]
    calendar: Option<String>,
}

/// What is left of the expansion limits of one list request.
struct Limits {
    work: u64,
    objects: usize,
    deadline: tokio::time::Instant,
    cancel: Arc<AtomicBool>,
}

/// Sets the cancel flag when the request is dropped, so running
/// expansions stop at their next budget charge.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn instance_time(time: InstanceTime) -> (i64, bool, bool) {
    match time {
        InstanceTime::Utc(s) => (s, false, false),
        InstanceTime::Floating(s) => (s, false, true),
        InstanceTime::Date(s) => (s, true, false),
    }
}

/// The instances of one (visible) object in `[start, end)`. `None` if the
/// object could not be evaluated within `budget`.
fn object_instances(
    collection_slug: &str,
    writable: bool,
    hides_private: bool,
    item: &Item,
    range: (i64, i64),
    budget: &mut Budget,
    max: usize,
) -> Option<(Vec<EventInstance>, bool)> {
    let root = parse(&item.content).ok()?;
    let expanded = instances(&root, range.0, range.1, budget, max)?;
    let recurring = events::is_recurring(&root);
    let master_private = redact::master_private(&root);
    let mut out = Vec::with_capacity(expanded.instances.len());
    for instance in expanded.instances {
        let Some(component) = root.components.get(instance.component) else {
            continue;
        };
        if component.name != "VEVENT" {
            continue;
        }
        let (start, all_day, floating) = instance_time(instance.start);
        let (end, ..) = instance_time(instance.end);
        let busy = hides_private && redact::is_private(component, master_private);
        out.push(EventInstance {
            calendar: collection_slug.to_owned(),
            name: item.name.clone(),
            etag: item.etag.clone(),
            recurrence_id: instance.recurrence_id,
            summary: events::text(component, "SUMMARY"),
            location: if busy {
                None
            } else {
                events::text(component, "LOCATION")
            },
            all_day,
            floating,
            start,
            end: end.max(start),
            busy,
            cancelled: component
                .property("STATUS")
                .is_some_and(|s| s.value.trim().eq_ignore_ascii_case("CANCELLED")),
            recurring,
            writable: writable && !busy,
        });
    }
    Some((out, expanded.complete))
}

/// Instances of one calendar, appended to `list`. `false` once a limit
/// was reached.
async fn calendar_instances(
    state: &AppState,
    collection: &Collection,
    range: (i64, i64),
    limits: &mut Limits,
    list: &mut EventList,
) -> Result<bool, ApiError> {
    let slug = collection.slug.to_string();
    let writable = !collection.read_only;
    let hides_private = rest::hides_private(collection);
    // Local times may be up to ±14 h from their naive value.
    const SLACK: i64 = 14 * 3600;
    let (query_start, query_end) = (range.0 - SLACK, range.1 + SLACK);
    let mut after: Option<String> = None;
    loop {
        let page = state
            .store
            .query_candidates(
                collection.id,
                Some("VEVENT"),
                Some(query_start),
                Some(query_end),
                after.as_deref(),
                PAGE,
            )
            .await?;
        let mut items = Vec::with_capacity(page.len());
        for candidate in &page {
            if limits.objects == 0 {
                return Ok(false);
            }
            limits.objects -= 1;
            if let Some(item) = state
                .store
                .item(collection.id, &candidate.meta.name)
                .await?
            {
                items.push(rest::visible_item(collection, item));
            }
        }
        let permit =
            match tokio::time::timeout_at(limits.deadline, state.expansion.clone().acquire_owned())
                .await
            {
                Ok(Ok(permit)) => permit,
                _ => return Ok(false),
            };
        let room = MAX_INSTANCES.saturating_sub(list.instances.len());
        let (work, cancel, slug_owned) = (limits.work, limits.cancel.clone(), slug.clone());
        let joined = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut left = work;
            let mut room = room;
            let mut found = Vec::new();
            let mut complete = true;
            for item in &items {
                if left == 0 || room == 0 {
                    complete = false;
                    break;
                }
                let granted = OBJECT_BUDGET.min(left);
                let mut budget = Budget::with_cancel(granted, cancel.clone());
                match object_instances(
                    &slug_owned,
                    writable,
                    hides_private,
                    item,
                    range,
                    &mut budget,
                    room,
                ) {
                    Some((instances, whole)) => {
                        room = room.saturating_sub(instances.len());
                        complete &= whole;
                        found.extend(instances);
                    }
                    // Not evaluable within the limits: left out, and the
                    // list says it is incomplete.
                    None => complete = false,
                }
                left = left.saturating_sub(granted.saturating_sub(budget.remaining()));
            }
            (found, complete, left)
        })
        .await;
        let Ok((found, complete, left)) = joined else {
            return Ok(false);
        };
        limits.work = left;
        list.instances.extend(found);
        if !complete || tokio::time::Instant::now() >= limits.deadline {
            return Ok(false);
        }
        if page.len() < PAGE as usize {
            return Ok(true);
        }
        after = page.last().map(|c| c.meta.name.clone());
    }
}

pub async fn list(
    State(state): State<AppState>,
    session: SessionUser,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<EventList>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::BadRequest("invalid query"))?;
    if query.end <= query.start || query.end - query.start > MAX_RANGE {
        return Err(ApiError::BadRequest(
            "end must be after start, at most 400 days later",
        ));
    }
    let user = &session.user;
    let calendars = match &query.calendar {
        Some(slug) => vec![calendar(&state, user, slug).await?],
        None => home(&state, user).await?,
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let _guard = CancelOnDrop(cancel.clone());
    let mut limits = Limits {
        work: LIST_WORK,
        objects: MAX_OBJECTS,
        deadline: tokio::time::Instant::now() + LIST_DEADLINE,
        cancel,
    };
    let mut list = EventList::default();
    for collection in &calendars {
        if !calendar_instances(
            &state,
            collection,
            (query.start, query.end),
            &mut limits,
            &mut list,
        )
        .await?
        {
            list.truncated = true;
            break;
        }
    }
    list.instances.sort_by(|a, b| {
        (a.start, a.end, &a.summary, &a.name).cmp(&(b.start, b.end, &b.summary, &b.name))
    });
    Ok(Json(list))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceQuery {
    #[serde(default)]
    recurrence_id: Option<String>,
}

/// The stored object as the user may see it, parsed.
async fn load(
    state: &AppState,
    collection: &Collection,
    name: &str,
) -> Result<(Item, Option<String>, Component), ApiError> {
    if !rest::valid_name(name) {
        return Err(ApiError::NotFound);
    }
    let (item, schedule_tag) = state
        .store
        .item_with_schedule_tag(collection.id, name)
        .await?
        .ok_or(ApiError::NotFound)?;
    let item = rest::visible_item(collection, item);
    let root = parse(&item.content).map_err(|_| ApiError::NotFound)?;
    if events::master_index(&root).is_none() {
        return Err(ApiError::NotFound);
    }
    Ok((item, schedule_tag, root))
}

pub async fn get(
    State(state): State<AppState>,
    session: SessionUser,
    Path((calendar_slug, name)): Path<(String, String)>,
    query: Result<Query<InstanceQuery>, QueryRejection>,
) -> Result<Json<EventDetail>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::BadRequest("invalid query"))?;
    let collection = calendar(&state, &session.user, &calendar_slug).await?;
    let (item, schedule_tag, root) = load(&state, &collection, &name).await?;
    let detail = events::detail(&root, query.recurrence_id.as_deref()).map_err(edit_error)?;
    let master_private = redact::master_private(&root);
    let busy = rest::hides_private(&collection)
        && root
            .components
            .iter()
            .any(|c| c.name == "VEVENT" && redact::is_private(c, master_private));
    // Sharees may not change the owner's scheduling objects (DAV refuses
    // them too); busy blocks are never written back.
    let writable =
        !collection.read_only && !busy && !(collection.shared.is_some() && schedule_tag.is_some());
    Ok(Json(EventDetail {
        calendar: collection.slug.to_string(),
        name: item.name,
        etag: item.etag,
        uid: detail.uid,
        writable,
        busy,
        fields: detail.fields,
        recurrence_id: query.recurrence_id,
        is_override: detail.is_override,
        recurrence: detail.recurrence,
        recurrence_rule: detail.recurrence_rule,
        organizer: detail.organizer,
        attendees: detail.attendees,
        attachments: detail.attachments,
    }))
}

/// The `If-Match` ETag of a change (required).
fn if_match(headers: &HeaderMap) -> Result<String, ApiError> {
    let value = headers
        .get(IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| v.len() >= 2 && v.starts_with('"') && v.ends_with('"') && v.len() <= 128)
        .ok_or(ApiError::BadRequest(
            "If-Match header with the event's ETag required",
        ))?;
    Ok(value.to_owned())
}

/// Translates the response of a DAV handler.
async fn dav_outcome(response: Response) -> Result<Option<String>, ApiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned));
    }
    let body = to_bytes(response.into_body(), MAX_DAV_BODY)
        .await
        .unwrap_or_default();
    let body = String::from_utf8_lossy(&body);
    Err(match status {
        StatusCode::PRECONDITION_FAILED => ApiError::PreconditionFailed,
        StatusCode::NOT_FOUND | StatusCode::CONFLICT => ApiError::NotFound,
        StatusCode::INSUFFICIENT_STORAGE => ApiError::Limit("storage limit reached"),
        StatusCode::SERVICE_UNAVAILABLE => ApiError::Unavailable,
        StatusCode::PAYLOAD_TOO_LARGE => ApiError::TooLarge,
        StatusCode::FORBIDDEN if body.contains("max-resource-size") => ApiError::TooLarge,
        StatusCode::FORBIDDEN if body.contains("no-uid-conflict") => {
            ApiError::Conflict("the calendar already has an event with this UID")
        }
        StatusCode::FORBIDDEN if body.contains("valid-calendar-data") => {
            ApiError::InvalidData("the event is not valid".to_owned())
        }
        StatusCode::FORBIDDEN if body.contains("supported-calendar-component") => {
            ApiError::BadRequest("the calendar does not accept events")
        }
        StatusCode::FORBIDDEN if body.contains("valid-managed-id") => {
            ApiError::BadRequest("the event refers to an unknown attachment")
        }
        StatusCode::FORBIDDEN if body.contains("another owner") => {
            ApiError::BadRequest("events can only be moved between calendars of the same owner")
        }
        StatusCode::FORBIDDEN => ApiError::Forbidden,
        StatusCode::BAD_REQUEST => ApiError::BadRequest("the change was refused"),
        _ => ApiError::Internal,
    })
}

/// The ETag after a change: from the response, or (when the server
/// rewrote the object, e.g. its attachments) as stored now.
async fn saved(
    state: &AppState,
    collection: &Collection,
    name: &str,
    etag: Option<String>,
) -> Result<EventSaved, ApiError> {
    let etag = match etag {
        Some(etag) => Some(etag),
        None => state
            .store
            .item_meta(collection.id, name)
            .await?
            .map(|meta| meta.etag),
    };
    Ok(EventSaved {
        calendar: collection.slug.to_string(),
        name: name.to_owned(),
        etag,
    })
}

fn serialise(root: &Component) -> Result<String, ApiError> {
    write::to_string(root).map_err(|_| ApiError::BadRequest("the event cannot be written"))
}

/// Refuses changes of calendars the user cannot write before anything
/// else (like DAV, which answers 403 whatever the precondition).
fn writable(collection: &Collection) -> Result<(), ApiError> {
    if collection.read_only {
        Err(ApiError::Forbidden)
    } else {
        Ok(())
    }
}

/// Loads the object for a change: the ETag must still be the one the UI
/// saw.
async fn load_for_change(
    state: &AppState,
    collection: &Collection,
    name: &str,
    expected: &str,
) -> Result<(Item, Component), ApiError> {
    writable(collection)?;
    let (item, _, root) = load(state, collection, name).await?;
    if item.etag != expected {
        return Err(ApiError::PreconditionFailed);
    }
    Ok((item, root))
}

pub async fn create(
    State(state): State<AppState>,
    session: SessionUser,
    Path(calendar_slug): Path<String>,
    ApiJson(input): ApiJson<EventInput>,
) -> Result<(StatusCode, Json<EventSaved>), ApiError> {
    let user = &session.user;
    let collection = calendar(&state, user, &calendar_slug).await?;
    writable(&collection)?;
    let token = random_token::<16>();
    let uid = format!("{token}@calcraft");
    let name = format!("{token}.ics");
    let root = events::new_object(&input, &uid, now()).map_err(edit_error)?;
    let body = serialise(&root)?;
    let etag =
        dav_outcome(rest::put(&state, user, &collection, &name, body, Expect::New).await).await?;
    Ok((
        StatusCode::CREATED,
        Json(saved(&state, &collection, &name, etag).await?),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    session: SessionUser,
    Path((calendar_slug, name)): Path<(String, String)>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<EventInput>,
) -> Result<Json<EventSaved>, ApiError> {
    let user = &session.user;
    // The calendar first: other users' paths answer 404 whatever the headers.
    let collection = calendar(&state, user, &calendar_slug).await?;
    let expected = if_match(&headers)?;
    let (item, mut root) = load_for_change(&state, &collection, &name, &expected).await?;
    events::apply(&mut root, &input, now()).map_err(edit_error)?;
    let body = serialise(&root)?;
    let response = rest::put(
        &state,
        user,
        &collection,
        &item.name,
        body,
        Expect::Etag(&expected),
    )
    .await;
    let etag = dav_outcome(response).await?;
    Ok(Json(saved(&state, &collection, &item.name, etag).await?))
}

pub async fn update_times(
    State(state): State<AppState>,
    session: SessionUser,
    Path((calendar_slug, name)): Path<(String, String)>,
    headers: HeaderMap,
    ApiJson(times): ApiJson<EventTimes>,
) -> Result<Json<EventSaved>, ApiError> {
    let user = &session.user;
    // The calendar first: other users' paths answer 404 whatever the headers.
    let collection = calendar(&state, user, &calendar_slug).await?;
    let expected = if_match(&headers)?;
    let (item, mut root) = load_for_change(&state, &collection, &name, &expected).await?;
    events::apply_times(&mut root, &times, now()).map_err(edit_error)?;
    let body = serialise(&root)?;
    let response = rest::put(
        &state,
        user,
        &collection,
        &item.name,
        body,
        Expect::Etag(&expected),
    )
    .await;
    let etag = dav_outcome(response).await?;
    Ok(Json(saved(&state, &collection, &item.name, etag).await?))
}

pub async fn delete(
    State(state): State<AppState>,
    session: SessionUser,
    Path((calendar_slug, name)): Path<(String, String)>,
    headers: HeaderMap,
    query: Result<Query<InstanceQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::BadRequest("invalid query"))?;
    let user = &session.user;
    // The calendar first: other users' paths answer 404 whatever the headers.
    let collection = calendar(&state, user, &calendar_slug).await?;
    let expected = if_match(&headers)?;
    let (item, mut root) = load_for_change(&state, &collection, &name, &expected).await?;
    match query.recurrence_id {
        // One instance: the object stays, without it.
        Some(rid) => {
            events::exclude(&mut root, &rid, now()).map_err(edit_error)?;
            let body = serialise(&root)?;
            let response = rest::put(
                &state,
                user,
                &collection,
                &item.name,
                body,
                Expect::Etag(&expected),
            )
            .await;
            let etag = dav_outcome(response).await?;
            Ok(Json(saved(&state, &collection, &item.name, etag).await?).into_response())
        }
        None => {
            dav_outcome(rest::delete(&state, user, &collection, &item.name, &expected).await)
                .await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
    }
}

pub async fn move_event(
    State(state): State<AppState>,
    session: SessionUser,
    Path((calendar_slug, name)): Path<(String, String)>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<MoveEvent>,
) -> Result<Json<EventSaved>, ApiError> {
    let user = &session.user;
    // The calendar first: other users' paths answer 404 whatever the headers.
    let collection = calendar(&state, user, &calendar_slug).await?;
    let expected = if_match(&headers)?;
    let target = calendar(&state, user, &request.calendar).await?;
    let (item, _) = load_for_change(&state, &collection, &name, &expected).await?;
    writable(&target)?;
    let response = rest::move_to(
        &state,
        user,
        &collection,
        &item.name,
        &target.slug,
        &expected,
    )
    .await;
    let etag = dav_outcome(response).await?;
    Ok(Json(saved(&state, &target, &item.name, etag).await?))
}

/// Download of a managed attachment, with the DAV GET's headers.
pub async fn attachment(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    rest::attachment(&state, &session.user, &id, &headers)
        .await?
        .ok_or(ApiError::NotFound)
}
