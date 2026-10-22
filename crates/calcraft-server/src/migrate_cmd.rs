//! `calcraft migrate`.

use std::{path::PathBuf, time::Duration};

use anyhow::bail;
use calcraft_core::CollectionKind;
use calcraft_server::{
    config::Secret,
    migrate::{self, DEFAULT_BATCH, Options},
    transfer::OnConflict,
};
use calcraft_store::Store;
use clap::Args;

/// Environment variable that may hold the source password (for scripts;
/// a password file or the prompt is preferable).
const PASSWORD_ENV: &str = "CALCRAFT_MIGRATE_PASSWORD";

#[derive(Debug, Args)]
pub struct MigrateArgs {
    /// URL of the source server, e.g. https://nas.example.com:5001/caldav/
    /// (Synology), https://cloud.example.com/remote.php/dav/ (Nextcloud) or
    /// just https://dav.example.com/.
    #[arg(long, value_name = "URL")]
    from: String,
    /// CardDAV URL, if the server serves address books elsewhere (Synology
    /// Contacts: https://nas.example.com:5001/carddav/). Same server only.
    #[arg(long, value_name = "URL")]
    carddav_from: Option<String>,
    /// User name at the source server. The password is asked for, or read
    /// from --password-file or the environment variable
    /// CALCRAFT_MIGRATE_PASSWORD; never pass it on the command line.
    #[arg(long)]
    source_user: String,
    /// File whose first line is the source password (mode 600).
    #[arg(long, value_name = "FILE")]
    password_file: Option<PathBuf>,
    /// Local CalCraft user that receives the collections.
    #[arg(long)]
    user: String,
    /// Discover, fetch and validate everything, but write nothing.
    #[arg(long)]
    dry_run: bool,
    /// Only calendars (including task lists) or only address books.
    #[arg(long, value_parser = ["calendars", "addressbooks"])]
    only: Option<String>,
    /// Target slug for a source collection (last path segment), e.g.
    /// --map home=personal. Repeatable.
    #[arg(long, value_name = "SOURCE=TARGET")]
    map: Vec<String>,
    /// What to do with objects whose UID already exists: skip or replace.
    #[arg(long, default_value = "skip", value_parser = ["skip", "replace"])]
    on_conflict: String,
    /// Also copy collections other users shared with the source account.
    #[arg(long)]
    include_shared: bool,
    /// Allow plain http (password and data unencrypted). Only for a source
    /// on the same machine or a trusted network.
    #[arg(long)]
    allow_http: bool,
    /// PEM file with additional trusted CA certificates, e.g. the
    /// self-signed certificate of a NAS.
    #[arg(long, value_name = "PEM")]
    ca_file: Option<PathBuf>,
    /// Largest accepted response (listings, multiget batches) in MiB.
    #[arg(long, default_value_t = 32, value_parser = clap::value_parser!(u16).range(1..=256))]
    max_response_mib: u16,
    /// Deadline per request in seconds.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
    timeout: u64,
    /// Pause before every request in milliseconds, to spare a slow source.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=60_000))]
    delay_ms: u64,
    /// Objects per multiget request.
    #[arg(long, default_value_t = DEFAULT_BATCH as u16, value_parser = clap::value_parser!(u16).range(1..=500))]
    batch_size: u16,
}

fn password(args: &MigrateArgs) -> anyhow::Result<Secret> {
    if let Some(path) = &args.password_file {
        return migrate::read_password_file(path);
    }
    if let Ok(password) = std::env::var(PASSWORD_ENV)
        && !password.is_empty()
    {
        return Ok(Secret::new(password));
    }
    let password = rpassword::prompt_password(format!(
        "Password of {} at the source server: ",
        calcraft_server::transfer::printable(&args.source_user)
    ))?;
    if password.is_empty() {
        bail!("no password given");
    }
    Ok(Secret::new(password))
}

pub async fn run(store: &Store, args: MigrateArgs, max_vcard_bytes: usize) -> anyhow::Result<()> {
    let password = password(&args)?;
    let mut options = Options::new(&args.from, &args.source_user, password, &args.user);
    options.carddav_from = args.carddav_from;
    options.dry_run = args.dry_run;
    options.only = match args.only.as_deref() {
        Some("calendars") => Some(CollectionKind::Calendar),
        Some(_) => Some(CollectionKind::Addressbook),
        None => None,
    };
    options.map = args.map;
    options.on_conflict = OnConflict::from_name(&args.on_conflict).unwrap_or_default();
    options.include_shared = args.include_shared;
    options.allow_http = args.allow_http;
    options.ca_file = args.ca_file;
    options.max_response = usize::from(args.max_response_mib) * 1024 * 1024;
    options.timeout = Duration::from_secs(args.timeout);
    options.delay = Duration::from_millis(args.delay_ms);
    options.batch_size = usize::from(args.batch_size);
    options.max_vcard_bytes = max_vcard_bytes;

    let summary = migrate::run(store, &options).await?;
    summary.write(&mut std::io::stdout().lock())?;
    if summary.failed() {
        bail!("some collections were not copied completely; run the command again to resume");
    }
    Ok(())
}
