//! Web interface of CalCraft.
//!
//! The application itself is the Leptos crate `calcraft-ui`, compiled to
//! WebAssembly by `scripts/build-ui.sh`. This crate embeds the resulting
//! files (see `build.rs`) and serves them:
//!
//! - `GET /` returns the app shell (`Cache-Control: no-cache`),
//! - `GET /assets/<name>-<hash>.<ext>` returns the content-hashed assets
//!   (application, stylesheet, icons, web app manifest), cached for a year
//!   as immutable,
//! - `GET /favicon.ico` returns the icon for clients that ask for it at
//!   the conventional path (cached for a day).
//!
//! If the assets were not built, a static placeholder page is served
//! instead. Neither page uses inline scripts or styles, so both work under
//! the strict Content Security Policy set by the server.

use axum::{
    Router,
    extract::Path,
    http::{
        HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
    routing::get,
};

/// Cache policy for content-hashed assets.
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Cache policy for the app shell: always revalidate, so that a new
/// release is picked up immediately.
pub const NO_CACHE: &str = "no-cache";
/// Cache policy for `/favicon.ico` (fixed name, so not immutable).
pub const ONE_DAY: &str = "public, max-age=86400";

/// A file embedded into the binary.
#[derive(Debug)]
pub struct Asset {
    /// File name below `/assets/`, including the content hash.
    pub name: &'static str,
    pub content_type: &'static str,
    pub body: &'static [u8],
}

#[cfg(calcraft_ui_assets)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

/// Whether the web UI assets were embedded at build time.
pub const ASSETS_EMBEDDED: bool = cfg!(calcraft_ui_assets);

#[cfg(calcraft_ui_assets)]
use generated::FILES;
#[cfg(not(calcraft_ui_assets))]
static FILES: &[Asset] = &[];

/// The embedded assets (empty without a UI build).
pub fn assets() -> &'static [Asset] {
    FILES
}

/// Routes of the web UI below the root path (static assets). The root page
/// itself is served through [`index`] so that the server can combine it
/// with DAV handling on `/`.
pub fn router() -> Router {
    Router::new()
        .route("/assets/{name}", get(asset))
        .route("/favicon.ico", get(favicon))
}

fn serve(name: &str, cache_control: &'static str) -> Response {
    match assets().iter().find(|asset| asset.name == name) {
        Some(asset) => (
            [
                (CONTENT_TYPE, HeaderValue::from_static(asset.content_type)),
                (CACHE_CONTROL, HeaderValue::from_static(cache_control)),
            ],
            asset.body,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn asset(Path(name): Path<String>) -> Response {
    serve(&name, IMMUTABLE)
}

async fn favicon() -> Response {
    #[cfg(calcraft_ui_assets)]
    let name = generated::FAVICON_ICO;
    #[cfg(not(calcraft_ui_assets))]
    let name = "";
    serve(name, ONE_DAY)
}

/// The page served on `GET /`.
pub async fn index() -> Response {
    (
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (CACHE_CONTROL, HeaderValue::from_static(NO_CACHE)),
        ],
        page(),
    )
        .into_response()
}

#[cfg(calcraft_ui_assets)]
fn page() -> String {
    format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"color-scheme\" content=\"light dark\">\n\
         <meta name=\"theme-color\" content=\"#ffffff\" media=\"(prefers-color-scheme: light)\">\n\
         <meta name=\"theme-color\" content=\"#1e293b\" media=\"(prefers-color-scheme: dark)\">\n\
         <title>{name}</title>\n\
         <link rel=\"icon\" href=\"/assets/{ico}\" sizes=\"32x32\">\n\
         <link rel=\"icon\" href=\"/assets/{svg}\" type=\"image/svg+xml\">\n\
         <link rel=\"apple-touch-icon\" href=\"/assets/{touch}\">\n\
         <link rel=\"manifest\" href=\"/assets/{manifest}\">\n\
         <link rel=\"stylesheet\" href=\"/assets/{css}\">\n\
         <script type=\"module\" src=\"/assets/{script}\"></script>\n\
         </head>\n\
         <body>\n\
         <noscript>\n\
         <p>{name} needs JavaScript and WebAssembly for its web interface.</p>\n\
         <p>Version {version} · <a href=\"{source}\">Source code</a> (AGPL-3.0-or-later)</p>\n\
         </noscript>\n\
         </body>\n\
         </html>\n",
        name = calcraft_core::NAME,
        version = calcraft_core::VERSION,
        source = calcraft_core::SOURCE_URL,
        css = generated::STYLESHEET,
        script = generated::SCRIPT,
        ico = generated::FAVICON_ICO,
        svg = generated::FAVICON_SVG,
        touch = generated::APPLE_TOUCH_ICON,
        manifest = generated::MANIFEST,
    )
}

#[cfg(not(calcraft_ui_assets))]
fn page() -> String {
    format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head><meta charset=\"utf-8\"><title>{name}</title></head>\n\
         <body>\n\
         <h1>{name}</h1>\n\
         <p>Version {version}. The web interface was not included in this build \
         (run <code>scripts/build-ui.sh</code> before building the server).</p>\n\
         <p><a href=\"{source}\">Source code</a> (AGPL-3.0-or-later)</p>\n\
         </body>\n\
         </html>\n",
        name = calcraft_core::NAME,
        version = calcraft_core::VERSION,
        source = calcraft_core::SOURCE_URL,
    )
}

#[cfg(test)]
mod tests;
