//! Groups through the REST API (docs/sharing.md, "Groups"):
//! administrators manage groups and members, users see the groups they
//! belong to, owners share collections with groups.
//!
//! A group a user is not a member of answers like a missing one (404),
//! except for administrators. Group names are never logged.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use calcraft_core::{
    GroupName, Username,
    api::{
        self, CreateGroup, GroupInfo, GroupMemberInfo, GroupShareInfo, GroupShareResult,
        MemberAddedInfo, SetShareRequest,
    },
};
use calcraft_store::{Group, GroupMember, GroupShare, ShareAccess, StoreError};

use super::{AdminUser, ApiError, ApiJson, SessionUser, collections::own_collection, sharing};
use crate::{
    app::AppState,
    auth::now,
    groups::shareable_group,
    validate::{MAX_DISPLAY_NAME, clean_text},
};

fn info(group: Group, members: Vec<GroupMemberInfo>) -> GroupInfo {
    GroupInfo {
        name: group.name.to_string(),
        display_name: group.display_name,
        member_count: group.members,
        members,
        created_at: group.created_at,
    }
}

fn member(member: GroupMember, show_disabled: bool) -> GroupMemberInfo {
    GroupMemberInfo {
        username: member.username.to_string(),
        display_name: member.display_name,
        disabled: show_disabled && member.disabled,
    }
}

fn share_info(share: GroupShare) -> GroupShareInfo {
    GroupShareInfo {
        group: share.group.to_string(),
        display_name: share.group_display_name,
        access: sharing::access(share.access),
        members: share.members,
        created_at: share.created_at,
    }
}

/// The group named `name`, without a membership check (administrators,
/// and owners ending an existing group share).
async fn group_by_name(state: &AppState, name: &str) -> Result<Group, ApiError> {
    let name: GroupName = name.parse().map_err(|_| ApiError::NotFound)?;
    state
        .store
        .group_by_name(&name)
        .await?
        .ok_or(ApiError::NotFound)
}

