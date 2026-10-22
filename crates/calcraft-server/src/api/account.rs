//! Login, session, password, two-factor authentication and app passwords.

use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header::SET_COOKIE},
    response::{IntoResponse, Response},
};
use calcraft_core::{
    Username,
    api::{
        AppPasswordInfo, ChangePassword, CreateAppPassword, CreatedAppPassword, LoginRequest, Me,
        RecoveryCodes, TotpDisable, TotpEnable, TotpSetup,
    },
};
use calcraft_store::{RECOVERY_CODES, TotpEnableOutcome, User};

use super::{ApiError, ApiJson, Client, SESSION_TTL, SessionUser, session_cookie};
use crate::{
    app::AppState,
    auth::{
        APP_PASSWORD_PREFIX, Attempt, AuthOutcome, MAX_PASSWORD_LEN, PasswordError,
        hash_password_async, now,
    },
    net::ClientKey,
    secrets::{base32_encode, hash_secret, random_bytes, random_token, totp_uri, totp_verify},
    validate::clean_text,
};

const MAX_APP_PASSWORD_NAME: usize = 64;

pub fn me_of(user: &User) -> Me {
    Me {
        username: user.username.to_string(),
        display_name: user.display_name.clone(),
        is_admin: user.is_admin,
        totp_enabled: user.totp_enabled,
    }
}

fn parse_username(name: &str) -> Result<Username, ApiError> {
    name.trim()
        .to_ascii_lowercase()
        .parse()
        .map_err(|_| ApiError::InvalidCredentials)
}

/// Normalises a recovery code as typed by a user (`abcd-efgh-…`).
fn normalize_recovery_code(code: &str) -> String {
    code.trim()
        .chars()
        .filter(|c| *c != '-' && *c != ' ')
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Checks a TOTP code (with replay protection) or consumes a recovery code.
/// Arms `attempt` once the code is compared (see [`Attempt::arm`]).
async fn second_factor(
    state: &AppState,
    attempt: &Attempt<'_>,
    user: &User,
    code: &str,
) -> Result<bool, ApiError> {
    let Some(totp) = state.store.totp_state(user.id).await? else {
        return Ok(false);
    };
    attempt.arm();
    if let Some(step) = totp_verify(&totp.secret, code, now()) {
        return Ok(state.store.accept_totp_step(user.id, step).await?);
    }
    let normalized = normalize_recovery_code(code);
    if normalized.len() != 16 {
        return Ok(false);
    }
    Ok(state
        .store
        .use_recovery_code(user.id, &hash_secret(&normalized))
        .await?)
}

/// Full authentication including the second factor, used by login and
/// sensitive account changes. One reserved attempt covers both factors, so
/// concurrent requests cannot bypass the lockout; failures of either factor
/// count, and the counter is only reset after both succeeded. A locked
/// account, or a client with too many failed logins, looks exactly like
/// wrong credentials.
async fn authenticate(
    state: &AppState,
    client: Option<ClientKey>,
    username: &Username,
    password: &str,
    code: Option<&str>,
) -> Result<User, ApiError> {
    if password.is_empty() || password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::InvalidCredentials);
    }
    let attempt = match state.auth.reserve(username, client).await {
        Ok(attempt) => attempt,
        Err(refusal) => {
            state.auth.refused(refusal).await;
            return Err(ApiError::InvalidCredentials);
        }
    };
    let user = match state
        .auth
        .check_first_factor_reserved(&attempt, password)
        .await
    {
        AuthOutcome::Authenticated(user) => user,
        AuthOutcome::Denied | AuthOutcome::LockedOut => return Err(ApiError::InvalidCredentials),
        AuthOutcome::Error => return Err(ApiError::Internal),
        AuthOutcome::Busy => return Err(ApiError::Unavailable),
    };
    // The password was right: loading the second factor must not count as
    // a failure if the request is cancelled; comparing it does.
    attempt.disarm();
    if user.totp_enabled {
        let Some(code) = code else {
            attempt.settle();
            return Err(ApiError::TotpRequired);
        };
        // A request cancelled while the code is checked counts as a failure
        // (the attempt is dropped unsettled and armed).
        match second_factor(state, &attempt, &user, code).await {
            Ok(true) => {}
            Ok(false) => {
                attempt.fail();
                return Err(ApiError::InvalidCredentials);
            }
            Err(err) => {
                attempt.settle();
                return Err(err);
            }
        }
    }
    state.auth.clear_failures(username);
    attempt.settle();
    Ok(user)
}

