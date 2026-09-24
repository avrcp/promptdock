use thiserror::Error;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::config::LoggingConfig;

pub fn init(config: &LoggingConfig) -> Result<(), TelemetryError> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.filter))
        .map_err(|_| TelemetryError::Filter)?;

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(false),
        )
        .try_init()
        .map_err(|_| TelemetryError::Install)?;

    std::panic::set_hook(Box::new(|_| {
        tracing::error!(operation = "process.panic", "unexpected panic");
    }));
    Ok(())
}

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("logging filter is invalid")]
    Filter,
    #[error("logging subscriber could not be installed")]
    Install,
}
