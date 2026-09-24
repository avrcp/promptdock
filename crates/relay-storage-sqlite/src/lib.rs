use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use sqlx::{
    ConnectOptions, Connection, Row as _, SqliteConnection, SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseOptions {
    pub path: PathBuf,
    pub max_connections: u32,
    pub busy_timeout: Duration,
}

impl DatabaseOptions {
    pub fn new(path: PathBuf, max_connections: u32, busy_timeout: Duration) -> Self {
        Self {
            path,
            max_connections,
            busy_timeout,
        }
    }

    fn busy_timeout(&self) -> Duration {
        self.busy_timeout
    }
}

pub const SCHEMA_IDENTITY: &str = "promptdock-relay-v4";
pub const SCHEMA_REVISION: i64 = 3;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

fn baseline_migration() -> Result<&'static sqlx::migrate::Migration, DatabaseError> {
    match MIGRATOR.migrations.as_ref() {
        // R4 remains a single clean-break baseline migration. The SQLx ledger
        // version is the file's stable 001 identifier; the product schema
        // revision is carried by app_metadata and PRAGMA user_version.
        [migration] if migration.version == 1 => Ok(migration),
        _ => Err(DatabaseError::MigrationState),
    }
}

pub async fn open(config: &DatabaseOptions) -> Result<SqlitePool, DatabaseError> {
    create_parent_directory(&config.path).await?;
    verify_existing_before_write(&config.path).await?;

    let options = SqliteConnectOptions::new()
        .filename(&config.path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .foreign_keys(true)
        // This clears freed SQLite cells during the redaction transaction. It
        // reduces persistence of expired confirmation bodies, but it is not a
        // physical-media secure-deletion guarantee.
        .pragma("secure_delete", "ON")
        .busy_timeout(config.busy_timeout())
        .disable_statement_logging();

    let pool = SqlitePoolOptions::new()
        .min_connections(1)
        .max_connections(config.max_connections)
        .connect_with(options)
        .await
        .map_err(DatabaseError::Open)?;

    if let Err(error) = MIGRATOR.run(&pool).await {
        pool.close().await;
        return Err(DatabaseError::Migrate(error));
    }

    let verified = async {
        let mut connection = pool.acquire().await.map_err(DatabaseError::Verify)?;
        verify_schema(&mut connection).await
    }
    .await;
    if let Err(error) = verified {
        pool.close().await;
        return Err(error);
    }
    Ok(pool)
}

async fn verify_existing_before_write(path: &Path) -> Result<(), DatabaseError> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(DatabaseError::Preflight),
    };
    if metadata.len() == 0 {
        return Ok(());
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .immutable(true)
        .create_if_missing(false)
        .disable_statement_logging();
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .map_err(|_| DatabaseError::Preflight)?;
    let quick_check = sqlx::query_scalar::<_, String>("PRAGMA quick_check")
        .fetch_all(&mut connection)
        .await
        .map_err(|_| DatabaseError::Preflight)?;
    if quick_check.as_slice() != ["ok"] {
        return Err(DatabaseError::Preflight);
    }
    verify_exact_schema(&mut connection).await
}

