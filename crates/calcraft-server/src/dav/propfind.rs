//! PROPFIND (RFC 4918 §9.1).

use axum::{
    extract::Request,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use calcraft_dav::{Depth, MAX_PROPFIND_BODY, Multistatus, Propfind};

use super::{
    Ctx, bad_request, dav_error, multistatus, not_found,
    paths::{Resource, collection_href, href_of, item_href},
    props::{dead_properties, item_properties, properties, respond},
    read_body, store_error,
};

pub async fn handle(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let depth = match Depth::parse(request.headers().get("depth").map(HeaderValue::as_bytes)) {
        Ok(Depth::Infinity) => {
            return dav_error(StatusCode::FORBIDDEN, "<d:propfind-finite-depth/>");
        }
        Ok(depth) => depth,
        Err(err) => return bad_request(&err),
    };
    let body = match read_body(request, MAX_PROPFIND_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    let query = match Propfind::parse(&body) {
        Ok(query) => query,
        Err(err) => return bad_request(&err),
    };

    let user = ctx.user;
    let username = &user.username;
    let mut responses = Vec::new();
    match &resource {
        Resource::Unmapped(..) => return not_found(),
        Resource::Item(collection, name) => match ctx.state.store.item(collection.id, name).await {
            Ok(Some(item)) => {
                let props =
                    item_properties(user, collection.kind, &item.etag, item.content.len(), None);
                responses.push(respond(&href_of(&resource, username), &props, &query));
            }
            Ok(None) => return not_found(),
            Err(err) => return store_error(err),
        },
        _ => {
            let mut props = properties(&resource, user, ctx.state.max_vcard_bytes);
            if let Resource::Collection(collection) = &resource {
                match ctx.state.store.dead_properties(collection.id).await {
                    Ok(stored) => props.extend(dead_properties(&stored)),
                    Err(err) => return store_error(err),
                }
            }
            responses.push(respond(&href_of(&resource, username), &props, &query));
        }
    }

    if depth == Depth::One {
        match &resource {
            Resource::Home(kind) => {
                let store = &ctx.state.store;
                let (collections, stored) = match (
                    store.collections(user.id, *kind).await,
                    store.dead_properties_of_owner(user.id, *kind).await,
                ) {
                    (Ok(collections), Ok(stored)) => (collections, stored),
                    (Err(err), _) | (_, Err(err)) => return store_error(err),
                };
                for collection in collections {
                    let href = collection_href(&collection, username);
                    let id = collection.id;
                    let mut props = properties(
                        &Resource::Collection(collection),
                        user,
                        ctx.state.max_vcard_bytes,
                    );
                    props.extend(dead_properties(
                        stored.iter().filter(|(c, _)| *c == id).map(|(_, p)| p),
                    ));
                    responses.push(respond(&href, &props, &query));
                }
            }
            Resource::Collection(collection) => match ctx.state.store.items(collection.id).await {
                Ok(items) => {
                    for item in items {
                        let size = usize::try_from(item.size).unwrap_or(0);
                        let props = item_properties(user, collection.kind, &item.etag, size, None);
                        responses.push(respond(
                            &item_href(collection, username, &item.name),
                            &props,
                            &query,
                        ));
                    }
                }
                Err(err) => return store_error(err),
            },
            _ => {}
        }
    }

    multistatus(&Multistatus {
        responses,
        sync_token: None,
    })
}
