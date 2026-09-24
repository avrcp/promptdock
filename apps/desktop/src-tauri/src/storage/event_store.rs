use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Value};

use crate::agent::{
    content_hash, AgentEventEnvelopeV2, AgentEventPayloadV2, AgentOutcome, AttentionKind,
    CodexUsageSnapshot, CompletionConfidence, OutputKind, SettlingReason, SourceClassification,
};
use crate::error::AppError;
use crate::source::derive_stable_key;
use crate::storage::{RunRepository, SourceRepository};

const EVENT_ROW_NAMESPACE: &str = "agent-event-row-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EventAppendResult {
    Inserted {
        event_row_id: String,
        source_key: String,
    },
    Replay {
        event_row_id: String,
        source_key: String,
        observation_advanced: bool,
    },
}

impl EventAppendResult {
    #[cfg(test)]
    pub(crate) fn is_inserted(&self) -> bool {
        matches!(self, Self::Inserted { .. })
    }

    pub(crate) fn source_key(&self) -> &str {
        match self {
            Self::Inserted { source_key, .. } | Self::Replay { source_key, .. } => source_key,
        }
    }

    pub(crate) fn event_row_id(&self) -> &str {
        match self {
            Self::Inserted { event_row_id, .. } | Self::Replay { event_row_id, .. } => event_row_id,
        }
    }
}

pub(crate) struct EventStore;

impl EventStore {
    pub(crate) fn append(
        tx: &Transaction<'_>,
        envelope: &AgentEventEnvelopeV2,
    ) -> Result<EventAppendResult, AppError> {
        envelope.validate().map_err(|error| {
            AppError::internal("INVALID_AGENT_EVENT", "Agent 事件无效", error.to_string())
        })?;
        let source_key = SourceRepository::ensure(tx, &envelope.source, envelope.observed_at)?;
        let payload_hash = envelope.payload_hash().map_err(|error| {
            AppError::internal(
                "AGENT_EVENT_HASH_FAILED",
                "Agent 事件无法生成语义哈希",
                error.to_string(),
            )
        })?;
        let existing = tx
            .query_row(
                "SELECT id, payload_hash, last_observed_at FROM agent_events
                 WHERE source_key = ? AND event_id = ?",
                params![source_key, envelope.event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((event_row_id, stored_hash, last_observed_at)) = existing {
            if stored_hash != payload_hash {
                return Err(AppError::new(
                    "AGENT_EVENT_ID_CONFLICT",
                    "同一来源中的 Agent 事件 ID 对应了不同内容",
                ));
            }
            let observation_advanced = envelope.observed_at > last_observed_at;
            if observation_advanced {
                tx.execute(
                    "UPDATE agent_events SET last_observed_at = ? WHERE id = ?",
                    params![envelope.observed_at, event_row_id],
                )?;
            }
            return Ok(EventAppendResult::Replay {
                event_row_id,
                source_key,
                observation_advanced,
            });
        }

        let event_row_id = derive_stable_key(
            EVENT_ROW_NAMESPACE,
            &[source_key.as_str(), envelope.event_id.as_str()],
        );
        crate::storage::retention::check_capacity(tx, 0)?;
        let retention_class = retention_class(tx, envelope)?;
        let metadata_json = crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::AgentEventMetadata,
            &event_metadata(envelope),
        )?;
        tx.execute(
            "INSERT INTO agent_events (
                id, source_key, event_id, event_kind, conversation_key, run_key,
                parent_run_key, agent_id, agent_role, outcome, completion_confidence,
                occurred_at, observed_at, last_observed_at, retention_class, metadata_json, payload_hash
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                event_row_id,
                source_key,
                envelope.event_id,
                envelope.payload.kind().as_str(),
                envelope.correlation.conversation_key,
                envelope.correlation.run_key,
                envelope.correlation.parent_run_key,
                envelope.correlation.agent_id,
                envelope.correlation.agent_role,
                envelope.payload.outcome().map(outcome_name),
                envelope
                    .payload
                    .completion_confidence()
                    .map(CompletionConfidence::as_str),
                envelope.occurred_at,
                envelope.observed_at,
                envelope.observed_at,
                retention_class,
                metadata_json,
                payload_hash,
            ],
        )?;
        Ok(EventAppendResult::Inserted {
            event_row_id,
            source_key,
        })
    }

