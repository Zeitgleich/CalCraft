//! Import and export of whole collections, shared by the REST API and the
//! CLI.
//!
//! Conversion happens in `calcraft-ical`; this module adds storage. Every
//! imported object goes through [`check_object`], the same validation as a
//! DAV PUT, and through [`Store::put_item`], which enforces the per-user
//! quota and the per-collection object limit.
//!
//! Exports are streamed: the objects of a collection are loaded one at a
//! time and converted chunks are sent through a bounded channel, so memory
//! use does not grow with the size of the collection.

use std::sync::LazyLock;

use bytes::Bytes;
use calcraft_core::{
    CollectionKind,
    api::{ImportRejection, ImportReport},
};
use calcraft_ical::{Exporter, Format, IcalError, ImportContext, Target, transfer};
use calcraft_store::{Collection, NewItem, Precondition, PutOutcome, Store, StoreError};
use tokio::sync::{Semaphore, mpsc};

use crate::{
    auth::now,
    dav::{Rejection, Validated, check_object, etag},
    secrets::random_bytes,
};

pub use calcraft_ical::{MAX_IMPORT_OBJECTS, MAX_IMPORT_SIZE};

/// Rejection reasons included in a report.
pub const MAX_REASONS: usize = 20;
/// Size of the chunks an export is sent in.
const CHUNK_SIZE: usize = 64 * 1024;
/// Chunks buffered between the exporter and the client.
pub const EXPORT_BUFFER: usize = 4;
const MAX_REPORTED_UID: usize = 200;

/// Imports run one after another per process at most this many at a time:
/// parsing a 20 MiB file takes noticeable memory and CPU.
static IMPORTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(2));

/// What to do with an object whose UID already exists in the collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnConflict {
    #[default]
    Skip,
    Replace,
}

impl OnConflict {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "skip" => Some(Self::Skip),
            "replace" => Some(Self::Replace),
            _ => None,
        }
    }
}

/// Errors that stop an import or export as a whole.
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("file too large")]
    TooLarge,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("export aborted")]
    Aborted,
}

fn target(kind: CollectionKind) -> Target {
    match kind {
        CollectionKind::Calendar => Target::Calendar,
        CollectionKind::Addressbook => Target::Addressbook,
    }
}

/// Parses a format name; without one, the native format of the collection.
pub fn format_for(kind: CollectionKind, name: Option<&str>) -> Result<Format, TransferError> {
    let format = match name {
        None => Format::default_for(target(kind)),
        Some(name) => Format::from_name(name)
            .ok_or_else(|| TransferError::Invalid(format!("unknown format {name:?}")))?,
    };
    if !format.supports(target(kind)) {
        return Err(TransferError::Invalid(format!(
            "format {} is not available for {}s",
            format.name(),
            kind.as_str()
        )));
    }
    Ok(format)
}

/// Download file name, e.g. `work.ics`. Slugs are restricted to a safe
/// ASCII alphabet, so the name needs no quoting.
pub fn file_name(collection: &Collection, format: Format) -> String {
    format!("{}.{}", collection.slug, format.extension())
}

