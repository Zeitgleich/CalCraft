//! CalCraft server: configuration, HTTP routing and middleware.

#[cfg(feature = "api")]
pub mod api;
pub mod app;
pub mod auth;
pub mod config;
pub mod dav;
#[cfg(feature = "https-client")]
pub mod https;
pub mod limits;
pub mod net;
#[cfg(feature = "oidc")]
pub mod oidc;
pub mod reindex;
pub mod secrets;
pub mod security;
#[cfg(feature = "subscriptions")]
pub mod subscriptions;
pub mod telemetry;
pub mod transfer;
pub mod trash;
#[cfg(feature = "update-check")]
pub mod updates;
pub mod validate;

pub use app::{AppState, router};
pub use config::Config;

/// Whether the web UI is compiled into this binary.
pub const WEB_UI_COMPILED: bool = cfg!(feature = "web-ui");

/// Whether the REST API is compiled into this binary.
pub const API_COMPILED: bool = cfg!(feature = "api");

/// Whether single sign-on (OpenID Connect) is compiled into this binary.
pub const OIDC_COMPILED: bool = cfg!(feature = "oidc");

/// Whether the optional update check is compiled into this binary.
pub const UPDATE_CHECK_COMPILED: bool = cfg!(feature = "update-check");

/// Whether calendar subscriptions are compiled into this binary.
pub const SUBSCRIPTIONS_COMPILED: bool = cfg!(feature = "subscriptions");
