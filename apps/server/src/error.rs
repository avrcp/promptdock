use thiserror::Error;

use crate::{
    auth::DeviceError, config::ConfigError, db::DatabaseError, operations::OperationsError,
    server::ServerError, telemetry::TelemetryError,
};

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Telemetry(#[from] TelemetryError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Device(#[from] DeviceError),
    #[error(transparent)]
    Operations(#[from] OperationsError),
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error("command output could not be serialized")]
    Output,
    #[error("doctor checks failed")]
    DoctorFailed,
}
