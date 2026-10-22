//! User administration and the update notice (admins only).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use calcraft_core::{
    Username,
    api::{AdminUser as AdminUserInfo, CreateUser, UpdateInfo, UpdateUser},
};
use calcraft_store::User;

use super::{AdminUser, ApiError, ApiJson};
use crate::{
    app::AppState,
    auth::{PasswordError, hash_password_async},
    validate::{MAX_DISPLAY_NAME, clean_text},
};

fn info(user: User, sso_linked: bool) -> AdminUserInfo {
    AdminUserInfo {
        username: user.username.to_string(),
        display_name: user.display_name,
        is_admin: user.is_admin,
        disabled: user.disabled,
        totp_enabled: user.totp_enabled,
        sso_linked,
    }
}

pub async fn list(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Vec<AdminUserInfo>>, ApiError> {
    let linked = state.store.oidc_linked_user_ids().await?;
    Ok(Json(
        state
            .store
            .list_users()
            .await?
            .into_iter()
            .map(|user| {
                let sso_linked = linked.contains(&user.id);
                info(user, sso_linked)
            })
            .collect(),
    ))
}

pub async fn create(
    State(state): State<AppState>,
    _admin: AdminUser,
    ApiJson(request): ApiJson<CreateUser>,
) -> Result<(StatusCode, Json<AdminUserInfo>), ApiError> {
    let username: Username = request
        .username
        .parse()
        .map_err(|_| ApiError::BadRequest("invalid username: use a-z, 0-9, '.', '_' or '-'"))?;
    let display_name = match request.display_name.as_deref() {
        Some(name) => clean_text(name, MAX_DISPLAY_NAME)
            .ok_or(ApiError::BadRequest("invalid display name"))?
            .into(),
        None => None,
    }
    .filter(|name: &String| !name.is_empty());
    let hash = hash_password_async(request.password)
        .await
        .map_err(|err| match err {
            PasswordError::TooShort => ApiError::BadRequest("password is too short"),
            PasswordError::TooLong => ApiError::BadRequest("password is too long"),
            PasswordError::Reserved => ApiError::BadRequest("password must not start with ccap_"),
            PasswordError::Hash => ApiError::Internal,
        })?;
    let user = state
        .store
        .create_user_with_role(&username, &hash, display_name.as_deref(), request.is_admin)
        .await?;
    Ok((StatusCode::CREATED, Json(info(user, false))))
}

pub async fn update(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(username): Path<String>,
    ApiJson(request): ApiJson<UpdateUser>,
) -> Result<StatusCode, ApiError> {
    let username: Username = username.parse().map_err(|_| ApiError::NotFound)?;
    // Admins cannot lock themselves out.
    if username == admin.username
        && (request.disabled == Some(true) || request.is_admin == Some(false))
    {
        return Err(ApiError::Conflict("you cannot disable or demote yourself"));
    }
    let target = state
        .store
        .user_credentials(&username)
        .await?
        .ok_or(ApiError::NotFound)?
        .user;
    if let Some(disabled) = request.disabled {
        state.store.set_disabled(&username, disabled).await?;
        if disabled {
            state.store.delete_sessions_of(target.id).await?;
        }
    }
    if let Some(is_admin) = request.is_admin {
        state.store.set_admin(&username, is_admin).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Removes the SSO identities bound to a user (like `calcraft user
/// unbind-sso`) and ends the user's web sessions. The account can only be
/// linked again after an operator approval (`calcraft user
/// allow-sso-link`). Admins cannot unlink themselves.
pub async fn unlink_sso(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(username): Path<String>,
) -> Result<StatusCode, ApiError> {
    let username: Username = username.parse().map_err(|_| ApiError::NotFound)?;
    let target = state
        .store
        .user_credentials(&username)
        .await?
        .ok_or(ApiError::NotFound)?
        .user;
    if target.id == admin.id {
        return Err(ApiError::Conflict(
            "you cannot unlink your own single sign-on",
        ));
    }
    let removed = state.store.unbind_oidc_identities(target.id).await?;
    state.store.delete_sessions_of(target.id).await?;
    // Never log the subject.
    tracing::info!(admin = %admin.username, user = %username, removed, "admin removed SSO link");
    Ok(StatusCode::NO_CONTENT)
}

/// Result of the optional update check; `enabled: false` if it is off or
/// not compiled in.
pub async fn update_check(State(state): State<AppState>, _admin: AdminUser) -> Json<UpdateInfo> {
    #[cfg(feature = "update-check")]
    if let Some(check) = &state.updates {
        return Json(check.info());
    }
    #[cfg(not(feature = "update-check"))]
    let _ = state;
    Json(UpdateInfo {
        enabled: false,
        current: calcraft_core::VERSION.to_owned(),
        latest: None,
        newer: false,
        url: None,
        checked_at: None,
    })
}
