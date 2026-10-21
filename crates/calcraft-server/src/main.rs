mod backup_cmd;
mod users;

use std::{net::SocketAddr, path::PathBuf, process::ExitCode};

use anyhow::Context;
use calcraft_server::{API_COMPILED, AppState, Config, WEB_UI_COMPILED, telemetry};
use calcraft_store::Store;
use clap::{Args, Parser, Subcommand};
use tokio::net::TcpListener;

#[derive(Debug, Parser)]
#[command(
    name = "calcraft",
    version,
    about = "Self-hosted CalDAV/CardDAV server"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the server.
    Serve(ServeArgs),
    /// Validate the configuration and print the effective values.
    CheckConfig(ConfigArgs),
    /// Manage local user accounts.
    User {
        #[command(flatten)]
        config: ConfigArgs,
        #[command(subcommand)]
        command: users::UserCommand,
    },
    /// Write a consistent backup of the database (safe while the server runs).
    Backup(backup_cmd::BackupArgs),
    /// Replace the database with a backup (the server must be stopped).
    Restore(backup_cmd::RestoreArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ConfigArgs {
    /// Path to the TOML configuration file.
    #[arg(short, long, env = "CALCRAFT_CONFIG")]
    pub(crate) config: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[command(flatten)]
    config: ConfigArgs,
    /// Address to listen on, overrides `server.bind`.
    #[arg(long)]
    bind: Option<SocketAddr>,
    /// Disable the web interface.
    #[arg(long)]
    no_web_ui: bool,
    /// Disable the REST API.
    #[arg(long)]
    no_api: bool,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::CheckConfig(args) => {
            let config = Config::load(args.config.as_deref())?;
            println!("{config:#?}");
            Ok(())
        }
        Command::Serve(args) => {
            let mut config = Config::load(args.config.config.as_deref())?;
            if let Some(bind) = args.bind {
                config.server.bind = bind;
            }
            if args.no_web_ui {
                config.web_ui.enabled = false;
            }
            if args.no_api {
                config.api.enabled = false;
            }
            telemetry::init(&config.logging)?;
            runtime()?.block_on(serve(config))
        }
        Command::User { config, command } => {
            let config = Config::load(config.config.as_deref())?;
            telemetry::init_cli(&config.logging)?;
            runtime()?.block_on(async {
                let store = open_store(&config).await?;
                users::run(&store, command).await
            })
        }
        Command::Backup(args) => {
            let config = Config::load(args.config.config.as_deref())?;
            telemetry::init_cli(&config.logging)?;
            runtime()?.block_on(backup_cmd::backup(&config, &args))
        }
        Command::Restore(args) => {
            let config = Config::load(args.config.config.as_deref())?;
            telemetry::init_cli(&config.logging)?;
            runtime()?.block_on(backup_cmd::restore(&config, &args))
        }
    }
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Runtime::new().context("failed to start async runtime")
}

async fn open_store(config: &Config) -> anyhow::Result<Store> {
    Store::open_with(&config.storage.database, &config.storage.store_options())
        .await
        .with_context(|| {
            format!(
                "failed to open database {}",
                config.storage.database.display()
            )
        })
}

async fn serve(config: Config) -> anyhow::Result<()> {
    if config.web_ui.enabled && !WEB_UI_COMPILED {
        tracing::warn!("web UI is enabled in the configuration but not compiled into this binary");
    }
    if config.api.enabled && !API_COMPILED {
        tracing::warn!(
            "REST API is enabled in the configuration but not compiled into this binary"
        );
    }

    if !config.server.bind.ip().is_loopback() && !config.security.hsts {
        tracing::warn!(
            bind = %config.server.bind,
            "listening on a non-loopback address: DAV clients send passwords with HTTP Basic \
             authentication, so serve CalCraft only behind TLS (reverse proxy) and set security.hsts = true"
        );
    }

    let mut state = AppState::new(open_store(&config).await?)?;
    state.secure_cookies = config.security.secure_cookies;
    let listener = TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.server.bind))?;
    tracing::info!(
        address = %listener.local_addr()?,
        web_ui = config.web_ui.enabled && WEB_UI_COMPILED,
        api = config.api.enabled && API_COMPILED,
        "CalCraft {} listening",
        calcraft_core::VERSION,
    );

    axum::serve(listener, calcraft_server::router(&config, state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    tracing::info!("shut down");
    Ok(())
}

/// Resolves on Ctrl+C on all platforms and additionally on SIGTERM on Unix.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "failed to listen for Ctrl+C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::error!(%err, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received");
}
