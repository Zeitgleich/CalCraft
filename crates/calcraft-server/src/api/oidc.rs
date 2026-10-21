//! SSO endpoints: `GET /api/v1/auth/oidc/login` and `…/callback`.
//!
//! Both are top-level browser navigations, so they answer with redirects
//! instead of JSON. Redirect targets are fixed relative paths (`/` or
//! `/?sso_error=<code>` with a code from a fixed list); nothing from the
//! request is reflected, so there is no open redirect.
//!
//! Account mapping: the configured claim names the local user (never the
//! e-mail address). Accounts created by SSO are bound to issuer + subject
//! at creation; an existing local account is only bound after a one-time
//! operator approval for that IdP subject (`calcraft user allow-sso-link
//! --subject`), never by name alone.
//! Later logins must present the bound subject. Unknown users are only
//! created with `oidc.auto_create_users`; disabled users are rejected.

use axum::{
    extract::{Query, State, rejection::QueryRejection},
    http::{
        HeaderMap, StatusCode,
        header::{CACHE_CONTROL, COOKIE, LOCATION, SET_COOKIE},
    },
    response::{IntoResponse, Response},
};
use calcraft_store::{ApprovedBinding, OidcBinding, User};

use super::{ApiError, account::start_session};
use crate::{
    app::AppState,
    auth::{UNUSABLE_PASSWORD_HASH, now},
    oidc::{BINDING_COOKIE, CallbackParams, Oidc, OidcError, PENDING_TTL, jwt::IdToken},
};

/// Path the binding cookie is restricted to.
const COOKIE_PATH: &str = "/api/v1/auth/oidc";

/// `Set-Cookie` for the browser binding. `SameSite=Lax`, because the
/// callback is a cross-site top-level navigation from the IdP and a
/// `Strict` cookie would not be sent.
fn binding_cookie(value: Option<&str>, secure: bool) -> String {
    let (value, max_age) = match value {
        Some(value) => (value, PENDING_TTL.as_secs()),
        None => ("", 0),
    };
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{BINDING_COOKIE}={value}; Path={COOKIE_PATH}; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    )
}

fn binding_from(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == BINDING_COOKIE)
        .map(|(_, value)| value)
}

/// Why an SSO login failed, as shown to the browser (`/?sso_error=…`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// Missing, expired, replayed or foreign state; malformed callback.
    InvalidRequest,
    /// The user cancelled or the IdP refused.
    Denied,
    /// IdP unreachable or misconfigured.
    Unavailable,
    /// Token invalid, account disabled, subject mismatch, not allowed.
    LoginFailed,
    /// Valid IdP login, but no local account and auto-creation is off.
    NoAccount,
    /// A local account with this name exists but was never bound, and an
    /// operator has not approved linking (`calcraft user allow-sso-link`).
    AccountExists,
    /// The account has local 2FA and `oidc.require_local_totp` is set.
    LocalTotpRequired,
}

impl Failure {
    fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Denied => "denied",
            Self::Unavailable => "unavailable",
            Self::LoginFailed => "login_failed",
            Self::NoAccount => "no_account",
            Self::AccountExists => "account_exists",
            Self::LocalTotpRequired => "local_2fa_required",
        }
    }
}

fn redirect(location: &str, cookies: &[String]) -> Response {
    let mut response = (
        StatusCode::SEE_OTHER,
        [
            (LOCATION, location.to_owned()),
            (CACHE_CONTROL, "no-store".to_owned()),
        ],
    )
        .into_response();
    for cookie in cookies {
        if let Ok(value) = cookie.parse() {
            response.headers_mut().append(SET_COOKIE, value);
        }
    }
    response
}

fn failure(reason: Failure, secure: bool) -> Response {
    redirect(
        &format!("/?sso_error={}", reason.code()),
        &[binding_cookie(None, secure)],
    )
}

pub async fn login(State(state): State<AppState>) -> Response {
    let Some(oidc) = state.oidc.clone() else {
        return ApiError::NotFound.into_response();
    };
    match oidc.begin().await {
        Ok(start) => redirect(
            &start.location,
            &[binding_cookie(Some(&start.binding), state.secure_cookies)],
        ),
        Err(err) => {
            tracing::warn!(%err, "cannot start SSO login");
            failure(Failure::Unavailable, state.secure_cookies)
        }
    }
}

pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    params: Result<Query<CallbackParams>, QueryRejection>,
) -> Response {
    let Some(oidc) = state.oidc.clone() else {
        return ApiError::NotFound.into_response();
    };
    let secure = state.secure_cookies;
    let Ok(Query(params)) = params else {
        return failure(Failure::InvalidRequest, secure);
    };
    let token = match oidc.complete(&params, binding_from(&headers)).await {
        Ok(token) => token,
        Err(err) => {
            let reason = match err {
                OidcError::InvalidState | OidcError::BadCallback => Failure::InvalidRequest,
                OidcError::IdpError => Failure::Denied,
                OidcError::Unavailable(_) => Failure::Unavailable,
                OidcError::Exchange(_) | OidcError::Token(_) => Failure::LoginFailed,
            };
            tracing::warn!(%err, "SSO login rejected");
            return failure(reason, secure);
        }
    };
    let user = match map_user(&state, &oidc, &token).await {
        Ok(user) => user,
        Err(reason) => return failure(reason, secure),
    };
    match start_session(&state, &user).await {
        Ok(session) => {
            tracing::info!(user = %user.username, "SSO login");
            redirect("/", &[binding_cookie(None, secure), session])
        }
        Err(_) => failure(Failure::Unavailable, secure),
    }
}

