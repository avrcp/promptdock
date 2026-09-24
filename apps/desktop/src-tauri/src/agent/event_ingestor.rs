use std::sync::Arc;

use rusqlite::Transaction;

use super::run_reducer::{reduce_run_with_quiet, terminal_at, RunStateError, RunTransition};
use crate::agent::{
    AgentEventEnvelopeV2, AgentEventPayloadV2, AttentionKind, CompletionConfidence,
};
use crate::db::Db;
use crate::error::AppError;
use crate::event_runtime::{AgentEventProcessingOutcome, AgentEventProcessor};
use crate::model::{
    NotificationMode, NotificationSettings, ResultContentMode, SourceCheckpointUpdate,
};
use crate::notification::content_builder::{
    BuildAttentionNotificationInput, BuildNotificationInput, NotificationContentBuilder,
};
use crate::notification::outbox::{
    AttentionNotificationInsert, NotificationEventKind, NotificationPayloadV2, OutboxRepository,
    RunNotificationInsert,
};
use crate::notification::{
    attention_notification_policy_fingerprint, run_notification_policy_fingerprint,
};
use crate::settings::SettingsStore;
use crate::storage::{
    AgentRunRecord, EventAppendResult, EventStore, OutputRepository, RunRepository,
    SourceRepository,
};

const TERMINAL_NOTIFICATION_DEBOUNCE_MS: i64 = 3_000;

#[derive(Clone)]
struct AgentNotificationPolicy {
    notifications: NotificationSettings,
    result_content_mode: ResultContentMode,
}

trait AgentNotificationPolicyProvider: Send + Sync + 'static {
    fn policy(&self) -> Result<AgentNotificationPolicy, AppError>;
}

impl AgentNotificationPolicyProvider for SettingsStore {
    fn policy(&self) -> Result<AgentNotificationPolicy, AppError> {
        let snapshot = self.get()?;
        Ok(AgentNotificationPolicy {
            notifications: snapshot.notifications,
            result_content_mode: snapshot.capture.result_content_mode,
        })
    }
}

#[derive(Clone)]
pub(crate) struct AgentEventIngestor {
    db: Arc<Db>,
    settings: Arc<dyn AgentNotificationPolicyProvider>,
    wake_outbox: Arc<dyn Fn() + Send + Sync>,
    wake_finalizer: Arc<dyn Fn() + Send + Sync>,
}

impl AgentEventIngestor {
    pub(crate) fn new(
        db: Arc<Db>,
        settings: SettingsStore,
        wake_outbox: Arc<dyn Fn() + Send + Sync>,
        wake_finalizer: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            db,
            settings: Arc::new(settings),
            wake_outbox,
            wake_finalizer,
        }
    }

    pub(crate) fn checkpoint_policy_skip(
        &self,
        checkpoint: &SourceCheckpointUpdate,
    ) -> Result<(), AppError> {
        self.db.with_transaction(|tx| {
            SourceRepository::checkpoint(tx, &checkpoint.source, checkpoint)?;
            Ok(())
        })
    }

    #[cfg(test)]
    fn with_settings(db: Arc<Db>, settings: NotificationSettings) -> Self {
        let result_content_mode = settings.result_content_mode;
        db.with_connection(|connection| {
            connection.execute(
                "UPDATE desktop_privacy SET capture_mode='full_final', content_epoch=0 WHERE singleton=1",
                [],
            )?;
            Ok(())
        })
        .expect("enable fixture output capture");
        Self {
            db,
            settings: Arc::new(FixedSettings(AgentNotificationPolicy {
                notifications: settings,
                result_content_mode,
            })),
            wake_outbox: Arc::new(|| {}),
            wake_finalizer: Arc::new(|| {}),
        }
    }
}

impl AgentEventProcessor for AgentEventIngestor {
    fn process(
        &self,
        envelope: AgentEventEnvelopeV2,
        checkpoint: &SourceCheckpointUpdate,
    ) -> Result<AgentEventProcessingOutcome, AppError> {
        envelope.validate().map_err(|error| {
            AppError::internal("INVALID_AGENT_EVENT", "Agent 事件无效", error.to_string())
        })?;
        if envelope.source != checkpoint.source {
            return Err(AppError::new(
                "SOURCE_IDENTITY_CONFLICT",
                "Agent 事件与 checkpoint 的来源身份不一致",
            ));
        }

        // Settings live outside SQLite. Snapshot them before opening the
        // transaction so the atomic persistence boundary never performs file I/O.
        let policy = self.settings.policy()?;
        if !policy.notifications.validate() {
            return Err(AppError::new(
                "INVALID_NOTIFICATION_SETTINGS",
                "通知设置无效",
            ));
        }

        let committed = self.db.with_transaction(|tx| {
            crate::desktop_policy::require_applied(tx, policy.notifications.policy_revision)?;
            let append = EventStore::append(tx, &envelope)?;
            let source_key = append.source_key().to_owned();
            let event_row_id = append.event_row_id().to_owned();
            let result = match append {
                EventAppendResult::Replay { .. } => CommittedOutcome::default(),
                EventAppendResult::Inserted { .. } => project_event(
                    tx,
                    &event_row_id,
                    &source_key,
                    &envelope,
                    &policy,
                    checkpoint.updated_at,
                )?,
            };
            SourceRepository::checkpoint(tx, &envelope.source, checkpoint)?;
            Ok(result)
        })?;

        if committed.outbox_changed {
            (self.wake_outbox)();
        }
        if committed.finalizer_changed {
            (self.wake_finalizer)();
        }
        Ok(committed.outcome)
    }

    fn checkpoint_rejection(
        &self,
        rejection_code: &'static str,
        checkpoint: &SourceCheckpointUpdate,
    ) -> Result<(), AppError> {
        let rejected = SourceCheckpointUpdate {
            source: checkpoint.source.clone(),
            cursor_json: checkpoint.cursor_json.clone(),
            source_revision: checkpoint.source_revision.clone(),
            status: "degraded".into(),
            last_error_code: Some(rejection_code.into()),
            updated_at: checkpoint.updated_at,
        };
        self.db.with_transaction(|tx| {
            SourceRepository::checkpoint(tx, &rejected.source, &rejected)?;
            Ok(())
        })
    }
}

#[derive(Default)]
struct CommittedOutcome {
    outcome: AgentEventProcessingOutcome,
    outbox_changed: bool,
    finalizer_changed: bool,
}

