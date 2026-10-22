//! Logging setup.

use tracing::Subscriber;
use tracing_subscriber::{EnvFilter, fmt, fmt::MakeWriter};

use crate::config::{LogFormat, LoggingConfig};

/// Installs the global tracing subscriber. `RUST_LOG` overrides the configured level.
pub fn init(config: &LoggingConfig) -> anyhow::Result<()> {
    install(config, std::io::stdout)
}

/// Like [`init`], but logs to standard error, for CLI commands whose
/// standard output is their result (e.g. `user list`). Migrations and
/// pre-migration backups triggered by them are visible this way.
pub fn init_cli(config: &LoggingConfig) -> anyhow::Result<()> {
    install(config, std::io::stderr)
}

fn install<W>(config: &LoggingConfig, writer: W) -> anyhow::Result<()>
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    use tracing_subscriber::util::SubscriberInitExt;

    let filter =
        EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(&config.level))?;
    let result = match config.format {
        LogFormat::Pretty => fmt()
            .with_env_filter(filter)
            .with_writer(writer)
            .finish()
            .try_init(),
        LogFormat::Json => json_subscriber(filter, writer).try_init(),
    };
    result.map_err(|err| anyhow::anyhow!(err))
}

/// The subscriber behind `logging.format = "json"`, writing to `writer`.
/// Public so that tests can capture exactly what production logs.
pub fn json_subscriber<W>(filter: EnvFilter, writer: W) -> impl Subscriber + Send + Sync + 'static
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .json()
        .finish()
}
