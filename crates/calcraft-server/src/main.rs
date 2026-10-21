mod users;

use std::{net::SocketAddr, path::PathBuf, process::ExitCode};

use anyhow::Context;
use calcraft_server::{API_COMPILED, AppState, Config, OIDC_COMPILED, WEB_UI_COMPILED, telemetry};
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
}

#[derive(Debug, Args)]
struct ConfigArgs {
    /// Path to the TOML configuration file.
    #[arg(short, long, env = "CALCRAFT_CONFIG")]
    config: Option<PathBuf>,
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
            runtime()?.block_on(async {
                let store = open_store(&config).await?;
                users::run(&store, command).await
            })
        }
    }
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Runtime::new().context("failed to start async runtime")
}

async fn open_store(config: &Config) -> anyhow::Result<Store> {
    Store::open(&config.storage.database)
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

    let mut state = AppState::new(open_store(&config).await?)?;
    state.secure_cookies = config.security.secure_cookies;
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
    let listener = TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.server.bind))?;
    tracing::info!(
        address = %listener.local_addr()?,
        web_ui = config.web_ui.enabled && WEB_UI_COMPILED,
        api = config.api.enabled && API_COMPILED,
        oidc = config.oidc.enabled && OIDC_COMPILED,
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
