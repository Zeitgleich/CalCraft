//! Managed attachments (RFC 8607): settings and helpers shared by the DAV
//! handlers (`dav/attachments.rs`) and the request body limit. See
//! docs/attachments.md for the design and the access model.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use calcraft_ical::{Property, attach};
use calcraft_store::AttachmentMeta;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::AttachmentsConfig;

/// Uploads held in memory at the same time (server-wide); beyond that
/// uploads get `503` at once instead of waiting.
pub const MAX_CONCURRENT_UPLOADS: usize = 16;
/// Uploads of one user at the same time: a user sending slow bodies blocks
/// at most this many of the [`MAX_CONCURRENT_UPLOADS`] slots.
pub const UPLOADS_PER_USER: usize = 2;
/// Downloads (each holds the file in memory until it is sent) at the same
/// time, server-wide and per user.
pub const MAX_CONCURRENT_DOWNLOADS: usize = 16;
pub const DOWNLOADS_PER_USER: usize = 4;
/// Path below which attachments are served.
pub const ATTACHMENTS_PATH: &str = "/dav/attachments/";

/// A bounded number of concurrent operations, server-wide and per user.
/// Taking a slot never waits: callers answer `503` when none is free.
#[derive(Debug)]
pub struct Slots {
    global: Arc<Semaphore>,
    per_user: Arc<Mutex<HashMap<i64, usize>>>,
    per_user_max: usize,
}

/// A taken slot; released when dropped.
#[derive(Debug)]
pub struct Slot {
    _permit: OwnedSemaphorePermit,
    per_user: Arc<Mutex<HashMap<i64, usize>>>,
    user: i64,
}

impl Slots {
    pub fn new(global: usize, per_user: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global)),
            per_user: Arc::default(),
            per_user_max: per_user,
        }
    }

    /// A slot for `user`, or `None` if the user or the server is at its
    /// limit.
    pub fn try_take(&self, user: i64) -> Option<Slot> {
        let mut counts = self.per_user.lock().unwrap_or_else(PoisonError::into_inner);
        let count = counts.entry(user).or_insert(0);
        if *count >= self.per_user_max {
            return None;
        }
        let permit = Arc::clone(&self.global).try_acquire_owned().ok()?;
        *count += 1;
        Some(Slot {
            _permit: permit,
            per_user: Arc::clone(&self.per_user),
            user,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut counts = self.per_user.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = counts.get_mut(&self.user) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.user);
            }
        }
    }
}

/// Managed attachments, if enabled (`[attachments] enabled`).
#[derive(Debug)]
pub struct Attachments {
    pub config: AttachmentsConfig,
    /// Bounds the memory held by uploads in flight (at most
    /// [`MAX_CONCURRENT_UPLOADS`] × `max_bytes`).
    pub uploads: Slots,
    /// Bounds the memory held by downloads until they are sent.
    pub downloads: Slots,
}

impl Attachments {
    pub fn new(config: &AttachmentsConfig) -> Self {
        Self {
            config: config.clone(),
            uploads: Slots::new(MAX_CONCURRENT_UPLOADS, UPLOADS_PER_USER),
            downloads: Slots::new(MAX_CONCURRENT_DOWNLOADS, DOWNLOADS_PER_USER),
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
