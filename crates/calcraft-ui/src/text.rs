//! All user-visible texts of the web interface (English).
//!
//! Kept in one place so that translations can be added later (PLAN.md,
//! UI-5) by turning these constants into a lookup per language.

// Navigation and layout
pub const NAV_LABEL: &str = "Main navigation";
pub const NAV_OVERVIEW: &str = "Overview";
pub const NAV_SETTINGS: &str = "Account";
pub const NAV_APP_PASSWORDS: &str = "App passwords";
pub const NAV_ADMIN: &str = "Administration";
pub const LOGOUT: &str = "Log out";
pub const LOADING: &str = "Loading …";
pub const SOURCE_CODE: &str = "Source code";
pub const LICENSE_NOTE: &str = "(AGPL-3.0-or-later)";
pub const UNAVAILABLE_TITLE: &str = "Service unavailable";
pub const UNAVAILABLE_HINT: &str = "The REST API could not be reached. It may be disabled in the server configuration (api.enabled).";

// Common words
pub const USERNAME: &str = "Username";
pub const PASSWORD: &str = "Password";
pub const DISPLAY_NAME: &str = "Display name";
pub const DESCRIPTION: &str = "Description";
pub const NAME: &str = "Name";
pub const CREATE: &str = "Create";
pub const SAVE: &str = "Save";
pub const EDIT: &str = "Edit";
pub const CANCEL: &str = "Cancel";
pub const DELETE: &str = "Delete";
pub const ACTIONS: &str = "Actions";
pub const STATUS: &str = "Status";
pub const NEVER: &str = "never";
pub const ROLE: &str = "Role";
pub const ROLE_ADMIN: &str = "Administrator";
pub const ROLE_USER: &str = "User";

// Login
pub const LOGIN_TITLE: &str = "Log in";
pub const LOGIN: &str = "Log in";
pub const LOGIN_ENTER_CODE: &str = "Enter the code from your authenticator app or a recovery code.";
pub const LOGGED_OUT: &str = "You have been logged out.";
pub const TOTP_CODE: &str = "Two-factor code";
pub const TOTP_CODE_HINT: &str = "6-digit code from your authenticator app, or a recovery code.";

// Overview
pub const OVERVIEW_TITLE: &str = "Calendars and address books";
pub const DAV_SETUP_HINT: &str = "To set up a client (Thunderbird, DAVx⁵, Apple Calendar, …) use the server address with your username and an app password:";
pub const CALENDARS: &str = "Calendars";
pub const ADDRESS_BOOKS: &str = "Address books";
pub const CALENDAR: &str = "Calendar";
pub const ADDRESS_BOOK: &str = "Address book";
pub const NO_CALENDARS: &str = "No calendars yet.";
pub const NO_ADDRESS_BOOKS: &str = "No address books yet.";
pub const DAV_URL: &str = "DAV URL:";
pub const COMPONENTS: &str = "Contains:";
pub const CREATE_COLLECTION: &str = "New calendar or address book";
pub const KIND: &str = "Type";
pub const SLUG: &str = "Short name (part of the URL)";
pub const SLUG_HINT: &str = "Lowercase letters a-z, digits and . _ -, starting with a letter or digit. Cannot be changed later.";
pub const COLOR: &str = "Color";
pub const USE_COLOR: &str = "Use a color";
pub const DELETE_CONFIRM: &str = "Delete permanently";

pub fn confirm_delete_collection(name: &str) -> String {
    format!("Delete “{name}” with all its entries? This cannot be undone.")
}

// Account settings
pub const SETTINGS_TITLE: &str = "Account";
pub const ACCOUNT: &str = "Your account";
pub const CHANGE_PASSWORD: &str = "Change password";
pub const CURRENT_PASSWORD: &str = "Current password";
pub const NEW_PASSWORD: &str = "New password";
pub const CONFIRM_PASSWORD: &str = "Repeat new password";
pub const PASSWORD_RULES: &str = "At least 12 characters.";
pub const PASSWORD_CHANGE_LOGOUT: &str = "All sessions end after a password change; you will have to log in again. App passwords stay valid.";
pub const PASSWORD_CHANGED: &str = "Your password has been changed. Please log in again.";

// Two-factor authentication
pub const TWO_FACTOR: &str = "Two-factor authentication";
pub const TWO_FACTOR_ON: &str = "2FA";
pub const TOTP_OFF: &str = "Two-factor authentication is off.";
pub const TOTP_ON: &str = "Two-factor authentication is on.";
pub const TOTP_SET_UP: &str = "Set up two-factor authentication";
pub const TOTP_SETUP_HINT: &str = "Add this account to your authenticator app, either by entering the secret or by using the otpauth URI. Then enter the code the app shows.";
pub const TOTP_SECRET: &str = "Secret";
pub const TOTP_URI: &str = "otpauth URI";
pub const TOTP_CONFIRM_CODE: &str = "Code from the app";
pub const TOTP_ENABLE: &str = "Enable";
pub const TOTP_ENABLED: &str = "Two-factor authentication is now enabled.";
pub const TOTP_DISABLE: &str = "Disable two-factor authentication";
pub const TOTP_DISABLE_HINT: &str = "Confirm with your password and a current code.";
pub const TOTP_DISABLED: &str = "Two-factor authentication has been disabled.";
pub const RECOVERY_CODES: &str = "Recovery codes";
pub const RECOVERY_CODES_HINT: &str = "Store these codes in a safe place. Each code can be used once instead of an authenticator code. They are shown only now.";
pub const RECOVERY_CODES_SAVED: &str = "I have saved the codes";