    pub(crate) fn latest_codex_usage(
        tx: &Transaction<'_>,
        run_key: &str,
    ) -> Result<Option<CodexUsageSnapshot>, AppError> {
        let protected = tx
            .query_row(
                "SELECT metadata_json FROM agent_events
                 WHERE run_key = ? AND event_kind = 'run_settling'
                 ORDER BY occurred_at DESC, id DESC LIMIT 1",
                [run_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(protected) = protected else {
            return Ok(None);
        };
        let metadata: Value = crate::content_crypto::unprotect_json(
            crate::content_crypto::ContentPurpose::AgentEventMetadata,
            &protected,
        )?;
        metadata
            .get("codexUsage")
            .filter(|value| !value.is_null())
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                AppError::internal(
                    "AGENT_EVENT_METADATA_CORRUPT",
                    "Agent 事件元数据损坏",
                    error.to_string(),
                )
            })
    }
}

fn retention_class(
    tx: &rusqlite::Transaction<'_>,
    envelope: &AgentEventEnvelopeV2,
) -> Result<&'static str, AppError> {
    match envelope
        .metadata
        .as_ref()
        .map(|metadata| metadata.source_classification)
    {
        Some(SourceClassification::Unknown) => {
            let attributable_to_existing_user_run = envelope
                .correlation
                .run_key
                .as_deref()
                .map(|run_key| RunRepository::find(tx, run_key))
                .transpose()?
                .flatten()
                .is_some_and(|run| run.parent_run_key.is_none());
            Ok(if attributable_to_existing_user_run {
                "normal"
            } else {
                "unknown"
            })
        }
        Some(SourceClassification::Verification) => Ok("verification"),
        _ => Ok("normal"),
    }
}

fn event_metadata(envelope: &AgentEventEnvelopeV2) -> Value {
    let mut metadata = match &envelope.payload {
        AgentEventPayloadV2::PromptSubmitted(_) => json!({}),
        AgentEventPayloadV2::RunStarted(event) => json!({ "agentLabel": event.agent_label }),
        AgentEventPayloadV2::RunSettling(event) => {
            json!({
                "reason": settling_reason_name(event.reason),
                "codexUsage": event.codex_usage,
            })
        }
        AgentEventPayloadV2::RunCompleted(_)
        | AgentEventPayloadV2::RunFailed(_)
        | AgentEventPayloadV2::RunInterrupted(_)
        | AgentEventPayloadV2::RunCancelled(_) => json!({}),
        AgentEventPayloadV2::OutputProduced(event) => json!({
            "outputKind": output_kind_name(event.output_kind),
            "contentHash": content_hash(&event.text),
        }),
        AgentEventPayloadV2::AttentionRequired(event) => json!({
            "attentionKind": attention_kind_name(event.attention_kind),
            "safeSummary": event.safe_summary,
            "expiresAt": event.expires_at,
        }),
    };
    if let Some(capture) = &envelope.metadata {
        metadata["captureMetadata"] = serde_json::to_value(capture).unwrap_or(Value::Null);
    }
    metadata
}

fn outcome_name(value: AgentOutcome) -> &'static str {
    match value {
        AgentOutcome::Success => "success",
        AgentOutcome::Failure => "failure",
        AgentOutcome::Interrupted => "interrupted",
        AgentOutcome::Cancelled => "cancelled",
        AgentOutcome::Unknown => "unknown",
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

fn attention_kind_name(value: AttentionKind) -> &'static str {
    match value {
        AttentionKind::Permission => "permission",
        AttentionKind::UserInput => "user_input",
        AttentionKind::Confirmation => "confirmation",
    }
}

