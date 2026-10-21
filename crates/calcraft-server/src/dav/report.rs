//! REPORT: calendar-multiget and calendar-query (RFC 4791 §7.8, §7.9),
//! addressbook-multiget and addressbook-query (RFC 6352 §8.6, §8.7) and
//! sync-collection (RFC 6578).
//!
//! Responses are bounded. Every storage query carries a row limit, the
//! number of results and the total size of returned object data are capped,
//! and object content is only loaded when it is requested. Queries and
//! sync-collection return truncated results with a 507 entry for the
//! collection (RFC 6578 §3.6, RFC 4791 §7.9); sync clients continue with
//! the returned token. Multiget fails as a whole instead, because the
//! client asked for specific objects.

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

/// Reasons a REPORT fails after parsing.
enum ReportError {
    TooManyResults,
    InvalidSyncToken,
    Unsupported,
    Store(StoreError),
}

impl From<StoreError> for ReportError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

impl IntoResponse for ReportError {
    fn into_response(self) -> Response {
        match self {
            Self::TooManyResults => dav_error(
                StatusCode::INSUFFICIENT_STORAGE,
                "<d:number-of-matches-within-limits/>",
            ),
            Self::InvalidSyncToken => dav_error(StatusCode::FORBIDDEN, "<d:valid-sync-token/>"),
            Self::Unsupported => dav_error(StatusCode::FORBIDDEN, "<d:supported-report/>"),
            Self::Store(err) => store_error(err),
        }
    }
}

fn wants_data(props: &Propfind, kind: CollectionKind) -> bool {
    let (ns, name) = data_property(kind);
    match props {
        Propfind::AllProp => true,
        Propfind::PropName => false,
        Propfind::Prop(names) => names.iter().any(|n| n.is(ns, name)),
    }
}

/// Counts results and returned bytes against the limits.
struct Budget {
    max_results: usize,
    with_data: bool,
    count: usize,
    bytes: i64,
}

impl Budget {
    fn new(max_results: usize, with_data: bool) -> Self {
        Self {
            max_results,
            with_data,
            count: 0,
            bytes: 0,
        }
    }

    /// Reserves room for one more result; `false` if a limit would be exceeded.
    fn take(&mut self, meta: Option<&ItemMeta>) -> bool {
        let bytes = match meta {
            Some(meta) if self.with_data => self.bytes.saturating_add(meta.size),
            _ => self.bytes,
        };
        if self.count >= self.max_results || bytes > MAX_DATA_BYTES {
            return false;
        }
        self.count += 1;
        self.bytes = bytes;
        true
    }
}

/// Builds the response entry for one object, loading content only if
/// needed. `None` if the object vanished since its metadata was read.
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

fn truncated(collection: &Collection, ctx: Ctx<'_>) -> DavResponse {
    DavResponse::with_status(collection_href(collection, &ctx.user.username), 507)
}

/// Responses for a query result, truncated at the limits with a 507 entry.
async fn query_responses(
    ctx: Ctx<'_>,
    collection: &Collection,
    metas: &[ItemMeta],
    props: &Propfind,
    responses: &mut Vec<DavResponse>,
) -> Result<(), ReportError> {
    let with_data = wants_data(props, collection.kind);
    let mut budget = Budget::new(MAX_RESULTS, with_data);
    for meta in metas {
        if !budget.take(Some(meta)) {
            responses.push(truncated(collection, ctx));
            return Ok(());
        }
        if let Some(response) = object_response(ctx, collection, meta, props, with_data).await? {
            responses.push(response);
        }
    }
    Ok(())
}

/// Row limit for storage queries: one more than can be returned, so that
/// truncation is detected without loading everything.
fn row_limit(max: usize) -> u32 {
    u32::try_from(max.saturating_add(1)).unwrap_or(u32::MAX)
}

