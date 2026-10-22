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
    props::{item_properties, properties, respond},
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
                let props = item_properties(user, collection, &item.etag, item.content.len(), None);
                responses.push(respond(&href_of(&resource, username), &props, &query));
            }
            Ok(None) => return not_found(),
            Err(err) => return store_error(err),
        },
        _ => {
            let props = properties(&resource, user, ctx.state.max_vcard_bytes);
            responses.push(respond(&href_of(&resource, username), &props, &query));
        }
    }

    if depth == Depth::One {
        match &resource {
            Resource::Home(kind) => match ctx.state.store.collections(user.id, *kind).await {
                Ok(collections) => {
                    for collection in collections {
                        let href = collection_href(&collection, username);
                        let props = properties(
                            &Resource::Collection(collection),
                            user,
                            ctx.state.max_vcard_bytes,
                        );
                        responses.push(respond(&href, &props, &query));
                    }
                }
                Err(err) => return store_error(err),
            },
            Resource::Collection(collection) => match ctx.state.store.items(collection.id).await {
                Ok(items) => {
                    for item in items {
                        let size = usize::try_from(item.size).unwrap_or(0);
                        let props = item_properties(user, collection, &item.etag, size, None);
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
