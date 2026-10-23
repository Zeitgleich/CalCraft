//! E-mail reminder settings of the logged-in user and the queue overview
//! for administrators. 404 while reminders are off (or not compiled in).

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use calcraft_core::api::{
    AdminReminders, MailStatus, ReminderInfo, ReminderModeName, ReminderTest, UpdateReminders,
};
use calcraft_store::{MailCounts, ReminderMode};

use super::{AdminUser, ApiError, ApiJson, SessionUser};
use crate::{app::AppState, auth::now, reminders::Reminders};

/// Window of the user's status.
const USER_WINDOW: i64 = 7 * 86_400;
/// Window of the administrators' overview.
const ADMIN_WINDOW: i64 = 86_400;

fn reminders(state: &AppState) -> Result<&Arc<Reminders>, ApiError> {
    state.reminders.as_ref().ok_or(ApiError::NotFound)
}

fn name(mode: ReminderMode) -> ReminderModeName {
    match mode {
        ReminderMode::Off => ReminderModeName::Off,
        ReminderMode::Email => ReminderModeName::Email,
        ReminderMode::All => ReminderModeName::All,
    }
}

fn status(counts: &MailCounts) -> MailStatus {
    MailStatus {
        pending: counts.pending,
        sent: counts.sent,
        failed: counts.failed,
        dropped: counts.dropped,
        last_error: counts.last_error.as_ref().map(|(code, _)| code.clone()),
        last_error_at: counts.last_error.as_ref().map(|(_, at)| *at),
    }
}

pub async fn get(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<Json<ReminderInfo>, ApiError> {
    let reminders = reminders(&state)?;
    let user = &session.user;
    let settings = state
        .store
        .reminder_settings(user.id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let address = crate::reminders::own_address(&state.store, reminders.scheduling(), user).await?;
    let counts = state
        .store
        .mail_counts(Some(user.id), now() - USER_WINDOW)
        .await?;
    Ok(Json(ReminderInfo {
        mode: name(settings.mode.unwrap_or_else(|| reminders.default_mode())),
        custom: settings.mode.is_some(),
        default_mode: name(reminders.default_mode()),
        include_description: settings.include_description,
        address,
        scheduled: state.store.scheduled_reminders(Some(user.id)).await?,
        status: status(&counts),
    }))
}

pub async fn update(
    State(state): State<AppState>,
    session: SessionUser,
    ApiJson(request): ApiJson<UpdateReminders>,
) -> Result<StatusCode, ApiError> {
    reminders(&state)?;
    let mode = match request.mode.as_deref() {
        None => None,
        Some("default") => Some(None),
        Some(value) => Some(Some(ReminderMode::parse(value).ok_or(
            ApiError::BadRequest("mode must be off, email, all or default"),
        )?)),
    };
    state
        .store
        .set_reminder_settings(session.user.id, mode, request.include_description)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Queues a test mail to the user's own address (counts towards the
/// user's limits).
pub async fn test(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<Response, ApiError> {
    let reminders = reminders(&state)?;
    match reminders.queue_test(&session.user).await? {
        None => Err(ApiError::Conflict(
            "no e-mail address is set for this account; ask an administrator",
        )),
        Some(queued) => Ok((StatusCode::ACCEPTED, Json(ReminderTest { queued })).into_response()),
    }
}

pub async fn admin(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<AdminReminders>, ApiError> {
    let reminders = reminders(&state)?;
    let counts = state.store.mail_counts(None, now() - ADMIN_WINDOW).await?;
    let sender = reminders.sender_status();
    Ok(Json(AdminReminders {
        default_mode: name(reminders.default_mode()),
        scheduled: state.store.scheduled_reminders(None).await?,
        status: status(&counts),
        cancelled: counts.cancelled,
        last_success_at: sender.last_success,
        last_smtp_error: sender.last_error.map(|(code, _)| code.to_owned()),
        last_smtp_error_at: sender.last_error.map(|(_, at)| at),
    }))
}
