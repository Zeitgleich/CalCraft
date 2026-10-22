//! REPORT: calendar-multiget and calendar-query (RFC 4791 §7.8, §7.9),
//! addressbook-multiget and addressbook-query (RFC 6352 §8.6, §8.7) and
//! sync-collection (RFC 6578).
//!
//! Responses are bounded. Every storage query carries a row limit, the
//! number of results and the total size of returned object data are capped,
//! and object content is only loaded when it is requested or needed for an
//! exact recurrence check. Queries and
//! sync-collection return truncated results with a 507 entry for the
//! collection (RFC 6578 §3.6, RFC 4791 §7.9); sync clients continue with
//! the returned token. Multiget fails as a whole instead, because the
//! client asked for specific objects.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use calcraft_core::CollectionKind;
use calcraft_dav::{
    CalendarFilter, DavError, MAX_REPORT_BODY, Multistatus, Propfind, Report,
    Response as DavResponse,
};
use calcraft_ical::{Budget as Work, OBJECT_BUDGET, matches_time_range};
use calcraft_store::{Collection, Item, ItemMeta, StoreError};

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
/// Candidates read per storage query in calendar-query.
const QUERY_PAGE: u32 = 500;
/// Limits of the exact recurrence checks in one calendar-query: work (see
/// [`calcraft_ical::Budget`]; each object gets at most [`OBJECT_BUDGET`]),
/// number of checked objects, bytes of object data read for them, and
/// wall-clock time. Beyond any of them, the remaining candidates are
/// returned without the check: conservatively, never dropped.
const QUERY_WORK: u64 = 2_000_000;
const MAX_EXACT_CHECKS: usize = 1_000;
const MAX_CHECK_BYTES: i64 = 64 * 1024 * 1024;
const QUERY_DEADLINE: Duration = Duration::from_secs(5);

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
    let href = item_href(collection, &user.username, &meta.name);
    let size = usize::try_from(meta.size).unwrap_or(0);
    if !with_data {
        let available = item_properties(user, collection, &meta.etag, size, None);
        return Ok(Some(respond(&href, &available, props)));
    }
    let Some(item) = ctx.state.store.item(collection.id, &meta.name).await? else {
        return Ok(None);
    };
    Ok(Some(item_response(ctx, collection, &item, props)))
}

/// The response entry for a loaded object.
fn item_response(
    ctx: Ctx<'_>,
    collection: &Collection,
    item: &Item,
    props: &Propfind,
) -> DavResponse {
    let href = item_href(collection, &ctx.user.username, &item.name);
    let available = item_properties(
        ctx.user,
        collection,
        &item.etag,
        item.content.len(),
        Some(&item.content),
    );
    respond(&href, &available, props)
}

/// Sets the cancel flag when dropped, i.e. also when the request future is
/// dropped (client gone, timeout): running checks then stop at their next
/// budget charge.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// What is left for exact recurrence checks in one calendar-query.
struct ExactChecks {
    work: u64,
    checks: usize,
    bytes: i64,
    deadline: tokio::time::Instant,
    cancel: CancelOnDrop,
}

impl ExactChecks {
    fn new() -> Self {
        Self {
            work: QUERY_WORK,
            checks: 0,
            bytes: 0,
            deadline: tokio::time::Instant::now() + QUERY_DEADLINE,
            cancel: CancelOnDrop(Arc::new(AtomicBool::new(false))),
        }
    }

    /// Whether another object of `size` bytes may be checked.
    fn allow(&self, size: i64) -> bool {
        self.work > 0
            && self.checks < MAX_EXACT_CHECKS
            && self.bytes.saturating_add(size) <= MAX_CHECK_BYTES
            && tokio::time::Instant::now() < self.deadline
    }
}

/// Outcome of checking one recurring object.
enum Checked {
    /// The object vanished.
    Gone,
    /// No instance in the range.
    NoMatch,
    /// An instance (possibly) in the range; the loaded object for reuse.
    Match(Option<Item>),
}

/// Whether a recurring object has an instance in the filter's time range.
/// Runs on the blocking pool, at most `expansion` permits at a time, within
/// the limits of `checks`; when they do not allow a check the object counts
/// as a match.
async fn check_instances(
    ctx: Ctx<'_>,
    collection: &Collection,
    meta: &ItemMeta,
    filter: &CalendarFilter,
    checks: &mut ExactChecks,
) -> Result<Checked, StoreError> {
    if !checks.allow(meta.size) {
        return Ok(Checked::Match(None));
    }
    let permit =
        match tokio::time::timeout_at(checks.deadline, ctx.state.expansion.clone().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            _ => return Ok(Checked::Match(None)),
        };
    let Some(item) = ctx.state.store.item(collection.id, &meta.name).await? else {
        return Ok(Checked::Gone);
    };
    checks.checks += 1;
    checks.bytes = checks
        .bytes
        .saturating_add(i64::try_from(item.content.len()).unwrap_or(i64::MAX));
    let granted = OBJECT_BUDGET.min(checks.work);
    let cancel = checks.cancel.0.clone();
    let (start, end) = (filter.start, filter.end);
    let checked = tokio::task::spawn_blocking(move || {
        // Held until the check ends, even if the request is gone.
        let _permit = permit;
        let mut budget = Work::with_cancel(granted, cancel);
        let matched = matches_time_range(&item.content, start, end, &mut budget);
        (matched, budget.remaining(), item)
    })
    .await;
    Ok(match checked {
        Ok((matched, left, item)) => {
            checks.work = checks.work.saturating_sub(granted.saturating_sub(left));
            if matched {
                Checked::Match(Some(item))
            } else {
                Checked::NoMatch
            }
        }
        // The check cannot panic; if it did, the object is returned.
        Err(_) => Checked::Match(None),
    })
}

/// calendar-query: index candidates in name order, recurring ones checked
/// against their instances, truncated at the limits with a 507 entry.
async fn calendar_query(
    ctx: Ctx<'_>,
    collection: &Collection,
    filter: &CalendarFilter,
    props: &Propfind,
    responses: &mut Vec<DavResponse>,
) -> Result<(), ReportError> {
    let with_data = wants_data(props, collection.kind);
    let mut budget = Budget::new(MAX_RESULTS, with_data);
    let has_range = filter.start.is_some() || filter.end.is_some();
    let mut checks = ExactChecks::new();
    let mut after: Option<String> = None;
    loop {
        let page = ctx
            .state
            .store
            .query_candidates(
                collection.id,
                filter.component.map(|kind| kind.as_str()),
                filter.start,
                filter.end,
                after.as_deref(),
                QUERY_PAGE,
            )
            .await?;
        for candidate in &page {
            let meta = &candidate.meta;
            let mut loaded = None;
            if has_range && candidate.recurring {
                match check_instances(ctx, collection, meta, filter, &mut checks).await? {
                    Checked::Gone | Checked::NoMatch => continue,
                    Checked::Match(item) => loaded = item,
                }
            }
            if !budget.take(Some(meta)) {
                responses.push(truncated(collection, ctx));
                return Ok(());
            }
            let response = match loaded {
                Some(item) if with_data => Some(item_response(ctx, collection, &item, props)),
                _ => object_response(ctx, collection, meta, props, with_data).await?,
            };
            responses.extend(response);
        }
        if page.len() < QUERY_PAGE as usize {
            return Ok(());
        }
        after = page.last().map(|c| c.meta.name.clone());
    }
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
            calendar_query(ctx, &collection, &filter, &props, &mut responses).await
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
mod tests;
