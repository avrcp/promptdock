//! Docker-only proof that a legacy Relay v3 database is rejected in place by
//! the production runtime image without modifying its database or sidecars.

use std::path::{Path, PathBuf};

use sqlx::{
    ConnectOptions as _, Connection as _, Executor as _, SqliteConnection,
    sqlite::SqliteConnectOptions,
};

fn database_path() -> PathBuf {
    std::env::var_os("PROMPTDOCK_SCHEMA_UPGRADE_DB")
        .map(PathBuf::from)
        .expect("PROMPTDOCK_SCHEMA_UPGRADE_DB is required for this ignored Docker test")
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[tokio::test]
#[ignore = "run only from scripts/server/docker-verify.ps1 against a named volume"]
async fn seed_v3_named_volume() {
    let path = database_path();
    tokio::fs::create_dir_all(path.parent().expect("database parent"))
        .await
        .expect("database directory");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true)
            .disable_statement_logging(),
    )
    .await
    .expect("v3 database");
    connection
        .execute("CREATE TABLE app_metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .await
        .expect("v3 metadata");
    connection
        .execute(
            "INSERT INTO app_metadata(key, value) VALUES
             ('schema_identity', 'promptdock-relay-v3'),
             ('schema_revision', '6')",
        )
        .await
        .expect("v3 identity");
    connection
        .execute("CREATE TABLE audit_marker(value TEXT NOT NULL)")
        .await
        .expect("audit marker table");
    connection
        .execute("INSERT INTO audit_marker VALUES('V3_MUST_REMAIN_UNCHANGED')")
        .await
        .expect("audit marker");
    connection
        .execute("PRAGMA user_version = 6")
        .await
        .expect("v3 user version");
    connection.close().await.expect("close v3 database");
}

#[tokio::test]
#[ignore = "run only from scripts/server/docker-verify.ps1 against a named volume"]
async fn verify_v3_named_volume_unchanged() {
    let path = database_path();
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .read_only(true)
            .immutable(true)
            .create_if_missing(false)
            .disable_statement_logging(),
    )
    .await
    .expect("unchanged v3 database");
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT value FROM app_metadata WHERE key='schema_identity'",
        )
        .fetch_one(&mut connection)
        .await
        .expect("v3 identity"),
        "promptdock-relay-v3"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT value FROM app_metadata WHERE key='schema_revision'",
        )
        .fetch_one(&mut connection)
        .await
        .expect("v3 revision"),
        "6"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut connection)
            .await
            .expect("v3 user version"),
        6
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT value FROM audit_marker")
            .fetch_one(&mut connection)
            .await
            .expect("audit marker"),
        "V3_MUST_REMAIN_UNCHANGED"
    );
    connection.close().await.expect("close v3 verification");

    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(
            !sidecar(&path, suffix).exists(),
            "rejected v3 database gained sidecar {suffix}"
        );
    }
}
