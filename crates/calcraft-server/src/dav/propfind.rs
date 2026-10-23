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
    props::{AttachmentLimits, dead_properties, item_properties, properties, respond},
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
    let limits = ctx.state.attachments.as_ref().map(|a| AttachmentLimits {
        max_size: a.config.max_bytes,
        max_per_resource: a.config.max_per_object,
    });
    let mut responses = Vec::new();
    match &resource {
        Resource::Unmapped(..) => return not_found(),
        // Routed to `sharing::notifications` before.
        Resource::Notifications | Resource::Notification(_) => return not_found(),
        Resource::Inbox | Resource::Outbox | Resource::InboxItem(_) => {
            return match super::scheduling::propfind(ctx, &resource, &query, depth == Depth::One)
                .await
            {
                Ok(responses) => multistatus(&Multistatus {
                    responses,
                    sync_token: None,
                }),
                Err(response) => response,
            };
        }
        Resource::Item(collection, name) => match ctx.state.store.item(collection.id, name).await {
            Ok(Some(item)) => {
                let item = super::sharing::sharee_item(collection, item);
                let props = item_properties(user, collection, &item.etag, item.content.len(), None);
                responses.push(respond(&href_of(&resource, username), &props, &query));
            }
            Ok(None) => return not_found(),
            Err(err) => return store_error(err),
        },
        _ => {
            let mut props = properties(&resource, user, ctx.state.max_vcard_bytes, limits);
            if let (Resource::Principal, Some(scheduling)) = (&resource, &ctx.state.scheduling) {
                match scheduling.address_set(&ctx.state.store, user).await {
                    Ok(addresses) => {
                        props.extend(super::scheduling::principal_properties(user, &addresses));
                    }
                    Err(err) => return store_error(err),
                }
            }
            if let Resource::Principal = &resource
                && ctx.state.sharing
            {
                props.push(super::sharing::principal_properties(user));
            }
            if let Resource::Collection(collection) = &resource {
                let store = &ctx.state.store;
                let shares = if collection.shared.is_none() && ctx.state.sharing {
                    match store.shares_of_collection(collection.id).await {
                        Ok(shares) => shares,
                        Err(err) => return store_error(err),
                    }
                } else {
                    Vec::new()
                };
                if ctx.state.sharing {
                    let shares: Vec<_> = shares.iter().collect();
                    super::sharing::collection_properties(&mut props, collection, &shares, user);
                }
                match store.dead_properties(collection.id).await {
                    Ok(stored) if collection.shared.is_some() => {
                        let visible = super::sharing::sharee_dead_properties(collection, stored);
                        props.extend(dead_properties(collection.id, &visible));
                    }
                    Ok(stored) => props.extend(dead_properties(collection.id, &stored)),
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
                let sharing = ctx.state.sharing;
                let (collections, stored) = match (
                    store.collections(user.id, *kind).await,
                    store.dead_properties_of_owner(user.id, *kind).await,
                ) {
                    (Ok(collections), Ok(stored)) => (collections, stored),
                    (Err(err), _) | (_, Err(err)) => return store_error(err),
                };
                let (shares, shared) = if sharing {
                    match (
                        store.shares_of_owner(user.id, *kind).await,
                        store.shared_collections(user.id, *kind).await,
                    ) {
                        (Ok(shares), Ok(shared)) => (shares, shared),
                        (Err(err), _) | (_, Err(err)) => return store_error(err),
                    }
                } else {
                    (Vec::new(), Vec::new())
                };
                for collection in collections {
                    let href = collection_href(&collection, username);
                    let id = collection.id;
                    let own_shares = super::sharing::shares_of(&shares, &collection);
                    let resource = Resource::Collection(collection);
                    let mut props = properties(&resource, user, ctx.state.max_vcard_bytes, limits);
                    if let (true, Resource::Collection(collection)) = (sharing, &resource) {
                        super::sharing::collection_properties(
                            &mut props,
                            collection,
                            &own_shares,
                            user,
                        );
                    }
                    props.extend(dead_properties(
                        id,
                        stored.iter().filter(|(c, _)| *c == id).map(|(_, p)| p),
                    ));
                    responses.push(respond(&href, &props, &query));
                }
                // Collections shared with the user, after their own.
                for collection in shared {
                    let href = collection_href(&collection, username);
                    let stored = match store.dead_properties(collection.id).await {
                        Ok(stored) => stored,
                        Err(err) => return store_error(err),
                    };
                    let visible = super::sharing::sharee_dead_properties(&collection, stored);
                    let id = collection.id;
                    let resource = Resource::Collection(collection);
                    let mut props = properties(&resource, user, ctx.state.max_vcard_bytes, limits);
                    if let Resource::Collection(collection) = &resource {
                        super::sharing::collection_properties(&mut props, collection, &[], user);
                    }
                    props.extend(dead_properties(id, &visible));
                    responses.push(respond(&href, &props, &query));
                }
            }
            Resource::Collection(collection) => match ctx.state.store.items(collection.id).await {
                Ok(items) => {
                    for mut item in items {
                        // Read-only sharees: ETag and length of the stubs of
                        // private events.
                        if super::sharing::hides_private(collection) {
                            match ctx.state.store.item(collection.id, &item.name).await {
                                Ok(Some(full)) => {
                                    let seen = super::sharing::sharee_item(collection, full);
                                    item.etag = seen.etag;
                                    item.size = i64::try_from(seen.content.len()).unwrap_or(0);
                                }
                                Ok(None) => continue,
                                Err(err) => return store_error(err),
                            }
                        }
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