fn project_event(
    tx: &Transaction<'_>,
    event_row_id: &str,
    source_key: &str,
    envelope: &AgentEventEnvelopeV2,
    policy: &AgentNotificationPolicy,
    ingested_at: i64,
) -> Result<CommittedOutcome, AppError> {
    let mut result = CommittedOutcome::default();
    if let Some(metadata) = &envelope.metadata {
        use crate::agent::SourceClassification;
        let suppressed = match metadata.source_classification {
            SourceClassification::ExplicitNonTarget | SourceClassification::Verification => true,
            SourceClassification::Unknown => envelope
                .correlation
                .run_key
                .as_deref()
                .map(|key| RunRepository::find(tx, key))
                .transpose()?
                .flatten()
                .is_none(),
            SourceClassification::UserTurn => false,
        };
        if suppressed {
            return Ok(result);
        }
    }
    let settings = &policy.notifications;
    match &envelope.payload {
        // The desktop capture adapter filters PromptSubmitted in production.
        // If a legacy/replayed event reaches this generic ingestor, retain only
        // its event/checkpoint fact; do not rebuild the Prompt Catalog.
        AgentEventPayloadV2::PromptSubmitted(_) => {}
        AgentEventPayloadV2::RunStarted(started) => {
            let transition =
                reduce_and_store(tx, envelope, policy.notifications.completion_quiet_ms)?;
            apply_reopen_side_effects(tx, envelope, &transition)?;
            let run = &transition.run;
            if settings.enabled
                && !matches!(settings.mode, NotificationMode::CompletionOnly)
                && run.status == "running"
            {
                let not_before = envelope.occurred_at;
                let dedupe_key = format!("run_started:{}", run.run_key);
                let payload = build_notification_payload(
                    tx,
                    settings,
                    run,
                    NotificationEventKind::RunStarted,
                    policy.result_content_mode.captures_text(),
                    Some(started),
                )?;
                let content_policy_hash =
                    crate::notification::started_notification_policy_fingerprint();
                result.outbox_changed = OutboxRepository::insert_tx(
                    tx,
                    RunNotificationInsert {
                        agent_run_key: &run.run_key,
                        kind: NotificationEventKind::RunStarted,
                        dedupe_key: &dedupe_key,
                        payload: &payload,
                        content_policy_hash: &content_policy_hash,
                        created_at: envelope.occurred_at,
                        not_before,
                    },
                )?;
            }
            result.finalizer_changed = true;
        }
        AgentEventPayloadV2::RunSettling(_) => {
            let transition =
                reduce_and_store(tx, envelope, policy.notifications.completion_quiet_ms)?;
            cancel_start(
                tx,
                &transition.run.run_key,
                envelope.occurred_at,
                "RUN_SETTLING",
            )?;
            result.finalizer_changed = true;
        }
        AgentEventPayloadV2::RunCompleted(_terminal) => {
            let transition =
                reduce_and_store(tx, envelope, policy.notifications.completion_quiet_ms)?;
            if !(transition.replaced_inferred_completion && transition.terminal_applied) {
                result.outbox_changed = project_terminal_notification(
                    tx,
                    envelope,
                    policy,
                    &transition,
                    NotificationEventKind::RunCompleted,
                )?;
            }
            result.finalizer_changed = true;
        }
        AgentEventPayloadV2::RunFailed(_) => {
            let transition =
                reduce_and_store(tx, envelope, policy.notifications.completion_quiet_ms)?;
            cancel_replaced_inferred_completion(tx, envelope, &transition)?;
            result.outbox_changed = project_terminal_notification(
                tx,
                envelope,
                policy,
                &transition,
                NotificationEventKind::RunFailed,
            )?;
            result.finalizer_changed = true;
        }
        AgentEventPayloadV2::RunInterrupted(_) => {
            let transition =
                reduce_and_store(tx, envelope, policy.notifications.completion_quiet_ms)?;
            cancel_replaced_inferred_completion(tx, envelope, &transition)?;
            result.outbox_changed = project_terminal_notification(
                tx,
                envelope,
                policy,
                &transition,
                NotificationEventKind::RunInterrupted,
            )?;
            result.finalizer_changed = true;
        }
        AgentEventPayloadV2::RunCancelled(_) => {
            let transition =
                reduce_and_store(tx, envelope, policy.notifications.completion_quiet_ms)?;
            cancel_replaced_inferred_completion(tx, envelope, &transition)?;
            if transition.terminal_applied {
                cancel_start(
                    tx,
                    &transition.run.run_key,
                    envelope.occurred_at,
                    "RUN_CANCELLED",
                )?;
            }
            result.finalizer_changed = true;
        }
        AgentEventPayloadV2::OutputProduced(output) => {
            let transition =
                reduce_and_store(tx, envelope, policy.notifications.completion_quiet_ms)?;
            apply_reopen_side_effects(tx, envelope, &transition)?;
            if output_capture_allowed(tx, output.capture_generation, output.content_mode)? {
                OutputRepository::append(tx, source_key, envelope, output)?;
            }
            result.finalizer_changed = true;
        }
        AgentEventPayloadV2::AttentionRequired(attention) => {
            tx.execute(
                "INSERT INTO attention_events (
                    id, run_key, kind, safe_summary, occurred_at, expires_at, resolution
                 ) VALUES (?, ?, ?, ?, ?, ?, NULL)",
                rusqlite::params![
                    event_row_id,
                    envelope.correlation.run_key,
                    attention_kind_name(attention.attention_kind),
                    attention.safe_summary,
                    envelope.occurred_at,
                    attention
                        .expires_at
                        .or_else(|| envelope.occurred_at.checked_add(30 * 60 * 1_000)),
                ],
            )?;
            let may_notify = crate::storage::activity_projection::record_attention(
                tx,
                envelope,
                event_row_id,
                ingested_at,
            )?;
            if settings.enabled && settings.attention_enabled && may_notify {
                let run = envelope
                    .correlation
                    .run_key
                    .as_deref()
                    .map(|run_key| RunRepository::find(tx, run_key))
                    .transpose()?
                    .flatten();
                let fallback_agent_label = display_agent_kind(envelope.source.agent_kind.as_str());
                let agent_label = run.as_ref().map_or(fallback_agent_label.as_str(), |run| {
                    run.agent_label.as_str()
                });
                let payload =
                    NotificationContentBuilder::build_attention(BuildAttentionNotificationInput {
                        attention_kind: attention.attention_kind,
                        agent_label,
                        safe_summary: &attention.safe_summary,
                        occurred_at: envelope.occurred_at,
                        settings,
                    })?
                    .into_safe_payload();
                let dedupe_key = format!("attention_required:{source_key}:{}", envelope.event_id);
                let content_policy_hash = attention_notification_policy_fingerprint(settings);
                result.outbox_changed = OutboxRepository::insert_attention_tx(
                    tx,
                    AttentionNotificationInsert {
                        agent_run_key: run.as_ref().map(|run| run.run_key.as_str()),
                        dedupe_key: &dedupe_key,
                        payload: &payload,
                        content_policy_hash: &content_policy_hash,
                        created_at: envelope.occurred_at,
                        observed_at: ingested_at,
                        expires_at: attention.expires_at,
                    },
                )?;
                if result.outbox_changed {
                    crate::storage::activity_projection::notice_created(
                        tx,
                        envelope.correlation.run_key.as_deref(),
                        ingested_at,
                    )?;
                }
            }
        }
    }
    Ok(result)
}

fn display_agent_kind(agent_kind: &str) -> String {
    let mut characters = agent_kind.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => "Agent".to_owned(),
    }
}

fn output_capture_allowed(
    tx: &Transaction<'_>,
    event_generation: Option<i64>,
    event_mode: ResultContentMode,
) -> Result<bool, AppError> {
    let (mode, epoch): (String, i64) = tx.query_row(
        "SELECT capture_mode, content_epoch FROM desktop_privacy WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let current_mode = match mode.as_str() {
        "status_only" => ResultContentMode::StatusOnly,
        "redacted_excerpt" => ResultContentMode::RedactedExcerpt,
        "full_final" => ResultContentMode::FullFinal,
        _ => return Err(AppError::new("CAPTURE_POLICY_CORRUPT", "正文采集模式无效")),
    };
    Ok(event_generation == Some(epoch) && event_mode.rank() <= current_mode.rank())
}

