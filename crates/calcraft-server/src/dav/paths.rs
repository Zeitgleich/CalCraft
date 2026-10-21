//! Mapping between request paths, hrefs and resources.
//!
//! This is the central authorization point of the DAV tree: a path only
//! resolves to a resource if it belongs to the authenticated user. Every
//! handler starts from a resolved resource, and every storage query takes
//! the id of a collection obtained here.

use calcraft_core::{CollectionKind, Slug, Username};
use calcraft_store::{Collection, StoreError, User};

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
}

/// Path below `/dav/` after splitting, still unvalidated against the user.
enum Target {
    Root,
    Principal(Username),
    Home(CollectionKind, Username),
    Collection(CollectionKind, Username, Slug),
    Item(CollectionKind, Username, Slug, String),
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
        Some(Target::Collection(..) | Target::Item(..)) => None,
    })
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets() {
        assert!(matches!(parse_target("/dav"), Some(Target::Root)));
        assert!(matches!(parse_target("/dav/"), Some(Target::Root)));
        assert!(matches!(
            parse_target("/dav/principals/alice/"),
            Some(Target::Principal(_))
        ));
        assert!(matches!(
            parse_target("/dav/calendars/alice"),
            Some(Target::Home(CollectionKind::Calendar, _))
        ));
        assert!(matches!(
            parse_target("/dav/addressbooks/alice/default/"),
            Some(Target::Collection(CollectionKind::Addressbook, _, _))
        ));
        let Some(Target::Item(_, _, _, name)) =
            parse_target("/dav/calendars/alice/default/a%40b.ics")
        else {
            panic!("expected item");
        };
        assert_eq!(name, "a@b.ics");
    }

    #[test]
    fn rejects_malformed_targets() {
        for path in [
            "/dav/principals",
            "/dav/principals/../admin/",
            "/dav/calendars/alice/../bob/",
            "/dav/calendars//default/",
            "/dav/calendars/Alice/",
            "/dav/calendars/alice/%2e%2e/",
            "/dav/other/alice/",
            "/dav/calendars/alice/default/extra/item/",
            "/dav/calendars/alice/default/..",
            "/dav/calendars/alice/default/.hidden.ics",
            "/dav/calendars/alice/default/..%2fx.ics",
            "/dav/calendars/alice/default/%252e%252e%252f",
            "/dav/calendars/alice/default/a%5cb.ics",
            "/dav/calendars/alice/default/c:%2fx.ics",
            "/dav/calendars/alice/default/a%00.ics",
            "/dav/calendars/alice/default/a%zz.ics",
            "/dav/calendars/alice/default/a%e4.ics",
            "/davx/",
        ] {
            assert!(parse_target(path).is_none(), "{path}");
        }
        let long = format!("/dav/calendars/alice/default/{}.ics", "a".repeat(300));
        assert!(parse_target(&long).is_none());
    }

    #[test]
    fn extracts_item_names_from_hrefs() {
        let base = "/dav/calendars/alice/default/";
        assert_eq!(
            item_name_from_href("/dav/calendars/alice/default/x.ics", base).as_deref(),
            Some("x.ics")
        );
        assert_eq!(
            item_name_from_href(
                "https://cal.example.com/dav/calendars/alice/default/x.ics",
                base
            )
            .as_deref(),
            Some("x.ics")
        );
        for href in [
            "/dav/calendars/bob/default/x.ics",
            "/dav/calendars/alice/default/sub/x.ics",
            "/dav/calendars/alice/default/../../bob/default/x.ics",
            "/dav/calendars/alice/other/x.ics",
            "/dav/calendars/alice/default/",
        ] {
            assert!(item_name_from_href(href, base).is_none(), "{href}");
        }
    }
}
