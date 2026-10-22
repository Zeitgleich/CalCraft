//! `calcraft migrate`: copies calendars, task lists and address books from
//! another CalDAV/CardDAV server (Synology, Radicale, Baïkal, Nextcloud, …)
//! into a local user.
//!
//! Discovery follows RFC 6764 (`.well-known`), RFC 5397
//! (`current-user-principal`) and the home sets of RFC 4791 and RFC 6352.
//! Objects are listed with PROPFIND and fetched with multiget in batches,
//! or one by one with GET if the server has no multiget. Every object goes
//! through the validation of `calcraft import` ([`crate::transfer`]) and
//! [`Store::put_item`] (quota, object limit).
//!
//! Network policy: this is an administrator's command, run by hand against
//! a server the administrator names, usually a NAS on the local network.
//! Private and loopback addresses are therefore allowed, unlike for
//! server-side fetches (webcal subscriptions), where users choose the URL
//! and the server must not be turned into a proxy into its own network.
//! Still, requests only go to the origin of `--from`: redirects to other
//! origins are refused (except http to https on the same host), hrefs of
//! other origins are ignored, and the password is only sent there, as HTTP
//! Basic over TLS (plain HTTP only with `--allow-http`).

mod remote;

use std::{
    collections::HashSet,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, bail};
use axum::http::Uri;
use calcraft_core::{CollectionKind, Slug, Username, api::ImportReport};
use calcraft_dav::xml::{NS_APPLE_ICAL, NS_CALDAV, NS_CALENDARSERVER, NS_CARDDAV, NS_DAV};
use calcraft_ical::{ComponentKind, MAX_OBJECT_SIZE};
use calcraft_store::{Collection, MAX_ITEMS_PER_COLLECTION, Store, StoreError};

use crate::{
    config::Secret,
    https::{HttpClient, Limits},
    transfer::{OnConflict, Writer, prepare_fetched, printable},
    validate::{MAX_DESCRIPTION, MAX_DISPLAY_NAME, clean_color, clean_text},
};
use remote::{Entry, Remote, RemoteError, path_key, percent_decode};

/// Default cap for one response (listings and multiget batches).
pub const DEFAULT_MAX_RESPONSE: usize = 32 * 1024 * 1024;
/// Largest cap `--max-response-mib` accepts.
pub const MAX_RESPONSE_LIMIT: usize = 256 * 1024 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_BATCH: usize = 50;
pub const MAX_BATCH: usize = 500;
/// Largest accepted password file.
const MAX_PASSWORD_FILE: u64 = 4096;
const NS_OWNCLOUD: &str = "http://owncloud.org/ns";

const PRINCIPAL_PROPS: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
    "<d:propfind xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\" ",
    "xmlns:card=\"urn:ietf:params:xml:ns:carddav\"><d:prop>",
    "<d:current-user-principal/><c:calendar-home-set/><card:addressbook-home-set/>",
    "</d:prop></d:propfind>"
);

const COLLECTION_PROPS: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
    "<d:propfind xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\" ",
    "xmlns:card=\"urn:ietf:params:xml:ns:carddav\" xmlns:ic=\"http://apple.com/ns/ical/\" ",
    "xmlns:oc=\"http://owncloud.org/ns\"><d:prop>",
    "<d:resourcetype/><d:displayname/><d:owner/><oc:owner-principal/>",
    "<ic:calendar-color/><c:supported-calendar-component-set/><c:calendar-description/>",
    "<c:calendar-timezone/><card:addressbook-description/>",
    "</d:prop></d:propfind>"
);

const ITEM_PROPS: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
    "<d:propfind xmlns:d=\"DAV:\"><d:prop>",
    "<d:resourcetype/><d:getetag/>",
    "</d:prop></d:propfind>"
);

/// What to migrate from where.
#[derive(Debug, Clone)]
pub struct Options {
    /// CalDAV URL (or just the server) of the source.
    pub from: String,
    /// CardDAV URL, if the server has a separate one (Synology).
    pub carddav_from: Option<String>,
    pub source_user: String,
    pub password: Secret,
    /// Local user that receives the collections.
    pub user: String,
    pub dry_run: bool,
    pub only: Option<CollectionKind>,
    /// `source=target` slug overrides.
    pub map: Vec<String>,
    pub on_conflict: OnConflict,
    /// Also copy collections other users shared with the source account.
    pub include_shared: bool,
    pub allow_http: bool,
    /// Additional trusted root certificates (self-signed NAS certificates).
    pub ca_file: Option<PathBuf>,
    pub max_response: usize,
    pub timeout: Duration,
    /// Pause before every request.
    pub delay: Duration,
    pub batch_size: usize,
    /// `storage.max_vcard_bytes`.
    pub max_vcard_bytes: usize,
}

