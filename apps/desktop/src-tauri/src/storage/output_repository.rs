use rusqlite::{params, OptionalExtension, Transaction};

use crate::agent::{source_hash_sha256, AgentEventEnvelopeV2, OutputKind, OutputProducedV2};
use crate::error::AppError;
use crate::model::ResultContentMode;
use crate::source::derive_stable_key;

const OUTPUT_ROW_NAMESPACE: &str = "agent-output-row-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutputAppendResult {
    Inserted {
        output_id: String,
        result_revision: i64,
    },
    Replay {
        output_id: String,
        result_revision: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredResultContent {
    pub text: String,
    pub content_mode: ResultContentMode,
    pub content_available: bool,
    pub content_bytes: usize,
    pub result_revision: i64,
    pub source_hash: Option<String>,
}

pub(crate) struct OutputRepository;

impl OutputRepository {
    pub(crate) fn append(
        tx: &Transaction<'_>,
        source_key: &str,
        envelope: &AgentEventEnvelopeV2,
        output: &OutputProducedV2,
    ) -> Result<OutputAppendResult, AppError> {
        let run_key = envelope.correlation.run_key.as_deref().ok_or_else(|| {
            AppError::new("MISSING_RUN_CORRELATION", "Agent 输出事件缺少 Run 关联")
        })?;
        let hash = source_hash_sha256(&output.text);
        let existing = tx
            .query_row(
                "SELECT id, run_key, output_kind, content_hash, result_revision FROM agent_outputs
                 WHERE source_key = ? AND event_id = ?",
                params![source_key, envelope.event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        let output_kind = output_kind_name(output.output_kind);
        if let Some((output_id, stored_run, stored_kind, stored_hash, result_revision)) = existing {
            if stored_run != run_key || stored_kind != output_kind || stored_hash != hash {
                return Err(AppError::new(
                    "AGENT_OUTPUT_ID_CONFLICT",
                    "同一来源中的 Agent 输出事件 ID 对应了不同内容",
                ));
            }
            return Ok(OutputAppendResult::Replay {
                output_id,
                result_revision,
            });
        }

        let output_id = derive_stable_key(
            OUTPUT_ROW_NAMESPACE,
            &[source_key, envelope.event_id.as_str()],
        );
        let result_revision: i64 = tx.query_row(
            "SELECT COALESCE(MAX(result_revision), 0) + 1 FROM agent_outputs WHERE run_key = ?",
            [run_key],
            |row| row.get(0),
        )?;
        crate::storage::retention::check_capacity(
            tx,
            if output.content_available {
                output.text.len()
            } else {
                0
            },
        )?;
        let protected_text = crate::content_crypto::protect_text(
            crate::content_crypto::ContentPurpose::AgentOutput,
            output.text.as_bytes(),
        )?;
        tx.execute(
            "INSERT INTO agent_outputs (
                id, run_key, source_key, event_id, output_kind, raw_text, content_hash,
                content_mode, content_available, content_bytes, result_revision,
                is_final, occurred_at, observed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                output_id,
                run_key,
                source_key,
                envelope.event_id,
                output_kind,
                protected_text,
                hash,
                content_mode_name(output.content_mode),
                i64::from(output.content_available),
                if output.content_available {
                    output.text.len()
                } else {
                    0
                },
                result_revision,
                i64::from(matches!(output.output_kind, OutputKind::AssistantFinal)),
                envelope.occurred_at,
                envelope.observed_at,
            ],
        )?;
        Ok(OutputAppendResult::Inserted {
            output_id,
            result_revision,
        })
    }

    pub(crate) fn latest_result_for_run(
        tx: &Transaction<'_>,
        run_key: &str,
        observed_after: i64,
    ) -> Result<Option<StoredResultContent>, AppError> {
        let stored = tx
            .query_row(
                "SELECT raw_text, content_mode, content_available, content_bytes,
                        result_revision, content_hash
                 FROM agent_outputs
                 WHERE run_key = ?
                   AND output_kind IN ('assistant_final', 'assistant_candidate')
                   AND observed_at > ?
                 ORDER BY result_revision DESC
                 LIMIT 1",
                rusqlite::params![run_key, observed_after],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, usize>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((protected, mode, content_available, content_bytes, result_revision, hash)) =
            stored
        else {
            return Ok(None);
        };
        let content_mode = parse_content_mode(&mode)?;
        // Retention scrubs only terminal content after the notification/result
        // identity is durable.  A scrubbed row remains a lightweight revision
        // record and must not be mistaken for corrupt ciphertext.
        if !content_available {
            return Ok(Some(StoredResultContent {
                text: String::new(),
                content_mode,
                content_available: false,
                content_bytes,
                result_revision,
                source_hash: None,
            }));
        }
        let plaintext = crate::content_crypto::unprotect_text(
            crate::content_crypto::ContentPurpose::AgentOutput,
            &protected,
        )?;
        let text = String::from_utf8(plaintext.to_vec())
            .map_err(|_| AppError::new("AGENT_OUTPUT_CORRUPT", "Agent 输出正文不是有效 UTF-8"))?;
        if text.len() != content_bytes || source_hash_sha256(&text) != hash {
            return Err(AppError::new(
                "AGENT_OUTPUT_CORRUPT",
                "Agent 输出正文校验失败",
            ));
        }
        Ok(Some(StoredResultContent {
            text,
            content_mode,
            content_available,
            content_bytes,
            result_revision,
            source_hash: content_available.then_some(hash),
        }))
    }
}

fn content_mode_name(value: ResultContentMode) -> &'static str {
    match value {
        ResultContentMode::StatusOnly => "status_only",
        ResultContentMode::RedactedExcerpt => "redacted_excerpt",
        ResultContentMode::FullFinal => "full_final",
    }
}

fn parse_content_mode(value: &str) -> Result<ResultContentMode, AppError> {
    match value {
        "status_only" => Ok(ResultContentMode::StatusOnly),
        "redacted_excerpt" => Ok(ResultContentMode::RedactedExcerpt),
        "full_final" => Ok(ResultContentMode::FullFinal),
        _ => Err(AppError::new(
            "AGENT_OUTPUT_CORRUPT",
            "Agent 输出内容模式无效",
        )),
    }
}

fn output_kind_name(value: OutputKind) -> &'static str {
    match value {
        OutputKind::AssistantFinal => "assistant_final",
        OutputKind::AssistantCandidate => "assistant_candidate",
        OutputKind::ErrorSummary => "error_summary",
        OutputKind::SystemSummary => "system_summary",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentCorrelationV2, AgentEventPayloadV2, AGENT_EVENT_SCHEMA_VERSION};
    use crate::db::Db;
    use crate::source::{AgentKind, SourceDescriptor};
    use crate::storage::SourceRepository;

    fn envelope(event_id: &str, at: i64) -> AgentEventEnvelopeV2 {
        AgentEventEnvelopeV2 {
            metadata: None,
            schema_version: AGENT_EVENT_SCHEMA_VERSION,
            source: SourceDescriptor::new(
                AgentKind::new("fixture-agent").unwrap(),
                "events",
                "jsonl",
                "default",
            )
            .unwrap(),
            event_id: event_id.into(),
            occurred_at: at,
            observed_at: at,
            correlation: AgentCorrelationV2 {
                run_key: Some("run-1".into()),
                ..AgentCorrelationV2::default()
            },
            payload: AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "unused fixture payload".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(source_hash_sha256("unused fixture payload")),
                capture_generation: Some(0),
            }),
        }
    }

    #[test]
    fn output_append_is_idempotent_and_never_promotes_a_candidate() {
        let db = Db::open_in_memory().unwrap();
        db.with_transaction(|tx| {
            let first_envelope = envelope("output-1", 10);
            let source_key = SourceRepository::ensure(tx, &first_envelope.source, 10)?;
            let first = OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "first".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(source_hash_sha256("first")),
                capture_generation: Some(0),
            };
            let second_envelope = envelope("output-2", 20);
            let second = OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "second".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(source_hash_sha256("second")),
                capture_generation: Some(0),
            };
            assert!(matches!(
                OutputRepository::append(tx, &source_key, &first_envelope, &first)?,
                OutputAppendResult::Inserted { .. }
            ));
            assert!(matches!(
                OutputRepository::append(tx, &source_key, &first_envelope, &first)?,
                OutputAppendResult::Replay { .. }
            ));
            OutputRepository::append(tx, &source_key, &second_envelope, &second)?;
            Ok(())
        })
        .unwrap();
        let final_event = db
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM agent_outputs WHERE is_final = 1",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(final_event, 0);
    }

    #[test]
    fn database_and_offline_backup_do_not_contain_full_final_plaintext() {
        let directory = tempfile::tempdir().unwrap();
        let sentinel = "FULL_FINAL_PLAINTEXT_SENTINEL_7b34cce9";
        let db = Db::open(directory.path()).unwrap();
        db.with_transaction(|tx| {
            let event = envelope("full-output", 30);
            let source_key = SourceRepository::ensure(tx, &event.source, 30)?;
            OutputRepository::append(
                tx,
                &source_key,
                &event,
                &OutputProducedV2 {
                    output_kind: OutputKind::AssistantFinal,
                    text: sentinel.into(),
                    content_mode: ResultContentMode::FullFinal,
                    content_available: true,
                    source_hash: Some(source_hash_sha256(sentinel)),
                    capture_generation: Some(1),
                },
            )?;
            Ok(())
        })
        .unwrap();
        db.with_connection(|connection| {
            connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            Ok(())
        })
        .unwrap();
        drop(db);

        let database = directory.path().join("promptdock.db");
        let backup = directory.path().join("promptdock.backup.db");
        std::fs::copy(&database, &backup).unwrap();
        for path in [database, backup] {
            let bytes = std::fs::read(path).unwrap();
            assert!(!bytes
                .windows(sentinel.len())
                .any(|window| window == sentinel.as_bytes()));
        }
    }
}
