//! Mapping between request paths, hrefs and resources.
//!
//! This is the central authorization point of the DAV tree: a path only
//! resolves to a resource if it belongs to the authenticated user. Every
//! handler starts from a resolved resource, and every storage query takes
//! the id of a collection obtained here.

use calcraft_core::{CollectionKind, Slug, Username};
use calcraft_store::{AttachmentMeta, Collection, StoreError, User};

use crate::app::{AppState, DAV_ROOT};

const MAX_ITEM_NAME_LEN: usize = 255;

/// A resource in the DAV tree, already checked to belong to the user.
#[derive(Debug, Clone)]
pub enum Resource {
    Root,
    Principal,
    Home(CollectionKind),
    Collection(Collection),
    /// A collection path of the user that does not exist yet (MKCALENDAR).
    Unmapped(CollectionKind, Slug),
    /// An object path inside an existing collection; the object may not exist.
    Item(Collection, String),
    /// The user's scheduling inbox (RFC 6638 §2.2); only with scheduling on.
    Inbox,
    /// A message path in the inbox; the message may not exist.
    InboxItem(String),
    /// The user's scheduling outbox (RFC 6638 §2.1); only with scheduling on.
    Outbox,
    /// A managed attachment (RFC 8607) the user may read; only with
    /// attachments on. Attachments the user may not read do not resolve.
    Attachment(AttachmentMeta),
}

/// Path below `/dav/` after splitting, still unvalidated against the user.
enum Target {
    Attachment(String),
    Root,
    Principal(Username),
    Home(CollectionKind, Username),
    Collection(CollectionKind, Username, Slug),
    Item(CollectionKind, Username, Slug, String),
    Inbox(Username),
    InboxItem(Username, String),
    Outbox(Username),
}

/// Decodes `%XX` escapes. Rejects encoded slashes and invalid UTF-8.
fn percent_decode(segment: &str) -> Option<String> {
    if !segment.contains('%') {
        return Some(segment.to_owned());
    }
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = segment.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Validates an object name such as `3F2504E0-4F89.ics`.
pub fn valid_item_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ITEM_NAME_LEN
        && !name.starts_with('.')
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~' | b'@' | b'+' | b'=')
        })
}

fn parse_target(path: &str) -> Option<Target> {
    let rest = path.strip_prefix("/dav")?;
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() {
        return Some(Target::Root);
    }
    let segments: Vec<&str> = rest.split('/').collect();
    let kind = |segment: &str| match segment {
        "calendars" => Some(CollectionKind::Calendar),
        "addressbooks" => Some(CollectionKind::Addressbook),
        _ => None,
    };
    match segments.as_slice() {
        ["principals", user] => Some(Target::Principal(user.parse().ok()?)),
        ["scheduling", user, "inbox"] => Some(Target::Inbox(user.parse().ok()?)),
        ["scheduling", user, "outbox"] => Some(Target::Outbox(user.parse().ok()?)),
        ["scheduling", user, "inbox", item] => {
            let name = percent_decode(item)?;
            valid_item_name(&name).then_some(())?;
            Some(Target::InboxItem(user.parse().ok()?, name))
        }
        ["scheduling", ..] => None,
        // Exactly the form the server issues: no decoding, no variants.
        ["attachments", id] => {
            calcraft_store::valid_managed_id(id).then(|| Target::Attachment((*id).to_owned()))
        }
        ["attachments", ..] => None,
        [home, user] => Some(Target::Home(kind(home)?, user.parse().ok()?)),
        [home, user, slug] => Some(Target::Collection(
            kind(home)?,
            user.parse().ok()?,
            slug.parse().ok()?,
        )),
        [home, user, slug, item] => {
            let name = percent_decode(item)?;
            valid_item_name(&name).then_some(())?;
            Some(Target::Item(
                kind(home)?,
                user.parse().ok()?,
                slug.parse().ok()?,
                name,
            ))
        }
        _ => None,
    }
}

/// Resolves a path for the authenticated user. Foreign or invalid paths yield `None`.
pub async fn resolve(
    state: &AppState,
    user: &User,
    path: &str,
) -> Result<Option<Resource>, StoreError> {
    let own = |owner: &Username| owner == &user.username;
    // Without scheduling the inbox and outbox do not exist.
    let scheduling = state.scheduling.is_some();
    Ok(match parse_target(path) {
        None => None,
        Some(Target::Root) => Some(Resource::Root),
        Some(Target::Principal(owner)) => own(&owner).then_some(Resource::Principal),
        Some(Target::Home(kind, owner)) => own(&owner).then_some(Resource::Home(kind)),
        Some(Target::Collection(kind, owner, slug)) if own(&owner) => {
            match state.store.collection(user.id, kind, &slug).await? {
                Some(collection) => Some(Resource::Collection(collection)),
                None => Some(Resource::Unmapped(kind, slug)),
            }
        }
        Some(Target::Item(kind, owner, slug, name)) if own(&owner) => state
            .store
            .collection(user.id, kind, &slug)
            .await?
            .map(|collection| Resource::Item(collection, name)),
        Some(Target::Inbox(owner)) => (scheduling && own(&owner)).then_some(Resource::Inbox),
        Some(Target::Outbox(owner)) => (scheduling && own(&owner)).then_some(Resource::Outbox),
        Some(Target::InboxItem(owner, name)) => {
            (scheduling && own(&owner)).then_some(Resource::InboxItem(name))
        }
        Some(Target::Collection(..) | Target::Item(..)) => None,
        // Readable means: own, or referenced by one of the user's objects
        // (an invitation). Anything else looks like a missing path.
        Some(Target::Attachment(id)) if state.attachments.is_some() => state
            .store
            .readable_attachment(&id, user.id)
            .await?
            .map(Resource::Attachment),
        Some(Target::Attachment(_)) => None,
    })
}

