//! Public read-only links of calendars: `GET /public/<token>.ics` without
//! login (`[publish] enabled`, off by default). See docs/sharing.md.
//!
//! * The token is 256 random bits (64 hex digits). Only its BLAKE3 hash is
//!   stored; a request is looked up by the hash and the stored hash is
//!   compared again in constant time. Unknown, revoked, malformed tokens,
//!   tokens of disabled owners and a disabled feature all get the same
//!   `404` as any unknown path. Trace spans record `/public/` only, never
//!   the token.
//! * Clients with many unknown tokens are blocked for a while
//!   ([`MISSES_PER_CLIENT`] per [`MISS_WINDOW`], then `429` for every
//!   publish request of that client), on top of the general per-client
//!   limits.
//! * The calendar is rendered from the stored objects (no recurrence
//!   expansion), VEVENTs only, at most `publish.max_objects` objects and
//!   `publish.max_bytes` bytes. Never published: alarms (VALARM),
//!   attachments (ATTACH), and – unless the link includes them – ORGANIZER
//!   and ATTENDEE. UIDs are replaced by a hash per link. A "busy only"
//!   link has nothing but the times; events with `CLASS:PRIVATE` or
//!   `CONFIDENTIAL` (or any class other than PUBLIC) are shown as busy
//!   blocks, left out or shown as configured per link.
//! * Served as `text/calendar` (never HTML) with `nosniff`, a sandboxing
//!   CSP, `Cache-Control: private, max-age=300` and an ETag (`304` on
//!   `If-None-Match`).

use std::{
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use axum::{
    extract::{Path, Request, State},
    http::{
        HeaderValue, Method, StatusCode,
        header::{
            CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG,
            IF_NONE_MATCH, RETRY_AFTER,
        },
    },
    response::{IntoResponse, Response},
};
use calcraft_ical::{Component, Property, parse, write::write_component};
use calcraft_store::{Collection, PrivateEvents, PublishLink, Store, StoreError};

use crate::{
    app::AppState,
    config::PublishConfig,
    limits::FailureTable,
    net::ClientKey,
    secrets::{hash_secret, hashes_equal},
};

/// Path below which links are served.
pub const PUBLIC_PATH: &str = "/public/";
/// Unknown tokens a client may try per [`MISS_WINDOW`].
pub const MISSES_PER_CLIENT: u32 = 30;
/// Window of [`MISSES_PER_CLIENT`].
pub const MISS_WINDOW: Duration = Duration::from_secs(600);
/// Clients tracked for the miss limit.
const TRACKED_CLIENTS: usize = 10_000;
/// Hex digits of a token (256 bits).
pub const TOKEN_HEX_LEN: usize = 64;
const CSP: &str = "default-src 'none'; sandbox";
const PRODID: &str = "-//CalCraft//Published calendar//EN";

/// Publishing, if enabled: settings and the miss limit.
#[derive(Debug)]
pub struct Publish {
    pub config: PublishConfig,
    misses: Mutex<FailureTable>,
}

impl Publish {
    pub fn new(config: &PublishConfig) -> Self {
        Self {
            config: config.clone(),
            misses: Mutex::new(FailureTable::new(
                TRACKED_CLIENTS,
                MISSES_PER_CLIENT,
                MISS_WINDOW,
            )),
        }
    }

    /// The `https://` and `webcal://` URLs of a token.
    pub fn urls(&self, token: &str) -> (String, String) {
        let https = format!("{}{PUBLIC_PATH}{token}.ics", self.config.base());
        let webcal = match https.split_once("://") {
            Some((_, rest)) => format!("webcal://{rest}"),
            None => https.clone(),
        };
        (https, webcal)
    }

    fn blocked(&self, client: Option<ClientKey>) -> bool {
        let Some(client) = client else { return false };
        let misses = self.misses.lock().unwrap_or_else(PoisonError::into_inner);
        misses.is_blocked(client, Instant::now())
    }

    fn miss(&self, client: Option<ClientKey>) {
        if let Some(client) = client {
            let mut misses = self.misses.lock().unwrap_or_else(PoisonError::into_inner);
            misses.record(client, Instant::now());
        }
    }
}

/// A new secret token and its stored hash.
pub fn new_token() -> (String, String) {
    let token: String = crate::secrets::random_bytes::<32>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let hash = hash_secret(&token);
    (token, hash)
}

/// The answer for everything that is not a valid link, byte-identical to
/// the router's fallback for unknown paths.
fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn token_of(file: &str) -> Option<&str> {
    let token = file.strip_suffix(".ics")?;
    (token.len() == TOKEN_HEX_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(token)
}

/// `GET`/`HEAD /public/<token>.ics`.
pub async fn serve(
    State(state): State<AppState>,
    Path(file): Path<String>,
    request: Request,
) -> Response {
    // Only GET and HEAD; anything else looks like an unknown path, so the
    // route does not even reveal that publishing is on.
    let Some(publish) = &state.publish else {
        return not_found();
    };
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return not_found();
    }
    let client = request.extensions().get::<ClientKey>().copied();
    if publish.blocked(client) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(RETRY_AFTER, MISS_WINDOW.as_secs().to_string())],
            "too many requests",
        )
            .into_response();
    }
    let Some(token) = token_of(&file) else {
        publish.miss(client);
        return not_found();
    };
    let hash = hash_secret(token);
    let found = match state.store.published_calendar(&hash).await {
        Ok(found) => found,
        Err(err) => return store_error(&err),
    };
    let Some((link, stored, collection)) =
        found.filter(|(_, stored, _)| hashes_equal(stored, &hash))
    else {
        publish.miss(client);
        return not_found();
    };
    let etag = etag(&link, &collection, &publish.config);
    let headers = request.headers();
    let unchanged = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .map(|tag| tag.trim().trim_start_matches("W/"))
                .any(|tag| tag == etag || tag == "*")
        });
    drop(stored);
    if unchanged {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        set_headers(&mut response, &etag);
        response.headers_mut().remove(CONTENT_TYPE);
        return response;
    }
    let body = match render(&state.store, &link, &collection, &publish.config).await {
        Ok(body) => body,
        Err(err) => return store_error(&err),
    };
    let with_body = request.method() != Method::HEAD;
    let mut response = if with_body {
        (StatusCode::OK, body).into_response()
    } else {
        StatusCode::OK.into_response()
    };
    set_headers(&mut response, &etag);
    response
}

