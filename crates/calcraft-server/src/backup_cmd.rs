//! `calcraft backup` and `calcraft restore`.

use std::{
    io::{BufRead, IsTerminal, Write},
    path::PathBuf,
};

use anyhow::{Context, bail};
use calcraft_server::Config;
use clap::Args;

use crate::ConfigArgs;

#[derive(Debug, Args)]
pub struct BackupArgs {
    #[command(flatten)]
    pub config: ConfigArgs,
    /// File to write the backup to.
    pub file: PathBuf,
    /// Replace the file if it exists.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    #[command(flatten)]
    pub config: ConfigArgs,
    /// Backup file written by `calcraft backup`.
    pub file: PathBuf,
    /// Do not ask for confirmation.
    #[arg(long)]
    pub yes: bool,
}

pub async fn backup(config: &Config, args: &BackupArgs) -> anyhow::Result<()> {
    let database = &config.storage.database;
    let info = calcraft_store::backup_database(database, &args.file, args.force)
        .await
        .with_context(|| format!("backup of {} failed", database.display()))?;
    println!(
        "backup written to {} ({} bytes, migration {})",
        info.path.display(),
        info.size,
        info.migration_version
    );
    Ok(())
}

pub async fn restore(config: &Config, args: &RestoreArgs) -> anyhow::Result<()> {
    let database = &config.storage.database;
    if !args.yes {
        confirm(&format!(
            "Replace database {} with backup {}? The current database is kept as a copy. [y/N] ",
            database.display(),
            args.file.display()
        ))?;
    }
    let info = calcraft_store::restore_database(database, &args.file)
        .await
        .with_context(|| format!("restore of {} failed", args.file.display()))?;
    println!(
        "restored {} from {} ({} bytes, migration {})",
        database.display(),
        info.backup.path.display(),
        info.backup.size,
        info.backup.migration_version
    );
    if let Some(previous) = info.previous {
        println!("previous database kept as {}", previous.display());
    }
    let latest = calcraft_store::latest_migration();
    if info.backup.migration_version < latest {
        println!(
            "the backup has migration {}, this version migrates it to {latest} on the next start",
            info.backup.migration_version
        );
    }
    Ok(())
}

fn confirm(question: &str) -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!("refusing to restore without confirmation: pass --yes");
    }
    let mut stdout = std::io::stdout();
    stdout.write_all(question.as_bytes())?;
    stdout.flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes") {
        bail!("restore cancelled");
    }
    Ok(())
}
