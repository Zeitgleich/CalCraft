//! REPORT: calendar-multiget and calendar-query (RFC 4791 §7.8, §7.9),
//! addressbook-multiget and addressbook-query (RFC 6352 §8.6, §8.7) and
//! sync-collection (RFC 6578).
//!
//! Responses are bounded: duplicate hrefs are dropped, the number of
//! results and the total size of returned object data are capped, and
//! object content is only loaded when it is requested. The limits are
//! checked on metadata before any content is read, so a single request
//! cannot make the server buffer gigabytes.

use std::collections::HashSet;

use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use calcraft_core::CollectionKind;
use calcraft_dav::{
    DavError, MAX_REPORT_BODY, Multistatus, Propfind, Report, Response as DavResponse,
};
use calcraft_store::{Collection, ItemMeta, StoreError};

use super::{
    Ctx, bad_request, dav_error,
    paths::{Resource, collection_href, item_href, item_name_from_href},
    props::{data_property, item_properties, respond},
    read_body, store_error, sync,
};

/// Maximum number of objects in one REPORT response.
const MAX_RESULTS: usize = 5_000;
/// Maximum total size of object data in one REPORT response.
const MAX_DATA_BYTES: i64 = 32 * 1024 * 1024;

fn wants_data(props: &Propfind, kind: CollectionKind) -> bool {
    let (ns, name) = data_property(kind);
    match props {
        Propfind::AllProp => true,
        Propfind::PropName => false,
        Propfind::Prop(names) => names.iter().any(|n| n.is(ns, name)),
    }
}

/// RFC 5323 / RFC 6578: the result would exceed the server's limits.
fn too_many_results() -> Response {
    dav_error(
        StatusCode::INSUFFICIENT_STORAGE,
        "<d:number-of-matches-within-limits/>",
    )
}

/// Reasons a REPORT fails after parsing.
enum ReportError {
    TooManyResults,
    InvalidSyncToken,
    Unsupported,
    Store(StoreError),
}

impl IntoResponse for ReportError {
    fn into_response(self) -> Response {
        match self {
            Self::TooManyResults => too_many_results(),
            Self::InvalidSyncToken => dav_error(StatusCode::FORBIDDEN, "<d:valid-sync-token/>"),
            Self::Unsupported => unsupported_report(),
            Self::Store(err) => store_error(err),
        }
    }
}

fn unsupported_report() -> Response {
    dav_error(StatusCode::FORBIDDEN, "<d:supported-report/>")
}