fn store_failure(err: calcraft_store::StoreError) -> Failure {
    tracing::error!(%err, "storage error during SSO login");
    Failure::Unavailable
}

/// Finds (or creates) the local account for a verified ID token.
async fn map_user(state: &AppState, oidc: &Oidc, token: &IdToken) -> Result<User, Failure> {
    let claims = &token.claims;
    let issuer = oidc.issuer();
    let Some(username) = oidc.username(claims) else {
        tracing::warn!("SSO login rejected: username claim missing or not a valid username");
        return Err(Failure::LoginFailed);
    };
    if !oidc.allowed(claims) {
        tracing::warn!(user = %username, "SSO login rejected: required claim missing");
        return Err(Failure::LoginFailed);
    }
    let bound_to = state
        .store
        .user_id_by_oidc_identity(issuer, &token.subject)
        .await
        .map_err(store_failure)?;
    let admin = oidc.admin(claims);

    let existing = state
        .store
        .user_credentials(&username)
        .await
        .map_err(store_failure)?
        .map(|c| c.user);
    let (mut user, created) = match existing {
        Some(user) => (user, false),
        None if oidc.auto_create_users() => {
            if bound_to.is_some() {
                // The IdP account is bound to another local user (renamed?).
                tracing::warn!(user = %username, "SSO login rejected: identity bound to another account");
                return Err(Failure::LoginFailed);
            }
            let user = state
                .store
                .create_user_with_role(
                    &username,
                    UNUSABLE_PASSWORD_HASH,
                    Oidc::display_name(claims).as_deref(),
                    admin.unwrap_or(false),
                )
                .await
                .map_err(store_failure)?;
            tracing::info!(user = %username, admin = user.is_admin, "created user on first SSO login");
            (user, true)
        }
        None => {
            tracing::info!(user = %username, "SSO login rejected: no such local user");
            return Err(Failure::NoAccount);
        }
    };
    if user.disabled {
        tracing::info!(user = %username, "SSO login rejected: account disabled");
        return Err(Failure::LoginFailed);
    }
    if bound_to.is_some_and(|id| id != user.id) {
        tracing::warn!(user = %username, "SSO login rejected: identity bound to another account");
        return Err(Failure::LoginFailed);
    }
    if oidc.require_local_totp() && user.totp_enabled {
        tracing::info!(user = %username, "SSO login rejected: account uses local 2FA");
        return Err(Failure::LocalTotpRequired);
    }
    // Never link a pre-existing account just because the name matches
    // (IdPs may let users pick names): it needs a one-time operator
    // approval for exactly this issuer + subject. Accounts created by SSO
    // above are bound right away.
    let unbound = !created
        && bound_to.is_none()
        && !state
            .store
            .has_oidc_identity(user.id, issuer)
            .await
            .map_err(store_failure)?;
    let binding = if unbound {
        // Approval and binding in one transaction.
        match state
            .store
            .bind_approved_oidc_identity(user.id, issuer, &token.subject, now())
            .await
            .map_err(store_failure)?
        {
            ApprovedBinding::Bound => {
                tracing::info!(user = %username, "linked existing account to SSO (operator approval)");
                OidcBinding::Bound
            }
            ApprovedBinding::NotApproved => {
                tracing::warn!(
                    user = %username,
                    "SSO login rejected: existing account not approved for this IdP identity (calcraft user allow-sso-link)"
                );
                return Err(Failure::AccountExists);
            }
            ApprovedBinding::Mismatch => OidcBinding::Mismatch,
        }
    } else {
        state
            .store
            .bind_oidc_identity(user.id, issuer, &token.subject, now())
            .await
            .map_err(store_failure)?
    };
    match binding {
        OidcBinding::Bound => {
            tracing::info!(user = %username, "bound SSO identity to account");
        }
        OidcBinding::Matches => {}
        OidcBinding::Mismatch => {
            tracing::warn!(
                user = %username,
                "SSO login rejected: account is bound to a different identity of this issuer"
            );
            return Err(Failure::LoginFailed);
        }
    }
    if let Some(admin) = admin
        && admin != user.is_admin
    {
        state
            .store
            .set_admin(&user.username, admin)
            .await
            .map_err(store_failure)?;
        tracing::info!(user = %username, admin, "admin rights updated from SSO claim");
        user.is_admin = admin;
    }
    Ok(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_cookie_attributes() {
        let cookie = binding_cookie(Some("v"), true);
        for part in [
            "HttpOnly",
            "SameSite=Lax",
            "Secure",
            "Path=/api/v1/auth/oidc",
            "Max-Age=600",
        ] {
            assert!(cookie.contains(part), "{part}");
        }
        assert!(binding_cookie(None, false).contains("Max-Age=0"));
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, "calcraft_oidc=abc; x=1".parse().unwrap());
        assert_eq!(binding_from(&headers), Some("abc"));
    }
}
