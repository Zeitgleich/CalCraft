//! Web interface of CalCraft.
//!
//! The Leptos application replaces this placeholder in milestone M4. The page
//! deliberately uses no inline scripts or styles so it works under the strict
//! Content Security Policy set by the server.

use axum::{Router, response::Html};

/// Routes of the web UI below the root path (static assets, later the app).
/// The root page itself is served through [`index`] so that the server can
/// combine it with DAV handling on `/`.
pub fn router() -> Router {
    Router::new()
}

/// The page served on `GET /`.
pub async fn index() -> Html<String> {
    Html(format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head><meta charset=\"utf-8\"><title>{name}</title></head>\n\
         <body>\n\
         <h1>{name}</h1>\n\
         <p>Version {version}. The web interface is under development.</p>\n\
         <p><a href=\"{source}\">Source code</a> (AGPL-3.0-or-later)</p>\n\
         </body>\n\
         </html>\n",
        name = calcraft_core::NAME,
        version = calcraft_core::VERSION,
        source = calcraft_core::SOURCE_URL,
    ))
}
