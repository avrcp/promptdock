use rusqlite::{params, Transaction};

use crate::error::AppError;
use crate::model::SourceCheckpointUpdate;
use crate::source::SourceDescriptor;

pub(crate) struct SourceRepository;

impl SourceRepository {
    pub(crate) fn ensure(
        tx: &Transaction<'_>,
        source: &SourceDescriptor,
        observed_at: i64,
    ) -> Result<String, AppError> {
        source.validate().map_err(|error| {
            AppError::internal(
                "INVALID_SOURCE_DESCRIPTOR",
                "采集来源身份无效",
                error.to_string(),
            )
        })?;
        let source_key = source.source_key();
        let changed = tx.execute(
            "INSERT INTO source_streams (
                source_key, agent_kind, source_id, source_kind, instance_id,
                created_at, last_seen_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_key) DO UPDATE SET
                last_seen_at = CASE
                    WHEN source_streams.last_seen_at IS NULL THEN excluded.last_seen_at
                    ELSE MAX(source_streams.last_seen_at, excluded.last_seen_at)
                END
             WHERE source_streams.agent_kind = excluded.agent_kind
               AND source_streams.source_id = excluded.source_id
               AND source_streams.source_kind = excluded.source_kind
               AND source_streams.instance_id = excluded.instance_id",
            params![
                source_key,
                source.agent_kind.as_str(),
                source.source_id,
                source.source_kind,
                source.instance_id,
                observed_at,
                observed_at,
            ],
        )?;
        if changed != 1 {
            return Err(AppError::new(
                "SOURCE_IDENTITY_CONFLICT",
                "采集来源哈希与已存来源身份不一致",
            ));
        }
        Ok(source_key)
    }

    pub(crate) fn checkpoint(
        tx: &Transaction<'_>,
        expected_source: &SourceDescriptor,
        update: &SourceCheckpointUpdate,
    ) -> Result<String, AppError> {
        if &update.source != expected_source {
            return Err(AppError::new(
                "SOURCE_IDENTITY_CONFLICT",
                "Agent 事件与 checkpoint 的来源身份不一致",
            ));
        }
        let source_key = Self::ensure(tx, &update.source, update.updated_at)?;
        tx.execute(
            "INSERT INTO source_checkpoints (
                source_key, cursor_json, source_revision, status, last_error_code, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_key) DO UPDATE SET
                cursor_json = excluded.cursor_json,
                source_revision = excluded.source_revision,
                status = excluded.status,
                last_error_code = excluded.last_error_code,
                updated_at = excluded.updated_at",
            params![
                source_key,
                update.cursor_json,
                update.source_revision,
                update.status,
                update.last_error_code,
                update.updated_at,
            ],
        )?;
        Ok(source_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::source::AgentKind;

    fn source(instance: &str) -> SourceDescriptor {
        SourceDescriptor::new(
            AgentKind::new("fixture-agent").unwrap(),
            "events",
            "jsonl",
            instance,
        )
        .unwrap()
    }

    #[test]
    fn ensure_is_idempotent_and_last_seen_is_monotonic() {
        let db = Db::open_in_memory().unwrap();
        let descriptor = source("default");
        db.with_transaction(|tx| {
            let key = SourceRepository::ensure(tx, &descriptor, 20)?;
            assert_eq!(key, descriptor.source_key());
            SourceRepository::ensure(tx, &descriptor, 10)?;
            Ok(())
        })
        .unwrap();
        let last_seen: i64 = db
            .with_connection(|connection| {
                connection
                    .query_row("SELECT last_seen_at FROM source_streams", [], |row| {
                        row.get(0)
                    })
                    .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(last_seen, 20_i64);
    }

    #[test]
    fn checkpoint_is_written_for_the_complete_source_identity() {
        let db = Db::open_in_memory().unwrap();
        let descriptor = source("secondary");
        let update = SourceCheckpointUpdate {
            source: descriptor.clone(),
            cursor_json: r#"{"offset":42}"#.into(),
            source_revision: Some("revision-a".into()),
            status: "active".into(),
            last_error_code: None,
            updated_at: 42,
        };
        db.with_transaction(|tx| {
            assert_eq!(
                SourceRepository::checkpoint(tx, &descriptor, &update)?,
                descriptor.source_key()
            );
            Ok(())
        })
        .unwrap();
        let stored = db
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT cursor_json FROM source_checkpoints WHERE source_key = ?",
                        [descriptor.source_key()],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(stored, update.cursor_json);
    }

    #[test]
    fn checkpoint_rejects_a_different_source_before_writing() {
        let db = Db::open_in_memory().unwrap();
        let expected = source("primary");
        let update = SourceCheckpointUpdate {
            source: source("other"),
            cursor_json: r#"{"offset":1}"#.into(),
            source_revision: None,
            status: "active".into(),
            last_error_code: None,
            updated_at: 1,
        };
        let error = db
            .with_transaction(|tx| SourceRepository::checkpoint(tx, &expected, &update).map(|_| ()))
            .unwrap_err();
        assert_eq!(error.code, "SOURCE_IDENTITY_CONFLICT");
        let count = db
            .with_connection(|connection| {
                connection
                    .query_row("SELECT COUNT(*) FROM source_streams", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}