impl Options {
    pub fn new(from: &str, source_user: &str, password: Secret, user: &str) -> Self {
        Self {
            from: from.to_owned(),
            carddav_from: None,
            source_user: source_user.to_owned(),
            password,
            user: user.to_owned(),
            dry_run: false,
            only: None,
            map: Vec::new(),
            on_conflict: OnConflict::Skip,
            include_shared: false,
            allow_http: false,
            ca_file: None,
            max_response: DEFAULT_MAX_RESPONSE,
            timeout: DEFAULT_TIMEOUT,
            delay: Duration::ZERO,
            batch_size: DEFAULT_BATCH,
            max_vcard_bytes: 1024 * 1024,
        }
    }
}

/// Outcome for one source collection.
#[derive(Debug)]
pub struct CollectionReport {
    pub kind: CollectionKind,
    /// Path of the collection on the source server (printable).
    pub source: String,
    pub target: Slug,
    pub display_name: Option<String>,
    /// The target collection did not exist before.
    pub new: bool,
    pub report: ImportReport,
    /// Why the collection could not be copied completely.
    pub error: Option<String>,
}

/// Outcome of a migration.
#[derive(Debug, Default)]
pub struct Summary {
    pub dry_run: bool,
    pub warnings: Vec<String>,
    /// Remote collections left out, with the reason.
    pub skipped: Vec<String>,
    pub collections: Vec<CollectionReport>,
}

impl Summary {
    /// Whether a collection could not be copied completely.
    pub fn failed(&self) -> bool {
        self.collections.iter().any(|c| c.error.is_some())
    }

    /// Human-readable report: counts and reasons, never object contents.
    pub fn write(&self, out: &mut dyn Write) -> std::io::Result<()> {
        for warning in &self.warnings {
            writeln!(out, "warning: {warning}")?;
        }
        for skipped in &self.skipped {
            writeln!(out, "skipped {skipped}")?;
        }
        for c in &self.collections {
            let name = c
                .display_name
                .as_deref()
                .map(|n| format!(" \"{}\"", printable(n)))
                .unwrap_or_default();
            let state = match (c.new, self.dry_run) {
                (true, true) => "would be created",
                (true, false) => "created",
                (false, _) => "existing",
            };
            let r = &c.report;
            writeln!(
                out,
                "{}{name} {} -> {}/{} ({state}): created {}, replaced {}, skipped {}, rejected {}",
                c.kind.as_str(),
                c.source,
                c.kind.as_str(),
                c.target,
                r.created,
                r.replaced,
                r.skipped,
                r.rejected
            )?;
            for error in &r.errors {
                match &error.uid {
                    Some(uid) => writeln!(out, "  rejected {uid}: {}", error.reason)?,
                    None => writeln!(out, "  rejected {}", error.reason)?,
                }
            }
            if r.rejected > r.errors.len() as u64 {
                writeln!(out, "  … and {} more", r.rejected - r.errors.len() as u64)?;
            }
            if let Some(error) = &c.error {
                writeln!(out, "  error: {error}")?;
            }
        }
        if self.collections.is_empty() {
            writeln!(out, "no calendars or address books found")?;
        }
        if self.dry_run {
            writeln!(out, "dry run: nothing was written")?;
        }
        Ok(())
    }
}

/// Reads a password from the first line of a file that only its owner can
/// read (on Unix: no group or other permission bits).
pub fn read_password_file(path: &Path) -> anyhow::Result<Secret> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to open password file {}", path.display()))?;
    let metadata = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!(
                "password file {} is accessible by group or others (run chmod 600)",
                path.display()
            );
        }
    }
    if !metadata.is_file() || metadata.len() > MAX_PASSWORD_FILE {
        bail!(
            "password file {} must be a regular file of at most {MAX_PASSWORD_FILE} bytes",
            path.display()
        );
    }
    let mut text = String::new();
    file.take(MAX_PASSWORD_FILE)
        .read_to_string(&mut text)
        .context("password file is not UTF-8")?;
    let password = text.lines().next().unwrap_or_default();
    if password.is_empty() {
        bail!("password file {} is empty", path.display());
    }
    Ok(Secret::new(password))
}