fn password_error(err: PasswordError) -> ApiError {
    match err {
        PasswordError::TooShort => ApiError::BadRequest("new password is too short"),
        PasswordError::TooLong => ApiError::BadRequest("new password is too long"),
        PasswordError::Reserved => ApiError::BadRequest("password must not start with ccap_"),
        PasswordError::Hash => ApiError::Internal,
    }
}

/// Password-based actions are refused when `security.password_login` is
/// off: local 2FA only protects password logins, and there is no password
/// to change or confirm.
fn require_password_login(state: &AppState) -> Result<(), ApiError> {
    if state.auth.password_login() {
        Ok(())
    } else {
        Err(ApiError::PasswordLoginDisabled)
    }
}

pub async fn login(
    State(state): State<AppState>,
    Client(client): Client,
    ApiJson(request): ApiJson<LoginRequest>,
) -> Result<Response, ApiError> {
    require_password_login(&state)?;
    let username = parse_username(&request.username)?;
    let user = authenticate(
        &state,
        client,
        &username,
        &request.password,
        request.code.as_deref(),
    )
    .await?;
    let cookie = start_session(&state, &user).await?;
    Ok(([(SET_COOKIE, cookie)], Json(me_of(&user))).into_response())
}

/// Creates a session for an authenticated user and returns the
/// `Set-Cookie` value. Shared by password and SSO login.
pub(super) async fn start_session(state: &AppState, user: &User) -> Result<String, ApiError> {
    let token = random_token::<32>();
    let created = now();
    state
        .store
        .create_session(
            user.id,
            &hash_secret(&token),
            created,
            created + SESSION_TTL,
        )
        .await?;
    Ok(session_cookie(Some(&token), state.secure_cookies))
}