/// A random UUID (version 4) for objects without a UID.
fn new_uid() -> String {
    let mut b = random_bytes::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// Current time as an iCalendar UTC DATE-TIME.
fn dtstamp() -> String {
    let seconds = now();
    let days = seconds.div_euclid(86_400);
    let secs = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// An object ready for storage, or why it was rejected.
struct Prepared {
    uid: Option<String>,
    result: Result<(Validated, Vec<u8>), String>,
}

fn convert(
    collection: &Collection,
    data: &[u8],
    format: Format,
    max_vcard_bytes: usize,
) -> Result<Vec<Prepared>, TransferError> {
    let stamp = dtstamp();
    let mut uids = new_uid;
    let mut ctx = ImportContext {
        dtstamp: &stamp,
        new_uid: &mut uids,
    };
    let objects = transfer::import(data, format, target(collection.kind), &mut ctx).map_err(
        |err| match err {
            IcalError::TooLarge => TransferError::TooLarge,
            err => TransferError::Invalid(err.to_string()),
        },
    )?;
    Ok(objects
        .into_iter()
        .map(|object| {
            let result = object
                .data
                .map_err(|err| err.to_string())
                .and_then(
                    |bytes| match check_object(collection, &bytes, max_vcard_bytes) {
                        Ok(validated) => Ok((validated, bytes)),
                        Err(Rejection::Invalid(err)) => Err(err.to_string()),
                        Err(rejection) => Err(rejection.to_string()),
                    },
                );
            Prepared {
                uid: object.uid,
                result,
            }
        })
        .collect())
}

/// Replaces control and bidirectional formatting characters, so that
/// reported UIDs and reasons are safe to print on a terminal or in logs.
pub fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}

fn reject(report: &mut ImportReport, uid: Option<&str>, reason: &str) {
    report.rejected += 1;
    if report.errors.len() < MAX_REASONS {
        report.errors.push(ImportRejection {
            uid: uid.map(|u| printable(&u.chars().take(MAX_REPORTED_UID).collect::<String>())),
            reason: printable(reason),
        });
    }
}

/// Permission to run one import; at most two exist at a time. Callers
/// acquire it before reading the upload, so waiting imports hold no data.
pub struct ImportSlot(#[allow(dead_code)] tokio::sync::SemaphorePermit<'static>);

pub async fn import_slot() -> Result<ImportSlot, TransferError> {
    IMPORTS
        .acquire()
        .await
        .map(ImportSlot)
        .map_err(|_| TransferError::Invalid("server is shutting down".to_owned()))
}

/// Imports `data` into a collection the caller has already resolved for
/// its owner. UIDs are only ever matched within this collection. vCards
/// may be up to `max_vcard_bytes` (`storage.max_vcard_bytes`) large.
pub async fn import(
    store: &Store,
    collection: &Collection,
    format: Format,
    data: Bytes,
    on_conflict: OnConflict,
    max_vcard_bytes: usize,
    _slot: ImportSlot,
) -> Result<ImportReport, TransferError> {
    if data.len() > MAX_IMPORT_SIZE {
        return Err(TransferError::TooLarge);
    }
    let owned = collection.clone();
    let prepared =
        tokio::task::spawn_blocking(move || convert(&owned, &data, format, max_vcard_bytes))
            .await
            .map_err(|_| TransferError::Invalid("import failed".to_owned()))??;

    let extension = match collection.kind {
        CollectionKind::Calendar => "ics",
        CollectionKind::Addressbook => "vcf",
    };
    let mut report = ImportReport::default();
    let mut limit: Option<&'static str> = None;
    for object in prepared {
        let (validated, bytes) = match object.result {
            Ok(ok) => ok,
            Err(reason) => {
                reject(&mut report, object.uid.as_deref(), &reason);
                continue;
            }
        };
        let uid = validated.uid;
        if let Some(reason) = limit {
            reject(&mut report, Some(&uid), reason);
            continue;
        }
        let existing = store.item_name_by_uid(collection.id, &uid).await?;
        let (name, precondition) = match (existing, on_conflict) {
            (Some(_), OnConflict::Skip) => {
                report.skipped += 1;
                continue;
            }
            (Some(name), OnConflict::Replace) => (name, Precondition::MustExist),
            (None, _) => (
                format!(
                    "{}.{extension}",
                    &blake3::hash(uid.as_bytes()).to_hex()[..32]
                ),
                Precondition::MustNotExist,
            ),
        };
        let etag = etag(&bytes);
        let item = NewItem {
            uid: &uid,
            component: validated.component,
            range_start: validated.range_start,
            range_end: validated.range_end,
            etag: &etag,
            content: &bytes,
        };
        match store
            .put_item(collection.id, &name, &item, &precondition)
            .await
        {
            Ok(PutOutcome::Created) => report.created += 1,
            Ok(PutOutcome::Updated) => report.replaced += 1,
            // Changed concurrently, or the generated name is taken.
            Err(StoreError::PreconditionFailed | StoreError::UidConflict(_)) => {
                reject(&mut report, Some(&uid), "conflicts with an existing object");
            }
            Err(StoreError::Limit(reason)) => {
                // Quota or object limit: nothing further can be stored.
                reject(&mut report, Some(&uid), reason);
                limit = Some(reason);
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(report)
}

async fn send(
    tx: &mpsc::Sender<Result<Bytes, std::io::Error>>,
    chunk: String,
) -> Result<(), TransferError> {
    if chunk.is_empty() {
        return Ok(());
    }
    tx.send(Ok(Bytes::from(chunk)))
        .await
        .map_err(|_| TransferError::Aborted)
}

/// Streams a collection in `format` into `tx`. On failure after the start
/// an error is sent, so the receiver can abort the transfer instead of
/// delivering a truncated file as complete.
pub async fn export(
    store: &Store,
    collection: &Collection,
    format: Format,
    tx: mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> Result<(), TransferError> {
    let result = export_inner(store, collection, format, &tx).await;
    if let Err(err) = &result
        && !matches!(err, TransferError::Aborted)
    {
        let _ = tx.send(Err(std::io::Error::other(err.to_string()))).await;
    }
    result
}

async fn export_inner(
    store: &Store,
    collection: &Collection,
    format: Format,
    tx: &mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> Result<(), TransferError> {
    let mut exporter = Exporter::new(format, target(collection.kind))
        .map_err(|err| TransferError::Invalid(err.to_string()))?;
    let mut buffer = exporter
        .begin(collection.display_name.as_deref())
        .map_err(|err| TransferError::Invalid(err.to_string()))?;
    // Only names and sizes are listed; contents are loaded one by one.
    for meta in store.items(collection.id).await? {
        let Some(item) = store.item(collection.id, &meta.name).await? else {
            continue; // deleted meanwhile
        };
        match exporter.object(&item.content) {
            Ok(chunk) => buffer.push_str(&chunk),
            Err(err) => tracing::warn!(%err, name = %meta.name, "object skipped in export"),
        }
        if buffer.len() >= CHUNK_SIZE {
            send(tx, std::mem::take(&mut buffer)).await?;
        }
    }
    buffer.push_str(&exporter.end());
    send(tx, buffer).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_uuids_and_timestamps() {
        let uid = new_uid();
        assert_eq!(uid.len(), 36);
        assert_eq!(&uid[14..15], "4");
        assert_ne!(uid, new_uid());
        let stamp = dtstamp();
        assert!(calcraft_ical::parse_utc_datetime(&stamp).is_ok(), "{stamp}");
    }

    #[test]
    fn reports_are_printable() {
        assert_eq!(
            printable("a\u{1b}[31mb\r\n\u{202e}c"),
            "a\u{fffd}[31mb\u{fffd}\u{fffd}\u{fffd}c"
        );
        let mut report = ImportReport::default();
        reject(&mut report, Some("x\u{7}y"), "bad\u{1b}");
        assert_eq!(report.errors[0].uid.as_deref(), Some("x\u{fffd}y"));
        assert_eq!(report.errors[0].reason, "bad\u{fffd}");
    }

    #[test]
    fn formats_fit_the_collection() {
        assert_eq!(
            format_for(CollectionKind::Calendar, None).unwrap(),
            Format::ICalendar
        );
        assert_eq!(
            format_for(CollectionKind::Addressbook, Some("csv")).unwrap(),
            Format::Csv
        );
        assert!(format_for(CollectionKind::Addressbook, Some("ics")).is_err());
        assert!(format_for(CollectionKind::Calendar, Some("pdf")).is_err());
    }
}