/// A calendar or address book found on the source server.
#[derive(Debug)]
struct SourceCollection {
    uri: Uri,
    kind: CollectionKind,
    display_name: Option<String>,
    description: Option<String>,
    color: Option<String>,
    /// `VEVENT,VTODO` style, only components CalCraft supports.
    components: String,
}

impl SourceCollection {
    fn path(&self) -> String {
        printable(self.uri.path())
    }

    /// Last path segment, decoded.
    fn name(&self) -> String {
        percent_decode(self.uri.path())
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned()
    }
}

fn parse_url(text: &str, allow_http: bool) -> anyhow::Result<Uri> {
    let uri: Uri = text
        .trim()
        .parse()
        .with_context(|| format!("invalid URL {:?}", printable(text)))?;
    let origin = remote::Origin::of(&uri)?;
    if !origin.is_https() && !allow_http {
        bail!(
            "{} uses plain http: the password would be sent unencrypted. Use https \
             (with --ca-file for a self-signed certificate) or pass --allow-http",
            printable(text)
        );
    }
    Ok(uri)
}

/// Derives a target slug from the last path segment of the source.
fn derive_slug(name: &str, kind: CollectionKind) -> Slug {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
            c
        } else {
            '-'
        };
        if !(c == '-' && slug.ends_with('-')) {
            slug.push(c);
        }
        if slug.len() >= Slug::MAX_LEN {
            break;
        }
    }
    let slug = slug
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .trim_end_matches('-');
    slug.parse().unwrap_or_else(|_| fallback_slug(kind))
}

fn fallback_slug(kind: CollectionKind) -> Slug {
    // `calendar` and `addressbook` are valid slugs.
    #[allow(clippy::expect_used)]
    kind.as_str().parse().expect("constant slug is valid")
}

/// `base`, or `base-2`, `base-3`, … if already taken in this run.
fn unique_slug(
    base: Slug,
    kind: CollectionKind,
    used: &mut HashSet<(CollectionKind, Slug)>,
) -> Slug {
    if used.insert((kind, base.clone())) {
        return base;
    }
    for n in 2.. {
        let suffix = format!("-{n}");
        let stem: String = base
            .as_str()
            .chars()
            .take(Slug::MAX_LEN - suffix.len())
            .collect();
        if let Ok(slug) = format!("{stem}{suffix}").parse::<Slug>()
            && used.insert((kind, slug.clone()))
        {
            return slug;
        }
    }
    base
}

/// Parses `--map source=target` entries.
fn parse_map(entries: &[String]) -> anyhow::Result<Vec<(String, Slug)>> {
    entries
        .iter()
        .map(|entry| {
            let (from, to) = entry
                .split_once('=')
                .with_context(|| format!("--map {entry:?}: expected source-slug=target-slug"))?;
            let to: Slug = to
                .trim()
                .parse()
                .with_context(|| format!("--map {entry:?}: invalid target"))?;
            Ok((from.trim().to_owned(), to))
        })
        .collect()
}