fn enabled(state: &AppState) -> Result<(), ApiError> {
    if state.sharing {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /v1/me/groups`: the groups the user is a member of, each with its
/// enabled members.
pub async fn mine(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<Json<Vec<GroupInfo>>, ApiError> {
    let mut list = Vec::new();
    for group in state.store.groups_of_user(session.user.id).await? {
        let members = state
            .store
            .group_members(group.id)
            .await?
            .into_iter()
            .filter(|m| !m.disabled)
            .map(|m| member(m, false))
            .collect();
        list.push(info(group, members));
    }
    Ok(Json(list))
}

/// `GET /v1/collections/{kind}/{slug}/group-shares`.
pub async fn shares(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug)): Path<(String, String)>,
) -> Result<Json<Vec<GroupShareInfo>>, ApiError> {
    enabled(&state)?;
    let collection = own_collection(&state, &session.user, &kind, &slug).await?;
    let shares = state
        .store
        .group_shares_of_collection(collection.id)
        .await?;
    Ok(Json(shares.into_iter().map(share_info).collect()))
}

/// `PUT /v1/collections/{kind}/{slug}/group-shares/{group}`: shares with
/// every member of a group the user belongs to (administrators: any
/// group), or changes the access. 404 for an unknown group or one the user
/// is not a member of, 403 `read_only` for write access to a subscription.
pub async fn set_share(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug, group)): Path<(String, String, String)>,
    ApiJson(request): ApiJson<SetShareRequest>,
) -> Result<(StatusCode, Json<GroupShareResult>), ApiError> {
    enabled(&state)?;
    let user = &session.user;
    let collection = own_collection(&state, user, &kind, &slug).await?;
    let group = shareable_group(&state.store, user, &group)
        .await?
        .ok_or(ApiError::NotFound)?;
    let access = match request.access {
        api::ShareAccess::Read => ShareAccess::Read,
        api::ShareAccess::ReadWrite => ShareAccess::ReadWrite,
    };
    let outcome = match state
        .store
        .set_group_share(collection.id, user.id, group.id, access, now())
        .await
    {
        Ok(Some(outcome)) => outcome,
        Ok(None) | Err(StoreError::NoSuchGroup) => return Err(ApiError::NotFound),
        Err(err) => return Err(err.into()),
    };
    let status = if outcome.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(GroupShareResult {
            share: share_info(outcome.share),
            invited: outcome.invited,
            skipped: outcome.skipped,
        }),
    ))
}

/// `DELETE /v1/collections/{kind}/{slug}/group-shares/{group}`: ends the
/// share with a group; members lose access at once unless a direct share
/// or another group share grants it. Works also after the owner left the
/// group.
pub async fn remove_share(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug, group)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    enabled(&state)?;
    let user = &session.user;
    let collection = own_collection(&state, user, &kind, &slug).await?;
    let group = group_by_name(&state, &group).await?;
    if state
        .store
        .remove_group_share(collection.id, user.id, group.id)
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /v1/admin/groups`: all groups with their member counts.
pub async fn admin_list(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Vec<GroupInfo>>, ApiError> {
    Ok(Json(
        state
            .store
            .list_groups()
            .await?
            .into_iter()
            .map(|group| info(group, Vec::new()))
            .collect(),
    ))
}

/// `POST /v1/admin/groups`.
pub async fn admin_create(
    State(state): State<AppState>,
    _admin: AdminUser,
    ApiJson(request): ApiJson<CreateGroup>,
) -> Result<(StatusCode, Json<GroupInfo>), ApiError> {
    let name: GroupName = request
        .name
        .parse()
        .map_err(|_| ApiError::BadRequest("invalid group name: use a-z, 0-9, '.', '_' or '-'"))?;
    let display_name = match request.display_name.as_deref() {
        Some(name) => Some(
            clean_text(name, MAX_DISPLAY_NAME)
                .ok_or(ApiError::BadRequest("invalid display name"))?,
        ),
        None => None,
    }
    .filter(|name| !name.is_empty());
    let group = state
        .store
        .create_group(
            &name,
            display_name.as_deref(),
            state.groups.max_groups,
            now(),
        )
        .await?;
    tracing::info!("admin created a group");
    Ok((StatusCode::CREATED, Json(info(group, Vec::new()))))
}

/// `GET /v1/admin/groups/{group}`: a group with all its members.
pub async fn admin_get(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(group): Path<String>,
) -> Result<Json<GroupInfo>, ApiError> {
    let group = group_by_name(&state, &group).await?;
    let members = state
        .store
        .group_members(group.id)
        .await?
        .into_iter()
        .map(|m| member(m, true))
        .collect();
    Ok(Json(info(group, members)))
}

/// `DELETE /v1/admin/groups/{group}`: ends all its shares at once.
pub async fn admin_delete(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(group): Path<String>,
) -> Result<StatusCode, ApiError> {
    let group = group_by_name(&state, &group).await?;
    if state.store.delete_group(group.id).await? {
        tracing::info!("admin deleted a group");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

async fn user_by_name(state: &AppState, username: &str) -> Result<calcraft_store::User, ApiError> {
    let username: Username = username.parse().map_err(|_| ApiError::NotFound)?;
    state
        .store
        .user_by_name(&username)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `PUT /v1/admin/groups/{group}/members/{username}`: the user gets an
/// invitation for every collection shared with the group.
pub async fn admin_add_member(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path((group, username)): Path<(String, String)>,
) -> Result<Json<MemberAddedInfo>, ApiError> {
    let group = group_by_name(&state, &group).await?;
    let user = user_by_name(&state, &username).await?;
    let added = state
        .store
        .add_group_member(group.id, user.id, state.groups.max_members, now())
        .await
        .map_err(|err| match err {
            StoreError::NoSuchGroup | StoreError::NoSuchSharee => ApiError::NotFound,
            err => err.into(),
        })?;
    if added.added {
        tracing::info!(invited = added.invited, "admin added a group member");
    }
    Ok(Json(MemberAddedInfo {
        added: added.added,
        invited: added.invited,
    }))
}

/// `DELETE /v1/admin/groups/{group}/members/{username}`: the group's
/// shares end for the user at once.
pub async fn admin_remove_member(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path((group, username)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let group = group_by_name(&state, &group).await?;
    let user = user_by_name(&state, &username).await?;
    if state.store.remove_group_member(group.id, user.id).await? {
        tracing::info!("admin removed a group member");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}
