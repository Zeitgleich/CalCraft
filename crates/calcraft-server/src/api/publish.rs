//! Public read-only links of the user's calendars (docs/sharing.md):
//! create (the only time the secret URL is returned), list, revoke. With
//! `[publish] enabled = false` every route answers 404.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use calcraft_core::{
    CollectionKind,
    api::{self, CreatePublishLink, CreatedPublishLink, PublishLinkInfo},
};
use calcraft_store::{NewPublishLink, PrivateEvents, PublishLink};

use super::{ApiError, ApiJson, SessionUser, collections::own_collection};
use crate::{app::AppState, auth::now, publish::Publish};

fn private_events(value: PrivateEvents) -> api::PrivateEvents {
    match value {
        PrivateEvents::Busy => api::PrivateEvents::Busy,
        PrivateEvents::Hide => api::PrivateEvents::Hide,
        PrivateEvents::Show => api::PrivateEvents::Show,
    }
}

fn info(link: PublishLink) -> PublishLinkInfo {
    PublishLinkInfo {
        id: link.public_id,
        busy_only: link.busy_only,
        private_events: private_events(link.private_events),
        include_attendees: link.include_attendees,
        created_at: link.created_at,
    }
}

/// Publishing must be enabled, and only calendars are published.
async fn calendar(
    state: &AppState,
    session: &SessionUser,
    kind: &str,
    slug: &str,
) -> Result<(std::sync::Arc<Publish>, calcraft_store::Collection), ApiError> {
    let Some(publish) = state.publish.clone() else {
        return Err(ApiError::NotFound);
    };
    let collection = own_collection(state, &session.user, kind, slug).await?;
    if collection.kind != CollectionKind::Calendar {
        return Err(ApiError::NotFound);
    }
    Ok((publish, collection))
}

/// `GET /v1/collections/{kind}/{slug}/publish`.
pub async fn list(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug)): Path<(String, String)>,
) -> Result<Json<Vec<PublishLinkInfo>>, ApiError> {
    let (_, collection) = calendar(&state, &session, &kind, &slug).await?;
    let links = state.store.publish_links(collection.id).await?;
    Ok(Json(links.into_iter().map(info).collect()))
}

/// `POST /v1/collections/{kind}/{slug}/publish`.
pub async fn create(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug)): Path<(String, String)>,
    ApiJson(request): ApiJson<CreatePublishLink>,
) -> Result<(StatusCode, Json<CreatedPublishLink>), ApiError> {
    let (publish, collection) = calendar(&state, &session, &kind, &slug).await?;
    let (token, hash) = crate::publish::new_token();
    let link = NewPublishLink {
        token_hash: &hash,
        busy_only: request.busy_only,
        private_events: match request.private_events {
            api::PrivateEvents::Busy => PrivateEvents::Busy,
            api::PrivateEvents::Hide => PrivateEvents::Hide,
            api::PrivateEvents::Show => PrivateEvents::Show,
        },
        include_attendees: request.include_attendees,
        now: now(),
    };
    let created = state
        .store
        .create_publish_link(collection.id, session.user.id, &link)
        .await?
        .ok_or(ApiError::NotFound)?;
    let (url, webcal_url) = publish.urls(&token);
    Ok((
        StatusCode::CREATED,
        Json(CreatedPublishLink {
            link: info(created),
            url,
            webcal_url,
        }),
    ))
}

/// `DELETE /v1/collections/{kind}/{slug}/publish/{id}`: the link stops
/// working at once.
pub async fn delete(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug, id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let (_, collection) = calendar(&state, &session, &kind, &slug).await?;
    if !calcraft_store::valid_managed_id(&id) {
        return Err(ApiError::NotFound);
    }
    if state.store.delete_publish_link(collection.id, &id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}
