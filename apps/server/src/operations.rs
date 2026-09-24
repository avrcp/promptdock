use std::{
    collections::BTreeMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use sqlx::{Row as _, SqlitePool};
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use uuid::Uuid;

use crate::{
    config::Config,
    db,
    secret_store::{EncryptedFileSecretStore, SecretStore as _},
};

const DATABASE_BACKUP_FILE: &str = "relay.db";
const CONNECTION_BACKUP_FILE: &str = "wechat-connection.enc";
const MANIFEST_FILE: &str = "manifest.json";
const MAX_ENCRYPTED_CONNECTION_BYTES: u64 = 4 * 1024 * 1024;
const OUTBOX_STATUSES: [&str; 9] = [
    "pending_channel",
    "sending_channel",
    "retry_wait",
    "blocked_activation",
    "blocked_reconnect",
    "provider_accepted",
    "expired",
    "cancelled",
    "dead_letter",
];
const INBOUND_STATUSES: [&str; 6] = [
    "received",
    "dispatching",
    "waiting_gateway",
    "reply_queued",
    "expired",
    "dead_letter",
];

#[derive(Debug, Error)]
pub enum OperationsError {
    #[error("operational database command failed")]
    Database,
    #[error("operational status could not be collected")]
    Status,
    #[error("backup output must be a new or empty directory")]
    BackupTarget,
    #[error("backup source or destination could not be accessed safely")]
    BackupIo,
    #[error("backup manifest could not be serialized")]
    Manifest,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitReport {
    pub status: &'static str,
    pub schema_identity: &'static str,
    pub schema_revision: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusReport {
    pub status: &'static str,
    pub devices: DeviceStatus,
    pub outbox: BTreeMap<String, i64>,
    pub inbound: BTreeMap<String, i64>,
    pub selections: SelectionStatus,
    pub retention_backlog: RetentionBacklog,
    pub wechat: WechatFileStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionStatus {
    pub contexts: i64,
    pub entries: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionBacklog {
    pub outbox: i64,
    pub inbound: i64,
    pub selections: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatus {
    pub total: i64,
    pub enabled: i64,
    pub revoked: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatFileStatus {
    pub enabled: bool,
    pub encrypted_connection_present: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Failed,
    Disabled,
    Absent,
    Skipped,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    pub name: &'static str,
    pub status: CheckStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub status: &'static str,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn config_failure() -> Self {
        Self {
            status: "failed",
            checks: vec![DoctorCheck {
                name: "config",
                status: CheckStatus::Failed,
            }],
        }
    }

    pub fn is_healthy(&self) -> bool {
        self.status == "ok"
    }

    fn from_checks(checks: Vec<DoctorCheck>) -> Self {
        let healthy = checks
            .iter()
            .all(|check| !matches!(check.status, CheckStatus::Failed));
        Self {
            status: if healthy { "ok" } else { "failed" },
            checks,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupReport {
    pub status: &'static str,
    pub database_file: &'static str,
    pub encrypted_connection_included: bool,
    pub manifest_file: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupManifest {
    format_version: u16,
    created_at: i64,
    database: BackupFile,
    encrypted_connection: Option<BackupFile>,
    master_credential_included: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupFile {
    file: &'static str,
    bytes: u64,
    blake3: String,
}

pub async fn initialize(config: &Config) -> Result<InitReport, OperationsError> {
    let pool = db::open(&config.database)
        .await
        .map_err(|_| OperationsError::Database)?;
    pool.close().await;
    Ok(InitReport {
        status: "initialized",
        schema_identity: crate::db::SCHEMA_IDENTITY,
        schema_revision: crate::db::SCHEMA_REVISION,
    })
}

pub async fn status(config: &Config) -> Result<StatusReport, OperationsError> {
    let pool = db::open(&config.database)
        .await
        .map_err(|_| OperationsError::Database)?;
    let result = collect_status(&pool, config).await;
    pool.close().await;
    result
}

async fn collect_status(
    pool: &SqlitePool,
    config: &Config,
) -> Result<StatusReport, OperationsError> {
    let devices = sqlx::query(
        "SELECT COUNT(*) AS total, \
                COALESCE(SUM(CASE WHEN enabled = 1 THEN 1 ELSE 0 END), 0) AS enabled, \
                COALESCE(SUM(CASE WHEN revoked_at IS NOT NULL THEN 1 ELSE 0 END), 0) AS revoked \
         FROM devices",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| OperationsError::Status)?;
    let outbox_rows = sqlx::query(
        "SELECT status, COUNT(*) AS count FROM notification_outbox GROUP BY status ORDER BY status",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| OperationsError::Status)?;
    let mut outbox = OUTBOX_STATUSES
        .into_iter()
        .map(|status| (status.to_owned(), 0))
        .collect::<BTreeMap<_, _>>();
    for row in outbox_rows {
        outbox.insert(
            row.try_get("status").map_err(|_| OperationsError::Status)?,
            row.try_get("count").map_err(|_| OperationsError::Status)?,
        );
    }
    let inbound_rows = sqlx::query(
        "SELECT status, COUNT(*) AS count FROM inbound_commands GROUP BY status ORDER BY status",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| OperationsError::Status)?;
    let mut inbound = INBOUND_STATUSES
        .into_iter()
        .map(|status| (status.to_owned(), 0))
        .collect::<BTreeMap<_, _>>();
    for row in inbound_rows {
        inbound.insert(
            row.try_get("status").map_err(|_| OperationsError::Status)?,
            row.try_get("count").map_err(|_| OperationsError::Status)?,
        );
    }
    let now = unix_timestamp_ms()?;
    let accepted_before = now - i64::from(config.retention.accepted_days) * 86_400_000;
    let dead_before = now - i64::from(config.retention.dead_letter_days) * 86_400_000;
    let terminal_before = now - i64::from(config.retention.inbound_terminal_days) * 86_400_000;
    let expired_before = now - i64::from(config.retention.inbound_expired_days) * 86_400_000;
    let outbox_backlog: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_outbox WHERE
         (status IN ('provider_accepted','cancelled','expired') AND updated_at < ?1)
         OR (status='dead_letter' AND updated_at < ?2)",
    )
    .bind(accepted_before)
    .bind(dead_before)
    .fetch_one(pool)
    .await
    .map_err(|_| OperationsError::Status)?;
    let inbound_backlog: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbound_commands WHERE
         (status IN ('reply_queued','dead_letter') AND updated_at < ?1)
         OR (status='expired' AND updated_at < ?2)",
    )
    .bind(terminal_before)
    .bind(expired_before)
    .fetch_one(pool)
    .await
    .map_err(|_| OperationsError::Status)?;
    let selection_counts = sqlx::query(
        "SELECT
           (SELECT COUNT(*) FROM selection_contexts) AS contexts,
           (SELECT COUNT(*) FROM selection_entries) AS entries,
           (SELECT COUNT(*) FROM selection_contexts WHERE expires_at <= ?1) AS expired",
    )
    .bind(now)
    .fetch_one(pool)
    .await
    .map_err(|_| OperationsError::Status)?;
    let encrypted_connection_present =
        match tokio::fs::symlink_metadata(&config.wechat.connection_file).await {
            Ok(metadata) => metadata.file_type().is_file(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err(OperationsError::Status),
        };
    Ok(StatusReport {
        status: "ok",
        devices: DeviceStatus {
            total: devices
                .try_get("total")
                .map_err(|_| OperationsError::Status)?,
            enabled: devices
                .try_get("enabled")
                .map_err(|_| OperationsError::Status)?,
            revoked: devices
                .try_get("revoked")
                .map_err(|_| OperationsError::Status)?,
        },
        outbox,
        inbound,
        selections: SelectionStatus {
            contexts: selection_counts
                .try_get("contexts")
                .map_err(|_| OperationsError::Status)?,
            entries: selection_counts
                .try_get("entries")
                .map_err(|_| OperationsError::Status)?,
        },
        retention_backlog: RetentionBacklog {
            outbox: outbox_backlog,
            inbound: inbound_backlog,
            selections: selection_counts
                .try_get("expired")
                .map_err(|_| OperationsError::Status)?,
        },
        wechat: WechatFileStatus {
            enabled: config.wechat.enabled,
            encrypted_connection_present,
        },
    })
}

pub async fn doctor(config: &Config) -> DoctorReport {
    let mut checks = vec![DoctorCheck {
        name: "config",
        status: CheckStatus::Ok,
    }];
    match tokio::fs::try_exists(&config.database.path).await {
        Ok(true) => {}
        Ok(false) | Err(_) => {
            checks.push(DoctorCheck {
                name: "database_open",
                status: CheckStatus::Failed,
            });
            checks.push(DoctorCheck {
                name: "database_integrity",
                status: CheckStatus::Skipped,
            });
            checks.push(DoctorCheck {
                name: "database_foreign_keys",
                status: CheckStatus::Skipped,
            });
            checks.push(DoctorCheck {
                name: "database_pragmas",
                status: CheckStatus::Skipped,
            });
            checks.push(DoctorCheck {
                name: "database_schema",
                status: CheckStatus::Skipped,
            });
            append_wechat_checks(config, &mut checks).await;
            return DoctorReport::from_checks(checks);
        }
    }

    let pool = match db::open(&config.database).await {
        Ok(pool) => {
            checks.push(DoctorCheck {
                name: "database_open",
                status: CheckStatus::Ok,
            });
            pool
        }
        Err(_) => {
            checks.push(DoctorCheck {
                name: "database_open",
                status: CheckStatus::Failed,
            });
            append_wechat_checks(config, &mut checks).await;
            return DoctorReport::from_checks(checks);
        }
    };

    checks.push(DoctorCheck {
        name: "database_integrity",
        status: check_quick(&pool).await,
    });
    checks.push(DoctorCheck {
        name: "database_foreign_keys",
        status: check_foreign_keys(&pool).await,
    });
    checks.push(DoctorCheck {
        name: "database_pragmas",
        status: check_pragmas(&pool).await,
    });
    checks.push(DoctorCheck {
        name: "database_schema",
        status: check_schema(&pool).await,
    });
    pool.close().await;
    append_wechat_checks(config, &mut checks).await;
    DoctorReport::from_checks(checks)
}

async fn check_quick(pool: &SqlitePool) -> CheckStatus {
    match sqlx::query_scalar::<_, String>("PRAGMA quick_check")
        .fetch_all(pool)
        .await
    {
        Ok(rows) if rows.len() == 1 && rows[0] == "ok" => CheckStatus::Ok,
        Ok(_) | Err(_) => CheckStatus::Failed,
    }
}

async fn check_foreign_keys(pool: &SqlitePool) -> CheckStatus {
    match sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(pool)
        .await
    {
        Ok(rows) if rows.is_empty() => CheckStatus::Ok,
        Ok(_) | Err(_) => CheckStatus::Failed,
    }
}

async fn check_pragmas(pool: &SqlitePool) -> CheckStatus {
    let result: Result<(String, i64, i64, i64), sqlx::Error> = async {
        Ok((
            sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(pool)
                .await?,
            sqlx::query_scalar("PRAGMA synchronous")
                .fetch_one(pool)
                .await?,
            sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(pool)
                .await?,
            sqlx::query_scalar("PRAGMA busy_timeout")
                .fetch_one(pool)
                .await?,
        ))
    }
    .await;
    match result {
        Ok((journal, 2, 1, 5_000)) if journal.eq_ignore_ascii_case("wal") => CheckStatus::Ok,
        Ok(_) | Err(_) => CheckStatus::Failed,
    }
}

async fn check_schema(pool: &SqlitePool) -> CheckStatus {
    let result: Result<(String, String, i64, i64, i64, i64), sqlx::Error> = async {
        let identity =
            sqlx::query_scalar("SELECT value FROM app_metadata WHERE key = 'schema_identity'")
                .fetch_one(pool)
                .await?;
        let revision =
            sqlx::query_scalar("SELECT value FROM app_metadata WHERE key = 'schema_revision'")
                .fetch_one(pool)
                .await?;
        let user_version = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(pool)
            .await?;
        let required: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'table' AND name IN (
                'devices', 'device_scopes', 'notification_outbox', 'inbound_commands',
                'selection_contexts', 'selection_entries', 'control_confirmations', 'app_metadata'
             )",
        )
        .fetch_one(pool)
        .await?;
        let indexes: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN (
                'idx_device_scopes_scope','ux_outbox_origin_notification',
                'ux_outbox_origin_dedupe','idx_outbox_ready','idx_outbox_retention',
                'idx_inbound_commands_ready','idx_inbound_commands_claim',
                'idx_inbound_commands_retention','idx_selection_contexts_expiry',
                'idx_admin_outbox_created','idx_admin_outbox_interactive_updated',
                'idx_admin_inbound_created','idx_admin_inbound_reply_notification',
                'idx_control_confirmations_expiry','idx_control_confirmations_dispatch',
                'idx_control_confirmations_one_pending_sender'
             )",
        )
        .fetch_one(pool)
        .await?;
        let migrations: i64 = sqlx::query_scalar(
            "SELECT CASE WHEN COUNT(*) = 1
                    AND SUM(CASE WHEN version = 1 AND success = 1 THEN 1 ELSE 0 END) = 1
                 THEN 1 ELSE 0 END
             FROM _sqlx_migrations",
        )
        .fetch_one(pool)
        .await?;
        Ok((
            identity,
            revision,
            user_version,
            required,
            indexes,
            migrations,
        ))
    }
    .await;
    match result {
        Ok((identity, revision, user_version, 8, 16, 1))
            if identity == crate::db::SCHEMA_IDENTITY
                && revision == crate::db::SCHEMA_REVISION.to_string()
                && user_version == crate::db::SCHEMA_REVISION =>
        {
            CheckStatus::Ok
        }
        Ok(_) | Err(_) => CheckStatus::Failed,
    }
}

async fn append_wechat_checks(config: &Config, checks: &mut Vec<DoctorCheck>) {
    // A normal disabled-channel doctor run does not require the systemd secret.
    // Deployment acceptance deliberately supplies CREDENTIALS_DIRECTORY so a
    // restore verifies that the retained key can authenticate any retained
    // encrypted ConnectionBundle before the rollback window closes.
    if !config.wechat.enabled && std::env::var_os("CREDENTIALS_DIRECTORY").is_none() {
        checks.push(DoctorCheck {
            name: "wechat_master_credential",
            status: CheckStatus::Disabled,
        });
        checks.push(DoctorCheck {
            name: "wechat_encrypted_connection",
            status: CheckStatus::Disabled,
        });
        return;
    }
    match EncryptedFileSecretStore::from_systemd_credential(&config.wechat.connection_file).await {
        Ok(store) => {
            checks.push(DoctorCheck {
                name: "wechat_master_credential",
                status: CheckStatus::Ok,
            });
            checks.push(DoctorCheck {
                name: "wechat_encrypted_connection",
                status: match store.load().await {
                    Ok(Some(_)) => CheckStatus::Ok,
                    Ok(None) => CheckStatus::Absent,
                    Err(_) => CheckStatus::Failed,
                },
            });
        }
        Err(_) => {
            checks.push(DoctorCheck {
                name: "wechat_master_credential",
                status: CheckStatus::Failed,
            });
            checks.push(DoctorCheck {
                name: "wechat_encrypted_connection",
                status: CheckStatus::Skipped,
            });
        }
    }
}

pub async fn backup(config: &Config, output: &Path) -> Result<BackupReport, OperationsError> {
    let created_directory = prepare_output_directory(output).await?;
    let database_target = output.join(DATABASE_BACKUP_FILE);
    let connection_target = output.join(CONNECTION_BACKUP_FILE);
    let manifest_target = output.join(MANIFEST_FILE);

    let result = async {
        let pool = db::open(&config.database)
            .await
            .map_err(|_| OperationsError::Database)?;
        let checkpoint = db::checkpoint_wal(&pool)
            .await
            .map_err(|_| OperationsError::Database)?;
        // A backup is an auditable snapshot, not an eventually-consistent WAL
        // copy. Refuse to proceed while a reader prevents the truncate pass.
        if checkpoint.busy != 0 || checkpoint.log_frames != checkpoint.checkpointed_frames {
            pool.close().await;
            return Err(OperationsError::Database);
        }
        let database_path = database_target
            .to_str()
            .ok_or(OperationsError::BackupTarget)?;
        let vacuum = sqlx::query("VACUUM INTO ?1")
            .bind(database_path)
            .execute(&pool)
            .await
            .map_err(|_| OperationsError::Database);
        pool.close().await;
        vacuum?;
        set_private_file_permissions(&database_target).await?;
        let database = fingerprint_file(&database_target, DATABASE_BACKUP_FILE).await?;

        let encrypted_connection =
            match read_optional_encrypted_connection(&config.wechat.connection_file).await? {
                Some(bytes) => {
                    let length =
                        u64::try_from(bytes.len()).map_err(|_| OperationsError::BackupIo)?;
                    atomic_write(&connection_target, &bytes).await?;
                    Some(BackupFile {
                        file: CONNECTION_BACKUP_FILE,
                        bytes: length,
                        blake3: blake3::hash(&bytes).to_hex().to_string(),
                    })
                }
                None => None,
            };
        let encrypted_connection_included = encrypted_connection.is_some();
        let manifest = BackupManifest {
            format_version: 1,
            created_at: unix_timestamp_ms()?,
            database,
            encrypted_connection,
            master_credential_included: false,
        };
        let manifest =
            serde_json::to_vec_pretty(&manifest).map_err(|_| OperationsError::Manifest)?;
        atomic_write(&manifest_target, &manifest).await?;
        sync_directory(output).await?;
        Ok(BackupReport {
            status: "created",
            database_file: DATABASE_BACKUP_FILE,
            encrypted_connection_included,
            manifest_file: MANIFEST_FILE,
        })
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&manifest_target).await;
        let _ = tokio::fs::remove_file(&connection_target).await;
        let _ = tokio::fs::remove_file(&database_target).await;
        if created_directory {
            let _ = tokio::fs::remove_dir(output).await;
        }
    }
    result
}

async fn fingerprint_file(
    path: &Path,
    safe_name: &'static str,
) -> Result<BackupFile, OperationsError> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| OperationsError::BackupIo)?;
    if !metadata.file_type().is_file() {
        return Err(OperationsError::BackupIo);
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| OperationsError::BackupIo)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|_| OperationsError::BackupIo)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(BackupFile {
        file: safe_name,
        bytes: metadata.len(),
        blake3: hasher.finalize().to_hex().to_string(),
    })
}

async fn prepare_output_directory(output: &Path) -> Result<bool, OperationsError> {
    match tokio::fs::symlink_metadata(output).await {
        Ok(metadata) if metadata.file_type().is_dir() => {
            let mut entries = tokio::fs::read_dir(output)
                .await
                .map_err(|_| OperationsError::BackupIo)?;
            if entries
                .next_entry()
                .await
                .map_err(|_| OperationsError::BackupIo)?
                .is_some()
            {
                return Err(OperationsError::BackupTarget);
            }
            Ok(false)
        }
        Ok(_) => Err(OperationsError::BackupTarget),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir(output)
                .await
                .map_err(|_| OperationsError::BackupIo)?;
            set_private_directory_permissions(output).await?;
            Ok(true)
        }
        Err(_) => Err(OperationsError::BackupIo),
    }
}

async fn read_optional_encrypted_connection(
    source: &Path,
) -> Result<Option<Vec<u8>>, OperationsError> {
    let metadata = match tokio::fs::symlink_metadata(source).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(OperationsError::BackupIo),
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_ENCRYPTED_CONNECTION_BYTES {
        return Err(OperationsError::BackupIo);
    }
    let file = open_connection_source(source).await?;
    let mut bytes = Vec::new();
    file.take(MAX_ENCRYPTED_CONNECTION_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| OperationsError::BackupIo)?;
    if u64::try_from(bytes.len()).map_err(|_| OperationsError::BackupIo)?
        > MAX_ENCRYPTED_CONNECTION_BYTES
    {
        return Err(OperationsError::BackupIo);
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
async fn open_connection_source(source: &Path) -> Result<tokio::fs::File, OperationsError> {
    use rustix::fs::{Mode, OFlags};

    let source = source.to_owned();
    let file = tokio::task::spawn_blocking(move || {
        let descriptor = rustix::fs::open(
            &source,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let file: std::fs::File = descriptor.into();
        if !file.metadata()?.file_type().is_file() {
            return Err(std::io::Error::other("backup source is not regular"));
        }
        Ok::<_, std::io::Error>(file)
    })
    .await
    .map_err(|_| OperationsError::BackupIo)?
    .map_err(|_| OperationsError::BackupIo)?;
    Ok(tokio::fs::File::from_std(file))
}

#[cfg(not(unix))]
async fn open_connection_source(source: &Path) -> Result<tokio::fs::File, OperationsError> {
    let file = tokio::fs::File::open(source)
        .await
        .map_err(|_| OperationsError::BackupIo)?;
    if !file
        .metadata()
        .await
        .map_err(|_| OperationsError::BackupIo)?
        .is_file()
    {
        return Err(OperationsError::BackupIo);
    }
    Ok(file)
}

async fn atomic_write(target: &Path, bytes: &[u8]) -> Result<(), OperationsError> {
    let parent = target.parent().ok_or(OperationsError::BackupTarget)?;
    let temporary = parent.join(format!(".backup-{}.tmp", Uuid::new_v4()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|_| OperationsError::BackupIo)?;
        file.write_all(bytes)
            .await
            .map_err(|_| OperationsError::BackupIo)?;
        file.sync_all()
            .await
            .map_err(|_| OperationsError::BackupIo)?;
        drop(file);
        set_private_file_permissions(&temporary).await?;
        tokio::fs::rename(&temporary, target)
            .await
            .map_err(|_| OperationsError::BackupIo)?;
        sync_directory(parent).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

fn unix_timestamp_ms() -> Result<i64, OperationsError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| OperationsError::Manifest)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| OperationsError::Manifest)
}

#[cfg(unix)]
async fn set_private_file_permissions(path: &Path) -> Result<(), OperationsError> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|_| OperationsError::BackupIo)
}

#[cfg(not(unix))]
async fn set_private_file_permissions(_path: &Path) -> Result<(), OperationsError> {
    Ok(())
}

#[cfg(unix)]
async fn set_private_directory_permissions(path: &Path) -> Result<(), OperationsError> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|_| OperationsError::BackupIo)
}

#[cfg(not(unix))]
async fn set_private_directory_permissions(_path: &Path) -> Result<(), OperationsError> {
    Ok(())
}

#[cfg(unix)]
async fn sync_directory(path: &Path) -> Result<(), OperationsError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || std::fs::File::open(path).and_then(|file| file.sync_all()))
        .await
        .map_err(|_| OperationsError::BackupIo)?
        .map_err(|_| OperationsError::BackupIo)
}

