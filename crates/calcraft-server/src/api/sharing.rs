//! Sharing through the REST API (docs/sharing.md): the owner manages the
//! shares of a collection, the sharee accepts, declines or leaves shares,
//! administrators list all shares.
//!
//! Collections are always resolved for the session's user and shares by
//! the user's id, so somebody else's collection or share answers exactly
//! like one that does not exist (404). With `[sharing] enabled = false`
//! every route answers 404.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use calcraft_core::{
    Username,
    api::{self, AdminShareInfo, IncomingShareInfo, SetShareRequest, ShareInfo},
};
use calcraft_store::{SetShare, Share, ShareAccess, StoreError};

use super::{AdminUser, ApiError, ApiJson, SessionUser, collections::own_collection};
use crate::{
    app::{AppState, DAV_ROOT},
    auth::now,
};

/// Most shares listed for administrators.
const MAX_ADMIN_SHARES: u32 = 10_000;

fn access(access: ShareAccess) -> api::ShareAccess {
    match access {
        ShareAccess::Read => api::ShareAccess::Read,
        ShareAccess::ReadWrite => api::ShareAccess::ReadWrite,
    }
}

fn info(share: Share) -> ShareInfo {
    ShareInfo {
        accepted: share.accepted(),
        username: share.sharee.to_string(),
        display_name: share.sharee_display_name,
        access: access(share.access),
        created_at: share.created_at,
    }
}

fn incoming(share: Share, user: &Username) -> IncomingShareInfo {
    let home = match share.kind {
        calcraft_core::CollectionKind::Calendar => "calendars",
        calcraft_core::CollectionKind::Addressbook => "addressbooks",
    };
    IncomingShareInfo {
        accepted: share.accepted(),
        href: share
            .slug
            .as_ref()
            .map(|slug| format!("{DAV_ROOT}{home}/{user}/{slug}/")),
        id: share.public_id,
        kind: share.kind,
        owner: share.owner.to_string(),
        owner_display_name: share.owner_display_name,
        display_name: share.collection_display_name,
        access: access(share.access),
        created_at: share.created_at,
    }
}

fn enabled(state: &AppState) -> Result<(), ApiError> {
    if state.sharing {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /v1/collections/{kind}/{slug}/shares`.
pub async fn list(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug)): Path<(String, String)>,
) -> Result<Json<Vec<ShareInfo>>, ApiError> {
    enabled(&state)?;
    let collection = own_collection(&state, &session.user, &kind, &slug).await?;
    let shares = state.store.shares_of_collection(collection.id).await?;
    Ok(Json(shares.into_iter().map(info).collect()))
}

/// `PUT /v1/collections/{kind}/{slug}/shares/{username}`: shares with a
/// user (pending until they accept) or changes the access. 404 for an
/// unknown or disabled user, 400 for the owner themselves, 403
/// `read_only` for write access to a subscription.
pub async fn set(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug, username)): Path<(String, String, String)>,
    ApiJson(request): ApiJson<SetShareRequest>,
) -> Result<(StatusCode, Json<ShareInfo>), ApiError> {
    enabled(&state)?;
    let user = &session.user;
    let collection = own_collection(&state, user, &kind, &slug).await?;
    let username: Username = username.parse().map_err(|_| ApiError::NotFound)?;
    if username == user.username {
        return Err(ApiError::BadRequest(
            "a collection cannot be shared with its owner",
        ));
    }
    let sharee = state
        .store
        .user_by_name(&username)
        .await?
        .filter(|sharee| !sharee.disabled)
        .ok_or(ApiError::NotFound)?;
    let level = match request.access {
        api::ShareAccess::Read => ShareAccess::Read,
        api::ShareAccess::ReadWrite => ShareAccess::ReadWrite,
    };
    match state
        .store
        .set_share(collection.id, user.id, sharee.id, level, now())
        .await
    {
        Ok(Some(SetShare::Created(share))) => Ok((StatusCode::CREATED, Json(info(share)))),
        Ok(Some(SetShare::Updated(share))) => Ok((StatusCode::OK, Json(info(share)))),
        Ok(None) | Err(StoreError::NoSuchSharee) => Err(ApiError::NotFound),
        Err(StoreError::ShareDeclined) => Err(ApiError::Conflict(
            "the user declined or left one of your shares within the last day",
        )),
        Err(err) => Err(err.into()),
    }
}

/// `DELETE /v1/collections/{kind}/{slug}/shares/{username}`: ends a share
/// (or withdraws the invitation).
pub async fn remove(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug, username)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    enabled(&state)?;
    let user = &session.user;
    let collection = own_collection(&state, user, &kind, &slug).await?;
    let username: Username = username.parse().map_err(|_| ApiError::NotFound)?;
    let Some(sharee) = state.store.user_by_name(&username).await? else {
        return Err(ApiError::NotFound);
    };
    if state
        .store
        .remove_share(collection.id, user.id, sharee.id)
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /v1/shared`: collections shared with the user, pending and
/// accepted.
pub async fn incoming_list(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<Json<Vec<IncomingShareInfo>>, ApiError> {
    enabled(&state)?;
    let user = &session.user;
    let shares = state.store.incoming_shares(user.id).await?;
    Ok(Json(
        shares
            .into_iter()
            .map(|share| incoming(share, &user.username))
            .collect(),
    ))
}

fn share_id(id: &str) -> Result<&str, ApiError> {
    if calcraft_store::valid_managed_id(id) {
        Ok(id)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `POST /v1/shared/{id}/accept`: the collection appears in the user's
/// home.
pub async fn accept(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<String>,
) -> Result<Json<IncomingShareInfo>, ApiError> {
    enabled(&state)?;
    let user = &session.user;
    let share = state
        .store
        .accept_share(user.id, share_id(&id)?, now())
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(incoming(share, &user.username)))
}

/// `DELETE /v1/shared/{id}`: declines an invitation or leaves a share.
pub async fn leave(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    enabled(&state)?;
    if state
        .store
        .leave_share(session.user.id, share_id(&id)?, now())
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /v1/admin/shares`: every share (who, what, with whom), for
/// administrators.
pub async fn admin_list(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
) -> Result<Json<Vec<AdminShareInfo>>, ApiError> {
    let shares = state.store.all_shares(MAX_ADMIN_SHARES).await?;
    Ok(Json(
        shares
            .into_iter()
            .map(|share| AdminShareInfo {
                accepted: share.accepted(),
                owner: share.owner.to_string(),
                kind: share.kind,
                collection: share.owner_slug.to_string(),
                sharee: share.sharee.to_string(),
                access: access(share.access),
                created_at: share.created_at,
            })
            .collect(),
    ))
}
