//! Client-side form validation. The server stays authoritative; these
//! checks only give quicker and clearer feedback.

use calcraft_core::{Slug, Username};

use crate::text;

/// Minimum password length, as enforced by the server.
pub const MIN_PASSWORD_LEN: usize = 12;
/// Maximum password length in bytes, as enforced by the server.
pub const MAX_PASSWORD_LEN: usize = 1024;
/// Prefix reserved for app passwords.
pub const APP_PASSWORD_PREFIX: &str = "ccap_";
/// Maximum length of an app password name.
pub const MAX_APP_PASSWORD_NAME: usize = 64;
/// Maximum length of a display name.
pub const MAX_DISPLAY_NAME: usize = 256;
/// Maximum length of a description.
pub const MAX_DESCRIPTION: usize = 4096;

/// `#RRGGBB` or `#RRGGBBAA`, the formats the server accepts.
pub fn is_valid_color(value: &str) -> bool {
    value
        .strip_prefix('#')
        .is_some_and(|hex| matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Normalizes a stored color for `<input type="color">`, which only
/// understands lowercase `#rrggbb` (an alpha channel is dropped).
pub fn color_for_picker(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if !is_valid_color(value) {
        return None;
    }
    Some(value[..7].to_ascii_lowercase())
}

/// Checks a collection slug (the last segment of the DAV URL).
pub fn check_slug(slug: &str) -> Result<(), &'static str> {
    slug.parse::<Slug>()
        .map(|_| ())
        .map_err(|_| text::ERR_SLUG_INVALID)
}

/// Checks a username for a new account.
pub fn check_username(username: &str) -> Result<(), &'static str> {
    username
        .parse::<Username>()
        .map(|_| ())
        .map_err(|_| text::ERR_USERNAME_INVALID)
}

/// Checks a new password and its confirmation.
pub fn check_new_password(password: &str, confirmation: &str) -> Result<(), &'static str> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(text::ERR_PASSWORD_TOO_SHORT);
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err(text::ERR_PASSWORD_TOO_LONG);
    }
    if password.starts_with(APP_PASSWORD_PREFIX) {
        return Err(text::ERR_PASSWORD_RESERVED);
    }
    if password != confirmation {
        return Err(text::ERR_PASSWORD_MISMATCH);
    }
    Ok(())
}

/// Longest feed URL the server accepts.
pub const MAX_FEED_URL: usize = 2048;

/// Checks a calendar feed URL: `https://`, `webcal://` (or `http://`,
/// which the server only accepts if its administrator allows it). The
/// server checks the address itself.
pub fn check_feed_url(url: &str) -> Result<(), &'static str> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    let scheme = ["https://", "webcal://", "webcals://", "http://"]
        .iter()
        .any(|prefix| lower.starts_with(prefix) && lower.len() > prefix.len());
    if !scheme || url.len() > MAX_FEED_URL || url.chars().any(char::is_whitespace) {
        return Err(text::ERR_FEED_URL_INVALID);
    }
    Ok(())
}

/// Checks the name of a new app password.
pub fn check_app_password_name(name: &str) -> Result<(), &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(text::ERR_NAME_REQUIRED);
    }
    if name.chars().count() > MAX_APP_PASSWORD_NAME {
        return Err(text::ERR_NAME_TOO_LONG);
    }
    Ok(())
}

/// Checks an optional text field against a maximum length in characters.
pub fn check_length(value: &str, max: usize) -> Result<(), &'static str> {
    if value.chars().count() > max {
        Err(text::ERR_TEXT_TOO_LONG)
    } else {
        Ok(())
    }
}

/// Removes spaces from a TOTP or recovery code as typed by the user; the
/// server accepts recovery codes with or without dashes.
pub fn clean_code(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Suggests a slug for a display name: lowercase ASCII letters and digits,
/// everything else becomes `-`.
pub fn suggest_slug(display_name: &str) -> String {
    let mut slug = String::new();
    for c in display_name.trim().chars() {
        let c = match c {
            'ä' | 'Ä' => "ae",
            'ö' | 'Ö' => "oe",
            'ü' | 'Ü' => "ue",
            'ß' => "ss",
            c if c.is_ascii_alphanumeric() => {
                slug.push(c.to_ascii_lowercase());
                continue;
            }
            _ => "-",
        };
        if c == "-" && (slug.is_empty() || slug.ends_with('-')) {
            continue;
        }
        slug.push_str(c);
    }
    let slug = slug.trim_end_matches('-');
    slug.chars().take(Slug::MAX_LEN).collect::<String>()
}

#[cfg(test)]
mod tests;