/// Copies the collections of `options.source_user` into `options.user`.
pub async fn run(store: &Store, options: &Options) -> anyhow::Result<Summary> {
    if options.batch_size == 0 || options.batch_size > MAX_BATCH {
        bail!("the batch size must be between 1 and {MAX_BATCH}");
    }
    if options.max_response == 0 || options.max_response > MAX_RESPONSE_LIMIT {
        bail!("the response limit must be between 1 byte and {MAX_RESPONSE_LIMIT} bytes");
    }
    let username: Username = options.user.parse()?;
    let Some(account) = store.user_credentials(&username).await? else {
        bail!("user {username} does not exist");
    };
    let owner_id = account.user.id;
    let map = parse_map(&options.map)?;
    let from = parse_url(&options.from, options.allow_http)?;
    let carddav_from = options
        .carddav_from
        .as_deref()
        .map(|url| parse_url(url, options.allow_http))
        .transpose()?;
    if let Some(url) = &carddav_from
        && remote::Origin::of(url)? != remote::Origin::of(&from)?
    {
        bail!("--carddav-from must be on the same server (scheme, host and port) as --from");
    }

    let mut summary = Summary {
        dry_run: options.dry_run,
        ..Summary::default()
    };
    if !remote::Origin::of(&from)?.is_https() {
        let warning = "--allow-http: the password and all data travel unencrypted".to_owned();
        tracing::warn!("{warning}");
        summary.warnings.push(warning);
    }
    let http = HttpClient::new(options.ca_file.as_deref(), options.allow_http)?;
    let limits = Limits {
        timeout: options.timeout,
        max_body: options.max_response,
    };
    let mut source = Remote::new(
        http,
        &from,
        &options.source_user,
        &options.password,
        limits,
        options.delay,
    )?;

    let (principal, homes) = discover(&mut source, &from, carddav_from.as_ref()).await?;
    let collections = list_collections(
        &mut source,
        &homes,
        principal.as_deref(),
        options,
        &mut summary,
    )
    .await?;

    let mut used = HashSet::new();
    let mut used_map = HashSet::new();
    for collection in collections {
        let name = collection.name();
        let mapped = map.iter().find(|(from, _)| {
            *from == name || *from == derive_slug(&name, collection.kind).as_str()
        });
        let base = match mapped {
            Some((from, to)) => {
                used_map.insert(from.clone());
                to.clone()
            }
            None => derive_slug(&name, collection.kind),
        };
        let target = unique_slug(base, collection.kind, &mut used);
        let report =
            copy_collection(store, &mut source, owner_id, &collection, target, options).await?;
        summary.collections.push(report);
    }
    for (from, _) in &map {
        if !used_map.contains(from) {
            summary.warnings.push(format!(
                "--map {}: no such source collection",
                printable(from)
            ));
        }
    }
    if source.upgraded {
        summary.warnings.push(format!(
            "the source server redirected to {}; use that URL next time",
            source.origin().base()
        ));
    }
    Ok(summary)
}

/// Finds the principal and the calendar and address book homes.
async fn discover(
    source: &mut Remote,
    from: &Uri,
    carddav_from: Option<&Uri>,
) -> Result<(Option<String>, Vec<Uri>), RemoteError> {
    let base = source.origin().base();
    let mut given = vec![from.clone()];
    given.extend(carddav_from.cloned());
    let mut starts = given.clone();
    for well_known in ["/.well-known/caldav", "/.well-known/carddav"] {
        if let Ok(uri) = format!("{base}{well_known}").parse() {
            starts.push(uri);
        }
    }
    let mut principals: Vec<Uri> = Vec::new();
    let mut homes: Vec<Uri> = Vec::new();
    for start in &starts {
        match source.propfind(start, "0", PRINCIPAL_PROPS).await {
            Ok((at, entries)) => {
                for entry in &entries {
                    add_homes(source, &at, entry, &mut homes);
                    if let Some(href) = entry.href_prop(NS_DAV, "current-user-principal")
                        && let Some(uri) = source.resolve(&at, href)
                        && !principals.iter().any(|p| path_key(p) == path_key(&uri))
                    {
                        principals.push(uri);
                    }
                }
            }
            Err(RemoteError::Unauthorized) => return Err(RemoteError::Unauthorized),
            Err(err) => {
                tracing::debug!(%err, path = %printable(start.path()), "discovery step failed");
            }
        }
    }
    for principal in &principals {
        match source.propfind(principal, "0", PRINCIPAL_PROPS).await {
            Ok((at, entries)) => {
                for entry in &entries {
                    add_homes(source, &at, entry, &mut homes);
                }
            }
            Err(RemoteError::Unauthorized) => return Err(RemoteError::Unauthorized),
            Err(err) => {
                tracing::debug!(%err, "principal lookup failed");
            }
        }
    }
    if homes.is_empty() {
        // No discovery support: the given URLs may be the homes themselves.
        homes = given;
    }
    let principal = principals.first().map(path_key);
    Ok((principal, homes))
}

fn add_homes(source: &Remote, at: &Uri, entry: &Entry, homes: &mut Vec<Uri>) {
    for (ns, name) in [
        (NS_CALDAV, "calendar-home-set"),
        (NS_CARDDAV, "addressbook-home-set"),
    ] {
        let Some(prop) = entry.prop(ns, name) else {
            continue;
        };
        for href in prop.children_named(NS_DAV, "href") {
            if let Some(uri) = source.resolve(at, &href.text)
                && !homes.iter().any(|h| path_key(h) == path_key(&uri))
            {
                homes.push(uri);
            }
        }
    }
}

