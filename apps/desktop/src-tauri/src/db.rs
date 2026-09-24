const MIGRATIONS: &[(i32, &str)] = &[(7, include_str!("../migrations/001_init.sql"))];

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use url::Url;

use crate::error::AppError;
use crate::model::{SourceCheckpointUpdate, SourceHealth};
use crate::source::SourceDescriptor;
use crate::storage::SourceRepository;

const SCHEMA_IDENTITY: &str = "promptdock-desktop-v7";
const SCHEMA_REVISION: &str = "1";

pub struct Db(Mutex<Connection>, AtomicU64);

impl Db {
    pub fn open(app_data_dir: &Path) -> Result<Self, AppError> {
        std::fs::create_dir_all(app_data_dir)
            .map_err(|error| AppError::store(error.to_string()))?;
        let path = app_data_dir.join("promptdock.db");
        if path.exists() {
            preflight_existing_database(&path)?;
        }
        Self::initialize(Connection::open(path).map_err(AppError::from)?)
    }

    fn initialize(mut conn: Connection) -> Result<Self, AppError> {
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        preflight_schema(&conn)?;
        run_migrations(&mut conn)?;
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        // The inbox checkpoint and its event projection commit in one transaction.
        // A rotated spool is only eligible for removal after this FULL WAL commit.
        conn.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self(Mutex::new(conn), AtomicU64::new(0)))
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, AppError> {
        self.0
            .lock()
            .map_err(|_| AppError::new("STORE_UNAVAILABLE", "数据库连接不可用"))
    }

    pub(crate) fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let conn = self.connection()?;
        let before = conn.total_changes();
        let result = operation(&conn);
        if conn.total_changes() != before {
            self.1.fetch_add(1, Ordering::Release);
        }
        result
    }

    /// Background receipt work skips a busy connection rather than blocking an async poll.
    pub(crate) fn try_with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let conn = self
            .0
            .try_lock()
            .map_err(|_| AppError::new("STORE_BUSY", "数据库正在处理其他操作"))?;
        let before = conn.total_changes();
        let result = operation(&conn);
        if conn.total_changes() != before {
            self.1.fetch_add(1, Ordering::Release);
        }
        result
    }

    pub(crate) fn with_transaction<T>(
        &self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let mut conn = self.connection()?;
        let before = conn.total_changes();
        let tx = conn.transaction()?;
        let result = operation(&tx)?;
        tx.commit()?;
        if conn.total_changes() != before {
            self.1.fetch_add(1, Ordering::Release);
        }
        Ok(result)
    }

    /// Monotonic process-local dirty revision; reading it never waits for SQLite.
    pub(crate) fn revision(&self) -> u64 {
        self.1.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self, AppError> {
        Self::initialize(Connection::open_in_memory().map_err(AppError::from)?)
    }

    pub(crate) fn source_cursor(
        &self,
        source: &SourceDescriptor,
    ) -> Result<Option<String>, AppError> {
        self.connection()?
            .query_row(
                "SELECT cursor_json FROM source_checkpoints WHERE source_key = ?",
                params![source.source_key()],
                |row| row.get(0),
            )
            .optional()
            .map_err(AppError::from)
    }

    pub(crate) fn source_health(
        &self,
        source: &SourceDescriptor,
    ) -> Result<Option<SourceHealth>, AppError> {
        self.connection()?.query_row(
            "SELECT status, last_error_code, updated_at FROM source_checkpoints WHERE source_key = ?",
            params![source.source_key()],
            |row| Ok(SourceHealth { status: row.get(0)?, last_error_code: row.get(1)?, updated_at: row.get(2)? }),
        ).optional().map_err(AppError::from)
    }

    pub(crate) fn checkpoint(&self, update: &SourceCheckpointUpdate) -> Result<(), AppError> {
        self.with_transaction(|tx| {
            SourceRepository::checkpoint(tx, &update.source, update).map(|_| ())
        })
    }

    #[cfg(test)]
    fn pragma_i64(&self, name: &str) -> i64 {
        self.connection()
            .unwrap()
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .unwrap()
    }
    #[cfg(test)]
    fn pragma_text(&self, name: &str) -> String {
        self.connection()
            .unwrap()
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .unwrap()
    }
}