fn settling_reason_name(value: SettlingReason) -> &'static str {
    match value {
        SettlingReason::AgentStopHook => "agent_stop_hook",
        SettlingReason::AdapterSettled => "adapter_settled",
        SettlingReason::WorkflowIdle => "workflow_idle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AgentCorrelationV2, AgentEventPayloadV2, AttentionRequiredV2, CaptureMetadata,
        CodexUsageSnapshot, OutputProducedV2, PromptOrigin, PromptSubmittedV2, RunSettlingV2,
        SourceClassification, AGENT_EVENT_SCHEMA_VERSION,
    };
    use crate::db::Db;
    use crate::source::{AgentKind, SourceDescriptor};

    fn event(source_id: &str, text: &str) -> AgentEventEnvelopeV2 {
        AgentEventEnvelopeV2 {
            metadata: None,
            schema_version: AGENT_EVENT_SCHEMA_VERSION,
            source: SourceDescriptor::new(
                AgentKind::new("fixture-agent").unwrap(),
                source_id,
                "jsonl",
                "default",
            )
            .unwrap(),
            event_id: "event-1".into(),
            occurred_at: 10,
            observed_at: 11,
            correlation: AgentCorrelationV2 {
                run_key: Some("run-1".into()),
                ..AgentCorrelationV2::default()
            },
            payload: AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: text.into(),
                content_mode: crate::model::ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256(text)),
                capture_generation: Some(0),
            }),
        }
    }

    #[test]
    fn capture_metadata_is_kept_as_small_encrypted_event_facts() {
        let mut envelope = event("metadata", "private output");
        envelope.metadata = Some(CaptureMetadata {
            helper_build: "fixture-build".into(),
            policy_revision: 7,
            source_classification: SourceClassification::Unknown,
            source_reason: "missing_transcript".into(),
        });
        let metadata = event_metadata(&envelope);
        assert_eq!(metadata["captureMetadata"]["policy_revision"], 7);
        assert_eq!(
            metadata["captureMetadata"]["source_classification"],
            "unknown"
        );
        let db = Db::open_in_memory().unwrap();
        let class = db
            .with_transaction(|tx| retention_class(tx, &envelope))
            .unwrap();
        assert_eq!(class, "unknown");
    }

    #[test]
    fn insertion_initializes_immutable_and_latest_observation_timestamps() {
        let db = Db::open_in_memory().unwrap();
        let envelope = event("primary", "private output");
        db.with_transaction(|tx| {
            let first = EventStore::append(tx, &envelope)?;
            let replay = EventStore::append(tx, &envelope)?;
            assert!(first.is_inserted());
            assert!(matches!(
                &replay,
                EventAppendResult::Replay {
                    observation_advanced: false,
                    ..
                }
            ));
            assert_eq!(first.source_key(), replay.source_key());
            Ok(())
        })
        .unwrap();
        let (count, occurred_at, observed_at, last_observed_at, metadata): (
            i64,
            i64,
            i64,
            i64,
            String,
        ) = db
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT COUNT(*), occurred_at, observed_at, last_observed_at,
                                metadata_json FROM agent_events",
                        [],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        },
                    )
                    .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(occurred_at, 10);
        assert_eq!(observed_at, 11);
        assert_eq!(last_observed_at, 11);
        assert!(!metadata.contains("private output"));
        let metadata: serde_json::Value = crate::content_crypto::unprotect_json(
            crate::content_crypto::ContentPurpose::AgentEventMetadata,
            &metadata,
        )
        .unwrap();
        assert!(metadata.get("contentHash").is_some());
    }

    #[test]
    fn latest_codex_usage_round_trips_from_protected_settling_metadata() {
        let db = Db::open_in_memory().unwrap();
        let expected = CodexUsageSnapshot {
            used_percent: Some(47),
            window_duration_mins: Some(10_080),
            resets_at: Some(1_800_000_000),
        };
        let mut settling = event("primary", "placeholder");
        settling.event_id = "settling-usage".into();
        settling.payload = AgentEventPayloadV2::RunSettling(RunSettlingV2 {
            reason: SettlingReason::AgentStopHook,
            codex_usage: Some(expected.clone()),
        });
        db.with_transaction(|tx| {
            EventStore::append(tx, &settling)?;
            assert_eq!(EventStore::latest_codex_usage(tx, "run-1")?, Some(expected));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn latest_codex_usage_returns_none_when_settling_had_no_usage() {
        let db = Db::open_in_memory().unwrap();
        let mut settling = event("primary", "placeholder");
        settling.event_id = "settling-no-usage".into();
        settling.payload = AgentEventPayloadV2::RunSettling(RunSettlingV2 {
            reason: SettlingReason::AgentStopHook,
            codex_usage: None,
        });
        db.with_transaction(|tx| {
            EventStore::append(tx, &settling)?;
            let result = EventStore::latest_codex_usage(tx, "run-1");
            assert!(
                result.is_ok(),
                "expected Ok(None) but got error: {:?}",
                result.err()
            );
            assert_eq!(result.unwrap(), None);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn later_replay_advances_only_last_observed_at() {
        let db = Db::open_in_memory().unwrap();
        let first = event("primary", "same output");
        db.with_transaction(|tx| EventStore::append(tx, &first).map(|_| ()))
            .unwrap();

        let mut later = first.clone();
        later.observed_at = 20;
        let result = db
            .with_transaction(|tx| EventStore::append(tx, &later))
            .unwrap();
        assert!(matches!(
            result,
            EventAppendResult::Replay {
                observation_advanced: true,
                ..
            }
        ));

        let timestamps: (i64, i64, i64) = db
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT occurred_at, observed_at, last_observed_at FROM agent_events",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(timestamps, (10, 11, 20));
    }

    #[test]
    fn earlier_out_of_order_replay_does_not_regress_last_observed_at() {
        let db = Db::open_in_memory().unwrap();
        let first = event("primary", "same output");
        db.with_transaction(|tx| EventStore::append(tx, &first).map(|_| ()))
            .unwrap();

        let mut later = first.clone();
        later.observed_at = 20;
        db.with_transaction(|tx| EventStore::append(tx, &later).map(|_| ()))
            .unwrap();

        let mut earlier = first;
        earlier.observed_at = 10;
        let result = db
            .with_transaction(|tx| EventStore::append(tx, &earlier))
            .unwrap();
        assert!(matches!(
            result,
            EventAppendResult::Replay {
                observation_advanced: false,
                ..
            }
        ));

        let last_observed_at: i64 = db
            .with_connection(|connection| {
                connection
                    .query_row("SELECT last_observed_at FROM agent_events", [], |row| {
                        row.get(0)
                    })
                    .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(last_observed_at, 20);
    }

    #[test]
    fn conflicting_replay_rolls_back_and_same_event_id_in_another_source_is_distinct() {
        let db = Db::open_in_memory().unwrap();
        let first = event("primary", "one");
        db.with_transaction(|tx| EventStore::append(tx, &first).map(|_| ()))
            .unwrap();

        let conflict = event("primary", "two");
        let error = db
            .with_transaction(|tx| EventStore::append(tx, &conflict).map(|_| ()))
            .unwrap_err();
        assert_eq!(error.code, "AGENT_EVENT_ID_CONFLICT");

        let other_source = event("secondary", "two");
        db.with_transaction(|tx| EventStore::append(tx, &other_source).map(|_| ()))
            .unwrap();
        let count = db
            .with_connection(|connection| {
                connection
                    .query_row("SELECT COUNT(*) FROM agent_events", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn metadata_omits_prompt_workspace_and_model_content() {
        let db = Db::open_in_memory().unwrap();
        let mut prompt = event("primary", "placeholder");
        prompt.event_id = "prompt".into();
        prompt.payload = AgentEventPayloadV2::PromptSubmitted(PromptSubmittedV2 {
            text: "TOP SECRET PROMPT".into(),
            workspace_path: Some(r"D:\private\project".into()),
            model_name: Some("private-model".into()),
            origin: PromptOrigin::Human,
        });
        let mut attention = event("primary", "placeholder");
        attention.event_id = "attention".into();
        attention.payload = AgentEventPayloadV2::AttentionRequired(AttentionRequiredV2 {
            attention_kind: AttentionKind::Permission,
            safe_summary: "Permission requested for fixture tool".into(),
            expires_at: Some(100),
        });
        db.with_transaction(|tx| {
            EventStore::append(tx, &prompt)?;
            EventStore::append(tx, &attention)?;
            Ok(())
        })
        .unwrap();
        let metadata = db
            .with_connection(|connection| {
                let mut statement = connection
                    .prepare("SELECT metadata_json FROM agent_events ORDER BY event_kind")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
            })
            .unwrap()
            .into_iter()
            .map(|protected| {
                crate::content_crypto::unprotect_json::<serde_json::Value>(
                    crate::content_crypto::ContentPurpose::AgentEventMetadata,
                    &protected,
                )
                .unwrap()
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!metadata.contains("TOP SECRET PROMPT"));
        assert!(metadata.contains("attentionKind"));
        assert!(metadata.contains("Permission requested for fixture tool"));
        assert!(!metadata.contains("origin"));
        assert!(!metadata.contains("private\\project"));
        assert!(!metadata.contains("private-model"));
    }

    fn insert_settling_with_raw_metadata(db: &Db, metadata: &Value) {
        let mut settling = event("raw-usage", "placeholder");
        settling.event_id = "settling-raw-usage".into();
        settling.payload = AgentEventPayloadV2::RunSettling(RunSettlingV2 {
            reason: SettlingReason::AgentStopHook,
            codex_usage: None,
        });
        db.with_transaction(|tx| {
            EventStore::append(tx, &settling)?;
            let protected = crate::content_crypto::protect_json(
                crate::content_crypto::ContentPurpose::AgentEventMetadata,
                metadata,
            )?;
            tx.execute(
                "UPDATE agent_events SET metadata_json = ? WHERE event_id = ?",
                params![protected, "settling-raw-usage"],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q01_missing_codex_usage_key_returns_none() {
        let db = Db::open_in_memory().unwrap();
        insert_settling_with_raw_metadata(&db, &json!({"reason": "agent_stop_hook"}));
        db.with_transaction(|tx| {
            assert_eq!(EventStore::latest_codex_usage(tx, "run-1")?, None);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q02_explicit_null_codex_usage_returns_none() {
        let db = Db::open_in_memory().unwrap();
        insert_settling_with_raw_metadata(
            &db,
            &json!({"reason": "agent_stop_hook", "codexUsage": null}),
        );
        db.with_transaction(|tx| {
            assert_eq!(EventStore::latest_codex_usage(tx, "run-1")?, None);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q03_valid_codex_usage_object_parses_correctly() {
        let db = Db::open_in_memory().unwrap();
        insert_settling_with_raw_metadata(
            &db,
            &json!({
                "reason": "agent_stop_hook",
                "codexUsage": {"usedPercent": 12, "windowDurationMins": 300, "resetsAt": 1800000000}
            }),
        );
        db.with_transaction(|tx| {
            let usage = EventStore::latest_codex_usage(tx, "run-1")?;
            assert_eq!(
                usage,
                Some(CodexUsageSnapshot {
                    used_percent: Some(12),
                    window_duration_mins: Some(300),
                    resets_at: Some(1_800_000_000),
                })
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q04_all_null_fields_parses_as_empty_snapshot() {
        let db = Db::open_in_memory().unwrap();
        insert_settling_with_raw_metadata(
            &db,
            &json!({
                "reason": "agent_stop_hook",
                "codexUsage": {"usedPercent": null, "windowDurationMins": null, "resetsAt": null}
            }),
        );
        db.with_transaction(|tx| {
            let usage = EventStore::latest_codex_usage(tx, "run-1")?;
            assert_eq!(
                usage,
                Some(CodexUsageSnapshot {
                    used_percent: None,
                    window_duration_mins: None,
                    resets_at: None,
                })
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q04b_empty_object_parses_as_empty_snapshot() {
        let db = Db::open_in_memory().unwrap();
        insert_settling_with_raw_metadata(
            &db,
            &json!({"reason": "agent_stop_hook", "codexUsage": {}}),
        );
        db.with_transaction(|tx| {
            let usage = EventStore::latest_codex_usage(tx, "run-1")?;
            assert_eq!(
                usage,
                Some(CodexUsageSnapshot {
                    used_percent: None,
                    window_duration_mins: None,
                    resets_at: None,
                })
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q05_string_codex_usage_is_rejected() {
        let db = Db::open_in_memory().unwrap();
        insert_settling_with_raw_metadata(
            &db,
            &json!({"reason": "agent_stop_hook", "codexUsage": "not-an-object"}),
        );
        db.with_transaction(|tx| {
            let error = EventStore::latest_codex_usage(tx, "run-1").unwrap_err();
            assert_eq!(error.code, "AGENT_EVENT_METADATA_CORRUPT");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q05b_boolean_codex_usage_is_rejected() {
        let db = Db::open_in_memory().unwrap();
        insert_settling_with_raw_metadata(
            &db,
            &json!({"reason": "agent_stop_hook", "codexUsage": true}),
        );
        db.with_transaction(|tx| {
            let error = EventStore::latest_codex_usage(tx, "run-1").unwrap_err();
            assert_eq!(error.code, "AGENT_EVENT_METADATA_CORRUPT");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q05c_array_codex_usage_is_rejected() {
        let db = Db::open_in_memory().unwrap();
        insert_settling_with_raw_metadata(
            &db,
            &json!({"reason": "agent_stop_hook", "codexUsage": [1, 2]}),
        );
        db.with_transaction(|tx| {
            let error = EventStore::latest_codex_usage(tx, "run-1").unwrap_err();
            assert_eq!(error.code, "AGENT_EVENT_METADATA_CORRUPT");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q05d_unknown_field_in_codex_usage_is_rejected() {
        let db = Db::open_in_memory().unwrap();
        insert_settling_with_raw_metadata(
            &db,
            &json!({
                "reason": "agent_stop_hook",
                "codexUsage": {"usedPercent": 10, "windowDurationMins": 300, "resetsAt": 100, "extraField": true}
            }),
        );
        db.with_transaction(|tx| {
            let error = EventStore::latest_codex_usage(tx, "run-1").unwrap_err();
            assert_eq!(error.code, "AGENT_EVENT_METADATA_CORRUPT");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q07_null_metadata_survives_db_close_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::open(dir.path()).unwrap();
            let mut settling = event("primary", "placeholder");
            settling.event_id = "settling-q07".into();
            settling.payload = AgentEventPayloadV2::RunSettling(RunSettlingV2 {
                reason: SettlingReason::AgentStopHook,
                codex_usage: None,
            });
            db.with_transaction(|tx| EventStore::append(tx, &settling).map(|_| ()))
                .unwrap();
        }
        {
            let db = Db::open(dir.path()).unwrap();
            db.with_transaction(|tx| {
                assert_eq!(EventStore::latest_codex_usage(tx, "run-1")?, None);
                Ok(())
            })
            .unwrap();
        }
    }
}