fn reduce_and_store(
    tx: &Transaction<'_>,
    envelope: &AgentEventEnvelopeV2,
    completion_quiet_ms: i64,
) -> Result<RunTransition, AppError> {
    let run_key = envelope
        .correlation
        .run_key
        .as_deref()
        .ok_or_else(|| AppError::new("MISSING_RUN_CORRELATION", "Agent 事件缺少 Run 关联"))?;
    let transition = reduce_run_with_quiet(
        RunRepository::find(tx, run_key)?,
        envelope,
        completion_quiet_ms,
    )
    .map_err(run_state_error)?;
    RunRepository::upsert(tx, &transition.run)?;
    crate::storage::activity_projection::record_run(tx, envelope)?;
    if transition.run.status != "running" {
        crate::storage::activity_projection::cancel_unsent_attention(
            tx,
            run_key,
            envelope.observed_at,
        )?;
    }
    Ok(transition)
}

fn project_terminal_notification(
    tx: &Transaction<'_>,
    envelope: &AgentEventEnvelopeV2,
    policy: &AgentNotificationPolicy,
    transition: &RunTransition,
    kind: NotificationEventKind,
) -> Result<bool, AppError> {
    let settings = &policy.notifications;
    if !transition.terminal_applied {
        return Ok(false);
    }
    cancel_start(
        tx,
        &transition.run.run_key,
        envelope.occurred_at,
        "RUN_TERMINAL",
    )?;
    if !settings.enabled
        || !settings.notify_ended
        || envelope.payload.completion_confidence() != Some(CompletionConfidence::Authoritative)
    {
        return Ok(false);
    }
    let completed_at = terminal_at(&transition.run).unwrap_or(envelope.occurred_at);
    let dedupe_key = format!(
        "{}:{}",
        notification_kind_name(kind),
        transition.run.run_key
    );
    let payload = build_notification_payload(
        tx,
        settings,
        &transition.run,
        kind,
        policy.result_content_mode.captures_text(),
        None,
    )?;
    let content_policy_hash =
        run_notification_policy_fingerprint(settings, policy.result_content_mode.captures_text());
    OutboxRepository::insert_tx(
        tx,
        RunNotificationInsert {
            agent_run_key: &transition.run.run_key,
            kind,
            dedupe_key: &dedupe_key,
            payload: &payload,
            content_policy_hash: &content_policy_hash,
            created_at: envelope.occurred_at,
            not_before: completed_at.saturating_add(TERMINAL_NOTIFICATION_DEBOUNCE_MS),
        },
    )
}

fn attention_kind_name(kind: AttentionKind) -> &'static str {
    match kind {
        AttentionKind::Permission => "permission",
        AttentionKind::UserInput => "user_input",
        AttentionKind::Confirmation => "confirmation",
    }
}

fn cancel_start(
    tx: &Transaction<'_>,
    run_key: &str,
    now: i64,
    reason: &'static str,
) -> Result<(), AppError> {
    OutboxRepository::cancel_pending_run_started_tx(
        tx,
        run_key,
        now,
        reason,
        "Run changed state before start notification delivery",
    )?;
    Ok(())
}

fn apply_reopen_side_effects(
    tx: &Transaction<'_>,
    envelope: &AgentEventEnvelopeV2,
    transition: &RunTransition,
) -> Result<(), AppError> {
    if transition.reopened_inferred_completion {
        OutboxRepository::cancel_pending_run_completed_tx(
            tx,
            &transition.run.run_key,
            envelope.observed_at,
            "RUN_REOPENED",
            "Run resumed after inferred completion",
        )?;
    }
    Ok(())
}

fn cancel_replaced_inferred_completion(
    tx: &Transaction<'_>,
    envelope: &AgentEventEnvelopeV2,
    transition: &RunTransition,
) -> Result<(), AppError> {
    if transition.replaced_inferred_completion {
        OutboxRepository::cancel_pending_run_completed_tx(
            tx,
            &transition.run.run_key,
            envelope.observed_at,
            "RUN_TERMINAL_CORRECTED",
            "Authoritative terminal state replaced inferred completion",
        )?;
    }
    Ok(())
}

fn run_state_error(error: RunStateError) -> AppError {
    match error {
        RunStateError::MissingRunCorrelation => {
            AppError::new("MISSING_RUN_CORRELATION", "Agent 事件缺少 Run 关联")
        }
        RunStateError::IdentityConflict => AppError::new(
            "AGENT_RUN_IDENTITY_CONFLICT",
            "Run 标识已属于另一个 Agent 实例",
        ),
        RunStateError::CorrelationConflict(field) => {
            AppError::internal("AGENT_RUN_CORRELATION_CONFLICT", "Run 关联身份冲突", field)
        }
        RunStateError::UnsupportedEvent => {
            AppError::new("UNSUPPORTED_RUN_EVENT", "该 Agent 事件不能更新 Run 投影")
        }
    }
}

fn build_notification_payload(
    tx: &Transaction<'_>,
    settings: &NotificationSettings,
    run: &AgentRunRecord,
    kind: NotificationEventKind,
    _capture_agent_outputs: bool,
    start_context: Option<&crate::agent::RunStartedV2>,
) -> Result<NotificationPayloadV2, AppError> {
    let output = if !matches!(kind, NotificationEventKind::RunStarted) {
        latest_result_for_run(tx, &run.run_key)?
    } else {
        None
    };
    Ok(NotificationContentBuilder::build(BuildNotificationInput {
        event_kind: kind,
        run,
        output: output.as_ref(),
        start_context,
        usage: EventStore::latest_codex_usage(tx, &run.run_key)?.as_ref(),
        settings,
    })?
    .into_safe_payload())
}

fn latest_result_for_run(
    tx: &Transaction<'_>,
    run_key: &str,
) -> Result<Option<crate::storage::StoredResultContent>, AppError> {
    let floor = tx.query_row(
        "SELECT excerpt_floor FROM desktop_privacy WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    OutputRepository::latest_result_for_run(tx, run_key, floor)
}

fn notification_kind_name(kind: NotificationEventKind) -> &'static str {
    match kind {
        NotificationEventKind::RunCompleted => "run_completed",
        NotificationEventKind::RunFailed => "run_failed",
        NotificationEventKind::RunInterrupted => "run_interrupted",
        _ => unreachable!("terminal notification kind is fixed"),
    }
}

#[cfg(test)]
struct FixedSettings(AgentNotificationPolicy);

#[cfg(test)]
impl AgentNotificationPolicyProvider for FixedSettings {
    fn policy(&self) -> Result<AgentNotificationPolicy, AppError> {
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AgentCorrelationV2, AttentionKind, AttentionRequiredV2, OutputKind, OutputProducedV2,
        PromptOrigin, PromptSubmittedV2, RunCancelledV2, RunCompletedV2, RunFailedV2,
        RunInterruptedV2, RunSettlingV2, RunStartedV2, SettlingReason, AGENT_EVENT_SCHEMA_VERSION,
    };
    use crate::source::{AgentKind, SourceDescriptor};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn source(instance: &str) -> SourceDescriptor {
        SourceDescriptor::new(
            AgentKind::new("fixture-agent").unwrap(),
            "agent-events",
            "jsonl",
            instance,
        )
        .unwrap()
    }