pub async fn logout(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<Response, ApiError> {
    state.store.delete_session(&session.token_hash).await?;
    let cookie = session_cookie(None, state.secure_cookies);
    Ok((StatusCode::NO_CONTENT, [(SET_COOKIE, cookie)]).into_response())
}

pub async fn me(session: SessionUser) -> Json<Me> {
    Json(me_of(&session.user))
}

pub async fn change_password(
    State(state): State<AppState>,
    Client(client): Client,
    session: SessionUser,
    ApiJson(request): ApiJson<ChangePassword>,
) -> Result<Response, ApiError> {
    require_password_login(&state)?;
    let user = session.user;
    authenticate(
        &state,
        client,
        &user.username,
        &request.current,
        request.code.as_deref(),
    )
    .await?;
    let hash = hash_password_async(request.new)
        .await
        .map_err(password_error)?;
    state.store.set_password_hash(&user.username, &hash).await?;
    // All sessions end; the client has to log in again.
    state.store.delete_sessions_of(user.id).await?;
    let cookie = session_cookie(None, state.secure_cookies);
    Ok((StatusCode::NO_CONTENT, [(SET_COOKIE, cookie)]).into_response())
}

pub async fn totp_setup(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<Json<TotpSetup>, ApiError> {
    require_password_login(&state)?;
    let user = session.user;
    if user.totp_enabled {
        return Err(ApiError::Conflict(
            "two-factor authentication is already enabled",
        ));
    }
    let secret = random_bytes::<20>();
    if !state.store.set_pending_totp(user.id, &secret).await? {
        return Err(ApiError::Conflict(
            "two-factor authentication is already enabled",
        ));
    }
    Ok(Json(TotpSetup {
        secret: base32_encode(&secret),
        uri: totp_uri(&secret, user.username.as_str()),
    }))
}

fn format_recovery_code(code: &str) -> String {
    code.as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

pub async fn totp_enable(
    State(state): State<AppState>,
    Client(client): Client,
    session: SessionUser,
    ApiJson(request): ApiJson<TotpEnable>,
) -> Result<Json<RecoveryCodes>, ApiError> {
    require_password_login(&state)?;
    let user = session.user;
    let Some(totp) = state.store.totp_state(user.id).await? else {
        return Err(ApiError::BadRequest("call totp/setup first"));
    };
    if totp.enabled {
        return Err(ApiError::Conflict(
            "two-factor authentication is already enabled",
        ));
    }
    // Code guesses take a login attempt like any other: client block,
    // account lockout and the concurrency bound apply.
    let attempt = match state.auth.reserve(&user.username, client).await {
        Ok(attempt) => attempt,
        Err(refusal) => {
            state.auth.refused(refusal).await;
            return Err(ApiError::InvalidCredentials);
        }
    };
    attempt.mark_known();
    attempt.arm();
    // Proves that the authenticator app was set up correctly.
    let Some(step) = totp_verify(&totp.secret, &request.code, now()) else {
        attempt.fail();
        return Err(ApiError::InvalidCredentials);
    };
    let codes: Vec<String> = (0..RECOVERY_CODES).map(|_| random_token::<10>()).collect();
    let hashes: Vec<String> = codes.iter().map(|code| hash_secret(code)).collect();
    // Accepting the step and enabling is one transaction, so of a double
    // submit exactly one request enables and the other sees it enabled.
    let outcome = state
        .store
        .enable_totp_with_step(user.id, &totp.secret, step, &hashes)
        .await;
    match outcome {
        Ok(TotpEnableOutcome::Enabled) => attempt.settle(),
        // A valid code: not a guess.
        Ok(TotpEnableOutcome::AlreadyEnabled) => {
            attempt.settle();
            return Err(ApiError::Conflict(
                "two-factor authentication is already enabled",
            ));
        }
        Ok(TotpEnableOutcome::NotSetUp) => {
            attempt.settle();
            return Err(ApiError::BadRequest("call totp/setup first"));
        }
        Ok(TotpEnableOutcome::StepUsed) => {
            attempt.fail();
            return Err(ApiError::InvalidCredentials);
        }
        Err(err) => {
            attempt.settle();
            return Err(err.into());
        }
    }
    drop(attempt);
    Ok(Json(RecoveryCodes {
        codes: codes.iter().map(|c| format_recovery_code(c)).collect(),
    }))
}

pub async fn totp_disable(
    State(state): State<AppState>,
    Client(client): Client,
    session: SessionUser,
    ApiJson(request): ApiJson<TotpDisable>,
) -> Result<StatusCode, ApiError> {
    require_password_login(&state)?;
    let user = session.user;
    if !user.totp_enabled {
        return Err(ApiError::Conflict(
            "two-factor authentication is not enabled",
        ));
    }
    authenticate(
        &state,
        client,
        &user.username,
        &request.password,
        Some(&request.code),
    )
    .await?;
    state.store.disable_totp(user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_app_passwords(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<Json<Vec<AppPasswordInfo>>, ApiError> {
    let passwords = state.store.app_passwords(session.user.id).await?;
    Ok(Json(
        passwords
            .into_iter()
            .map(|p| AppPasswordInfo {
                id: p.id,
                name: p.name,
                created_at: p.created_at,
                last_used_at: p.last_used_at,
            })
            .collect(),
    ))
}

pub async fn create_app_password(
    State(state): State<AppState>,
    session: SessionUser,
    ApiJson(request): ApiJson<CreateAppPassword>,
) -> Result<(StatusCode, Json<CreatedAppPassword>), ApiError> {
    let name = clean_text(&request.name, MAX_APP_PASSWORD_NAME)
        .filter(|name| !name.is_empty())
        .ok_or(ApiError::BadRequest("invalid name"))?;
    // 160 bits of randomness; only a hash is stored.
    let secret = random_token::<20>();
    let id = state
        .store
        .create_app_password(session.user.id, &name, &hash_secret(&secret), now())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedAppPassword {
            id,
            name,
            password: format!("{APP_PASSWORD_PREFIX}{id}_{secret}"),
        }),
    ))
}

pub async fn delete_app_password(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    if state.store.delete_app_password(session.user.id, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_codes_round_trip() {
        let code = "abcdefghijklmnop";
        let shown = format_recovery_code(code);
        assert_eq!(shown, "abcd-efgh-ijkl-mnop");
        assert_eq!(normalize_recovery_code(&shown.to_uppercase()), code);
        assert_eq!(normalize_recovery_code(" abcd efgh-ijkl mnop "), code);
    }
}
