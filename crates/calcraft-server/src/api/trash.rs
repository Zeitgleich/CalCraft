//! The trash of the logged-in user: list, restore, delete permanently.
//!
//! Entries are addressed by a random id and always looked up together with
//! the session's user, so another user's id answers exactly like an id that
//! does not exist (404).

use axum::{
    Json,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::StatusCode,
};
use calcraft_core::{
    CollectionKind, Slug,
    api::{RestoredTrashEntry, TrashEntryInfo, TrashKind, TrashList},
};
use calcraft_store::{TrashConflict, TrashEntry, TrashEntryKind};
use serde::Deserialize;

use super::{ApiError, SessionUser};
use crate::app::{AppState, DAV_ROOT};

/// Entries per page by default and at most.
pub const DEFAULT_PAGE: u32 = 100;
pub const MAX_PAGE: u32 = 200;
const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
}

fn kind(kind: TrashEntryKind) -> TrashKind {
    match kind {
        TrashEntryKind::Item => TrashKind::Item,
        TrashEntryKind::Collection => TrashKind::Collection,
    }
}

fn home(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Calendar => "calendars",
        CollectionKind::Addressbook => "addressbooks",
    }
}

fn info(entry: TrashEntry, retention_days: u32) -> TrashEntryInfo {
    let expires_at = entry
        .deleted_at
        .saturating_add(i64::from(retention_days).saturating_mul(SECONDS_PER_DAY));
    TrashEntryInfo {
        id: entry.id,
        kind: kind(entry.kind),
        collection_kind: entry.collection_kind,
        collection: entry.collection_slug,
        name: entry.name,
        summary: entry.summary,
        size: entry.size,
        items: entry.items,
        deleted_at: entry.deleted_at,
        expires_at,
    }
}

/// Ids are 32 lowercase hex digits; anything else cannot exist.
fn valid_id(id: &str) -> Result<&str, ApiError> {
    (id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .then_some(id)
        .ok_or(ApiError::NotFound)
}

pub async fn list(
    State(state): State<AppState>,
    session: SessionUser,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<TrashList>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::BadRequest("invalid query"))?;
    let limit = query.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let offset = query.offset.unwrap_or(0);
    let page = state
        .store
        .trash_entries(session.user.id, limit, offset)
        .await?;
    let retention_days = state.store.trash_retention_days();
    Ok(Json(TrashList {
        entries: page
            .entries
            .into_iter()
            .map(|entry| info(entry, retention_days))
            .collect(),
        total: page.total,
        bytes: page.bytes,
        retention_days,
        offset,
        limit,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreQuery {
    /// Restore an object into this collection (of the same kind) instead
    /// of its original one, e.g. after that was deleted permanently.
    #[serde(default)]
    collection: Option<String>,
}

pub async fn restore(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<String>,
    query: Result<Query<RestoreQuery>, QueryRejection>,
) -> Result<Json<RestoredTrashEntry>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::BadRequest("invalid query"))?;
    let id = valid_id(&id)?;
    let target: Option<Slug> = query
        .collection
        .map(|slug| {
            slug.parse()
                .map_err(|_| ApiError::RestoreConflict(TrashConflict::InvalidTarget))
        })
        .transpose()?;
    let user = &session.user;
    let restored = state
        .store
        .restore_trash_entry(user.id, id, target.as_ref())
        .await?
        .ok_or(ApiError::NotFound)?;
    // Restored rows carry no time-range index of the current version; the
    // re-index (idempotent, only stale rows) catches them up. Until then
    // queries treat them conservatively.
    crate::reindex::spawn(state.store.clone());
    let collection = restored.collection_slug.to_string();
    let href = format!(
        "{DAV_ROOT}{}/{}/{collection}/{}",
        home(restored.collection_kind),
        user.username,
        restored.name.as_deref().unwrap_or_default()
    );
    Ok(Json(RestoredTrashEntry {
        kind: kind(restored.kind),
        collection_kind: restored.collection_kind,
        collection,
        name: restored.name,
        etag: restored.etag,
        href,
    }))
}

pub async fn delete_entry(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = valid_id(&id)?;
    if state.store.delete_trash_entry(session.user.id, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

pub async fn empty(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<StatusCode, ApiError> {
    state.store.empty_trash(session.user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