// App passwords
pub const APP_PASSWORDS_TITLE: &str = "App passwords";
pub const APP_PASSWORDS_HINT: &str = "Calendar and contact apps log in with your username and an app password. Create one per device so that you can revoke it individually. With two-factor authentication on, DAV clients must use app passwords.";
pub const NO_APP_PASSWORDS: &str = "No app passwords yet.";
pub const CREATED: &str = "Created";
pub const LAST_USED: &str = "Last used";
pub const REVOKE: &str = "Revoke";
pub const REVOKE_CONFIRM: &str = "Revoke now";
pub const CREATE_APP_PASSWORD: &str = "New app password";
pub const APP_PASSWORD_NAME: &str = "Device or app";
pub const APP_PASSWORD_NAME_HINT: &str = "For example “Phone” or “Thunderbird at work”.";
pub const APP_PASSWORD_ONCE: &str =
    "Copy this password into your app now. It is shown only once and cannot be retrieved later.";
pub const APP_PASSWORD_DONE: &str = "Done";

pub fn app_password_created(name: &str) -> String {
    format!("App password for “{name}” created")
}

pub fn confirm_revoke(name: &str) -> String {
    format!("Revoke “{name}”? Apps using it stop syncing.")
}

// Administration
pub const ADMIN_TITLE: &str = "Administration";
pub const USERS: &str = "Users";
pub const ACTIVE: &str = "active";
pub const DISABLED: &str = "disabled";
pub const YOU: &str = "(you)";
pub const ENABLE: &str = "Enable";
pub const DISABLE: &str = "Disable";
pub const GRANT_ADMIN: &str = "Make administrator";
pub const REVOKE_ADMIN: &str = "Remove administrator";
pub const CREATE_USER: &str = "New user";
pub const USERNAME_HINT: &str =
    "Lowercase letters a-z, digits and . _ -, starting with a letter or digit.";
pub const MAKE_ADMIN: &str = "Administrator";

pub fn user_created(username: &str) -> String {
    format!("User “{username}” created.")
}

// Update notice
pub const RELEASE_NOTES: &str = "Release notes";
pub const UPDATE_CHECK_OFF: &str = "Update notices are off. To be told about new releases, set check = true in the [updates] section of the configuration (the server then asks GitHub regularly; GitHub sees its IP address).";

pub fn update_available(version: &str) -> String {
    format!("New version {version} available.")
}

// Errors
pub const ERR_INVALID_CREDENTIALS: &str =
    "Wrong username, password or code, or too many attempts. Please wait a moment and try again.";
pub const ERR_SESSION_EXPIRED: &str = "Your session has expired. Please log in again.";
pub const ERR_TOTP_REQUIRED: &str = "Please enter your two-factor code.";
pub const ERR_FORBIDDEN: &str = "You are not allowed to do this.";
pub const ERR_CSRF: &str = "The request was rejected (missing CSRF header).";
pub const ERR_NETWORK: &str = "The server could not be reached. Please check your connection.";
pub const ERR_NOT_FOUND: &str = "Not found. It may have been deleted in the meantime.";
pub const ERR_UNKNOWN: &str = "Something went wrong.";
pub const ERR_LOGIN_FIELDS: &str = "Please enter username and password.";
pub const ERR_CURRENT_PASSWORD: &str = "Please enter your current password.";
pub const ERR_DISABLE_FIELDS: &str = "Please enter your password and a code.";
pub const ERR_SLUG_INVALID: &str = "Invalid short name: use 1-64 characters a-z, 0-9, '.', '_' or '-', starting with a letter or digit.";
pub const ERR_USERNAME_INVALID: &str = "Invalid username: use 1-64 characters a-z, 0-9, '.', '_' or '-', starting with a letter or digit.";
pub const ERR_PASSWORD_TOO_SHORT: &str = "The password must be at least 12 characters long.";
pub const ERR_PASSWORD_TOO_LONG: &str = "The password is too long.";
pub const ERR_PASSWORD_RESERVED: &str = "The password must not start with “ccap_”.";
pub const ERR_PASSWORD_MISMATCH: &str = "The passwords do not match.";
pub const ERR_NAME_REQUIRED: &str = "Please enter a name.";
pub const ERR_NAME_TOO_LONG: &str = "The name is too long (at most 64 characters).";
pub const ERR_TEXT_TOO_LONG: &str = "The text is too long.";
pub const ERR_COLOR_INVALID: &str = "The color must have the form #RRGGBB.";
