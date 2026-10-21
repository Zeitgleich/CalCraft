//! User administration (admins only).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use calcraft_core::{
    Username,
    api::{AdminUser as AdminUserInfo, CreateUser, UpdateUser},
};
use calcraft_store::User;

use super::{AdminUser, ApiError, ApiJson};
use crate::{
    app::AppState,
    auth::{PasswordError, hash_password_async},
    validate::{MAX_DISPLAY_NAME, clean_text},
};

fn info(user: User) -> AdminUserInfo {
    AdminUserInfo {
        username: user.username.to_string(),
        display_name: user.display_name,
        is_admin: user.is_admin,
        disabled: user.disabled,
        totp_enabled: user.totp_enabled,
    }
}

pub async fn list(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Vec<AdminUserInfo>>, ApiError> {
    Ok(Json(
        state
            .store
            .list_users()
            .await?
            .into_iter()
            .map(info)
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
    Ok((StatusCode::CREATED, Json(info(user))))
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
