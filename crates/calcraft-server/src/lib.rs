//! CalCraft server: configuration, HTTP routing and middleware.

pub mod app;
pub mod config;
pub mod security;
pub mod telemetry;

pub use app::router;
pub use config::Config;

/// Whether the web UI is compiled into this binary.
pub const WEB_UI_COMPILED: bool = cfg!(feature = "web-ui");

/// Whether the REST API is compiled into this binary.
pub const API_COMPILED: bool = cfg!(feature = "api");
