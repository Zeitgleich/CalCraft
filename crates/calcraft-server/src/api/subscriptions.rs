//! Calendar subscriptions of the logged-in user (`/api/v1/subscriptions`).
//!
//! Every route needs a session (and, for changes, the CSRF header); a
//! subscription of another user is "not found" like a missing one. While
//! subscriptions are disabled (`[subscriptions] enabled = false`) every
//! route answers 404 after the session check.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use calcraft_core::{
    Slug,
    api::{CreateSubscription, SubscriptionInfo, UpdateSubscription},
};
use calcraft_store::{Subscription, User};

use super::{ApiError, ApiJson, SessionUser};
use crate::{
    app::{AppState, DAV_ROOT},
    auth::now,
    config::{MAX_SUBSCRIPTION_REFRESH_SECS, MIN_SUBSCRIPTION_REFRESH_SECS},
    subscriptions::{CreateRequest, ServiceError, SubscriptionError, Subscriptions},
    validate::{MAX_DISPLAY_NAME, clean_color, clean_text},
};

fn service(state: &AppState) -> Result<&Arc<Subscriptions>, ApiError> {
    state.subscriptions.as_ref().ok_or(ApiError::NotFound)
}

/// Readable text for a stored error code.
fn error_message(code: &str, detail: Option<&str>) -> String {
    let text = match code {
        "invalid_url" => "the feed URL is not allowed",
        "blocked_address" => "the feed's address is not allowed",
        "fetch_failed" => "the feed could not be fetched",
        "not_icalendar" => "the feed is not a valid iCalendar file",
        "too_large" => "the feed is too large",
        "limit_reached" => "a storage limit was reached",
        _ => "the last refresh failed",
    };
    match detail {
        Some(detail) => format!("{text} ({detail})"),
        None => text.to_owned(),
    }
}

fn minutes(secs: i64) -> u32 {
    u32::try_from(secs / 60).unwrap_or(u32::MAX)
}

fn info(service: &Subscriptions, sub: Subscription, user: &User) -> SubscriptionInfo {
    let effective = service.interval(sub.refresh_secs, sub.feed_ttl_secs);
    SubscriptionInfo {
        href: format!(
            "{DAV_ROOT}calendars/{}/{}/",
            user.username, sub.collection.slug
        ),
        slug: sub.collection.slug.to_string(),
        url: sub.source_url,
        display_name: sub.collection.display_name,
        color: sub.collection.color,
        refresh_minutes: sub.refresh_secs.map(minutes),
        effective_refresh_minutes: minutes(effective),
        objects: u32::try_from(sub.items).unwrap_or(u32::MAX),
        skipped: u32::try_from(sub.skipped).unwrap_or(u32::MAX),
        last_attempt_at: sub.last_attempt_at,
        last_success_at: sub.last_success_at,
        next_refresh_at: sub.next_refresh_at,
        last_error_message: sub
            .last_error
            .as_deref()
            .map(|code| error_message(code, sub.last_error_detail.as_deref())),
        last_error: sub.last_error,
    }
}

impl From<ServiceError> for ApiError {
    fn from(err: ServiceError) -> Self {
        match err {
            ServiceError::Feed(err) => {
                let status = match err {
                    SubscriptionError::InvalidUrl(_) | SubscriptionError::BlockedAddress => {
                        StatusCode::BAD_REQUEST
                    }
                    SubscriptionError::FetchFailed(_) | SubscriptionError::HttpStatus(_) => {
                        StatusCode::BAD_GATEWAY
                    }
                    SubscriptionError::NotICalendar | SubscriptionError::TooLarge => {
                        StatusCode::UNPROCESSABLE_ENTITY
                    }
                    SubscriptionError::LimitReached(_) => StatusCode::CONFLICT,
                    SubscriptionError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
                };
                // The detail of the other errors is part of the message already.
                let message = match (&err, err.detail()) {
                    (SubscriptionError::FetchFailed(_), Some(detail)) => {
                        format!("{err} ({detail})")
                    }
                    _ => err.to_string(),
                };
                Self::Subscription(status, err.code(), message)
            }
            ServiceError::Store(err) => err.into(),
            ServiceError::NotFound => Self::NotFound,
        }
    }
}

/// Validates an optional refresh interval in minutes; 0 means automatic.
fn refresh_secs(minutes: Option<u32>) -> Result<Option<i64>, ApiError> {
    match minutes {
        None | Some(0) => Ok(None),
        Some(minutes) => {
            let secs = i64::from(minutes) * 60;
            if (MIN_SUBSCRIPTION_REFRESH_SECS..=MAX_SUBSCRIPTION_REFRESH_SECS).contains(&secs) {
                Ok(Some(secs))
            } else {
                Err(ApiError::BadRequest(
                    "refresh_minutes must be between 15 and 10080 (or 0 for automatic)",
                ))
            }
        }
    }
}

