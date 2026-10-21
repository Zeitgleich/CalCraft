//! CalCraft server: configuration, HTTP routing and middleware.

#[cfg(feature = "api")]
pub mod api;
pub mod app;
pub mod auth;
pub mod config;
pub mod dav;
pub mod secrets;
pub mod security;
pub mod telemetry;
pub mod validate;

pub use app::{AppState, router};
pub use config::Config;

/// Whether the web UI is compiled into this binary.
pub const WEB_UI_COMPILED: bool = cfg!(feature = "web-ui");

/// Whether the REST API is compiled into this binary.
pub const API_COMPILED: bool = cfg!(feature = "api");
