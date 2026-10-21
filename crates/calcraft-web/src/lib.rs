//! Web interface of CalCraft.
//!
//! The application itself is the Leptos crate `calcraft-ui`, compiled to
//! WebAssembly by `scripts/build-ui.sh`. This crate embeds the resulting
//! files (see `build.rs`) and serves them:
//!
//! - `GET /` returns the app shell (`Cache-Control: no-cache`),
//! - `GET /assets/<name>-<hash>.<ext>` returns the content-hashed assets,
//!   cached for a year as immutable.
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
    Router::new().route("/assets/{name}", get(asset))
}

async fn asset(Path(name): Path<String>) -> Response {
    match assets().iter().find(|asset| asset.name == name) {
        Some(asset) => (
            [
                (CONTENT_TYPE, HeaderValue::from_static(asset.content_type)),
                (CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE)),
            ],
            asset.body,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
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
         <title>{name}</title>\n\
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
mod tests {
    use super::*;

    #[test]
    fn page_links_source_without_inline_code() {
        let page = page();
        assert!(page.contains(calcraft_core::SOURCE_URL));
        assert!(!page.contains("style="));
        assert!(!page.contains("<style"));
        // Scripts only by reference.
        for line in page.lines().filter(|l| l.contains("<script")) {
            assert!(line.contains(" src=\"/assets/"), "{line}");
        }
    }

    #[test]
    fn assets_are_complete_and_hashed() {
        let assets = assets();
        if !ASSETS_EMBEDDED {
            assert!(assets.is_empty());
            return;
        }
        for ext in ["js", "wasm", "css"] {
            assert!(assets.iter().any(|a| a.name.ends_with(ext)), "{ext}");
        }
        for asset in assets {
            let (stem, _) = asset.name.rsplit_once('.').unwrap_or_default();
            let (_, hash) = stem.rsplit_once('-').unwrap_or_default();
            assert_eq!(hash.len(), 16, "{}", asset.name);
            assert!(!asset.body.is_empty());
        }
    }
}