    fn event(
        source: SourceDescriptor,
        event_id: &str,
        run_key: Option<&str>,
        at: i64,
        payload: AgentEventPayloadV2,
    ) -> AgentEventEnvelopeV2 {
        AgentEventEnvelopeV2 {
            metadata: None,
            schema_version: AGENT_EVENT_SCHEMA_VERSION,
            source,
            event_id: event_id.into(),
            occurred_at: at,
            observed_at: at,
            correlation: AgentCorrelationV2 {
                conversation_key: run_key.map(|_| "conversation-1".into()),
                run_key: run_key.map(str::to_owned),
                ..AgentCorrelationV2::default()
            },
            payload,
        }
    }

    fn checkpoint(envelope: &AgentEventEnvelopeV2, offset: i64) -> SourceCheckpointUpdate {
        SourceCheckpointUpdate {
            source: envelope.source.clone(),
            cursor_json: format!(r#"{{"offset":{offset}}}"#),
            source_revision: Some("fixture-revision".into()),
            status: "active".into(),
            last_error_code: None,
            updated_at: envelope.observed_at,
        }
    }

    fn enabled_settings() -> NotificationSettings {
        NotificationSettings {
            enabled: true,
            mode: NotificationMode::StartAndCompletion,
            result_content_mode: crate::model::ResultContentMode::RedactedExcerpt,
            ..NotificationSettings::default()
        }
    }

    #[test]
    fn new_attention_advances_watermark_replay_does_not_and_stop_historicalizes() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut settings = enabled_settings();
        settings.attention_enabled = true;
        let ingestor = AgentEventIngestor::with_settings(db.clone(), settings);
        let start = event(
            source("default"),
            "start",
            Some("run-watermark"),
            1,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Codex".into(),
                workspace_path: Some(r"D:\private\AstroLink".into()),
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        ingestor
            .process(start.clone(), &checkpoint(&start, 1))
            .unwrap();
        let attention = |id: &str, at| {
            event(
                source("default"),
                id,
                Some("run-watermark"),
                at,
                AgentEventPayloadV2::AttentionRequired(AttentionRequiredV2 {
                    attention_kind: AttentionKind::Permission,
                    safe_summary: "Shell / command execution".into(),
                    expires_at: None,
                }),
            )
        };
        let first = attention("permission-1", 10);
        ingestor
            .process(first.clone(), &checkpoint(&first, 2))
            .unwrap();
        let revision = scalar(&db, "SELECT activity_revision FROM run_presentation");
        ingestor
            .process(first.clone(), &checkpoint(&first, 3))
            .unwrap();
        assert_eq!(
            scalar(&db, "SELECT activity_revision FROM run_presentation"),
            revision
        );
        assert_eq!(
            scalar(&db, "SELECT attention_revision FROM run_presentation"),
            1
        );
        let second = attention("permission-2", 11);
        ingestor
            .process(second.clone(), &checkpoint(&second, 4))
            .unwrap();
        assert_eq!(
            scalar(&db, "SELECT attention_revision FROM run_presentation"),
            2
        );
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM attention_events"), 2);
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind='attention_required'"
            ),
            1
        );
        assert_eq!(
            text(&db, "SELECT workspace_label FROM run_presentation"),
            "AstroLink"
        );
        let stop = event(
            source("default"),
            "stop",
            Some("run-watermark"),
            12,
            AgentEventPayloadV2::RunSettling(RunSettlingV2 {
                reason: SettlingReason::AgentStopHook,
                codex_usage: None,
            }),
        );
        ingestor
            .process(stop.clone(), &checkpoint(&stop, 5))
            .unwrap();
        assert_eq!(scalar(&db,"SELECT COUNT(*) FROM notification_outbox WHERE event_kind='attention_required' AND status='cancelled'"),1);
        let late = attention("late", 70_000);
        ingestor
            .process(late.clone(), &checkpoint(&late, 6))
            .unwrap();
        assert_eq!(
            scalar(&db, "SELECT attention_revision FROM run_presentation"),
            3
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind='attention_required'"
            ),
            1
        );
        assert_eq!(text(&db, "SELECT status FROM agent_runs"), "settling");
        assert_eq!(text(&db, "SELECT outcome FROM agent_runs"), "unknown");
    }

    #[test]
    fn attention_revision_overflow_rolls_back_event_and_checkpoint() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(db.clone(), enabled_settings());
        let start = event(
            source("default"),
            "start",
            Some("run-overflow"),
            1,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Codex".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        ingestor
            .process(start.clone(), &checkpoint(&start, 1))
            .unwrap();
        db.with_connection(|c| {
            c.execute(
                "UPDATE run_presentation SET attention_revision=9007199254740991",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let attention = event(
            source("default"),
            "overflow",
            Some("run-overflow"),
            2,
            AgentEventPayloadV2::AttentionRequired(AttentionRequiredV2 {
                attention_kind: AttentionKind::Permission,
                safe_summary: "File access".into(),
                expires_at: None,
            }),
        );
        assert_eq!(
            ingestor
                .process(attention.clone(), &checkpoint(&attention, 2))
                .unwrap_err()
                .code,
            "ATTENTION_REVISION_OVERFLOW"
        );
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_events"), 1);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM attention_events"), 0);
        assert_eq!(
            text(&db, "SELECT cursor_json FROM source_checkpoints"),
            r#"{"offset":1}"#
        );
    }

    #[test]
    fn rejected_record_advances_the_cursor_with_degraded_health() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let rejected = event(
            source("default"),
            "rejected",
            Some("run-1"),
            10,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        let expected = checkpoint(&rejected, 42);

        ingestor
            .checkpoint_rejection("AGENT_EVENT_ID_CONFLICT", &expected)
            .unwrap();

        assert_eq!(
            db.source_cursor(&rejected.source).unwrap().as_deref(),
            Some(expected.cursor_json.as_str())
        );
        let health = db.source_health(&rejected.source).unwrap().unwrap();
        assert_eq!(health.status, "degraded");
        assert_eq!(
            health.last_error_code.as_deref(),
            Some("AGENT_EVENT_ID_CONFLICT")
        );
    }

    #[test]
    fn intentional_policy_skip_advances_the_cursor_without_degrading_source() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let skipped = event(
            source("default"),
            "skipped",
            Some("run-1"),
            10,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        let expected = checkpoint(&skipped, 43);

        ingestor.checkpoint_policy_skip(&expected).unwrap();

        assert_eq!(
            db.source_cursor(&skipped.source).unwrap().as_deref(),
            Some(expected.cursor_json.as_str())
        );
        let health = db.source_health(&skipped.source).unwrap().unwrap();
        assert_eq!(health.status, "active");
        assert_eq!(health.last_error_code, None);
    }

    fn scalar(db: &Db, sql: &str) -> i64 {
        db.with_connection(|connection| {
            connection
                .query_row(sql, [], |row| row.get(0))
                .map_err(AppError::from)
        })
        .unwrap()
    }

    fn text(db: &Db, sql: &str) -> String {
        db.with_connection(|connection| {
            connection
                .query_row(sql, [], |row| row.get(0))
                .map_err(AppError::from)
        })
        .unwrap()
    }

    #[test]
    fn all_nine_facts_project_and_checkpoint_without_stop_completion() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let descriptor = source("default");
        let events = [
            event(
                descriptor.clone(),
                "prompt",
                Some("run-main"),
                10,
                AgentEventPayloadV2::PromptSubmitted(PromptSubmittedV2 {
                    text: "private prompt".into(),
                    workspace_path: Some(r"D:\private\PromptDock".into()),
                    model_name: Some("fixture-model".into()),
                    origin: PromptOrigin::Human,
                }),
            ),
            event(
                descriptor.clone(),
                "started",
                Some("run-main"),
                11,
                AgentEventPayloadV2::RunStarted(RunStartedV2 {
                    agent_label: "Fixture Agent".into(),
                    workspace_path: Some(r"D:\private\PromptDock".into()),
                    model_name: Some("fixture-model".into()),
                    task: None,
                    reasoning_effort: None,
                }),
            ),
            event(
                descriptor.clone(),
                "output",
                Some("run-main"),
                12,
                AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                    output_kind: OutputKind::AssistantCandidate,
                    text: "private output".into(),
                    content_mode: ResultContentMode::RedactedExcerpt,
                    content_available: true,
                    source_hash: Some(crate::agent::source_hash_sha256("private output")),
                    capture_generation: Some(0),
                }),
            ),
            event(
                descriptor.clone(),
                "settling",
                Some("run-main"),
                13,
                AgentEventPayloadV2::RunSettling(RunSettlingV2 {
                    reason: SettlingReason::AgentStopHook,
                    codex_usage: None,
                }),
            ),
            event(
                descriptor.clone(),
                "completed",
                Some("run-completed"),
                14,
                AgentEventPayloadV2::RunCompleted(RunCompletedV2 {
                    completion_confidence: CompletionConfidence::Authoritative,
                }),
            ),
            event(
                descriptor.clone(),
                "failed",
                Some("run-failed"),
                15,
                AgentEventPayloadV2::RunFailed(RunFailedV2 {
                    completion_confidence: CompletionConfidence::Authoritative,
                }),
            ),
            event(
                descriptor.clone(),
                "interrupted",
                Some("run-interrupted"),
                16,
                AgentEventPayloadV2::RunInterrupted(RunInterruptedV2 {
                    completion_confidence: CompletionConfidence::Authoritative,
                }),
            ),
            event(
                descriptor.clone(),
                "cancelled",
                Some("run-cancelled"),
                17,
                AgentEventPayloadV2::RunCancelled(RunCancelledV2 {
                    completion_confidence: CompletionConfidence::Authoritative,
                }),
            ),
            event(
                descriptor,
                "attention",
                None,
                18,
                AgentEventPayloadV2::AttentionRequired(AttentionRequiredV2 {
                    attention_kind: AttentionKind::Permission,
                    safe_summary: "Permission requested".into(),
                    expires_at: Some(30),
                }),
            ),
        ];

        for (index, envelope) in events.into_iter().enumerate() {
            ingestor
                .process(envelope.clone(), &checkpoint(&envelope, index as i64 + 1))
                .unwrap();
        }

        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_events"), 9);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_runs"), 5);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_outputs"), 1);
        assert_eq!(
            text(
                &db,
                "SELECT status FROM agent_runs WHERE run_key = 'run-main'"
            ),
            "settling"
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_completed'"
            ),
            1
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'attention_required'"
            ),
            0
        );
        assert_eq!(
            text(&db, "SELECT cursor_json FROM source_checkpoints"),
            r#"{"offset":9}"#
        );
    }

    #[test]
    fn replay_only_advances_checkpoint_and_conflict_rolls_back_everything() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let descriptor = source("default");
        let first = event(
            descriptor.clone(),
            "output-1",
            Some("run-1"),
            10,
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "first".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256("first")),
                capture_generation: Some(0),
            }),
        );
        ingestor
            .process(first.clone(), &checkpoint(&first, 1))
            .unwrap();
        ingestor
            .process(first.clone(), &checkpoint(&first, 2))
            .unwrap();
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_events"), 1);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_outputs"), 1);
        assert_eq!(
            text(&db, "SELECT cursor_json FROM source_checkpoints"),
            r#"{"offset":2}"#
        );

        let conflict = event(
            descriptor,
            "output-1",
            Some("run-1"),
            10,
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "different".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256("different")),
                capture_generation: Some(0),
            }),
        );
        let error = ingestor
            .process(conflict.clone(), &checkpoint(&conflict, 3))
            .unwrap_err();
        assert_eq!(error.code, "AGENT_EVENT_ID_CONFLICT");
        assert_eq!(
            text(&db, "SELECT cursor_json FROM source_checkpoints"),
            r#"{"offset":2}"#
        );
        let stored = text(&db, "SELECT raw_text FROM agent_outputs");
        assert!(!stored.contains("first"));
        assert_eq!(
            &*crate::content_crypto::unprotect_text(
                crate::content_crypto::ContentPurpose::AgentOutput,
                &stored,
            )
            .unwrap(),
            b"first"
        );
    }

    #[test]
    fn revoked_or_stale_capture_generation_never_persists_raw_output() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        db.with_connection(|connection| {
            connection.execute(
                "UPDATE desktop_privacy SET capture_mode='status_only', content_epoch=2, excerpt_floor=2000",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let stale = event(
            source("default"),
            "output-generation-1",
            Some("run-privacy"),
            1000,
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "revoked raw output".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256("revoked raw output")),
                capture_generation: Some(1),
            }),
        );
        ingestor
            .process(stale.clone(), &checkpoint(&stale, 1))
            .unwrap();
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_outputs"), 0);
        assert_eq!(
            text(&db, "SELECT cursor_json FROM source_checkpoints"),
            r#"{"offset":1}"#
        );

        db.with_connection(|connection| {
            connection.execute(
                "UPDATE desktop_privacy SET capture_mode='redacted_excerpt', content_epoch=3, excerpt_floor=1000",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let same_millisecond_old_generation = event(
            source("default"),
            "output-generation-2",
            Some("run-privacy"),
            1000,
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "same-millisecond stale output".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256(
                    "same-millisecond stale output",
                )),
                capture_generation: Some(2),
            }),
        );
        ingestor
            .process(
                same_millisecond_old_generation.clone(),
                &checkpoint(&same_millisecond_old_generation, 2),
            )
            .unwrap();
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_outputs"), 0);
    }

    #[test]
    fn run_identity_replay_commits_but_conflict_rolls_back_event_and_checkpoint() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let mut first = event(
            source("default"),
            "output-1",
            Some("run-1"),
            10,
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "first".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256("first")),
                capture_generation: Some(0),
            }),
        );
        first.correlation.agent_id = Some("private-agent-1".into());
        first.correlation.agent_role = Some("reviewer".into());
        ingestor
            .process(first.clone(), &checkpoint(&first, 1))
            .unwrap();
        ingestor
            .process(first.clone(), &checkpoint(&first, 2))
            .unwrap();
        assert_eq!(
            text(&db, "SELECT agent_id FROM agent_runs"),
            "private-agent-1"
        );

        let mut conflict = event(
            source("default"),
            "output-2",
            Some("run-1"),
            20,
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "second".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256("second")),
                capture_generation: Some(0),
            }),
        );
        conflict.correlation.agent_id = Some("private-agent-2".into());
        conflict.correlation.agent_role = Some("reviewer".into());
        let error = ingestor
            .process(conflict.clone(), &checkpoint(&conflict, 3))
            .unwrap_err();
        assert_eq!(error.code, "AGENT_RUN_CORRELATION_CONFLICT");
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_events"), 1);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_outputs"), 1);
        assert_eq!(
            text(&db, "SELECT cursor_json FROM source_checkpoints"),
            r#"{"offset":2}"#
        );
        assert_eq!(
            text(&db, "SELECT agent_id FROM agent_runs"),
            "private-agent-1"
        );
    }

    #[test]
    fn attention_notification_is_safe_high_priority_and_replay_idempotent() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut notifications = enabled_settings();
        notifications.attention_enabled = true;
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake = Arc::clone(&wake_count);
        let ingestor = AgentEventIngestor {
            db: Arc::clone(&db),
            settings: Arc::new(FixedSettings(AgentNotificationPolicy {
                notifications,
                result_content_mode: ResultContentMode::StatusOnly,
            })),
            wake_outbox: Arc::new(move || {
                wake.fetch_add(1, Ordering::AcqRel);
            }),
            wake_finalizer: Arc::new(|| {}),
        };
        let descriptor = SourceDescriptor::new(
            crate::source::AgentKind::new("codex").unwrap(),
            "agent-events",
            "jsonl",
            "default",
        )
        .unwrap();
        let attention = event(
            descriptor,
            "permission-event",
            Some("run-not-yet-projected"),
            10,
            AgentEventPayloadV2::AttentionRequired(AttentionRequiredV2 {
                attention_kind: AttentionKind::Permission,
                safe_summary: "Shell / command execution token=private-value".into(),
                expires_at: Some(10 + 30 * 60 * 1_000),
            }),
        );

        ingestor
            .process(attention.clone(), &checkpoint(&attention, 1))
            .unwrap();
        ingestor
            .process(attention.clone(), &checkpoint(&attention, 2))
            .unwrap();

        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM notification_outbox"), 1);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM attention_events"), 1);
        assert_eq!(
            text(
                &db,
                "SELECT kind || ':' || safe_summary || ':' || run_key
                 FROM attention_events"
            ),
            "permission:Shell / command execution token=private-value:run-not-yet-projected"
        );
        assert_eq!(scalar(&db, "SELECT priority FROM notification_outbox"), 120);
        assert_eq!(
            scalar(&db, "SELECT expires_at FROM notification_outbox"),
            10 + 30 * 60 * 1_000
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE agent_run_key IS NULL
                 AND length(content_policy_hash) = 64"
            ),
            1
        );
        let payload = text(&db, "SELECT payload_json FROM notification_outbox");
        assert!(!payload.contains("private-value"));
        let payload: NotificationPayloadV2 = crate::content_crypto::unprotect_json(
            crate::content_crypto::ContentPurpose::NotificationPayload,
            &payload,
        )
        .unwrap();
        assert!(payload.body.contains("Codex 正在等待你的操作"));
        assert!(payload.body.contains("Shell / command execution"));
        assert!(payload.body.contains("[REDACTED]"));
        assert_eq!(wake_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn attention_fact_is_persisted_when_notifications_are_disabled() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor =
            AgentEventIngestor::with_settings(Arc::clone(&db), NotificationSettings::default());
        let attention = event(
            source("default"),
            "historical-attention",
            Some("run-history"),
            10,
            AgentEventPayloadV2::AttentionRequired(AttentionRequiredV2 {
                attention_kind: AttentionKind::UserInput,
                safe_summary: "Input requested".into(),
                expires_at: None,
            }),
        );

        ingestor
            .process(attention.clone(), &checkpoint(&attention, 1))
            .unwrap();
        ingestor
            .process(attention.clone(), &checkpoint(&attention, 2))
            .unwrap();

        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM attention_events"), 1);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM notification_outbox"), 0);
        assert_eq!(
            text(
                &db,
                "SELECT kind || ':' || safe_summary FROM attention_events"
            ),
            "user_input:Input requested"
        );
    }

    #[test]
    fn attention_expired_before_ingestion_is_audited_but_never_wakes_delivery() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut notifications = enabled_settings();
        notifications.attention_enabled = true;
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake = Arc::clone(&wake_count);
        let ingestor = AgentEventIngestor {
            db: Arc::clone(&db),
            settings: Arc::new(FixedSettings(AgentNotificationPolicy {
                notifications,
                result_content_mode: ResultContentMode::StatusOnly,
            })),
            wake_outbox: Arc::new(move || {
                wake.fetch_add(1, Ordering::AcqRel);
            }),
            wake_finalizer: Arc::new(|| {}),
        };
        let attention = event(
            source("default"),
            "stale-permission",
            None,
            10,
            AgentEventPayloadV2::AttentionRequired(AttentionRequiredV2 {
                attention_kind: AttentionKind::Permission,
                safe_summary: "Agent operation".into(),
                expires_at: Some(100),
            }),
        );
        let mut stale_checkpoint = checkpoint(&attention, 1);
        stale_checkpoint.updated_at = 101;

        ingestor.process(attention, &stale_checkpoint).unwrap();

        assert_eq!(
            text(&db, "SELECT status FROM notification_outbox"),
            "expired"
        );
        assert_eq!(wake_count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn delayed_replay_does_not_extend_deadline_but_new_stop_does() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut notification_settings = enabled_settings();
        notification_settings.completion_quiet_ms = crate::model::COMPLETION_QUIET_MS_MIN;
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), notification_settings);
        let started = event(
            source("default"),
            "started",
            Some("run-1"),
            10,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        ingestor
            .process(started.clone(), &checkpoint(&started, 1))
            .unwrap();
        let stop = event(
            source("default"),
            "same-stop",
            Some("run-1"),
            20,
            AgentEventPayloadV2::RunSettling(RunSettlingV2 {
                reason: SettlingReason::AgentStopHook,
                codex_usage: None,
            }),
        );
        ingestor
            .process(stop.clone(), &checkpoint(&stop, 2))
            .unwrap();
        assert_eq!(
            scalar(
                &db,
                "SELECT settle_not_before FROM agent_runs WHERE run_key = 'run-1'"
            ),
            520
        );

        let mut later = stop.clone();
        later.observed_at = 35;
        ingestor
            .process(later.clone(), &checkpoint(&later, 3))
            .unwrap();
        assert_eq!(
            scalar(
                &db,
                "SELECT settle_not_before FROM agent_runs WHERE run_key = 'run-1'"
            ),
            520
        );

        ingestor
            .process(later.clone(), &checkpoint(&later, 4))
            .unwrap();
        assert_eq!(
            scalar(
                &db,
                "SELECT settle_not_before FROM agent_runs WHERE run_key = 'run-1'"
            ),
            520
        );
        later.event_id = "new-stop".into();
        ingestor
            .process(later.clone(), &checkpoint(&later, 5))
            .unwrap();
        assert_eq!(
            scalar(
                &db,
                "SELECT settle_not_before FROM agent_runs WHERE run_key = 'run-1'"
            ),
            535
        );
    }

    #[test]
    fn mismatched_checkpoint_and_run_identity_failure_have_zero_partial_writes() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let first = event(
            source("default"),
            "started-1",
            Some("run-1"),
            10,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        let mut wrong = checkpoint(&first, 1);
        wrong.source = source("wrong");
        assert_eq!(
            ingestor.process(first.clone(), &wrong).unwrap_err().code,
            "SOURCE_IDENTITY_CONFLICT"
        );
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_events"), 0);

        ingestor
            .process(first.clone(), &checkpoint(&first, 1))
            .unwrap();
        let rebound = event(
            source("other-instance"),
            "started-2",
            Some("run-1"),
            20,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Other".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        assert_eq!(
            ingestor
                .process(rebound.clone(), &checkpoint(&rebound, 2))
                .unwrap_err()
                .code,
            "AGENT_RUN_IDENTITY_CONFLICT"
        );
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_events"), 1);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM source_streams"), 1);
        assert_eq!(
            text(&db, "SELECT cursor_json FROM source_checkpoints"),
            r#"{"offset":1}"#
        );
    }

    #[test]
    fn disabled_notifications_do_not_disable_facts_projections_or_checkpoint() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut settings = enabled_settings();
        settings.enabled = false;
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), settings);
        let started = event(
            source("default"),
            "started",
            Some("run-1"),
            10,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        ingestor
            .process(started.clone(), &checkpoint(&started, 1))
            .unwrap();
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_events"), 1);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_runs"), 1);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM notification_outbox"), 0);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM source_checkpoints"), 1);
    }

    #[test]
    fn disabled_output_capture_keeps_event_and_run_but_drops_raw_output_projection() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor {
            db: Arc::clone(&db),
            settings: Arc::new(FixedSettings(AgentNotificationPolicy {
                notifications: enabled_settings(),
                result_content_mode: ResultContentMode::StatusOnly,
            })),
            wake_outbox: Arc::new(|| {}),
            wake_finalizer: Arc::new(|| {}),
        };
        let output = event(
            source("default"),
            "output",
            Some("run-1"),
            10,
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "must not persist".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256("must not persist")),
                capture_generation: Some(0),
            }),
        );

        ingestor
            .process(output.clone(), &checkpoint(&output, 1))
            .unwrap();

        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_events"), 1);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_runs"), 1);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM agent_outputs"), 0);
        assert!(!text(&db, "SELECT metadata_json FROM agent_events").contains("must not persist"));
    }

    #[test]
    fn completed_notification_uses_safe_result_without_prompt_catalog_content() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let descriptor = source("default");
        let events = [
            event(
                descriptor.clone(),
                "prompt",
                Some("run-1"),
                10,
                AgentEventPayloadV2::PromptSubmitted(PromptSubmittedV2 {
                    text: "deploy with Authorization: Bearer task-secret".into(),
                    workspace_path: Some(r"D:\private\PromptDock".into()),
                    model_name: Some("fixture-model".into()),
                    origin: PromptOrigin::Human,
                }),
            ),
            event(
                descriptor.clone(),
                "started",
                Some("run-1"),
                11,
                AgentEventPayloadV2::RunStarted(RunStartedV2 {
                    agent_label: "Fixture Agent".into(),
                    workspace_path: Some(r"D:\private\PromptDock".into()),
                    model_name: Some("fixture-model".into()),
                    task: None,
                    reasoning_effort: None,
                }),
            ),
            event(
                descriptor.clone(),
                "output",
                Some("run-1"),
                12,
                AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                    output_kind: OutputKind::AssistantCandidate,
                    text: "done with password=[REDACTED]".into(),
                    content_mode: ResultContentMode::RedactedExcerpt,
                    content_available: true,
                    source_hash: Some(crate::agent::source_hash_sha256(
                        "done with password=[REDACTED]",
                    )),
                    capture_generation: Some(0),
                }),
            ),
            event(
                descriptor,
                "completed",
                Some("run-1"),
                13,
                AgentEventPayloadV2::RunCompleted(RunCompletedV2 {
                    completion_confidence: CompletionConfidence::Authoritative,
                }),
            ),
        ];
        for (index, envelope) in events.into_iter().enumerate() {
            ingestor
                .process(envelope.clone(), &checkpoint(&envelope, index as i64 + 1))
                .unwrap();
        }

        let payload = text(
            &db,
            "SELECT payload_json FROM notification_outbox
             WHERE event_kind = 'run_completed'",
        );
        assert!(!payload.contains("task-secret"));
        let payload: NotificationPayloadV2 = crate::content_crypto::unprotect_json(
            crate::content_crypto::ContentPurpose::NotificationPayload,
            &payload,
        )
        .unwrap();
        assert!(payload.body.contains("[REDACTED]"));
        assert!(!payload.body.contains("deploy with Authorization"));
        assert!(payload.body.contains("done with password"));
        assert!(!payload.body.contains("result-secret"));
    }

    #[test]
    fn completion_only_keeps_run_correlation_and_notify_ended_controls_terminal_outbox() {
        for (notify_ended, expected_completed) in [(true, 1), (false, 0)] {
            let db = Arc::new(Db::open_in_memory().unwrap());
            let mut settings = enabled_settings();
            settings.mode = NotificationMode::CompletionOnly;
            settings.notify_ended = notify_ended;
            let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), settings);
            let descriptor = source("default");
            let started = event(
                descriptor.clone(),
                "started",
                Some("run-1"),
                10,
                AgentEventPayloadV2::RunStarted(RunStartedV2 {
                    agent_label: "Codex".into(),
                    workspace_path: None,
                    model_name: None,
                    task: None,
                    reasoning_effort: None,
                }),
            );
            let completed = event(
                descriptor,
                "completed",
                Some("run-1"),
                20,
                AgentEventPayloadV2::RunCompleted(RunCompletedV2 {
                    completion_confidence: CompletionConfidence::Authoritative,
                }),
            );
            ingestor
                .process(started.clone(), &checkpoint(&started, 1))
                .unwrap();
            ingestor
                .process(completed.clone(), &checkpoint(&completed, 2))
                .unwrap();

            assert_eq!(text(&db, "SELECT status FROM agent_runs"), "completed");
            assert_eq!(
                scalar(
                    &db,
                    "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_started'"
                ),
                0
            );
            assert_eq!(
                scalar(
                    &db,
                    "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_completed'"
                ),
                expected_completed
            );
        }
    }

    #[test]
    fn outbox_wakes_once_after_commit_but_not_for_replay_or_rollback() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_db = Arc::clone(&db);
        let wake_count = Arc::clone(&wakes);
        let ingestor = AgentEventIngestor {
            db: Arc::clone(&db),
            settings: Arc::new(FixedSettings(AgentNotificationPolicy {
                notifications: enabled_settings(),
                result_content_mode: ResultContentMode::RedactedExcerpt,
            })),
            wake_outbox: Arc::new(move || {
                assert_eq!(
                    scalar(&wake_db, "SELECT COUNT(*) FROM source_checkpoints"),
                    1
                );
                wake_count.fetch_add(1, Ordering::SeqCst);
            }),
            wake_finalizer: Arc::new(|| {}),
        };
        let started = event(
            source("default"),
            "started",
            Some("run-1"),
            10,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        ingestor
            .process(started.clone(), &checkpoint(&started, 1))
            .unwrap();
        assert_eq!(wakes.load(Ordering::SeqCst), 1);

        ingestor
            .process(started.clone(), &checkpoint(&started, 2))
            .unwrap();
        assert_eq!(wakes.load(Ordering::SeqCst), 1);

        let conflict = event(
            source("default"),
            "started",
            Some("run-1"),
            10,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Different".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        assert_eq!(
            ingestor
                .process(conflict.clone(), &checkpoint(&conflict, 3))
                .unwrap_err()
                .code,
            "AGENT_EVENT_ID_CONFLICT"
        );
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn new_output_reopens_settling_but_never_reopens_a_terminal_run() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let descriptor = source("default");
        let started = event(
            descriptor.clone(),
            "started",
            Some("run-1"),
            10,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        let settling = event(
            descriptor.clone(),
            "settling",
            Some("run-1"),
            20,
            AgentEventPayloadV2::RunSettling(RunSettlingV2 {
                reason: SettlingReason::AgentStopHook,
                codex_usage: None,
            }),
        );
        let output = event(
            descriptor,
            "output",
            Some("run-1"),
            21,
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "continued output".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256("continued output")),
                capture_generation: Some(0),
            }),
        );
        for (offset, envelope) in [(1, &started), (2, &settling), (3, &output)] {
            ingestor
                .process(envelope.clone(), &checkpoint(envelope, offset))
                .unwrap();
        }
        assert_eq!(text(&db, "SELECT status FROM agent_runs"), "running");
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM agent_runs WHERE settling_at IS NULL"
            ),
            1
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_completed'"
            ),
            0
        );
    }

    #[test]
    fn inferred_completion_and_equal_confidence_conflicts_do_not_corrupt_projection() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let descriptor = source("default");
        let events = [
            event(
                descriptor.clone(),
                "started",
                Some("run-1"),
                10,
                AgentEventPayloadV2::RunStarted(RunStartedV2 {
                    agent_label: "Fixture".into(),
                    workspace_path: None,
                    model_name: None,
                    task: None,
                    reasoning_effort: None,
                }),
            ),
            event(
                descriptor.clone(),
                "inferred-completed",
                Some("run-1"),
                11,
                AgentEventPayloadV2::RunCompleted(RunCompletedV2 {
                    completion_confidence: CompletionConfidence::Inferred,
                }),
            ),
        ];
        for (index, envelope) in events.iter().enumerate() {
            ingestor
                .process(envelope.clone(), &checkpoint(envelope, index as i64 + 1))
                .unwrap();
        }
        assert_eq!(text(&db, "SELECT status FROM agent_runs"), "running");
        assert_eq!(
            text(
                &db,
                "SELECT status FROM notification_outbox WHERE event_kind = 'run_started'"
            ),
            "pending"
        );

        let failed = event(
            descriptor.clone(),
            "failed",
            Some("run-1"),
            12,
            AgentEventPayloadV2::RunFailed(RunFailedV2 {
                completion_confidence: CompletionConfidence::Authoritative,
            }),
        );
        let output = event(
            descriptor.clone(),
            "output-after-failure",
            Some("run-1"),
            13,
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "failure context".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256("failure context")),
                capture_generation: Some(0),
            }),
        );
        let completed = event(
            descriptor,
            "completed-conflict",
            Some("run-1"),
            14,
            AgentEventPayloadV2::RunCompleted(RunCompletedV2 {
                completion_confidence: CompletionConfidence::Authoritative,
            }),
        );
        for (offset, envelope) in [(3, &failed), (4, &output), (5, &completed)] {
            ingestor
                .process(envelope.clone(), &checkpoint(envelope, offset))
                .unwrap();
        }
        assert_eq!(text(&db, "SELECT status FROM agent_runs"), "failed");
        assert_eq!(scalar(&db, "SELECT SUM(is_final) FROM agent_outputs"), 0);
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_completed'"
            ),
            0
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_failed'"
            ),
            1
        );
    }

    #[test]
    fn late_start_enriches_terminal_projection_without_reopening_it() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let descriptor = source("default");
        let completed = event(
            descriptor.clone(),
            "completed",
            Some("run-1"),
            20,
            AgentEventPayloadV2::RunCompleted(RunCompletedV2 {
                completion_confidence: CompletionConfidence::Authoritative,
            }),
        );
        let started = event(
            descriptor,
            "started",
            Some("run-1"),
            10,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture Agent".into(),
                workspace_path: Some(r"D:\private\PromptDock".into()),
                model_name: Some("fixture-model".into()),
                task: None,
                reasoning_effort: None,
            }),
        );
        ingestor
            .process(completed.clone(), &checkpoint(&completed, 1))
            .unwrap();
        ingestor
            .process(started.clone(), &checkpoint(&started, 2))
            .unwrap();

        assert_eq!(text(&db, "SELECT status FROM agent_runs"), "completed");
        assert_eq!(
            text(&db, "SELECT agent_label FROM agent_runs"),
            "Fixture Agent"
        );
        assert_eq!(scalar(&db, "SELECT started_at FROM agent_runs"), 10);
    }

    #[test]
    fn q13_completion_only_mode_suppresses_start_notification_but_allows_stop() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let mut settings = enabled_settings();
        settings.mode = NotificationMode::CompletionOnly;
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), settings);
        let descriptor = source("default");

        let started = event(
            descriptor.clone(),
            "start-q13",
            Some("run-q13"),
            10,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
        );
        ingestor
            .process(started.clone(), &checkpoint(&started, 1))
            .unwrap();

        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_started'"
            ),
            0,
            "CompletionOnly mode must not enqueue start notifications"
        );
        assert_eq!(text(&db, "SELECT status FROM agent_runs"), "running");

        let settling = event(
            descriptor,
            "stop-q13",
            Some("run-q13"),
            20,
            AgentEventPayloadV2::RunSettling(RunSettlingV2 {
                reason: SettlingReason::AgentStopHook,
                codex_usage: None,
            }),
        );
        ingestor
            .process(settling.clone(), &checkpoint(&settling, 2))
            .unwrap();

        assert_eq!(text(&db, "SELECT status FROM agent_runs"), "settling");
    }

    #[test]
    fn q22_orphan_stop_with_unknown_classification_is_suppressed() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let ingestor = AgentEventIngestor::with_settings(Arc::clone(&db), enabled_settings());
        let descriptor = source("default");

        let mut settling = event(
            descriptor,
            "orphan-stop",
            Some("run-nonexistent"),
            10,
            AgentEventPayloadV2::RunSettling(RunSettlingV2 {
                reason: SettlingReason::AgentStopHook,
                codex_usage: None,
            }),
        );
        settling.metadata = Some(crate::agent::CaptureMetadata {
            helper_build: "test".into(),
            policy_revision: 0,
            source_classification: crate::agent::SourceClassification::Unknown,
            source_reason: "no_matching_run".into(),
        });

        let outcome = ingestor
            .process(settling.clone(), &checkpoint(&settling, 1))
            .unwrap();

        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM agent_runs WHERE run_key = 'run-nonexistent'"
            ),
            0,
            "orphan Stop must not create a run record"
        );
        assert_eq!(
            scalar(&db, "SELECT COUNT(*) FROM notification_outbox"),
            0,
            "orphan Stop must not create outbox entries"
        );
        let _ = outcome;
    }
}