/// Maximum length of a `Destination` header.
const MAX_DESTINATION_LEN: usize = 2048;

/// Why a `Destination` header was refused before resolving it.
#[derive(Debug, PartialEq, Eq)]
pub enum DestinationError {
    /// Not an absolute URI or absolute path, or with query, fragment, user
    /// info or dot segments (400).
    Malformed,
    /// Another scheme or host than the request: another server (502,
    /// RFC 4918 §9.8.5).
    OtherServer,
}

/// Lower-cased authority without the default port of `scheme`.
fn normalize_authority(authority: &str, scheme: Option<&str>) -> String {
    let authority = authority.to_ascii_lowercase();
    let default = match scheme {
        Some("http") => &[":80"][..],
        Some(_) => &[":443"][..],
        // The request's own scheme is not known behind a TLS-terminating
        // reverse proxy: accept either default port.
        None => &[":80", ":443"][..],
    };
    default
        .iter()
        .find_map(|port| authority.strip_suffix(port))
        .map_or(authority.clone(), str::to_owned)
}

/// The raw path of a `Destination` header (RFC 4918 §10.3), for
/// [`resolve`]. An absolute URI must name the host the request was sent to
/// (`request_authority`: the `Host` header, which the documented nginx and
/// Apache setups pass through unchanged, or the HTTP/2 authority); its
/// scheme may be `http` or `https`, since the reverse proxy terminates TLS.
/// The path is not decoded here: [`resolve`] applies exactly the rules of
/// request paths.
pub fn destination_path<'a>(
    value: &'a str,
    request_authority: Option<&str>,
) -> Result<&'a str, DestinationError> {
    if value.is_empty()
        || value.len() > MAX_DESTINATION_LEN
        || !value.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(DestinationError::Malformed);
    }
    let path = if value.starts_with('/') {
        // `//host/…` is a network-path reference, not a path.
        if value.starts_with("//") {
            return Err(DestinationError::Malformed);
        }
        value
    } else {
        let (scheme, rest) = value.split_once("://").ok_or(DestinationError::Malformed)?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(DestinationError::OtherServer);
        }
        let (authority, path) = match rest.find('/') {
            Some(i) => rest.split_at(i),
            None => (rest, "/"),
        };
        if authority.is_empty() || authority.contains(['@', '?', '#']) {
            return Err(DestinationError::Malformed);
        }
        let same = request_authority.is_some_and(|own| {
            normalize_authority(authority, Some(&scheme)) == normalize_authority(own, None)
        });
        if !same {
            return Err(DestinationError::OtherServer);
        }
        path
    };
    let dot_segment = |segment: &str| {
        let segment = segment.to_ascii_lowercase().replace("%2e", ".");
        segment == "." || segment == ".."
    };
    if path.contains(['?', '#']) || path.split('/').any(dot_segment) {
        return Err(DestinationError::Malformed);
    }
    Ok(path)
}

/// Extracts the object name from an href that must point into `collection_href`.
/// Accepts absolute URLs by ignoring scheme and host.
pub fn item_name_from_href(href: &str, collection_href: &str) -> Option<String> {
    let path = match href.split_once("://") {
        Some((_, rest)) => &rest[rest.find('/')?..],
        None => href,
    };
    let name = percent_decode(path.strip_prefix(collection_href)?)?;
    valid_item_name(&name).then_some(name)
}

pub fn principal_href(user: &Username) -> String {
    format!("{DAV_ROOT}principals/{user}/")
}

pub fn inbox_href(user: &Username) -> String {
    format!("{DAV_ROOT}scheduling/{user}/inbox/")
}

pub fn outbox_href(user: &Username) -> String {
    format!("{DAV_ROOT}scheduling/{user}/outbox/")
}

fn home_segment(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Calendar => "calendars",
        CollectionKind::Addressbook => "addressbooks",
    }
}

pub fn home_href(kind: CollectionKind, user: &Username) -> String {
    format!("{DAV_ROOT}{}/{user}/", home_segment(kind))
}

pub fn collection_href(collection: &Collection, user: &Username) -> String {
    format!("{}{}/", home_href(collection.kind, user), collection.slug)
}

pub fn item_href(collection: &Collection, user: &Username, name: &str) -> String {
    // Valid item names contain no characters that need escaping in a path.
    format!("{}{name}", collection_href(collection, user))
}

pub fn href_of(resource: &Resource, user: &Username) -> String {
    match resource {
        Resource::Root => DAV_ROOT.to_owned(),
        Resource::Principal => principal_href(user),
        Resource::Home(kind) => home_href(*kind, user),
        Resource::Collection(collection) => collection_href(collection, user),
        Resource::Unmapped(kind, slug) => format!("{}{slug}/", home_href(*kind, user)),
        Resource::Item(collection, name) => item_href(collection, user, name),
        Resource::Inbox => inbox_href(user),
        // Valid item names need no escaping.
        Resource::InboxItem(name) => format!("{}{name}", inbox_href(user)),
        Resource::Outbox => outbox_href(user),
        Resource::Attachment(meta) => attachment_href(&meta.managed_id),
    }
}

pub fn attachment_href(managed_id: &str) -> String {
    format!("{DAV_ROOT}attachments/{managed_id}")
}

#[cfg(test)]
mod tests;
