//! `calcraft export` and `calcraft import`.

use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{Context, bail};
use calcraft_core::{CollectionKind, Slug, Username};
use calcraft_server::transfer::{self, MAX_IMPORT_SIZE, OnConflict};
use calcraft_store::{Collection, Store};
use clap::Args;
use tokio::sync::mpsc;

/// Which collection of which user.
#[derive(Debug, Args)]
pub struct Target {
    /// Owner of the collection.
    #[arg(long)]
    user: String,
    /// `calendar/<slug>` or `addressbook/<slug>`.
    #[arg(long)]
    collection: String,
    /// ics, jcal, xcal or csv for calendars; vcf, jcard or csv for address
    /// books. Defaults to ics or vcf.
    #[arg(long)]
    format: Option<String>,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    #[command(flatten)]
    target: Target,
    /// Write to this file instead of standard output.
    #[arg(long, short)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    #[command(flatten)]
    target: Target,
    /// What to do with objects whose UID already exists: skip or replace.
    #[arg(long, default_value = "skip")]
    on_conflict: String,
    /// File to import, `-` for standard input.
    file: PathBuf,
}

async fn resolve(store: &Store, target: &Target) -> anyhow::Result<Collection> {
    let username: Username = target.user.parse()?;
    let Some(record) = store.user_credentials(&username).await? else {
        bail!("user {username} does not exist");
    };
    let Some((kind, slug)) = target.collection.split_once('/') else {
        bail!("--collection must be calendar/<slug> or addressbook/<slug>");
    };
    let kind: CollectionKind = kind
        .parse()
        .map_err(|()| anyhow::anyhow!("unknown collection kind {kind:?}"))?;
    let slug: Slug = slug.parse()?;
    store
        .collection(record.user.id, kind, &slug)
        .await?
        .with_context(|| format!("{username} has no {} {slug}", kind.as_str()))
}

pub async fn export(store: &Store, args: ExportArgs) -> anyhow::Result<()> {
    let collection = resolve(store, &args.target).await?;
    let format = transfer::format_for(collection.kind, args.target.format.as_deref())?;
    let mut out: Box<dyn Write> = match &args.output {
        Some(path) => Box::new(
            std::fs::File::create(path)
                .with_context(|| format!("failed to create {}", path.display()))?,
        ),
        None => Box::new(std::io::stdout().lock()),
    };
    let (tx, mut rx) = mpsc::channel(transfer::EXPORT_BUFFER);
    let store = store.clone();
    let task = tokio::spawn(async move { transfer::export(&store, &collection, format, tx).await });
    while let Some(chunk) = rx.recv().await {
        out.write_all(&chunk?).context("failed to write export")?;
    }
    out.flush().context("failed to write export")?;
    task.await.context("export task failed")??;
    Ok(())
}

pub async fn import(store: &Store, args: ImportArgs, max_vcard_bytes: usize) -> anyhow::Result<()> {
    let collection = resolve(store, &args.target).await?;
    let format = transfer::format_for(collection.kind, args.target.format.as_deref())?;
    let Some(on_conflict) = OnConflict::from_name(&args.on_conflict) else {
        bail!("--on-conflict must be skip or replace");
    };
    let mut data = Vec::new();
    let limit = u64::try_from(MAX_IMPORT_SIZE).unwrap_or(u64::MAX) + 1;
    if args.file.as_os_str() == "-" {
        std::io::stdin().lock().take(limit).read_to_end(&mut data)
    } else {
        std::fs::File::open(&args.file)
            .with_context(|| format!("failed to open {}", args.file.display()))?
            .take(limit)
            .read_to_end(&mut data)
    }
    .context("failed to read import file")?;
    if data.len() > MAX_IMPORT_SIZE {
        bail!("file too large (at most {MAX_IMPORT_SIZE} bytes)");
    }
    let slot = transfer::import_slot().await?;
    let report = transfer::import(
        store,
        &collection,
        format,
        data.into(),
        on_conflict,
        max_vcard_bytes,
        slot,
    )
    .await?;
    println!(
        "created {}, replaced {}, skipped {}, rejected {}",
        report.created, report.replaced, report.skipped, report.rejected
    );
    for error in &report.errors {
        eprintln!(
            "rejected {}: {}",
            transfer::printable(error.uid.as_deref().unwrap_or("(no UID)")),
            transfer::printable(&error.reason)
        );
    }
    if report.rejected > report.errors.len() as u64 {
        eprintln!(
            "… and {} more",
            report.rejected - report.errors.len() as u64
        );
    }
    Ok(())
}