fn store_error(err: &StoreError) -> Response {
    match err {
        StoreError::Busy => (StatusCode::SERVICE_UNAVAILABLE, [(RETRY_AFTER, "1")]).into_response(),
        err => {
            tracing::error!(%err, "storage error while publishing a calendar");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn set_headers(response: &mut Response, etag: &str) {
    let headers = response.headers_mut();
    let pairs = [
        (CONTENT_TYPE, "text/calendar; charset=utf-8"),
        (CONTENT_DISPOSITION, "inline; filename=\"calendar.ics\""),
        (CONTENT_SECURITY_POLICY, CSP),
        (CACHE_CONTROL, "private, max-age=300"),
    ];
    for (name, value) in pairs {
        headers.insert(name, HeaderValue::from_static(value));
    }
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(ETAG, value);
    }
}

/// An entity tag that changes with every change of the calendar (its sync
/// revision, also bumped by property changes) and of the link's options,
/// without rendering it.
fn etag(link: &PublishLink, collection: &Collection, config: &PublishConfig) -> String {
    let input = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}",
        link.public_id,
        collection.id,
        collection.token_salt,
        collection.sync_revision,
        link.busy_only,
        link.private_events.as_str(),
        link.include_attendees,
        config.max_bytes,
        config.max_objects
    );
    format!("\"{}\"", &blake3::hash(input.as_bytes()).to_hex()[..32])
}

/// Whether a component's class is anything but PUBLIC (RFC 5545 §3.8.1.3:
/// unknown classes are treated as private).
fn is_private(component: &Component) -> bool {
    component
        .property("CLASS")
        .is_some_and(|class| !class.value.trim().eq_ignore_ascii_case("PUBLIC"))
}

fn property(name: &str, value: &str) -> Property {
    Property {
        name: name.to_owned(),
        parameters: Vec::new(),
        value: value.to_owned(),
    }
}

/// The UID of a published event: a hash per link, so neither the original
/// UID nor its relation to other links is revealed.
fn published_uid(link: &PublishLink, uid: &str) -> String {
    let input = format!("{}\n{uid}", link.public_id);
    format!(
        "{}@calcraft-published",
        &blake3::hash(input.as_bytes()).to_hex()[..32]
    )
}

/// Properties kept in a busy block.
const BUSY_PROPERTIES: [&str; 8] = [
    "DTSTART",
    "DTEND",
    "DURATION",
    "RRULE",
    "RDATE",
    "EXDATE",
    "RECURRENCE-ID",
    "DTSTAMP",
];

