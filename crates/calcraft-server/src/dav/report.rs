//! REPORT: calendar-multiget and calendar-query (RFC 4791 §7.8, §7.9).
//!
//! Responses are bounded: duplicate hrefs are dropped, the number of
//! results and the total size of returned calendar data are capped, and
//! object content is only loaded when `calendar-data` is requested. The
//! limits are checked on metadata before any content is read, so a single
//! request cannot make the server buffer gigabytes.

use std::collections::HashSet;

use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use calcraft_core::CollectionKind;
use calcraft_dav::{
    DavError, MAX_REPORT_BODY, Multistatus, Propfind, Report, Response as DavResponse,
    xml::NS_CALDAV,
};
use calcraft_store::{Collection, ItemMeta, StoreError};

use super::{
    Ctx, bad_request, dav_error,
    paths::{Resource, collection_href, item_href, item_name_from_href},
    props::{item_properties, respond},
    read_body, store_error,
};

/// Maximum number of objects in one REPORT response.
const MAX_RESULTS: usize = 5_000;
/// Maximum total size of `calendar-data` in one REPORT response.
const MAX_DATA_BYTES: i64 = 32 * 1024 * 1024;

fn wants_data(props: &Propfind) -> bool {
    match props {
        Propfind::AllProp => true,
        Propfind::PropName => false,
        Propfind::Prop(names) => names.iter().any(|n| n.is(NS_CALDAV, "calendar-data")),
    }
}

/// RFC 5323 / RFC 4791: the result would exceed the server's limits.
fn too_many_results() -> Response {
    dav_error(
        StatusCode::INSUFFICIENT_STORAGE,
        "<d:number-of-matches-within-limits/>",
    )
}

/// Checks the result limits on metadata only.
fn within_limits<'a>(metas: impl IntoIterator<Item = &'a ItemMeta>, with_data: bool) -> bool {
    let mut count = 0usize;
    let mut bytes = 0i64;
    for meta in metas {
        count += 1;
        if with_data {
            bytes = bytes.saturating_add(meta.size);
        }
        if count > MAX_RESULTS || bytes > MAX_DATA_BYTES {
            return false;
        }
    }
    true
}

/// Builds the response entry for one object, loading content only if needed.
async fn object_response(
    ctx: Ctx<'_>,
    collection: &Collection,
    meta: &ItemMeta,
    props: &Propfind,
    with_data: bool,
) -> Result<Option<DavResponse>, StoreError> {
    let user = ctx.user;
    let href = item_href(collection, &user.username, &meta.name);
    let size = usize::try_from(meta.size).unwrap_or(0);
    if !with_data {
        let available = item_properties(user, &meta.etag, size, None);
        return Ok(Some(respond(&href, &available, props)));
    }
    // The object may have been deleted since the metadata query.
    let Some(item) = ctx.state.store.item(collection.id, &meta.name).await? else {
        return Ok(None);
    };
    let available = item_properties(user, &item.etag, item.content.len(), Some(&item.content));
    Ok(Some(respond(&href, &available, props)))
}

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

    let base = collection_href(&collection, &ctx.user.username);
    let mut responses = Vec::new();
    match report {
        Report::CalendarMultiget { props, hrefs } => {
            let with_data = wants_data(&props);
            // Each href is checked individually: only objects of this
            // collection are returned, everything else is a 404 entry.
            let mut seen = HashSet::new();
            let mut found: Vec<ItemMeta> = Vec::new();
            for href in hrefs {
                if !seen.insert(href.clone()) {
                    continue;
                }
                let meta = match item_name_from_href(&href, &base) {
                    Some(name) => match ctx.state.store.item_meta(collection.id, &name).await {
                        Ok(meta) => meta,
                        Err(err) => return store_error(err),
                    },
                    None => None,
                };
                match meta {
                    Some(meta) if !found.iter().any(|m| m.name == meta.name) => found.push(meta),
                    Some(_) => {}
                    None => responses.push(DavResponse::with_status(href, 404)),
                }
            }
            if !within_limits(&found, with_data) {
                return too_many_results();
            }
            for meta in &found {
                match object_response(ctx, &collection, meta, &props, with_data).await {
                    Ok(Some(response)) => responses.push(response),
                    Ok(None) => responses.push(DavResponse::with_status(
                        item_href(&collection, &ctx.user.username, &meta.name),
                        404,
                    )),
                    Err(err) => return store_error(err),
                }
            }
        }
        Report::CalendarQuery { props, filter } => {
            let with_data = wants_data(&props);
            let metas = match ctx
                .state
                .store
                .query_item_meta(
                    collection.id,
                    filter.component.map(|kind| kind.as_str()),
                    filter.start,
                    filter.end,
                )
                .await
            {
                Ok(metas) => metas,
                Err(err) => return store_error(err),
            };
            if !within_limits(&metas, with_data) {
                return too_many_results();
            }
            for meta in &metas {
                match object_response(ctx, &collection, meta, &props, with_data).await {
                    Ok(Some(response)) => responses.push(response),
                    Ok(None) => {}
                    Err(err) => return store_error(err),
                }
            }
        }
    }
    super::multistatus(&Multistatus { responses })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(size: i64) -> ItemMeta {
        ItemMeta {
            name: "x.ics".into(),
            etag: "\"e\"".into(),
            size,
        }
    }

    #[test]
    fn limits_count_and_bytes() {
        let small = vec![meta(1); MAX_RESULTS];
        assert!(within_limits(&small, true));
        let too_many = vec![meta(1); MAX_RESULTS + 1];
        assert!(!within_limits(&too_many, false));
        let big = vec![meta(1024 * 1024); 33];
        assert!(!within_limits(&big, true));
        // Without calendar-data, sizes do not count.
        assert!(within_limits(&big, false));
    }
}
