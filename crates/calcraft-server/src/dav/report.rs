//! REPORT: calendar-multiget and calendar-query (RFC 4791 §7.8, §7.9).

use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use calcraft_core::CollectionKind;
use calcraft_dav::{DavError, MAX_REPORT_BODY, Multistatus, Report, Response as DavResponse};

use super::{
    Ctx, bad_request, dav_error,
    paths::{Resource, collection_href, item_href, item_name_from_href},
    props::{item_properties, respond},
    read_body, store_error,
};

pub async fn handle(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let Resource::Collection(collection) = resource else {
        return dav_error(StatusCode::FORBIDDEN, "<d:supported-report/>");
    };
    if collection.kind != CollectionKind::Calendar {
        return dav_error(StatusCode::FORBIDDEN, "<d:supported-report/>");
    }
    let body = match read_body(request, MAX_REPORT_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    let report = match Report::parse(&body) {
        Ok(report) => report,
        Err(DavError::Unsupported("report")) => {
            return dav_error(StatusCode::FORBIDDEN, "<d:supported-report/>");
        }
        Err(err) => return bad_request(&err),
    };

    let user = ctx.user;
    let username = &user.username;
    let base = collection_href(&collection, username);
    let mut responses = Vec::new();
    match report {
        Report::CalendarMultiget { props, hrefs } => {
            for href in hrefs {
                // Each href is checked individually: only objects of this
                // collection are returned, everything else is a 404 entry.
                let Some(name) = item_name_from_href(&href, &base) else {
                    responses.push(DavResponse::with_status(href, 404));
                    continue;
                };
                match ctx.state.store.item(collection.id, &name).await {
                    Ok(Some(item)) => {
                        let available = item_properties(
                            user,
                            &item.etag,
                            item.content.len(),
                            Some(&item.content),
                        );
                        responses.push(respond(
                            &item_href(&collection, username, &name),
                            &available,
                            &props,
                        ));
                    }
                    Ok(None) => responses.push(DavResponse::with_status(href, 404)),
                    Err(err) => return store_error(err),
                }
            }
        }
        Report::CalendarQuery { props, filter } => {
            let items = ctx
                .state
                .store
                .query_items(
                    collection.id,
                    filter.component.map(|kind| kind.as_str()),
                    filter.start,
                    filter.end,
                )
                .await;
            match items {
                Ok(items) => {
                    for item in items {
                        let available = item_properties(
                            user,
                            &item.etag,
                            item.content.len(),
                            Some(&item.content),
                        );
                        responses.push(respond(
                            &item_href(&collection, username, &item.name),
                            &available,
                            &props,
                        ));
                    }
                }
                Err(err) => return store_error(err),
            }
        }
    }
    super::multistatus(&Multistatus { responses })
}