fn display_name(value: Option<&str>) -> Result<Option<Option<String>>, ApiError> {
    value
        .map(|v| {
            clean_text(v, MAX_DISPLAY_NAME)
                .map(|t| (!t.is_empty()).then_some(t))
                .ok_or(ApiError::BadRequest("invalid display name"))
        })
        .transpose()
}

fn color(value: Option<&str>) -> Result<Option<Option<String>>, ApiError> {
    value
        .map(|v| {
            if v.trim().is_empty() {
                Ok(None)
            } else {
                clean_color(v)
                    .map(Some)
                    .ok_or(ApiError::BadRequest("color must be #RRGGBB or #RRGGBBAA"))
            }
        })
        .transpose()
}

/// A subscription of the logged-in user; foreign and missing ones are
/// both "not found".
async fn own(state: &AppState, user: &User, slug: &str) -> Result<Subscription, ApiError> {
    let slug: Slug = slug.parse().map_err(|_| ApiError::NotFound)?;
    state
        .store
        .subscription(user.id, &slug)
        .await?
        .ok_or(ApiError::NotFound)
}

pub async fn list(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<Json<Vec<SubscriptionInfo>>, ApiError> {
    let service = service(&state)?;
    let user = &session.user;
    let subscriptions = state.store.subscriptions(user.id).await?;
    Ok(Json(
        subscriptions
            .into_iter()
            .map(|sub| info(service, sub, user))
            .collect(),
    ))
}

pub async fn create(
    State(state): State<AppState>,
    session: SessionUser,
    ApiJson(request): ApiJson<CreateSubscription>,
) -> Result<(StatusCode, Json<SubscriptionInfo>), ApiError> {
    let service = service(&state)?;
    let user = &session.user;
    let slug = request
        .slug
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::parse::<Slug>)
        .transpose()
        .map_err(|_| ApiError::BadRequest("invalid slug: use a-z, 0-9, '.', '_' or '-'"))?;
    let sub = service
        .create(
            user,
            CreateRequest {
                url: &request.url,
                slug,
                display_name: display_name(request.display_name.as_deref())?.flatten(),
                color: color(request.color.as_deref())?.flatten(),
                refresh_secs: refresh_secs(request.refresh_minutes)?,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(info(service, sub, user))))
}

pub async fn update(
    State(state): State<AppState>,
    session: SessionUser,
    Path(slug): Path<String>,
    ApiJson(request): ApiJson<UpdateSubscription>,
) -> Result<Json<SubscriptionInfo>, ApiError> {
    let service = service(&state)?;
    let user = &session.user;
    let sub = own(&state, user, &slug).await?;
    let name = display_name(request.display_name.as_deref())?;
    let color = color(request.color.as_deref())?;
    let id = sub.collection.id;
    if name.is_some() || color.is_some() {
        state
            .store
            .update_collection(
                id,
                name.as_ref().map(Option::as_deref),
                None,
                color.as_ref().map(Option::as_deref),
            )
            .await?;
    }
    if request.refresh_minutes.is_some() {
        let secs = refresh_secs(request.refresh_minutes)?;
        let interval = service.interval(secs, sub.feed_ttl_secs);
        let base = sub.last_success_at.unwrap_or_else(now);
        let next = base.saturating_add(interval).max(now());
        state.store.update_subscription(id, secs, next).await?;
    }
    let sub = state
        .store
        .subscription_by_collection(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(info(service, sub, user)))
}

/// Deletes the subscription and its calendar for good (not into the
/// trash: subscribing again restores the content).
pub async fn delete(
    State(state): State<AppState>,
    session: SessionUser,
    Path(slug): Path<String>,
) -> Result<StatusCode, ApiError> {
    service(&state)?;
    let sub = own(&state, &session.user, &slug).await?;
    state.store.delete_collection(sub.collection.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Refreshes now; at most once a minute per subscription. A failed fetch
/// is reported in `last_error` of the result.
pub async fn refresh(
    State(state): State<AppState>,
    session: SessionUser,
    Path(slug): Path<String>,
) -> Result<Json<SubscriptionInfo>, ApiError> {
    let service = service(&state)?;
    let user = &session.user;
    let sub = own(&state, user, &slug).await?;
    let sub = service.refresh(sub.collection.id).await?;
    Ok(Json(info(service, sub, user)))
}
