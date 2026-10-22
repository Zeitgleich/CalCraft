//! Single sign-on helpers: the login button and the error codes the
//! server's SSO callback reports back (`/?sso_error=<code>`).
//!
//! The query value comes from the URL and is therefore untrusted: it is
//! only ever compared against the fixed list of codes, never shown.

use calcraft_core::api::SsoLink;

use crate::{format_timestamp, text};

/// Query parameter the server uses to report a failed SSO login.
pub const SSO_ERROR_PARAM: &str = "sso_error";

/// Where the SSO login starts if the server sends no usable URL.
pub const DEFAULT_LOGIN_URL: &str = "/api/v1/auth/oidc/login";

/// Fixed text for an SSO error code. Unknown codes get a generic text, so
/// nothing from the URL is ever reflected.
pub fn error_message(code: &str) -> &'static str {
    match code {
        "invalid_request" => text::SSO_ERR_INVALID_REQUEST,
        "denied" => text::SSO_ERR_DENIED,
        "unavailable" => text::SSO_ERR_UNAVAILABLE,
        "login_failed" => text::SSO_ERR_LOGIN_FAILED,
        "no_account" => text::SSO_ERR_NO_ACCOUNT,
        "account_exists" => text::SSO_ERR_ACCOUNT_EXISTS,
        "local_2fa_required" => text::SSO_ERR_LOCAL_2FA_REQUIRED,
        _ => text::SSO_ERR_UNKNOWN,
    }
}

/// Splits `location.search` (`?a=1&sso_error=x`) into the SSO error code,
/// if present, and the remaining query string (with leading `?`, or empty).
pub fn take_error(search: &str) -> (Option<String>, String) {
    let query = search.strip_prefix('?').unwrap_or(search);
    let mut code = None;
    let mut rest = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=') {
            Some((SSO_ERROR_PARAM, value)) => code = Some(value.to_owned()),
            None if pair == SSO_ERROR_PARAM => code = Some(String::new()),
            _ => rest.push(pair),
        }
    }
    let rest = if rest.is_empty() {
        String::new()
    } else {
        format!("?{}", rest.join("&"))
    };
    (code, rest)
}

/// Text of the SSO button: the configured label, or a generic one.
pub fn button_label(label: &str) -> String {
    /// Keeps a misconfigured label from breaking the login layout.
    const MAX_CHARS: usize = 60;
    let label = label.trim();
    if label.is_empty() {
        text::SSO_LOGIN_GENERIC.to_owned()
    } else if label.chars().count() > MAX_CHARS {
        let mut short: String = label.chars().take(MAX_CHARS - 1).collect();
        short.push('…');
        short
    } else {
        label.to_owned()
    }
}

/// The login URL from the server, if it is a same-origin absolute path;
/// anything else (other origins, `//host`, schemes) falls back to the
/// default path.
pub fn login_url(url: &str) -> &str {
    let same_origin_path = url.starts_with('/')
        && !url.starts_with("//")
        && !url.starts_with("/\\")
        && !url.chars().any(|c| c.is_control() || c == '\\');
    if same_origin_path {
        url
    } else {
        DEFAULT_LOGIN_URL
    }
}

/// Host (with port) of an issuer URL for display, e.g.
/// `https://id.example.com/realms/staff` → `id.example.com`. Falls back to
/// the trimmed input if it has no `scheme://`.
pub fn issuer_host(issuer: &str) -> &str {
    let issuer = issuer.trim();
    let Some((_, rest)) = issuer.split_once("://") else {
        return issuer;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // Drop userinfo, should a misconfigured issuer contain any.
    let host = authority.rsplit('@').next().unwrap_or(authority);
    if host.is_empty() { issuer } else { host }
}

/// Status line of the account page, e.g. "Linked to single sign-on
/// (id.example.com) since 2026-10-21 11:14 UTC".
pub fn link_status(link: &SsoLink) -> String {
    let host = issuer_host(&link.issuer);
    if !link.linked {
        return format!("{} ({host})", text::SSO_NOT_LINKED);
    }
    match link.linked_at {
        Some(at) => format!(
            "{} ({host}) {} {}",
            text::SSO_LINKED,
            text::SINCE,
            format_timestamp(at)
        ),
        None => format!("{} ({host})", text::SSO_LINKED),
    }
}

/// "Last single sign-on login: …", if known.
pub fn last_login(link: &SsoLink) -> Option<String> {
    link.last_login_at
        .filter(|_| link.linked)
        .map(|at| format!("{} {}", text::SSO_LAST_LOGIN, format_timestamp(at)))
}

#[cfg(test)]
mod tests;