async fn verify_exact_schema(connection: &mut SqliteConnection) -> Result<(), DatabaseError> {
    let identity: String =
        sqlx::query_scalar("SELECT value FROM app_metadata WHERE key = 'schema_identity'")
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| DatabaseError::Identity)?;
    if identity != SCHEMA_IDENTITY {
        return Err(DatabaseError::Identity);
    }
    let revision: String =
        sqlx::query_scalar("SELECT value FROM app_metadata WHERE key = 'schema_revision'")
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| DatabaseError::Revision)?;
    if revision != SCHEMA_REVISION.to_string() {
        return Err(DatabaseError::Revision);
    }
    let metadata_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM app_metadata")
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| DatabaseError::Identity)?;
    if metadata_rows != 2 {
        return Err(DatabaseError::Identity);
    }
    let user_version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .map_err(DatabaseError::Verify)?;
    if user_version != SCHEMA_REVISION {
        return Err(DatabaseError::Revision);
    }
    let migration_rows = sqlx::query("SELECT version, success, checksum FROM _sqlx_migrations")
        .fetch_all(&mut *connection)
        .await
        .map_err(|_| DatabaseError::MigrationState)?;
    let expected_migration = baseline_migration()?;
    if migration_rows.len() != 1
        || migration_rows[0]
            .try_get::<i64, _>("version")
            .map_err(|_| DatabaseError::MigrationState)?
            != 1
        || !migration_rows[0]
            .try_get::<bool, _>("success")
            .map_err(|_| DatabaseError::MigrationState)?
        || migration_rows[0]
            .try_get::<Vec<u8>, _>("checksum")
            .map_err(|_| DatabaseError::MigrationState)?
            .as_slice()
            != expected_migration.checksum.as_ref()
    {
        return Err(DatabaseError::MigrationState);
    }
    Ok(())
}

async fn verify_schema(connection: &mut SqliteConnection) -> Result<(), DatabaseError> {
    verify_exact_schema(connection).await
}

/// Result reported by SQLite after a bounded WAL checkpoint attempt.
///
/// Callers that need a durable snapshot (for example backup) must reject a
/// busy or incomplete result. Redaction uses this as a best-effort follow-up
/// after its already-committed logical rewrite, so a long-running reader never
/// turns an otherwise successful terminal transition into a failed delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WalCheckpoint {
    pub busy: i64,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
}

pub async fn checkpoint_wal(pool: &SqlitePool) -> Result<WalCheckpoint, DatabaseError> {
    let row = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(pool)
        .await
        .map_err(DatabaseError::Checkpoint)?;
    Ok(WalCheckpoint {
        busy: row.try_get(0).map_err(DatabaseError::Checkpoint)?,
        log_frames: row.try_get(1).map_err(DatabaseError::Checkpoint)?,
        checkpointed_frames: row.try_get(2).map_err(DatabaseError::Checkpoint)?,
    })
}

