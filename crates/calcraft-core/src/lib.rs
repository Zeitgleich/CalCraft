//! Core domain types shared by all CalCraft crates.
//!
//! This crate must stay free of I/O so it can be compiled for `wasm32`.

pub mod api;
pub mod mail;
mod names;

pub use names::{CollectionKind, InvalidName, Slug, Username};

/// Product name.
pub const NAME: &str = "CalCraft";

/// CalCraft version, taken from the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Public location of the source code, linked from the web UI (AGPL-3.0 §13).
pub const SOURCE_URL: &str = env!("CARGO_PKG_REPOSITORY");