/// Whether an owner reference (href or ownCloud principal path) names
/// someone other than `principal`.
fn foreign_owner(owner: &str, principal: &str) -> bool {
    let owner = owner.trim();
    let owner = owner
        .strip_prefix("http://")
        .or_else(|| owner.strip_prefix("https://"))
        .map_or(owner, |rest| rest.find('/').map_or("", |i| &rest[i..]));
    let owner = percent_decode(owner);
    let owner = owner.trim_matches('/');
    let principal = principal.trim_matches('/');
    !owner.is_empty() && !principal.is_empty() && !principal.ends_with(owner)
}

async fn list_collections(
    source: &mut Remote,
    homes: &[Uri],
    principal: Option<&str>,
    options: &Options,
    summary: &mut Summary,
) -> anyhow::Result<Vec<SourceCollection>> {
    let mut found: Vec<SourceCollection> = Vec::new();
    let mut listed = 0;
    let mut last_error = None;
    for home in homes {
        let (at, entries) = match source.propfind(home, "1", COLLECTION_PROPS).await {
            Ok(result) => result,
            Err(RemoteError::Unauthorized) => return Err(RemoteError::Unauthorized.into()),
            Err(err) => {
                last_error = Some(err);
                continue;
            }
        };
        listed += 1;
        let home_key = path_key(&at);
        for entry in &entries {
            let Some(uri) = source.resolve(&at, &entry.href) else {
                continue;
            };
            let key = path_key(&uri);
            if key == home_key || found.iter().any(|c| path_key(&c.uri) == key) {
                continue;
            }
            let Some(types) = entry.prop(NS_DAV, "resourcetype") else {
                continue;
            };
            let path = printable(uri.path());
            if types.child(NS_CALDAV, "schedule-inbox").is_some()
                || types.child(NS_CALDAV, "schedule-outbox").is_some()
            {
                summary
                    .skipped
                    .push(format!("{path}: scheduling inbox or outbox"));
                continue;
            }
            if types.child(NS_CALENDARSERVER, "notification").is_some() {
                summary.skipped.push(format!("{path}: notifications"));
                continue;
            }
            if types.child(NS_CALENDARSERVER, "subscribed").is_some() {
                summary.skipped.push(format!(
                    "{path}: subscription (re-subscribe to the feed instead)"
                ));
                continue;
            }
            let kind = if types.child(NS_CALDAV, "calendar").is_some() {
                CollectionKind::Calendar
            } else if types.child(NS_CARDDAV, "addressbook").is_some() {
                CollectionKind::Addressbook
            } else {
                continue;
            };
            let owner = entry
                .href_prop(NS_DAV, "owner")
                .or_else(|| entry.text(NS_OWNCLOUD, "owner-principal"));
            let shared = types.child(NS_CALENDARSERVER, "shared").is_some()
                || matches!((owner, principal), (Some(o), Some(p)) if foreign_owner(o, p));
            if shared && !options.include_shared {
                summary.skipped.push(format!(
                    "{path}: shared by another user (use --include-shared to copy it)"
                ));
                continue;
            }
            if options.only.is_some_and(|only| only != kind) {
                summary.skipped.push(format!("{path}: excluded by --only"));
                continue;
            }
            found.push(collection_from(uri, kind, entry));
        }
    }
    if listed == 0
        && let Some(err) = last_error
    {
        return Err(anyhow::Error::from(err).context("failed to list the collections"));
    }
    found.sort_by_key(|c| (c.kind != CollectionKind::Calendar, c.uri.path().to_owned()));
    Ok(found)
}

