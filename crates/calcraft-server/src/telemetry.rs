//! Logging setup.

use tracing_subscriber::{EnvFilter, fmt};

use crate::config::{LogFormat, LoggingConfig};

/// Installs the global tracing subscriber. `RUST_LOG` overrides the configured level.
pub fn init(config: &LoggingConfig) -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(&config.level))?;
    let builder = fmt().with_env_filter(filter);
    let result = match config.format {
        LogFormat::Pretty => builder.try_init(),
        LogFormat::Json => builder.json().try_init(),
    };
    result.map_err(|err| anyhow::anyhow!(err))
}
