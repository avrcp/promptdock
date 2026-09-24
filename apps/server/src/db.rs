use crate::config::DatabaseConfig;

pub use relay_storage_sqlite::{
    DatabaseError, SCHEMA_IDENTITY, SCHEMA_REVISION, WalCheckpoint, checkpoint_wal,
};

pub async fn open(config: &DatabaseConfig) -> Result<sqlx::SqlitePool, DatabaseError> {
    relay_storage_sqlite::open(&relay_storage_sqlite::DatabaseOptions::new(
        config.path.clone(),
        config.max_connections,
        config.busy_timeout(),
    ))
    .await
}
