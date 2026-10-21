//! Import and export of the logged-in user's collections.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use axum::{
    Json,
    body::{Body, Bytes, to_bytes},
    extract::{Path, Query, State, rejection::QueryRejection},
    http::header,
    response::{IntoResponse, Response},
};
use calcraft_core::api::ImportReport;
use serde::Deserialize;
use tokio::sync::mpsc;

use super::{ApiError, SessionUser, collections::own_collection};
use crate::{
    app::AppState,
    transfer::{self, EXPORT_BUFFER, MAX_IMPORT_SIZE, OnConflict, TransferError},
};

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    format: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportQuery {
    format: Option<String>,
    on_conflict: Option<String>,
}

impl From<TransferError> for ApiError {
    fn from(err: TransferError) -> Self {
        match err {
            TransferError::TooLarge => Self::TooLarge,
            TransferError::Invalid(message) => Self::InvalidData(message),
            TransferError::Store(err) => err.into(),
            TransferError::Aborted => Self::Internal,
        }
    }
}

/// The receiving end of an export as a body stream.
struct ChunkStream(mpsc::Receiver<Result<Bytes, std::io::Error>>);

impl futures_core::Stream for ChunkStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

pub async fn export(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug)): Path<(String, String)>,
    query: Result<Query<ExportQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::BadRequest("invalid query"))?;
    let collection = own_collection(&state, &session.user, &kind, &slug).await?;
    let format = transfer::format_for(collection.kind, query.format.as_deref())?;
    let file_name = transfer::file_name(&collection, format);

    let (tx, rx) = mpsc::channel(EXPORT_BUFFER);
    let store = state.store.clone();
    tokio::spawn(async move {
        if let Err(err) = transfer::export(&store, &collection, format, tx).await
            && !matches!(err, TransferError::Aborted)
        {
            tracing::error!(%err, "export failed");
        }
    });
    Ok((
        [
            (header::CONTENT_TYPE, format.content_type().to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file_name}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        Body::from_stream(ChunkStream(rx)),
    )
        .into_response())
}

pub async fn import(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug)): Path<(String, String)>,
    query: Result<Query<ImportQuery>, QueryRejection>,
    body: Body,
) -> Result<Json<ImportReport>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::BadRequest("invalid query"))?;
    let collection = own_collection(&state, &session.user, &kind, &slug).await?;
    let format = transfer::format_for(collection.kind, query.format.as_deref())?;
    let on_conflict = match query.on_conflict.as_deref() {
        None => OnConflict::default(),
        Some(name) => OnConflict::from_name(name)
            .ok_or(ApiError::BadRequest("on_conflict must be skip or replace"))?,
    };
    // Wait for an import slot before reading the body, so queued imports
    // do not hold their uploads in memory.
    let slot = transfer::import_slot().await?;
    let data = to_bytes(body, MAX_IMPORT_SIZE)
        .await
        .map_err(|_| ApiError::TooLarge)?;
    let report =
        transfer::import(&state.store, &collection, format, data, on_conflict, slot).await?;
    tracing::info!(
        user = %session.user.username,
        collection = %collection.slug,
        created = report.created,
        replaced = report.replaced,
        skipped = report.skipped,
        rejected = report.rejected,
        "import"
    );
    Ok(Json(report))
}