#[cfg(not(unix))]
async fn sync_directory(_path: &Path) -> Result<(), OperationsError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DatabaseConfig, WechatConfig};

    fn test_config(directory: &tempfile::TempDir) -> Config {
        Config {
            database: DatabaseConfig {
                path: directory.path().join("relay.db"),
                ..DatabaseConfig::default()
            },
            wechat: WechatConfig {
                enabled: false,
                connection_file: directory.path().join("wechat-connection.enc"),
            },
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn status_and_doctor_are_safe_and_report_database_state() {
        let directory = tempfile::tempdir().expect("directory");
        let config = test_config(&directory);
        initialize(&config).await.expect("initialize");
        let seed_pool = db::open(&config.database).await.expect("database");
        let device_id = "00000000-0000-4000-8000-000000000001";
        let sender_fingerprint = format!("wx:{}", "a".repeat(64));
        let body_sentinel = "STATUS_BODY_SENTINEL_MUST_NOT_LEAK";
        let message_key_sentinel = "STATUS_MESSAGE_KEY_SENTINEL_MUST_NOT_LEAK";
        let handle_sentinel = "STATUS_HANDLE_SENTINEL_MUST_NOT_LEAK_123456";
        sqlx::query(
            "INSERT INTO devices (id, name, token_hash, created_at) VALUES (?1, 'SAFE', ?2, 1)",
        )
        .bind(device_id)
        .bind(vec![7_u8; 32])
        .execute(&seed_pool)
        .await
        .expect("device");
        sqlx::query(
            "INSERT INTO notification_outbox (
                id, origin_kind, origin_key, origin_device_id, notification_id, dedupe_key,
                payload_hash, kind, title, body, priority, status, not_before, expires_at,
                created_at, updated_at
             ) VALUES (
                'safe-outbox', 'device', ?1, ?2, 'notification', 'dedupe', ?3, 'test',
                'safe title', ?4, 1, 'pending_channel', 1, 100, 1, 1
             )",
        )
        .bind(format!("device:{device_id}"))
        .bind(device_id)
        .bind("b".repeat(64))
        .bind(body_sentinel)
        .execute(&seed_pool)
        .await
        .expect("outbox");
        sqlx::query(
            "INSERT INTO inbound_commands (
                message_key, sender_fingerprint, command_kind, command_json, payload_hash,
                status, expires_at, created_at, updated_at
             ) VALUES (?1, ?2, 'help', '{\"action\":\"help\"}', ?3, 'received', 100, 1, 1)",
        )
        .bind(message_key_sentinel)
        .bind(&sender_fingerprint)
        .bind("c".repeat(64))
        .execute(&seed_pool)
        .await
        .expect("inbound");
        sqlx::query(
            "INSERT INTO selection_contexts (sender_fingerprint, expires_at) VALUES (?1, 100)",
        )
        .bind(&sender_fingerprint)
        .execute(&seed_pool)
        .await
        .expect("selection context");
        sqlx::query(
            "INSERT INTO selection_entries (
                sender_fingerprint, slot, device_id, client_opaque_handle, item_kind
             ) VALUES (?1, 1, ?2, ?3, 'run')",
        )
        .bind(&sender_fingerprint)
        .bind(device_id)
        .bind(handle_sentinel)
        .execute(&seed_pool)
        .await
        .expect("selection entry");
        seed_pool.close().await;
        let report = status(&config).await.expect("status");
        assert_eq!(report.devices.total, 1);
        assert_eq!(report.outbox.len(), OUTBOX_STATUSES.len());
        assert_eq!(report.outbox["pending_channel"], 1);
        assert_eq!(report.inbound["received"], 1);
        assert_eq!(report.selections.contexts, 1);
        assert_eq!(report.selections.entries, 1);
        assert!(!report.wechat.enabled);
        assert!(!report.wechat.encrypted_connection_present);
        let serialized = serde_json::to_string(&report).expect("JSON");
        assert!(!serialized.contains(&directory.path().display().to_string()));
        for sentinel in [
            body_sentinel,
            message_key_sentinel,
            sender_fingerprint.as_str(),
            handle_sentinel,
        ] {
            assert!(!serialized.contains(sentinel));
        }

        let doctor = doctor(&config).await;
        assert!(doctor.is_healthy(), "{doctor:?}");
        assert!(
            doctor
                .checks
                .iter()
                .all(|check| !matches!(check.status, CheckStatus::Failed))
        );
    }

    #[tokio::test]
    async fn backup_is_consistent_atomic_and_never_includes_master_key() {
        let directory = tempfile::tempdir().expect("directory");
        let config = test_config(&directory);
        initialize(&config).await.expect("initialize");
        let seed_pool = db::open(&config.database).await.expect("database");
        sqlx::query(
            "INSERT INTO devices (id, name, token_hash, enabled, created_at) \
             VALUES ('00000000-0000-4000-8000-000000000001', 'BACKUP', ?1, 1, 1)",
        )
        .bind(vec![7_u8; 32])
        .execute(&seed_pool)
        .await
        .expect("seed");
        seed_pool.close().await;
        let encrypted = b"encrypted-connection-sentinel";
        tokio::fs::write(&config.wechat.connection_file, encrypted)
            .await
            .expect("connection file");
        let output = directory.path().join("backup");
        tokio::fs::create_dir(&output)
            .await
            .expect("empty backup directory");
        let report = backup(&config, &output).await.expect("backup");
        assert!(report.encrypted_connection_included);
        assert_eq!(
            tokio::fs::read(output.join(CONNECTION_BACKUP_FILE))
                .await
                .expect("connection backup"),
            encrypted
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(output.join(MANIFEST_FILE))
                .await
                .expect("manifest"),
        )
        .expect("manifest JSON");
        assert_eq!(manifest["masterCredentialIncluded"], false);
        assert!(
            !serde_json::to_string(&manifest)
                .expect("JSON")
                .contains("master.key")
        );
        let database_bytes = tokio::fs::read(output.join(DATABASE_BACKUP_FILE))
            .await
            .expect("database bytes");
        assert_eq!(
            manifest["database"]["blake3"],
            blake3::hash(&database_bytes).to_hex().to_string()
        );
        assert_eq!(
            manifest["encryptedConnection"]["blake3"],
            blake3::hash(encrypted).to_hex().to_string()
        );
        assert_eq!(
            manifest["database"]["bytes"],
            u64::try_from(database_bytes.len()).expect("database length")
        );

        let backup_config = DatabaseConfig {
            path: output.join(DATABASE_BACKUP_FILE),
            ..DatabaseConfig::default()
        };
        let backup_pool = db::open(&backup_config).await.expect("backup database");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices")
            .fetch_one(&backup_pool)
            .await
            .expect("device count");
        assert_eq!(count, 1);
        backup_pool.close().await;

        assert!(matches!(
            backup(&config, &output).await,
            Err(OperationsError::BackupTarget)
        ));
    }

    #[tokio::test]
    async fn backup_refuses_a_snapshot_when_a_wal_reader_blocks_the_checkpoint() {
        let directory = tempfile::tempdir().expect("directory");
        let config = test_config(&directory);
        initialize(&config).await.expect("initialize");
        let pool = db::open(&config.database).await.expect("database");
        sqlx::query("PRAGMA wal_autocheckpoint = 0")
            .execute(&pool)
            .await
            .expect("disable auto checkpoint");
        let mut reader = pool.acquire().await.expect("WAL reader");
        sqlx::query("BEGIN")
            .execute(&mut *reader)
            .await
            .expect("reader transaction");
        sqlx::query("SELECT * FROM devices")
            .execute(&mut *reader)
            .await
            .expect("reader snapshot");
        sqlx::query(
            "INSERT INTO devices(id, name, token_hash, created_at)
             VALUES('00000000-0000-4000-8000-000000000099', 'WAL', ?1, 1)",
        )
        .bind(vec![9_u8; 32])
        .execute(&pool)
        .await
        .expect("WAL write");

        let output = directory.path().join("busy-backup");
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(10), backup(&config, &output))
                .await
                .expect("checkpoint deadline"),
            Err(OperationsError::Database)
        ));
        assert!(
            !tokio::fs::try_exists(&output)
                .await
                .expect("backup output existence"),
            "failed checkpoint must not leave a partial snapshot"
        );
        sqlx::query("ROLLBACK")
            .execute(&mut *reader)
            .await
            .expect("reader rollback");
        drop(reader);
        pool.close().await;
    }

    #[tokio::test]
    async fn doctor_fails_closed_for_missing_database_and_wechat_credential() {
        let directory = tempfile::tempdir().expect("directory");
        let mut config = test_config(&directory);
        config.wechat.enabled = true;
        let report = doctor(&config).await;
        assert!(!report.is_healthy());
        let json = serde_json::to_string(&report).expect("JSON");
        assert!(!json.contains(&directory.path().display().to_string()));
        assert!(!json.contains("CREDENTIALS_DIRECTORY"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn newly_created_backup_artifacts_use_private_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("directory");
        let config = test_config(&directory);
        initialize(&config).await.expect("initialize");
        tokio::fs::write(&config.wechat.connection_file, b"ciphertext")
            .await
            .expect("connection file");
        let output = directory.path().join("backup");
        backup(&config, &output).await.expect("backup");
        assert_eq!(
            std::fs::metadata(&output)
                .expect("directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for file in [DATABASE_BACKUP_FILE, CONNECTION_BACKUP_FILE, MANIFEST_FILE] {
            assert_eq!(
                std::fs::metadata(output.join(file))
                    .expect("file")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn status_and_backup_reject_symlinked_connection_or_output() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("directory");
        let config = test_config(&directory);
        initialize(&config).await.expect("initialize");
        let real_connection = directory.path().join("real-connection.enc");
        tokio::fs::write(&real_connection, b"ciphertext")
            .await
            .expect("connection");
        symlink(&real_connection, &config.wechat.connection_file).expect("connection symlink");
        assert!(
            !status(&config)
                .await
                .expect("status")
                .wechat
                .encrypted_connection_present
        );
        assert!(matches!(
            backup(&config, &directory.path().join("backup-from-link")).await,
            Err(OperationsError::BackupIo)
        ));

        tokio::fs::remove_file(&config.wechat.connection_file)
            .await
            .expect("remove connection link");
        let real_output = directory.path().join("real-output");
        tokio::fs::create_dir(&real_output)
            .await
            .expect("real output");
        let output_link = directory.path().join("output-link");
        symlink(&real_output, &output_link).expect("output symlink");
        assert!(matches!(
            backup(&config, &output_link).await,
            Err(OperationsError::BackupTarget)
        ));
    }
}
