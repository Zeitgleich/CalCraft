//! REST API of CalCraft.
//!
//! The web UI uses this API exclusively. The OpenAPI description (utoipa)
//! follows in milestone M4.

use axum::{Json, Router, routing::get};
use serde::Serialize;

/// Builds the API router. It is meant to be nested under `/api`.
pub fn router() -> Router {
    Router::new().route("/v1/version", get(version))
}

#[derive(Debug, Serialize)]
struct VersionInfo {
    name: &'static str,
    version: &'static str,
    source: &'static str,
}

async fn version() -> Json<VersionInfo> {
    Json(VersionInfo {
        name: calcraft_core::NAME,
        version: calcraft_core::VERSION,
        source: calcraft_core::SOURCE_URL,
    })
}
