//! Managed attachments (RFC 8607): settings and helpers shared by the DAV
//! handlers (`dav/attachments.rs`) and the request body limit. See
//! docs/attachments.md for the design and the access model.

use calcraft_ical::{Property, attach};
use calcraft_store::AttachmentMeta;
use tokio::sync::Semaphore;

use crate::config::AttachmentsConfig;

/// Uploads read into memory at the same time; further uploads wait.
pub const MAX_CONCURRENT_UPLOADS: usize = 4;
/// Path below which attachments are served.
pub const ATTACHMENTS_PATH: &str = "/dav/attachments/";

/// Managed attachments, if enabled (`[attachments] enabled`).
#[derive(Debug)]
pub struct Attachments {
    pub config: AttachmentsConfig,
    /// Bounds the memory held by uploads in flight
    /// ([`MAX_CONCURRENT_UPLOADS`] × `max_bytes`).
    pub uploads: Semaphore,
}

impl Attachments {
    pub fn new(config: &AttachmentsConfig) -> Self {
        Self {
            config: config.clone(),
            uploads: Semaphore::new(MAX_CONCURRENT_UPLOADS),
        }
    }

    /// The download URL of an attachment, from `attachments.base_url`.
    pub fn url(&self, managed_id: &str) -> String {
        format!("{}{ATTACHMENTS_PATH}{managed_id}", self.config.base())
    }

    /// The canonical `ATTACH` property of a stored attachment.
    pub fn property(&self, meta: &AttachmentMeta) -> Property {
        attach::managed_property(
            &self.url(&meta.managed_id),
            &meta.managed_id,
            &meta.filename,
            &meta.media_type,
            u64::try_from(meta.size).unwrap_or(0),
        )
    }

    /// `attachments.max_bytes_per_user` for the store.
    pub fn max_user_bytes(&self) -> i64 {
        i64::try_from(self.config.max_bytes_per_user).unwrap_or(i64::MAX)
    }

    /// `attachments.max_per_object` as a count.
    pub fn max_per_object(&self) -> usize {
        usize::try_from(self.config.max_per_object).unwrap_or(usize::MAX)
    }
}

/// A new managed id: 128 random bits as lower-case hex.
pub fn new_managed_id() -> String {
    crate::secrets::random_bytes::<16>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether a request is an upload (`attachment-add` or
/// `attachment-update`), which may carry a body up to `max_bytes`.
pub fn is_upload(method: &axum::http::Method, path: &str, query: Option<&str>) -> bool {
    method == axum::http::Method::POST
        && path.starts_with("/dav/")
        && query.is_some_and(|query| {
            query
                .split('&')
                .any(|pair| pair == "action=attachment-add" || pair == "action=attachment-update")
        })
}

#[cfg(test)]
mod tests;