#[cfg(test)]
fn scalar_count(conn: &Connection, sql: &str) -> Result<i64, AppError> {
    conn.query_row(sql, [], |row| row.get(0))
        .map_err(AppError::from)
}

fn preflight_existing_database(path: &Path) -> Result<(), AppError> {
    let mut uri = Url::from_file_path(path)
        .map_err(|_| AppError::new("STORE_UNAVAILABLE", "无法构造数据库只读预检路径"))?;
    uri.query_pairs_mut().append_pair("immutable", "1");
    let conn = Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(AppError::from)?;
    let quick_check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if quick_check != "ok" {
        return Err(AppError::new(
            "CORRUPT_SCHEMA",
            "数据库完整性检查失败；请先移走或删除旧数据库",
        ));
    }
    if schema_version(&conn)? != supported_version() || !has_current_schema_identity(&conn)? {
        return Err(AppError::new(
            "UNSUPPORTED_SCHEMA",
            "数据库不属于当前 promptdock-desktop-v7 revision 1；请移走旧本地数据后重试",
        ));
    }
    Ok(())
}

fn supported_version() -> i64 {
    i64::from(MIGRATIONS.last().map(|(version, _)| *version).unwrap_or(0))
}
fn schema_version(conn: &Connection) -> Result<i64, AppError> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(AppError::from)
}

fn run_migrations(conn: &mut Connection) -> Result<(), AppError> {
    let current = schema_version(conn)?;
    if current > supported_version() {
        return Err(AppError::new(
            "UNSUPPORTED_SCHEMA",
            format!(
                "数据库版本 {current} 高于当前支持版本 {}",
                supported_version()
            ),
        ));
    }
    preflight_schema(conn)?;
    for (version, sql) in MIGRATIONS {
        if i64::from(*version) > current {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", *version)?;
            tx.commit()?;
        }
    }
    if !has_current_schema_identity(conn)? {
        return Err(AppError::new(
            "UNSUPPORTED_SCHEMA",
            "数据库不属于当前 promptdock-desktop-v7 基线；请移走旧本地数据后重试",
        ));
    }
    Ok(())
}

fn preflight_schema(conn: &Connection) -> Result<(), AppError> {
    let current = schema_version(conn)?;
    if current > supported_version() {
        return Err(AppError::new(
            "UNSUPPORTED_SCHEMA",
            "数据库版本高于当前支持版本",
        ));
    }
    if current == 0 {
        let has_tables: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%')", [], |row| row.get(0))?;
        if has_tables {
            return Err(AppError::new(
                "UNSUPPORTED_SCHEMA",
                "未版本化数据库不是空库；请先移走或删除旧数据库",
            ));
        }
        return Ok(());
    }
    if current != supported_version() || !has_current_schema_identity(conn)? {
        return Err(AppError::new(
            "UNSUPPORTED_SCHEMA",
            "数据库不属于当前 promptdock-desktop-v7 revision 1；请移走旧本地数据后重试",
        ));
    }
    Ok(())
}

fn has_current_schema_identity(conn: &Connection) -> Result<bool, AppError> {
    let has_metadata: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='app_metadata')",
        [],
        |row| row.get(0),
    )?;
    if !has_metadata {
        return Ok(false);
    }
    let identity = conn
        .query_row(
            "SELECT value FROM app_metadata WHERE key = 'schema_identity'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let revision = conn
        .query_row(
            "SELECT value FROM app_metadata WHERE key = 'schema_revision'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(
        identity.as_deref() == Some(SCHEMA_IDENTITY)
            && revision.as_deref() == Some(SCHEMA_REVISION),
    )
}

