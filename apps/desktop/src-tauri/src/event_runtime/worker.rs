use super::*;

pub(super) enum InboxRecord {
    Envelope(Box<AgentEventEnvelopeV2>),
    Rejected(&'static str),
}

pub(super) struct WorkItem {
    pub(super) record: InboxRecord,
    pub(super) checkpoint: SourceCheckpointUpdate,
    pub(super) ack: mpsc::SyncSender<bool>,
}

#[derive(Clone, Copy)]
pub(super) enum ProcessedRecord {
    Envelope(AgentEventProcessingOutcome),
    Rejected(&'static str),
}

pub(super) fn process_record(
    processor: &dyn AgentEventProcessor,
    record: InboxRecord,
    checkpoint: &SourceCheckpointUpdate,
) -> Result<ProcessedRecord, AppError> {
    match record {
        InboxRecord::Envelope(envelope) => match processor.process(*envelope, checkpoint) {
            Ok(outcome) => Ok(ProcessedRecord::Envelope(outcome)),
            Err(error) if error.code == "AGENT_EVENT_ID_CONFLICT" => {
                // The conflicting record can never become valid on retry: the
                // durable store already owns this source/event identity with a
                // different semantic hash. Advance it through the same
                // rejection checkpoint used for malformed inbox records so a
                // single permanent conflict cannot poison every later Hook
                // event. Database and protected-content failures remain
                // retryable and therefore are not classified here.
                processor.checkpoint_rejection("AGENT_EVENT_ID_CONFLICT", checkpoint)?;
                Ok(ProcessedRecord::Rejected("AGENT_EVENT_ID_CONFLICT"))
            }
            Err(error) => Err(error),
        },
        InboxRecord::Rejected(code) => {
            processor.checkpoint_rejection(code, checkpoint)?;
            Ok(ProcessedRecord::Rejected(code))
        }
    }
}

pub(super) fn record_rejection(
    inbox_path: &Path,
    rejection_code: &'static str,
    checkpoint: &SourceCheckpointUpdate,
) {
    let line = format!(
        "{} agent_event_runtime_rejected code={} source_id={}\n",
        checkpoint.updated_at, rejection_code, checkpoint.source.source_id
    );
    if let Err(error) = append_error(inbox_path, line.as_bytes()) {
        tracing::warn!(%error, rejection = rejection_code, "failed to record inbox rejection");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentEventIngestor;
    use crate::agent::{
        AgentCorrelationV2, AgentEventPayloadV2, RunStartedV2, AGENT_EVENT_SCHEMA_VERSION,
    };
    use crate::db::Db;
    use crate::settings::SettingsStore;
    use crate::source::AgentKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FailingProcessor {
        code: &'static str,
        rejected: AtomicUsize,
        reject_error: bool,
    }

    impl AgentEventProcessor for FailingProcessor {
        fn process(
            &self,
            _envelope: AgentEventEnvelopeV2,
            _checkpoint: &SourceCheckpointUpdate,
        ) -> Result<AgentEventProcessingOutcome, AppError> {
            Err(AppError::new(self.code, "fixture failure"))
        }

        fn checkpoint_rejection(
            &self,
            rejection_code: &'static str,
            _checkpoint: &SourceCheckpointUpdate,
        ) -> Result<(), AppError> {
            assert_eq!(rejection_code, "AGENT_EVENT_ID_CONFLICT");
            if self.reject_error {
                return Err(AppError::new(
                    "STORE_UNAVAILABLE",
                    "fixture checkpoint failure",
                ));
            }
            self.rejected.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn source() -> SourceDescriptor {
        SourceDescriptor::new(
            AgentKind::new("fixture").unwrap(),
            "fixture.events",
            "jsonl",
            "default",
        )
        .unwrap()
    }

    fn envelope() -> AgentEventEnvelopeV2 {
        AgentEventEnvelopeV2 {
            metadata: None,
            schema_version: AGENT_EVENT_SCHEMA_VERSION,
            source: source(),
            event_id: "event-1".into(),
            occurred_at: 1,
            observed_at: 1,
            correlation: AgentCorrelationV2 {
                conversation_key: Some("conversation-1".into()),
                run_key: Some("run-1".into()),
                ..AgentCorrelationV2::default()
            },
            payload: AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        }
    }

    fn checkpoint() -> SourceCheckpointUpdate {
        SourceCheckpointUpdate {
            source: source(),
            cursor_json: "{\"offset\":1}".into(),
            source_revision: None,
            status: "active".into(),
            last_error_code: None,
            updated_at: 1,
        }
    }

    #[test]
    fn permanent_event_id_conflict_is_rejected_and_checkpointed() {
        let processor = FailingProcessor {
            code: "AGENT_EVENT_ID_CONFLICT",
            rejected: AtomicUsize::new(0),
            reject_error: false,
        };
        let processed = process_record(
            &processor,
            InboxRecord::Envelope(Box::new(envelope())),
            &checkpoint(),
        )
        .unwrap();
        assert!(matches!(
            processed,
            ProcessedRecord::Rejected("AGENT_EVENT_ID_CONFLICT")
        ));
        assert_eq!(processor.rejected.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn transient_processing_failure_keeps_the_record_for_retry() {
        let processor = FailingProcessor {
            code: "STORE_UNAVAILABLE",
            rejected: AtomicUsize::new(0),
            reject_error: false,
        };
        let error = process_record(
            &processor,
            InboxRecord::Envelope(Box::new(envelope())),
            &checkpoint(),
        )
        .err()
        .unwrap();
        assert_eq!(error.code, "STORE_UNAVAILABLE");
        assert_eq!(processor.rejected.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn conflict_checkpoint_failure_keeps_the_record_for_retry() {
        let processor = FailingProcessor {
            code: "AGENT_EVENT_ID_CONFLICT",
            rejected: AtomicUsize::new(0),
            reject_error: true,
        };
        let error = process_record(
            &processor,
            InboxRecord::Envelope(Box::new(envelope())),
            &checkpoint(),
        )
        .err()
        .unwrap();
        assert_eq!(error.code, "STORE_UNAVAILABLE");
        assert_eq!(processor.rejected.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn durable_conflict_advances_then_allows_following_event_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let settings_path = directory.path().join("notification-settings.json");
        let db = Arc::new(Db::open(directory.path()).unwrap());
        let ingestor = AgentEventIngestor::new(
            Arc::clone(&db),
            SettingsStore::open(settings_path.clone()).unwrap(),
            Arc::new(|| {}),
            Arc::new(|| {}),
        );

        let first = envelope();
        let mut first_checkpoint = checkpoint();
        first_checkpoint.cursor_json = r#"{"offset":1}"#.into();
        assert!(matches!(
            process_record(
                &ingestor,
                InboxRecord::Envelope(Box::new(first.clone())),
                &first_checkpoint,
            )
            .unwrap(),
            ProcessedRecord::Envelope(_)
        ));

        let mut conflict = first.clone();
        conflict.payload = AgentEventPayloadV2::RunStarted(RunStartedV2 {
            agent_label: "Conflicting payload".into(),
            workspace_path: None,
            model_name: None,
            task: None,
            reasoning_effort: None,
        });
        let mut conflict_checkpoint = checkpoint();
        conflict_checkpoint.cursor_json = r#"{"offset":2}"#.into();
        assert!(matches!(
            process_record(
                &ingestor,
                InboxRecord::Envelope(Box::new(conflict)),
                &conflict_checkpoint,
            )
            .unwrap(),
            ProcessedRecord::Rejected("AGENT_EVENT_ID_CONFLICT")
        ));
        assert_eq!(
            db.source_cursor(&first.source).unwrap().as_deref(),
            Some(conflict_checkpoint.cursor_json.as_str())
        );
        assert_eq!(
            db.source_health(&first.source)
                .unwrap()
                .unwrap()
                .last_error_code
                .as_deref(),
            Some("AGENT_EVENT_ID_CONFLICT")
        );
        db.with_connection(|connection| {
            let (events, label): (i64, String) = connection.query_row(
                "SELECT (SELECT COUNT(*) FROM agent_events), agent_label FROM agent_runs WHERE run_key = 'run-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(events, 1);
            assert_eq!(label, "Fixture");
            Ok(())
        })
        .unwrap();

        drop(ingestor);
        drop(db);

        let reopened = Arc::new(Db::open(directory.path()).unwrap());
        let restarted = AgentEventIngestor::new(
            Arc::clone(&reopened),
            SettingsStore::open(settings_path).unwrap(),
            Arc::new(|| {}),
            Arc::new(|| {}),
        );
        let mut following = envelope();
        following.event_id = "event-2".into();
        following.correlation.run_key = Some("run-2".into());
        let mut following_checkpoint = checkpoint();
        following_checkpoint.cursor_json = r#"{"offset":3}"#.into();
        assert!(matches!(
            process_record(
                &restarted,
                InboxRecord::Envelope(Box::new(following.clone())),
                &following_checkpoint,
            )
            .unwrap(),
            ProcessedRecord::Envelope(_)
        ));
        assert_eq!(
            reopened
                .source_cursor(&following.source)
                .unwrap()
                .as_deref(),
            Some(following_checkpoint.cursor_json.as_str())
        );
        let health = reopened.source_health(&following.source).unwrap().unwrap();
        assert_eq!(health.status, "active");
        assert_eq!(health.last_error_code, None);
        reopened
            .with_connection(|connection| {
                let count: i64 =
                    connection
                        .query_row("SELECT COUNT(*) FROM agent_events", [], |row| row.get(0))?;
                assert_eq!(count, 2);
                Ok(())
            })
            .unwrap();
    }
}
