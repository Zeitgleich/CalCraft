//! DAV request handling: resource resolution, access control and PROPFIND.
//!
//! Every resource below `/dav/` belongs to exactly one user. Requests for
//! another user's resources are answered with 404, so that account names
//! cannot be probed.

use axum::{
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{
        HeaderValue, Method, StatusCode,
        header::{ALLOW, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER, WWW_AUTHENTICATE},
    },
    response::{IntoResponse, Response},
};
use calcraft_core::{CollectionKind, Slug, Username};
use calcraft_dav::{
    DavError, Depth, Element, MAX_PROPFIND_BODY, Multistatus, PropName, PropValue, Propfind,
    Response as DavResponse,
    xml::{NS_APPLE_ICAL, NS_CALDAV, NS_CALENDARSERVER, NS_CARDDAV, NS_DAV},
};
use calcraft_store::{Collection, User};

use crate::{
    app::{AppState, DAV_ROOT},
    auth::AuthOutcome,
};

const ALLOWED_METHODS: &str = "OPTIONS, PROPFIND";
/// DAV compliance classes. Calendar and address book access are advertised
/// once the corresponding reports are implemented.
const DAV_CLASSES: &str = "1";
const WWW_AUTHENTICATE_VALUE: &str = "Basic realm=\"CalCraft\", charset=\"UTF-8\"";
const XML_CONTENT_TYPE: &str = "application/xml; charset=utf-8";

/// Entry point for every request below `/dav`.
pub async fn handle(State(state): State<AppState>, request: Request) -> Response {
    if request.method() == Method::OPTIONS {
        return options();
    }

    let header = request
        .headers()
        .get(AUTHORIZATION)
        .map(HeaderValue::as_bytes);
    let user = match state.auth.authenticate(header).await {
        AuthOutcome::Authenticated(user) => user,
        AuthOutcome::Denied => return unauthorized(),
        AuthOutcome::LockedOut => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(RETRY_AFTER, "900")],
                "too many failed login attempts",
            )
                .into_response();
        }
        AuthOutcome::Error => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    if request.method().as_str() == "PROPFIND" {
        propfind(&state, &user, request).await
    } else {
        (StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, ALLOWED_METHODS)]).into_response()
    }
}

fn options() -> Response {
    (
        StatusCode::OK,
        [("dav", DAV_CLASSES), (ALLOW.as_str(), ALLOWED_METHODS)],
    )
        .into_response()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(WWW_AUTHENTICATE, WWW_AUTHENTICATE_VALUE)],
        "authentication required",
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn bad_request(err: &DavError) -> Response {
    match err {
        DavError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        DavError::BadRequest(reason) => (StatusCode::BAD_REQUEST, *reason).into_response(),
    }
}

/// A resource in the DAV tree.
#[derive(Debug, Clone)]
enum Resource {
    Root,
    Principal,
    Home(CollectionKind),
    Collection(Collection),
}

/// Path below `/dav/` after splitting, still unvalidated.
enum Target {
    Root,
    Principal(Username),
    Home(CollectionKind, Username),
    Collection(CollectionKind, Username, Slug),
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
        _ => None,
    }
}

fn principal_href(user: &Username) -> String {
    format!("{DAV_ROOT}principals/{user}/")
}

fn home_segment(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Calendar => "calendars",
        CollectionKind::Addressbook => "addressbooks",
    }
}

fn home_href(kind: CollectionKind, user: &Username) -> String {
    format!("{DAV_ROOT}{}/{user}/", home_segment(kind))
}

fn collection_href(collection: &Collection, user: &Username) -> String {
    format!("{}{}/", home_href(collection.kind, user), collection.slug)
}