/// Checks the result limits on metadata only.
fn within_limits<'a>(
    metas: impl IntoIterator<Item = &'a ItemMeta>,
    with_data: bool,
    max_results: usize,
) -> bool {
    let mut count = 0usize;
    let mut bytes = 0i64;
    for meta in metas {
        count += 1;
        if with_data {
            bytes = bytes.saturating_add(meta.size);
        }
        if count > max_results || bytes > MAX_DATA_BYTES {
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
    let kind = collection.kind;
    let href = item_href(collection, &user.username, &meta.name);
    let size = usize::try_from(meta.size).unwrap_or(0);
    if !with_data {
        let available = item_properties(user, kind, &meta.etag, size, None);
        return Ok(Some(respond(&href, &available, props)));
    }
    // The object may have been deleted since the metadata query.
    let Some(item) = ctx.state.store.item(collection.id, &meta.name).await? else {
        return Ok(None);
    };
    let available = item_properties(
        user,
        kind,
        &item.etag,
        item.content.len(),
        Some(&item.content),
    );
    Ok(Some(respond(&href, &available, props)))
}

/// Appends responses for the given objects after checking the limits.
async fn respond_objects(
    ctx: Ctx<'_>,
    collection: &Collection,
    metas: &[ItemMeta],
    props: &Propfind,
    responses: &mut Vec<DavResponse>,
    max_results: usize,
) -> Result<(), ReportError> {
    let with_data = wants_data(props, collection.kind);
    if !within_limits(metas, with_data, max_results) {
        return Err(ReportError::TooManyResults);
    }
    for meta in metas {
        match object_response(ctx, collection, meta, props, with_data).await {
            Ok(Some(response)) => responses.push(response),
            Ok(None) => responses.push(DavResponse::with_status(
                item_href(collection, &ctx.user.username, &meta.name),
                404,
            )),
            Err(err) => return Err(ReportError::Store(err)),
        }
    }
    Ok(())
}

pub async fn handle(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let Resource::Collection(collection) = resource else {
        return unsupported_report();
    };
    let body = match read_body(request, MAX_REPORT_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    let report = match Report::parse(&body) {
        Ok(report) => report,
        Err(DavError::Unsupported("report" | "sync-level infinite")) => {
            return unsupported_report();
        }
        Err(err) => return bad_request(&err),
    };

    let kind = collection.kind;
    let mut responses = Vec::new();
    let mut sync_token = None;
    let result = match report {
        Report::CalendarMultiget { props, hrefs } if kind == CollectionKind::Calendar => {
            multiget(ctx, &collection, &props, hrefs, &mut responses).await
        }
        Report::AddressbookMultiget { props, hrefs } if kind == CollectionKind::Addressbook => {
            multiget(ctx, &collection, &props, hrefs, &mut responses).await
        }
        Report::CalendarQuery { props, filter } if kind == CollectionKind::Calendar => {
            match ctx
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
                Ok(metas) => {
                    respond_objects(
                        ctx,
                        &collection,
                        &metas,
                        &props,
                        &mut responses,
                        MAX_RESULTS,
                    )
                    .await
                }
                Err(err) => Err(ReportError::Store(err)),
            }
        }
        Report::AddressbookQuery { props } if kind == CollectionKind::Addressbook => {
            match ctx.state.store.items(collection.id).await {
                Ok(metas) => {
                    respond_objects(
                        ctx,
                        &collection,
                        &metas,
                        &props,
                        &mut responses,
                        MAX_RESULTS,
                    )
                    .await
                }
                Err(err) => Err(ReportError::Store(err)),
            }
        }
        Report::SyncCollection {
            token,
            props,
            limit,
        } => {
            sync_token = Some(sync::token(&collection));
            sync_collection(ctx, &collection, &token, &props, limit, &mut responses).await
        }
        _ => Err(ReportError::Unsupported),
    };
    if let Err(err) = result {
        return err.into_response();
    }
    super::multistatus(&Multistatus {
        responses,
        sync_token,
    })
}

async fn multiget(
    ctx: Ctx<'_>,
    collection: &Collection,
    props: &Propfind,
    hrefs: Vec<String>,
    responses: &mut Vec<DavResponse>,
) -> Result<(), ReportError> {
    let base = collection_href(collection, &ctx.user.username);
    // Each href is checked individually: only objects of this collection
    // are returned, everything else is a 404 entry.
    let mut seen = HashSet::new();
    let mut found: Vec<ItemMeta> = Vec::new();
    for href in hrefs {
        if !seen.insert(href.clone()) {
            continue;
        }
        let meta = match item_name_from_href(&href, &base) {
            Some(name) => ctx
                .state
                .store
                .item_meta(collection.id, &name)
                .await
                .map_err(ReportError::Store)?,
            None => None,
        };
        match meta {
            Some(meta) if !found.iter().any(|m| m.name == meta.name) => found.push(meta),
            Some(_) => {}
            None => responses.push(DavResponse::with_status(href, 404)),
        }
    }
    respond_objects(ctx, collection, &found, props, responses, MAX_RESULTS).await
}

/// RFC 6578: objects changed since the token, deletions as 404 entries.
async fn sync_collection(
    ctx: Ctx<'_>,
    collection: &Collection,
    token: &str,
    props: &Propfind,
    limit: Option<usize>,
    responses: &mut Vec<DavResponse>,
) -> Result<(), ReportError> {
    let store = &ctx.state.store;
    // The token is bound to this collection; tokens of other collections
    // (including other users') are rejected, never interpreted.
    let since = if token.is_empty() {
        None
    } else {
        match sync::revision(token, collection) {
            Some(revision) => Some(revision),
            None => return Err(ReportError::InvalidSyncToken),
        }
    };
    let max_results = limit.map_or(MAX_RESULTS, |l| l.min(MAX_RESULTS));

    let (changed, deleted) = match since {
        // Initial sync: all current objects, no deletions.
        None => (
            store
                .items(collection.id)
                .await
                .map_err(ReportError::Store)?,
            Vec::new(),
        ),
        Some(since) => {
            let changes = store
                .changes_between(collection.id, since, collection.sync_revision)
                .await
                .map_err(ReportError::Store)?;
            let mut changed = Vec::new();
            let mut deleted = Vec::new();
            for (name, is_deleted) in changes {
                if is_deleted {
                    deleted.push(name);
                    continue;
                }
                // An object deleted after the snapshot is reported by the next sync.
                if let Some(meta) = store
                    .item_meta(collection.id, &name)
                    .await
                    .map_err(ReportError::Store)?
                {
                    changed.push(meta);
                }
            }
            (changed, deleted)
        }
    };
    if changed.len() + deleted.len() > max_results {
        return Err(ReportError::TooManyResults);
    }
    respond_objects(ctx, collection, &changed, props, responses, max_results).await?;
    for name in deleted {
        responses.push(DavResponse::with_status(
            item_href(collection, &ctx.user.username, &name),
            404,
        ));
    }
    Ok(())
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
        assert!(within_limits(&small, true, MAX_RESULTS));
        let too_many = vec![meta(1); MAX_RESULTS + 1];
        assert!(!within_limits(&too_many, false, MAX_RESULTS));
        let big = vec![meta(1024 * 1024); 33];
        assert!(!within_limits(&big, true, MAX_RESULTS));
        // Without object data, sizes do not count.
        assert!(within_limits(&big, false, MAX_RESULTS));
        assert!(!within_limits(&small[..3], false, 2));
    }
}