async fn create_parent_directory(path: &Path) -> Result<(), DatabaseError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|_| DatabaseError::Directory)
}

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("database directory could not be created")]
    Directory,
    #[error("existing database could not be verified without modification")]
    Preflight,
    #[error("database could not be opened")]
    Open(#[source] sqlx::Error),
    #[error("database migration failed")]
    Migrate(#[source] sqlx::migrate::MigrateError),
    #[error("database schema could not be verified")]
    Verify(#[source] sqlx::Error),
    #[error("database WAL checkpoint failed")]
    Checkpoint(#[source] sqlx::Error),
    #[error("database schema identity is invalid")]
    Identity,
    #[error("database schema revision is invalid")]
    Revision,
    #[error("database migration state is invalid")]
    MigrationState,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
        time::SystemTime,
    };

    use sqlx::Executor as _;
    use tempfile::TempDir;

    use super::*;

    fn test_config(directory: &TempDir) -> DatabaseOptions {
        DatabaseOptions::new(directory.path().join("relay.db"), 4, Duration::from_secs(5))
    }

    fn sidecar(path: &Path, suffix: &str) -> PathBuf {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        PathBuf::from(value)
    }

    fn database_files(path: &Path) -> [PathBuf; 4] {
        [
            path.to_owned(),
            sidecar(path, "-wal"),
            sidecar(path, "-shm"),
            sidecar(path, "-journal"),
        ]
    }

    #[derive(Debug, Eq, PartialEq)]
    struct FileSnapshot {
        bytes: Option<Vec<u8>>,
        modified: Option<SystemTime>,
    }

    async fn snapshot(path: &Path) -> FileSnapshot {
        match tokio::fs::metadata(path).await {
            Ok(metadata) => FileSnapshot {
                bytes: Some(tokio::fs::read(path).await.expect("snapshot bytes")),
                modified: Some(metadata.modified().expect("modified time")),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileSnapshot {
                bytes: None,
                modified: None,
            },
            Err(error) => panic!("snapshot metadata failed: {error}"),
        }
    }

    async fn snapshots(paths: &[PathBuf]) -> Vec<FileSnapshot> {
        let mut result = Vec::with_capacity(paths.len());
        for path in paths {
            result.push(snapshot(path).await);
        }
        result
    }

    async fn assert_snapshots_unchanged(paths: &[PathBuf], before: &[FileSnapshot]) {
        assert_eq!(snapshots(paths).await, before);
    }

    async fn create_v3_database(path: &Path) {
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true),
        )
        .await
        .expect("legacy database");
        connection
            .execute("CREATE TABLE app_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .await
            .expect("metadata table");
        sqlx::query(
            "INSERT INTO app_metadata(key, value) VALUES
             ('schema_identity', ?1), ('schema_revision', ?2)",
        )
        .bind("promptdock-relay-v3")
        .bind("6")
        .execute(&mut connection)
        .await
        .expect("metadata rows");
        sqlx::query("PRAGMA user_version = 6")
            .execute(&mut connection)
            .await
            .expect("legacy user version");
        connection.close().await.expect("close legacy database");
    }

    #[tokio::test]
    async fn fresh_database_and_exact_reopen_use_one_v4_r3_baseline() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = test_config(&directory);

        let pool = open(&config).await.expect("fresh database");
        let tables = sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master
             WHERE type='table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .expect("table inventory")
        .into_iter()
        .collect::<BTreeSet<_>>();
        let expected = [
            "_sqlx_migrations",
            "app_metadata",
            "control_confirmations",
            "device_scopes",
            "devices",
            "inbound_commands",
            "notification_bundle_segments",
            "notification_bundles",
            "notification_outbox",
            "result_actions",
            "result_shares",
            "results",
            "selection_contexts",
            "selection_entries",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        assert_eq!(tables, expected);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT value FROM app_metadata WHERE key='schema_identity'",
            )
            .fetch_one(&pool)
            .await
            .expect("schema identity"),
            SCHEMA_IDENTITY
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT value FROM app_metadata WHERE key='schema_revision'",
            )
            .fetch_one(&pool)
            .await
            .expect("schema revision"),
            "3"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await
                .expect("user version"),
            3
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM _sqlx_migrations")
                .fetch_one(&pool)
                .await
                .expect("migration count"),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA quick_check")
                .fetch_one(&pool)
                .await
                .expect("quick check"),
            "ok"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pragma_foreign_key_check")
                .fetch_one(&pool)
                .await
                .expect("foreign key check"),
            0
        );
        pool.close().await;

        let reopened = open(&config).await.expect("exact v4 reopen");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM _sqlx_migrations")
                .fetch_one(&reopened)
                .await
                .expect("reopened migration count"),
            1
        );
        reopened.close().await;
    }

    #[tokio::test]
    async fn v3_database_and_sidecars_are_rejected_before_write() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = test_config(&directory);
        create_v3_database(&config.path).await;
        tokio::fs::write(sidecar(&config.path, "-wal"), b"v3-wal-sentinel")
            .await
            .expect("legacy WAL sentinel");
        tokio::fs::write(sidecar(&config.path, "-shm"), b"v3-shm-sentinel")
            .await
            .expect("legacy SHM sentinel");
        let paths = database_files(&config.path);
        let before = snapshots(&paths).await;

        assert!(matches!(open(&config).await, Err(DatabaseError::Identity)));
        assert_snapshots_unchanged(&paths, &before).await;
    }

    #[tokio::test]
    async fn unidentified_database_is_rejected_before_write() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = test_config(&directory);
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&config.path)
                .create_if_missing(true),
        )
        .await
        .expect("unknown database");
        connection
            .execute("CREATE TABLE unrelated(value TEXT NOT NULL)")
            .await
            .expect("unknown table");
        connection
            .execute("INSERT INTO unrelated VALUES('do-not-touch')")
            .await
            .expect("unknown row");
        connection.close().await.expect("close unknown database");
        let paths = database_files(&config.path);
        let before = snapshots(&paths).await;

        assert!(matches!(open(&config).await, Err(DatabaseError::Identity)));
        assert_snapshots_unchanged(&paths, &before).await;
    }

    #[tokio::test]
    async fn old_revision_one_is_rejected_before_write() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = test_config(&directory);
        let pool = open(&config).await.expect("fresh database");
        pool.close().await;

        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&config.path)
                .create_if_missing(false),
        )
        .await
        .expect("tamper database");
        connection
            .execute("UPDATE app_metadata SET value='1' WHERE key='schema_revision'")
            .await
            .expect("tamper revision");
        connection
            .execute("PRAGMA user_version = 1")
            .await
            .expect("tamper user version");
        connection.close().await.expect("close tampered database");

        let paths = database_files(&config.path);
        let before = snapshots(&paths).await;
        assert!(matches!(open(&config).await, Err(DatabaseError::Revision)));
        assert_snapshots_unchanged(&paths, &before).await;
    }

    #[tokio::test]
    async fn migration_checksum_drift_is_rejected_before_write() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = test_config(&directory);
        let pool = open(&config).await.expect("fresh database");
        pool.close().await;

        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&config.path)
                .create_if_missing(false),
        )
        .await
        .expect("tamper database");
        connection
            .execute("UPDATE _sqlx_migrations SET checksum=x'00'")
            .await
            .expect("tamper migration checksum");
        connection.close().await.expect("close tampered database");

        let paths = database_files(&config.path);
        let before = snapshots(&paths).await;
        assert!(matches!(
            open(&config).await,
            Err(DatabaseError::MigrationState)
        ));
        assert_snapshots_unchanged(&paths, &before).await;
    }

    #[tokio::test]
    async fn corrupt_database_is_rejected_before_write() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = test_config(&directory);
        tokio::fs::write(&config.path, b"not-a-sqlite-database")
            .await
            .expect("corrupt database");
        let paths = database_files(&config.path);
        let before = snapshots(&paths).await;

        assert!(matches!(open(&config).await, Err(DatabaseError::Preflight)));
        assert_snapshots_unchanged(&paths, &before).await;
    }

    #[tokio::test]
    async fn every_pooled_connection_uses_durability_pragmas() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = test_config(&directory);
        let pool = open(&config).await.expect("database");

        let mut connections = Vec::new();
        for _ in 0..config.max_connections {
            let mut connection = pool.acquire().await.expect("pooled connection");
            assert_eq!(
                sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
                    .fetch_one(&mut *connection)
                    .await
                    .expect("foreign keys"),
                1
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("PRAGMA secure_delete")
                    .fetch_one(&mut *connection)
                    .await
                    .expect("secure delete"),
                1
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("PRAGMA synchronous")
                    .fetch_one(&mut *connection)
                    .await
                    .expect("synchronous"),
                2
            );
            connections.push(connection);
        }
        drop(connections);
        pool.close().await;
    }

    #[tokio::test]
    async fn baseline_constraints_reject_invalid_authority_rows() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = test_config(&directory);
        let pool = open(&config).await.expect("database");

        assert!(
            sqlx::query(
                "INSERT INTO devices(id, name, token_hash, created_at)
                 VALUES('not-a-uuid', 'INVALID', ?1, 1)",
            )
            .bind(vec![1_u8; 32])
            .execute(&pool)
            .await
            .is_err()
        );
        assert!(
            sqlx::query("INSERT INTO app_metadata(key, value) VALUES('legacy_alias', 'v3')")
                .execute(&pool)
                .await
                .is_err()
        );
        pool.close().await;
    }
}