/// Resolves a path for the authenticated user. Foreign or unknown paths yield `None`.
async fn resolve(
    state: &AppState,
    user: &User,
    path: &str,
) -> Result<Option<Resource>, calcraft_store::StoreError> {
    let owned_by_user = |owner: &Username| owner == &user.username;
    let resource = match parse_target(path) {
        None => None,
        Some(Target::Root) => Some(Resource::Root),
        Some(Target::Principal(owner)) => owned_by_user(&owner).then_some(Resource::Principal),
        Some(Target::Home(kind, owner)) => owned_by_user(&owner).then_some(Resource::Home(kind)),
        Some(Target::Collection(kind, owner, slug)) if owned_by_user(&owner) => state
            .store
            .collection(user.id, kind, &slug)
            .await?
            .map(Resource::Collection),
        Some(Target::Collection(..)) => None,
    };
    Ok(resource)
}

fn internal_error(err: calcraft_store::StoreError) -> Response {
    tracing::error!(%err, "storage error");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

async fn propfind(state: &AppState, user: &User, request: Request) -> Response {
    let depth = match Depth::parse(request.headers().get("depth").map(HeaderValue::as_bytes)) {
        Ok(Depth::Infinity) => {
            return (
                StatusCode::FORBIDDEN,
                [(CONTENT_TYPE, XML_CONTENT_TYPE)],
                r#"<?xml version="1.0" encoding="utf-8"?><d:error xmlns:d="DAV:"><d:propfind-finite-depth/></d:error>"#,
            )
                .into_response();
        }
        Ok(depth) => depth,
        Err(err) => return bad_request(&err),
    };
    let path = request.uri().path().to_owned();
    let body = match to_bytes(request.into_body(), MAX_PROPFIND_BODY).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let query = match Propfind::parse(&body) {
        Ok(query) => query,
        Err(err) => return bad_request(&err),
    };

    let resource = match resolve(state, user, &path).await {
        Ok(Some(resource)) => resource,
        Ok(None) => return not_found(),
        Err(err) => return internal_error(err),
    };

    let mut resources = vec![(href_of(&resource, &user.username), resource.clone())];
    if depth == Depth::One
        && let Resource::Home(kind) = resource
    {
        match state.store.collections(user.id, kind).await {
            Ok(collections) => resources.extend(collections.into_iter().map(|collection| {
                (
                    collection_href(&collection, &user.username),
                    Resource::Collection(collection),
                )
            })),
            Err(err) => return internal_error(err),
        }
    }

    let multistatus = Multistatus {
        responses: resources
            .iter()
            .map(|(href, resource)| respond(href, &properties(resource, user), &query))
            .collect(),
    };
    (
        StatusCode::MULTI_STATUS,
        [(CONTENT_TYPE, XML_CONTENT_TYPE)],
        Body::from(multistatus.to_xml()),
    )
        .into_response()
}

fn href_of(resource: &Resource, user: &Username) -> String {
    match resource {
        Resource::Root => DAV_ROOT.to_owned(),
        Resource::Principal => principal_href(user),
        Resource::Home(kind) => home_href(*kind, user),
        Resource::Collection(collection) => collection_href(collection, user),
    }
}

fn respond(href: &str, available: &[(PropName, PropValue)], query: &Propfind) -> DavResponse {
    let mut response = DavResponse::new(href);
    match query {
        Propfind::AllProp => response.found = available.to_vec(),
        Propfind::PropName => {
            response.found = available
                .iter()
                .map(|(name, _)| (name.clone(), PropValue::Empty))
                .collect();
        }
        Propfind::Prop(requested) => {
            for name in requested {
                match available.iter().find(|(candidate, _)| candidate == name) {
                    Some(found) => response.found.push(found.clone()),
                    None => response.not_found.push(name.clone()),
                }
            }
        }
    }
    response
}

fn prop(namespace: &str, name: &str, value: PropValue) -> (PropName, PropValue) {
    (PropName::new(namespace, name), value)
}

fn resourcetype(types: &[(&'static str, &'static str)]) -> (PropName, PropValue) {
    prop(
        NS_DAV,
        "resourcetype",
        PropValue::Elements(
            types
                .iter()
                .map(|(ns, name)| Element::new(ns, name))
                .collect(),
        ),
    )
}

/// Privileges of the owner on their own resources.
fn owner_privileges() -> PropValue {
    PropValue::Elements(
        [
            "read",
            "write",
            "write-properties",
            "write-content",
            "bind",
            "unbind",
            "read-current-user-privilege-set",
        ]
        .into_iter()
        .map(|privilege| Element::new(NS_DAV, "privilege").child(Element::new(NS_DAV, privilege)))
        .collect(),
    )
}

/// All live properties of a resource.
fn properties(resource: &Resource, user: &User) -> Vec<(PropName, PropValue)> {
    let username = &user.username;
    let principal = principal_href(username);
    let mut props = vec![
        prop(
            NS_DAV,
            "current-user-principal",
            PropValue::Href(principal.clone()),
        ),
        prop(NS_DAV, "current-user-privilege-set", owner_privileges()),
    ];
    match resource {
        Resource::Root => props.push(resourcetype(&[(NS_DAV, "collection")])),
        Resource::Principal => {
            props.extend([
                resourcetype(&[(NS_DAV, "collection"), (NS_DAV, "principal")]),
                prop(
                    NS_DAV,
                    "displayname",
                    PropValue::Text(
                        user.display_name
                            .clone()
                            .unwrap_or_else(|| username.to_string()),
                    ),
                ),
                prop(NS_DAV, "principal-URL", PropValue::Href(principal.clone())),
                prop(
                    NS_CALDAV,
                    "calendar-home-set",
                    PropValue::Href(home_href(CollectionKind::Calendar, username)),
                ),
                prop(
                    NS_CARDDAV,
                    "addressbook-home-set",
                    PropValue::Href(home_href(CollectionKind::Addressbook, username)),
                ),
            ]);
        }
        Resource::Home(_) => {
            props.extend([
                resourcetype(&[(NS_DAV, "collection")]),
                prop(NS_DAV, "owner", PropValue::Href(principal)),
            ]);
        }
        Resource::Collection(collection) => {
            let (kind_ns, kind_name, description_ns, description_name) = match collection.kind {
                CollectionKind::Calendar => {
                    (NS_CALDAV, "calendar", NS_CALDAV, "calendar-description")
                }
                CollectionKind::Addressbook => (
                    NS_CARDDAV,
                    "addressbook",
                    NS_CARDDAV,
                    "addressbook-description",
                ),
            };
            props.extend([
                resourcetype(&[(NS_DAV, "collection"), (kind_ns, kind_name)]),
                prop(NS_DAV, "owner", PropValue::Href(principal)),
                prop(
                    NS_CALENDARSERVER,
                    "getctag",
                    PropValue::Text(collection.sync_revision.to_string()),
                ),
            ]);
            if let Some(name) = &collection.display_name {
                props.push(prop(NS_DAV, "displayname", PropValue::Text(name.clone())));
            }
            if let Some(description) = &collection.description {
                props.push(prop(
                    description_ns,
                    description_name,
                    PropValue::Text(description.clone()),
                ));
            }
            if collection.kind == CollectionKind::Calendar {
                props.push(prop(
                    NS_CALDAV,
                    "supported-calendar-component-set",
                    PropValue::Elements(
                        ["VEVENT", "VTODO"]
                            .into_iter()
                            .map(|comp| Element::new(NS_CALDAV, "comp").attr("name", comp))
                            .collect(),
                    ),
                ));
                if let Some(color) = &collection.color {
                    props.push(prop(
                        NS_APPLE_ICAL,
                        "calendar-color",
                        PropValue::Text(color.clone()),
                    ));
                }
            }
        }
    }
    props
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
            "/davx/",
        ] {
            assert!(parse_target(path).is_none(), "{path}");
        }
    }
}