pub async fn handle(ctx: Ctx<'_>, resource: Resource, request: Request) -> Response {
    let Resource::Collection(collection) = resource else {
        return ReportError::Unsupported.into_response();
    };
    let body = match read_body(request, MAX_REPORT_BODY).await {
        Ok(body) => body,
        Err(err) => return err.into_response(),
    };
    let report = match Report::parse(&body) {
        Ok(report) => report,
        Err(DavError::Unsupported("report" | "sync-level infinite")) => {
            return ReportError::Unsupported.into_response();
        }
        Err(err) => return bad_request(&err),
    };

    let kind = collection.kind;
    let mut responses = Vec::new();
    let mut sync_token = None;
    let store = &ctx.state.store;
    let result = match report {
        Report::CalendarMultiget { props, hrefs } if kind == CollectionKind::Calendar => {
            multiget(ctx, &collection, &props, hrefs, &mut responses).await
        }
        Report::AddressbookMultiget { props, hrefs } if kind == CollectionKind::Addressbook => {
            multiget(ctx, &collection, &props, hrefs, &mut responses).await
        }
        Report::CalendarQuery { props, filter } if kind == CollectionKind::Calendar => {
            match store
                .query_item_meta(
                    collection.id,
                    filter.component.map(|kind| kind.as_str()),
                    filter.start,
                    filter.end,
                    row_limit(MAX_RESULTS),
                )
                .await
            {
                Ok(metas) => {
                    query_responses(ctx, &collection, &metas, &props, &mut responses).await
                }
                Err(err) => Err(err.into()),
            }
        }
        Report::AddressbookQuery { props } if kind == CollectionKind::Addressbook => {
            match store
                .items_limited(collection.id, row_limit(MAX_RESULTS))
                .await
            {
                Ok(metas) => {
                    query_responses(ctx, &collection, &metas, &props, &mut responses).await
                }
                Err(err) => Err(err.into()),
            }
        }
        Report::SyncCollection {
            token,
            props,
            limit,
        } => sync_collection(ctx, &collection, &token, &props, limit, &mut responses)
            .await
            .map(|token| sync_token = Some(token)),
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
    let with_data = wants_data(props, collection.kind);
    // Each href is checked individually: only objects of this collection
    // are returned, everything else is a 404 entry.
    let mut seen = HashSet::new();
    let mut found: Vec<ItemMeta> = Vec::new();
    for href in hrefs {
        if !seen.insert(href.clone()) {
            continue;
        }
        let meta = match item_name_from_href(&href, &base) {
            Some(name) => ctx.state.store.item_meta(collection.id, &name).await?,
            None => None,
        };
        match meta {
            Some(meta) if !found.iter().any(|m| m.name == meta.name) => found.push(meta),
            Some(_) => {}
            None => responses.push(DavResponse::with_status(href, 404)),
        }
    }
    let mut budget = Budget::new(MAX_RESULTS, with_data);
    if !found.iter().all(|meta| budget.take(Some(meta))) {
        return Err(ReportError::TooManyResults);
    }
    for meta in &found {
        match object_response(ctx, collection, meta, props, with_data).await? {
            Some(response) => responses.push(response),
            None => responses.push(DavResponse::with_status(
                item_href(collection, &ctx.user.username, &meta.name),
                404,
            )),
        }
    }
    Ok(())
}

/// RFC 6578: objects changed since the token, oldest change first,
/// deletions as 404 entries. Returns the new sync token. When the result is
/// truncated, the token points at the last included change, so the client
/// continues exactly where this response ends.
async fn sync_collection(
    ctx: Ctx<'_>,
    collection: &Collection,
    token: &str,
    props: &Propfind,
    limit: Option<usize>,
    responses: &mut Vec<DavResponse>,
) -> Result<String, ReportError> {
    let store = &ctx.state.store;
    // The token is bound to this collection; tokens of other collections
    // (including other users') are rejected, never interpreted.
    let since = if token.is_empty() {
        None
    } else {
        Some(sync::revision(token, collection).ok_or(ReportError::InvalidSyncToken)?)
    };
    let max_results = limit.map_or(MAX_RESULTS, |l| l.min(MAX_RESULTS));
    let rows = row_limit(max_results);

    // (revision, name, metadata if the object exists)
    let entries: Vec<(i64, String, Option<ItemMeta>)> = match since {
        // Initial sync: current objects in the order of their last change.
        None => store
            .items_by_revision(collection.id, rows)
            .await?
            .into_iter()
            .map(|(revision, meta)| (revision, meta.name.clone(), Some(meta)))
            .collect(),
        Some(since) => store
            .changes_since(collection.id, since, rows)
            .await?
            .into_iter()
            .map(|change| (change.revision, change.name, change.meta))
            .collect(),
    };

    let with_data = wants_data(props, collection.kind);
    let mut budget = Budget::new(max_results, with_data);
    let mut last_revision = None;
    for (revision, name, meta) in &entries {
        // Changes newer than the collection snapshot belong to the next sync.
        if *revision > collection.sync_revision {
            break;
        }
        if !budget.take(meta.as_ref()) {
            responses.push(truncated(collection, ctx));
            let resume = last_revision.unwrap_or_else(|| since.unwrap_or(0));
            return Ok(sync::token_at(collection, resume));
        }
        let response = match meta {
            Some(meta) => object_response(ctx, collection, meta, props, with_data).await?,
            None => None,
        };
        responses.push(response.unwrap_or_else(|| {
            DavResponse::with_status(item_href(collection, &ctx.user.username, name), 404)
        }));
        last_revision = Some(*revision);
    }
    Ok(sync::token(collection))
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
    fn budget_counts_results_and_bytes() {
        let mut budget = Budget::new(2, false);
        assert!(budget.take(Some(&meta(1))));
        assert!(budget.take(None));
        assert!(!budget.take(Some(&meta(1))));

        let mut bytes = Budget::new(100, true);
        for _ in 0..32 {
            assert!(bytes.take(Some(&meta(1024 * 1024))));
        }
        assert!(!bytes.take(Some(&meta(1))));
        // Without object data, sizes do not count.
        let mut no_data = Budget::new(100, false);
        assert!((0..50).all(|_| no_data.take(Some(&meta(1024 * 1024)))));
    }
}
