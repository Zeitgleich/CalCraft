mod backup_cmd;
mod transfer_cmd;
mod users;

use std::{net::SocketAddr, path::PathBuf, process::ExitCode};

use anyhow::Context;
use calcraft_server::{
    API_COMPILED, AppState, Config, OIDC_COMPILED, UPDATE_CHECK_COMPILED, WEB_UI_COMPILED,
    telemetry,
};
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
    /// Write a calendar or address book to a file (ics, jcal, xcal, vcf,
    /// jcard, csv).
    Export {
        #[command(flatten)]
        config: ConfigArgs,
        #[command(flatten)]
        args: transfer_cmd::ExportArgs,
    },
    /// Import a file into a calendar or address book. Every object is
    /// validated like a DAV upload.
    Import {
        #[command(flatten)]
        config: ConfigArgs,
        #[command(flatten)]
        args: transfer_cmd::ImportArgs,
    },
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
                users::run(&store, &config, command).await
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
        Command::Export { config, args } => {
            let config = Config::load(config.config.as_deref())?;
            telemetry::init_cli(&config.logging)?;
            runtime()?.block_on(async {
                let store = open_store(&config).await?;
                transfer_cmd::export(&store, args).await
            })
        }
        Command::Import { config, args } => {
            let config = Config::load(config.config.as_deref())?;
            telemetry::init_cli(&config.logging)?;
            runtime()?.block_on(async {
                let store = open_store(&config).await?;
                transfer_cmd::import(&store, args).await
            })
        }
    }
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Runtime::new().context("failed to start async runtime")
}

async fn open_store(config: &Config) -> anyhow::Result<Store> {
    let mut options = config.storage.store_options();
    // Overload must surface as 503 + Retry-After before the request timeout (408).
    options.acquire_timeout = options
        .acquire_timeout
        .min(config.server.request_timeout() / 2);
    Store::open_with(&config.storage.database, &options)
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

    if config.oidc.enabled && !OIDC_COMPILED {
        tracing::warn!("OIDC is enabled in the configuration but not compiled into this binary");
    }
    if config.oidc.enabled && !config.api.enabled {
        tracing::warn!(
            "OIDC is enabled but the REST API (which serves the SSO endpoints) is disabled"
        );
    }

    if config.updates.check && !UPDATE_CHECK_COMPILED {
        tracing::warn!(
            "the update check is enabled in the configuration but not compiled into this binary"
        );
    }
    if config.updates.check && !config.api.enabled {
        tracing::warn!(
            "the update check is enabled but the REST API (which shows its result) is disabled; not checking"
        );
    }

    if !config.security.password_login && !(OIDC_COMPILED && config.api.enabled) {
        anyhow::bail!(
            "security.password_login = false needs single sign-on, but OIDC is not compiled in or the API is disabled"
        );
    }

    let store = open_store(&config).await?;
    // Purges expired trash entries now and then hourly.
    calcraft_server::trash::spawn_purge(store.clone());
    let mut state = AppState::new(store)?;
    state.secure_cookies = config.security.secure_cookies;
    state
        .auth
        .set_password_login(config.security.password_login);
    #[cfg(feature = "oidc")]
    if config.oidc.enabled {
        let oidc =
            calcraft_server::oidc::Oidc::new(&config.oidc).context("invalid OIDC configuration")?;
        tokio::spawn({
            let oidc = oidc.clone();
            async move { oidc.warm_up().await }
        });
        state.oidc = Some(oidc);
    }
    #[cfg(feature = "update-check")]
    if config.updates.check && config.api.enabled {
        state.updates = Some(calcraft_server::updates::spawn(&config.updates));
    }
    let listener = TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.server.bind))?;
    tracing::info!(
        address = %listener.local_addr()?,
        web_ui = config.web_ui.enabled && WEB_UI_COMPILED,
        api = config.api.enabled && API_COMPILED,
        oidc = config.oidc.enabled && OIDC_COMPILED,
        update_check = config.updates.check && config.api.enabled && UPDATE_CHECK_COMPILED,
        "CalCraft {} listening",
        calcraft_core::VERSION,
    );

    if !config.server.trusted_proxies.is_empty() {
        tracing::info!(
            trusted_proxies = ?config.server.trusted_proxies.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "client addresses are taken from X-Forwarded-For of trusted proxies"
        );
    }
    // The peer address is needed for the per-client limits.
    let app =
        calcraft_server::router(&config, state).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
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