/// Properties never published.
const PRIVATE_PROPERTIES: [&str; 1] = ["ATTACH"];
const ADDRESS_PROPERTIES: [&str; 2] = ["ORGANIZER", "ATTENDEE"];

/// One VEVENT as published, or `None` if it is left out.
fn publish_event(link: &PublishLink, event: &Component) -> Option<Component> {
    let private = is_private(event);
    let busy = link.busy_only || (private && link.private_events == PrivateEvents::Busy);
    if private && link.private_events == PrivateEvents::Hide {
        return None;
    }
    let uid = event
        .property("UID")
        .map(|p| p.value.as_str())
        .unwrap_or("");
    let mut out = Component {
        name: "VEVENT".to_owned(),
        properties: vec![property("UID", &published_uid(link, uid))],
        components: Vec::new(),
    };
    if busy {
        // Free time and cancelled events are no busy blocks.
        let transparent = event
            .property("TRANSP")
            .is_some_and(|p| p.value.trim().eq_ignore_ascii_case("TRANSPARENT"));
        let cancelled = event
            .property("STATUS")
            .is_some_and(|p| p.value.trim().eq_ignore_ascii_case("CANCELLED"));
        if transparent || cancelled {
            return None;
        }
        out.properties.extend(
            event
                .properties
                .iter()
                .filter(|p| BUSY_PROPERTIES.contains(&p.name.as_str()))
                .cloned(),
        );
        out.properties.push(property("SUMMARY", "Busy"));
        return Some(out);
    }
    out.properties.extend(
        event
            .properties
            .iter()
            .filter(|p| p.name != "UID" && !PRIVATE_PROPERTIES.contains(&p.name.as_str()))
            .filter(|p| link.include_attendees || !ADDRESS_PROPERTIES.contains(&p.name.as_str()))
            .cloned(),
    );
    // No alarms (they would ring for every subscriber) and no other
    // nested components.
    Some(out)
}

/// The published calendar.
async fn render(
    store: &Store,
    link: &PublishLink,
    collection: &Collection,
    config: &PublishConfig,
) -> Result<String, StoreError> {
    let mut timezones: Vec<Component> = Vec::new();
    let mut events: Vec<Component> = Vec::new();
    let mut bytes = 0usize;
    let mut objects = 0u32;
    let metas = store.items(collection.id).await?;
    for meta in metas {
        if objects >= config.max_objects {
            break;
        }
        let Some(item) = store.item(collection.id, &meta.name).await? else {
            continue;
        };
        // Stored objects passed validation; anything unparsable is skipped.
        let Ok(tree) = parse(&item.content) else {
            continue;
        };
        let published: Vec<Component> = tree
            .components
            .iter()
            .filter(|c| c.name == "VEVENT")
            .filter_map(|event| publish_event(link, event))
            .collect();
        if published.is_empty() {
            continue;
        }
        let mut text = String::new();
        let new_timezones: Vec<&Component> = tree
            .components
            .iter()
            .filter(|c| c.name == "VTIMEZONE")
            .filter(|tz| {
                let id = tz.property("TZID").map(|p| &p.value);
                !timezones
                    .iter()
                    .any(|known| known.property("TZID").map(|p| &p.value) == id)
            })
            .collect();
        for component in new_timezones.iter().copied().chain(published.iter()) {
            if write_component(component, &mut text).is_err() {
                text.clear();
                break;
            }
        }
        if text.is_empty() || bytes.saturating_add(text.len()) > config.max_bytes {
            continue;
        }
        bytes += text.len();
        objects += 1;
        timezones.extend(new_timezones.into_iter().cloned());
        events.extend(published);
    }
    let mut calendar = Component {
        name: "VCALENDAR".to_owned(),
        properties: vec![
            property("VERSION", "2.0"),
            property("PRODID", PRODID),
            property("CALSCALE", "GREGORIAN"),
        ],
        components: Vec::new(),
    };
    if let Some(name) = &collection.display_name {
        let name = calcraft_ical::write::escape_text(name);
        calendar.properties.push(property("NAME", &name));
        calendar.properties.push(property("X-WR-CALNAME", &name));
    }
    calendar.components.extend(timezones);
    calendar.components.extend(events);
    let mut out = String::new();
    // Every part was written successfully above.
    let _ = write_component(&calendar, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests;