fn collection_from(uri: Uri, kind: CollectionKind, entry: &Entry) -> SourceCollection {
    let description = match kind {
        CollectionKind::Calendar => entry.text(NS_CALDAV, "calendar-description"),
        CollectionKind::Addressbook => entry.text(NS_CARDDAV, "addressbook-description"),
    };
    let color = entry
        .text(NS_APPLE_ICAL, "calendar-color")
        .and_then(|c| clean_color(c).or_else(|| clean_color(&format!("#{c}"))));
    let components = match kind {
        CollectionKind::Calendar => {
            let names: Vec<&str> = entry
                .prop(NS_CALDAV, "supported-calendar-component-set")
                .map(|set| {
                    set.children_named(NS_CALDAV, "comp")
                        .filter_map(|c| c.attribute("name"))
                        .filter_map(|n| ComponentKind::from_name(&n.to_ascii_uppercase()))
                        .map(ComponentKind::as_str)
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() {
                "VEVENT,VTODO".to_owned()
            } else {
                names.join(",")
            }
        }
        CollectionKind::Addressbook => "VCARD".to_owned(),
    };
    SourceCollection {
        uri,
        kind,
        display_name: entry
            .text(NS_DAV, "displayname")
            .and_then(|t| clean_text(t, MAX_DISPLAY_NAME)),
        description: description.and_then(|t| clean_text(t, MAX_DESCRIPTION)),
        color,
        components,
    }
}

/// Copies one collection; source errors end up in the report, store
/// failures end the migration.
async fn copy_collection(
    store: &Store,
    source: &mut Remote,
    owner_id: i64,
    from: &SourceCollection,
    target: Slug,
    options: &Options,
) -> anyhow::Result<CollectionReport> {
    let mut report = CollectionReport {
        kind: from.kind,
        source: from.path(),
        target: target.clone(),
        display_name: from.display_name.clone(),
        new: false,
        report: ImportReport::default(),
        error: None,
    };
    let collection = match store.collection(owner_id, from.kind, &target).await? {
        Some(existing) => existing,
        None if options.dry_run => {
            report.new = true;
            Collection {
                id: -1,
                owner_id,
                kind: from.kind,
                slug: target.clone(),
                display_name: from.display_name.clone(),
                description: from.description.clone(),
                color: from.color.clone(),
                sync_revision: 0,
                components: from.components.clone(),
                sync_floor: 0,
                token_salt: 0,
            }
        }
        None => {
            let created = store
                .create_collection(
                    owner_id,
                    from.kind,
                    &target,
                    from.display_name.as_deref(),
                    from.description.as_deref(),
                    from.color.as_deref(),
                    &from.components,
                )
                .await;
            match created {
                Ok(collection) => {
                    report.new = true;
                    collection
                }
                Err(StoreError::Limit(reason)) => {
                    report.error = Some(reason.to_owned());
                    return Ok(report);
                }
                Err(err) => return Err(err.into()),
            }
        }
    };
    tracing::info!(source = %report.source, target = %target, "copying collection");
    let mut writer = Writer::new(store, &collection, options.on_conflict, options.dry_run);
    if let Err(err) = copy_objects(source, from, &collection, options, &mut writer).await {
        match err {
            CopyError::Remote(RemoteError::Unauthorized) => {
                return Err(RemoteError::Unauthorized.into());
            }
            CopyError::Remote(err) => report.error = Some(err.to_string()),
            CopyError::Store(err) => return Err(err),
        }
    }
    report.report = writer.report;
    Ok(report)
}

enum CopyError {
    Remote(RemoteError),
    Store(anyhow::Error),
}

impl From<RemoteError> for CopyError {
    fn from(err: RemoteError) -> Self {
        Self::Remote(err)
    }
}

async fn copy_objects(
    source: &mut Remote,
    from: &SourceCollection,
    collection: &Collection,
    options: &Options,
    writer: &mut Writer<'_>,
) -> Result<(), CopyError> {
    let (at, entries) = source.propfind(&from.uri, "1", ITEM_PROPS).await?;
    let base = path_key(&at);
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for entry in &entries {
        let Some(uri) = source.resolve(&at, &entry.href) else {
            continue;
        };
        let key = path_key(&uri);
        let is_collection = entry
            .prop(NS_DAV, "resourcetype")
            .is_some_and(|t| t.child(NS_DAV, "collection").is_some());
        // Only direct members of this collection.
        let member = key
            .strip_prefix(&base)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
        if is_collection || !member || !seen.insert(key) {
            continue;
        }
        paths.push(uri.path().to_owned());
    }
    if paths.len() > usize::try_from(MAX_ITEMS_PER_COLLECTION).unwrap_or(usize::MAX) {
        return Err(RemoteError::Invalid(format!(
            "{} objects, more than the {MAX_ITEMS_PER_COLLECTION} a collection may hold",
            paths.len()
        ))
        .into());
    }
    let max_object = match from.kind {
        CollectionKind::Calendar => MAX_OBJECT_SIZE,
        CollectionKind::Addressbook => options.max_vcard_bytes,
    };
    let mut multiget = true;
    for chunk in paths.chunks(options.batch_size) {
        let fetched = fetch(source, from, &at, chunk, &mut multiget, max_object).await?;
        let owned = collection.clone();
        let max_vcard_bytes = options.max_vcard_bytes;
        let prepared = tokio::task::spawn_blocking(move || {
            fetched
                .into_iter()
                .map(|(name, data)| {
                    (
                        data.map(|data| prepare_fetched(&owned, &name, data, max_vcard_bytes)),
                        name,
                    )
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|_| CopyError::Store(anyhow::anyhow!("validation task failed")))?;
        for (result, name) in prepared {
            match result {
                Ok(objects) => {
                    for object in objects {
                        writer
                            .store(object)
                            .await
                            .map_err(|err| CopyError::Store(err.into()))?;
                    }
                }
                Err(reason) => writer.reject(None, &format!("{}: {reason}", printable(&name))),
            }
        }
    }
    Ok(())
}

/// Last path segment, decoded, to name an object in reports.
fn object_name(path: &str) -> String {
    percent_decode(path)
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Calendar or address data from a multiget. XML parsing turned CRLF into
/// LF (XML 1.0 §2.11); iCalendar and vCard use CRLF, so it is restored.
/// Surrounding whitespace from pretty-printed responses is dropped.
fn restore_line_endings(text: &str) -> Vec<u8> {
    let text = text.trim();
    let mut data = if text.contains('\r') {
        text.to_owned()
    } else {
        text.replace('\n', "\r\n")
    };
    data.push_str("\r\n");
    data.into_bytes()
}

type Fetched = Vec<(String, Result<Vec<u8>, String>)>;

/// Fetches one batch: multiget while the server supports it, GET for the
/// rest.
async fn fetch(
    source: &mut Remote,
    from: &SourceCollection,
    at: &Uri,
    paths: &[String],
    multiget: &mut bool,
    max_object: usize,
) -> Result<Fetched, RemoteError> {
    let mut fetched = Vec::with_capacity(paths.len());
    let mut pending: Vec<&String> = paths.iter().collect();
    if *multiget {
        match source.multiget(&from.uri, from.kind, paths).await? {
            Some((report_at, entries)) => {
                let data_prop = match from.kind {
                    CollectionKind::Calendar => (NS_CALDAV, "calendar-data"),
                    CollectionKind::Addressbook => (NS_CARDDAV, "address-data"),
                };
                for entry in &entries {
                    let Some(uri) = source.resolve(&report_at, &entry.href) else {
                        continue;
                    };
                    let key = path_key(&uri);
                    let Some(index) = pending
                        .iter()
                        .position(|p| percent_decode(p).trim_end_matches('/') == key)
                    else {
                        continue;
                    };
                    let name = object_name(pending[index]);
                    if let Some(data) = entry.prop(data_prop.0, data_prop.1)
                        && !data.text.trim().is_empty()
                    {
                        pending.swap_remove(index);
                        fetched.push((name, Ok(restore_line_endings(&data.text))));
                    } else if matches!(entry.status, Some(404 | 410)) {
                        pending.swap_remove(index);
                        fetched.push((name, Err("no longer exists on the source".to_owned())));
                    }
                }
            }
            None => {
                tracing::info!(
                    collection = %from.path(),
                    "the source server has no multiget, fetching objects one by one"
                );
                *multiget = false;
            }
        }
    }
    for path in pending {
        let name = object_name(path);
        let Some(uri) = source.resolve(at, path) else {
            fetched.push((name, Err("invalid href".to_owned())));
            continue;
        };
        match source.get(&uri, max_object).await {
            Ok(Some(body)) => fetched.push((name, Ok(body.to_vec()))),
            Ok(None) => fetched.push((name, Err("no longer exists on the source".to_owned()))),
            Err(RemoteError::Http(crate::https::HttpError::Body)) => {
                fetched.push((name, Err("object too large".to_owned())));
            }
            Err(RemoteError::Status { status, .. }) => {
                fetched.push((name, Err(format!("source answered {status}"))));
            }
            Err(err) => return Err(err),
        }
    }
    Ok(fetched)
}

#[cfg(test)]
mod tests;