pub fn now_ms() -> Result<i64, AppError> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::new("INVALID_SYSTEM_TIME", "系统时间早于 Unix Epoch"))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| AppError::new("INVALID_SYSTEM_TIME", "系统时间超出支持范围"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{AgentKind, SourceDescriptor};

    #[test]
    fn dirty_revision_is_lock_free_and_tracks_committed_changes() {
        let db = std::sync::Arc::new(Db::open_in_memory().unwrap());
        let initial = db.revision();
        db.with_connection(|conn| {
            conn.query_row("SELECT 1", [], |_| Ok(()))?;
            Ok(())
        })
        .unwrap();
        assert_eq!(db.revision(), initial);
        db.with_transaction(|tx| {
            tx.execute(
                "UPDATE desktop_privacy SET content_epoch=content_epoch+1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(db.revision() > initial);
        let committed = db.revision();
        let guard = db.connection().unwrap();
        assert_eq!(
            db.try_with_connection(|_| Ok(())).unwrap_err().code,
            "STORE_BUSY"
        );
        let reader = db.clone();
        let (send, recv) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            send.send(reader.revision()).unwrap();
        });
        let observed = recv.recv_timeout(Duration::from_millis(200));
        drop(guard);
        worker.join().unwrap();
        assert_eq!(observed.unwrap(), committed);
    }

    #[test]
    fn baseline_has_only_runtime_persistence_tables() {
        let db = Db::open_in_memory().unwrap();
        db.with_connection(|conn| {
            let tables = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
                .query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            assert_eq!(tables, vec!["agent_events", "agent_outputs", "agent_runs", "app_metadata", "attention_events", "desktop_privacy", "notification_outbox", "relay_result_publications", "retention_counters", "run_presentation", "source_checkpoints", "source_streams"]);
            for table in ["prompt_catalog", "prompt_preferences", "prompt_occurrences", "jobs", "workspaces", "remote_jobs", "notification_delivery_route"] {
                let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?)", [table], |row| row.get(0))?;
                assert!(!exists, "{table} must not exist");
            }
            Ok(())
        }).unwrap();
    }

    #[test]
    fn transaction_rolls_back_on_error() {
        let db = Db::open_in_memory().unwrap();
        let result: Result<(), AppError> = db.with_transaction(|tx| {
            tx.execute(
                "INSERT INTO app_metadata(key, value) VALUES ('rollback', 'no')",
                [],
            )?;
            Err(AppError::new("TEST", "rollback"))
        });
        assert!(result.is_err());
        db.with_connection(|conn| {
            assert_eq!(
                scalar_count(
                    conn,
                    "SELECT COUNT(*) FROM app_metadata WHERE key = 'rollback'"
                )?,
                0
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn checkpoint_round_trips_and_sqlite_is_configured() {
        let db = Db::open_in_memory().unwrap();
        let source = SourceDescriptor::new(
            AgentKind::new("fixture").unwrap(),
            "inbox",
            "jsonl",
            "default",
        )
        .unwrap();
        db.checkpoint(&SourceCheckpointUpdate {
            source: source.clone(),
            cursor_json: r#"{"offset":1}"#.into(),
            source_revision: None,
            status: "active".into(),
            last_error_code: None,
            updated_at: 1,
        })
        .unwrap();
        assert_eq!(
            db.source_cursor(&source).unwrap().as_deref(),
            Some(r#"{"offset":1}"#)
        );
        assert_eq!(db.pragma_i64("foreign_keys"), 1);
        assert_eq!(db.pragma_i64("busy_timeout"), 3000);
        assert_eq!(db.pragma_text("journal_mode"), "memory");
    }

    #[test]
    fn incompatible_schema_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE foreign_data (id TEXT PRIMARY KEY); PRAGMA user_version = 1;",
        )
        .unwrap();
        assert_eq!(
            run_migrations(&mut conn).unwrap_err().code,
            "UNSUPPORTED_SCHEMA"
        );
    }

    #[test]
    fn v6_is_rejected_without_writing_or_migrating_existing_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("promptdock.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE app_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT INTO app_metadata VALUES('schema_identity','promptdock-desktop-v6'),('schema_revision','1'),('pending-result','preserve'); PRAGMA user_version=6;").unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        let error = match Db::open(dir.path()) {
            Ok(_) => panic!("v6 must not open"),
            Err(e) => e,
        };
        assert_eq!(error.code, "UNSUPPORTED_SCHEMA");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!dir.path().join("promptdock.db-wal").exists());
    }
}
