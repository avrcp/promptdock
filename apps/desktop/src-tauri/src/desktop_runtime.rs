//! One event pipeline, one policy application owner, and bounded maintenance.
use crate::desktop_capture::CapturePolicy;
use crate::desktop_policy::{self, PolicySaveReceipt, PolicySaveStatus};
use crate::notification::{outbox::OutboxRepository, runtime::NotificationRuntimeState};
use crate::relay::state::RelayRuntimeState;
use crate::{agent, db::Db, error::AppError, settings::SettingsStore};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tauri::Emitter;

struct PolicyProcessor {
    inner: agent::AgentEventIngestor,
    inbox: PathBuf,
    settings: SettingsStore,
    diagnostics: crate::diagnostics::DiagnosticRing,
}

impl crate::event_runtime::AgentEventProcessor for PolicyProcessor {
    fn process(
        &self,
        envelope: agent::AgentEventEnvelopeV2,
        checkpoint: &crate::model::SourceCheckpointUpdate,
    ) -> Result<crate::event_runtime::AgentEventProcessingOutcome, AppError> {
        let policy = desktop_policy::read_authority(&self.inbox).inspect_err(|_| {
            let _ = self.settings.begin_apply();
        })?;
        let projected = self.settings.get()?;
        if projected.notifications.policy_revision != policy.revision {
            self.settings.begin_apply()?;
            return Err(AppError::new(
                "POLICY_PENDING_APPLY",
                "新策略尚未应用，事件保留等待重试",
            ));
        }
        let metadata = envelope
            .metadata
            .as_ref()
            .ok_or_else(|| AppError::new("INVALID_AGENT_EVENT", "受管事件缺少接管元数据"))?;
        if metadata.policy_revision > policy.revision {
            return Err(AppError::new(
                "POLICY_PENDING_APPLY",
                "事件策略尚未就绪，保留等待重试",
            ));
        }
        if let agent::AgentEventPayloadV2::OutputProduced(output) = &envelope.payload {
            if output.capture_generation != Some(policy.capture_generation)
                || output.content_mode.rank() > policy.policy.result_content_mode.rank()
            {
                self.inner.checkpoint_policy_skip(checkpoint)?;
                return Ok(crate::event_runtime::AgentEventProcessingOutcome);
            }
        }
        let started = std::time::Instant::now();
        let result = self.inner.process(envelope, checkpoint);
        self.diagnostics.push(
            crate::diagnostics::Component::Capture,
            crate::diagnostics::Stage::Append,
            if result.is_ok() {
                crate::diagnostics::SafeCode::Ok
            } else {
                crate::diagnostics::SafeCode::Rejected
            },
            started.elapsed().as_millis().min(u32::MAX as u128) as u32,
            1,
        );
        result
    }

    fn checkpoint_rejection(
        &self,
        code: &'static str,
        checkpoint: &crate::model::SourceCheckpointUpdate,
    ) -> Result<(), AppError> {
        self.inner.checkpoint_rejection(code, checkpoint)
    }
}

pub(crate) struct DesktopRuntime {
    pub diagnostics: crate::diagnostics::DiagnosticRing,
    pub epoch: u64,
    pub db: Arc<Db>,
    pub settings: SettingsStore,
    pub outbox: OutboxRepository,
    pub relay: RelayRuntimeState,
    pub delivery: NotificationRuntimeState,
    pub quota: crate::codex_quota::CodexQuotaRuntime,
    events: crate::event_runtime::DurableEventRuntime,
    finalizer: agent::RunFinalizer,
    pub operation: Arc<tokio::sync::Mutex<()>>,
    maintenance: tokio_util::sync::CancellationToken,
    maintenance_task: std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl DesktopRuntime {
    pub fn start(app: tauri::AppHandle, dir: &Path) -> Result<Self, AppError> {
        let diagnostics = crate::diagnostics::DiagnosticRing::enabled();
        let epoch = crate::db::now_ms()? as u64;
        let settings = SettingsStore::new();
        let db = Arc::new(Db::open(dir)?);
        let inbox = dir.join("agent-events.jsonl");
        // Retained policy is applied before any consumer or sender starts.
        desktop_policy::initialize(&db, &settings, &inbox)?;
        let outbox = OutboxRepository::new(db.clone());
        let relay = RelayRuntimeState::start_with_delivery(dir, outbox.clone());
        let status_app = app.clone();
        relay.on_status_changed(Arc::new(move |status| {
            let _ = status_app.emit("relay-status-changed", status);
        }));
        let readiness = relay.clone();
        let ready_policy = settings.clone();
        let transport = relay.delivery_transport().ok_or_else(|| {
            AppError::new("RELAY_CLIENT_UNAVAILABLE", "Relay HTTP 客户端暂时不可用")
        })?;
        let delivery = NotificationRuntimeState::start(
            outbox.clone(),
            settings.clone(),
            transport,
            Arc::new(move || !ready_policy.pending() && readiness.notification_delivery_ready()),
        )?;
        let quota = crate::codex_quota::CodexQuotaRuntime::new();
        quota.start();
        let wake_delivery = delivery.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = wake_delivery.wake();
        });
        let finalizer =
            agent::RunFinalizer::start(db.clone(), settings.clone(), quota.clone(), wake.clone());
        let finalizer_wake = finalizer.clone();
        let processor = Arc::new(PolicyProcessor {
            inner: agent::AgentEventIngestor::new(
                db.clone(),
                settings.clone(),
                wake,
                Arc::new(move || finalizer_wake.wake()),
            ),
            inbox: inbox.clone(),
            settings: settings.clone(),
            diagnostics: diagnostics.clone(),
        });
        let activate = finalizer.clone();
        let events = crate::event_runtime::DurableEventRuntime::start(
            db.clone(),
            processor,
            app.clone(),
            inbox.clone(),
            crate::adapters::codex::hooks::source_descriptor(),
            Arc::new(move || activate.activate()),
        );
        let operation = Arc::new(tokio::sync::Mutex::new(()));
        let maintenance = tokio_util::sync::CancellationToken::new();
        let cancel = maintenance.clone();
        let background_relay = relay.clone();
        let background_delivery = delivery.clone();
        let background_outbox = outbox.clone();
        let background_db = db.clone();
        let background_settings = settings.clone();
        let background_operation = operation.clone();
        let maintenance_task = tauri::async_runtime::spawn(async move {
            let mut previous = background_outbox.history_change_token();
            let mut probe_at = tokio::time::Instant::now();
            let mut receipt_at = tokio::time::Instant::now();
            let mut prune_at = tokio::time::Instant::now();
            let mut failures = 0usize;
            let mut unchanged = 0usize;
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                if background_settings.pending() {
                    let guard = tokio::select! { _ = cancel.cancelled() => break, guard = background_operation.lock() => guard };
                    let (db, settings, delivery, inbox) = (
                        background_db.clone(),
                        background_settings.clone(),
                        background_delivery.clone(),
                        inbox.clone(),
                    );
                    // Never detach blocking persistence on cancellation. The owner awaits it.
                    match tauri::async_runtime::spawn_blocking(move || {
                        recover_policy(&db, &settings, &delivery, &inbox)
                    })
                    .await
                    {
                        Ok(Ok(())) => {
                            let _ = app.emit("promptdock://capture-status-changed", ());
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(code = error.code, "policy application will retry")
                        }
                        Err(_) => tracing::error!(
                            code = "POLICY_APPLY_WORKER",
                            "policy application worker failed"
                        ),
                    }
                    drop(guard);
                }
                if cancel.is_cancelled() {
                    break;
                }
                if tokio::time::Instant::now() >= prune_at {
                    let db = background_db.clone();
                    match tauri::async_runtime::spawn_blocking(move || {
                        crate::storage::retention::prune_batch(&db, crate::db::now_ms()?)
                    })
                    .await
                    {
                        Ok(Ok(result)) if result.changed() => {
                            let _ = app.emit("deliveries-changed", ());
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(code = error.code, "retention batch deferred")
                        }
                        Err(_) => tracing::error!(
                            code = "RETENTION_WORKER_FAILED",
                            "retention batch failed"
                        ),
                        _ => {}
                    }
                    prune_at = tokio::time::Instant::now() + Duration::from_secs(60);
                }
                if tokio::time::Instant::now() >= probe_at {
                    let success = if background_relay.status().configured {
                        tokio::select! { _ = cancel.cancelled() => break, result = background_relay.test_connection() => result.is_ok() }
                    } else {
                        false
                    };
                    let delivery = background_delivery.clone();
                    let _ = tauri::async_runtime::spawn_blocking(move || {
                        delivery.reconcile()?;
                        delivery.wake()
                    })
                    .await;
                    failures = if success {
                        0
                    } else {
                        failures.saturating_add(1)
                    };
                    let seconds = if success {
                        60
                    } else {
                        [1, 2, 5, 10, 30][failures.saturating_sub(1).min(4)]
                    };
                    probe_at = tokio::time::Instant::now() + Duration::from_secs(seconds);
                }
                let token = background_outbox.history_change_token();
                let dirty = token != previous;
                if dirty {
                    previous = token;
                    unchanged = 0;
                    let _ = app.emit("deliveries-changed", ());
                    let _ = app.emit("activity-changed", ());
                }
                let (tray_app, tray_db, tray_delivery, tray_relay) = (
                    app.clone(),
                    background_db.clone(),
                    background_delivery.clone(),
                    background_relay.clone(),
                );
                // The existing maintenance owner awaits the bounded metadata scan.
                // Native updates are further coalesced by semantic tray summary.
                if let Ok(Err(error)) = tauri::async_runtime::spawn_blocking(move || {
                    crate::desktop_tray::refresh(&tray_app, &tray_db, &tray_delivery, &tray_relay)
                })
                .await
                {
                    tracing::warn!(code = error.code, "tray summary refresh failed");
                }
                if background_relay.status().configured
                    && (dirty || tokio::time::Instant::now() >= receipt_at)
                {
                    let result = tokio::select! { _ = cancel.cancelled() => break, result = background_relay.refresh_remote_notification_statuses() => result };
                    unchanged = match result {
                        Ok(ref summary) if summary.updated > 0 => 0,
                        _ => unchanged.saturating_add(1),
                    };
                    receipt_at = tokio::time::Instant::now()
                        + Duration::from_millis(
                            [500, 1000, 2000, 5000, 10000, 30000][unchanged.min(5)],
                        );
                }
                tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(500)) => {} }
            }
        });
        Ok(Self {
            diagnostics,
            epoch,
            db,
            settings,
            outbox,
            relay,
            delivery,
            quota,
            events,
            finalizer,
            operation,
            maintenance,
            maintenance_task: std::sync::Mutex::new(Some(maintenance_task)),
        })
    }

    pub(crate) async fn save_policy(
        &self,
        inbox: &Path,
        policy: CapturePolicy,
        expected_revision: i64,
    ) -> Result<PolicySaveReceipt, AppError> {
        let _operation = self.operation.lock().await;
        let (db, settings, delivery, inbox) = (
            self.db.clone(),
            self.settings.clone(),
            self.delivery.clone(),
            inbox.to_path_buf(),
        );
        tauri::async_runtime::spawn_blocking(move || {
            save_policy_local(
                &db,
                &settings,
                &delivery,
                &inbox,
                &policy,
                expected_revision,
            )
        })
        .await
        .map_err(|_| {
            AppError::new(
                "POLICY_SAVE_WORKER",
                "策略保存工作线程失败，请刷新已保存状态",
            )
        })?
    }

    pub fn shutdown(&self) -> bool {
        tracing::info!(component = "desktop", stage = "started", "shutdown");
        self.maintenance.cancel();
        self.events.begin_shutdown();
        self.finalizer.begin_shutdown();
        self.delivery.begin_shutdown();
        self.relay.begin_shutdown();
        let mut failed = false;
        if let Some(mut task) = self
            .maintenance_task
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            tracing::info!(component = "maintenance", stage = "started", "shutdown");
            failed |= tauri::async_runtime::block_on(async {
                let result = match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
                    Ok(result) => result,
                    Err(_) => {
                        tracing::error!(
                            component = "maintenance",
                            stage = "timeout",
                            "waiting for owned persistence before releasing data guard"
                        );
                        task.await
                    }
                };
                result.is_err()
            });
            tracing::info!(component = "maintenance", stage = "joined", "shutdown");
        }
        let report: crate::event_runtime::RuntimeShutdownReport = self.events.shutdown();
        failed |= !report.completed_cleanly();
        tracing::info!(component = "finalizer", stage = "started", "shutdown");
        failed |= !self.finalizer.shutdown();
        tracing::info!(component = "finalizer", stage = "joined", "shutdown");
        tracing::info!(component = "quota", stage = "started", "shutdown");
        tauri::async_runtime::block_on(self.quota.shutdown());
        tracing::info!(component = "quota", stage = "joined", "shutdown");
        if let Err(error) = self.delivery.shutdown() {
            failed = true;
            tracing::error!(
                component = "delivery",
                code = error.code,
                stage = "failed",
                "shutdown"
            );
        }
        if failed {
            tracing::error!(
                component = "desktop",
                stage = "failed",
                "shutdown joined with component errors"
            );
        } else {
            tracing::info!(component = "desktop", stage = "completed", "shutdown");
        }
        !failed
    }
}

fn recover_policy(
    db: &Db,
    settings: &SettingsStore,
    delivery: &NotificationRuntimeState,
    inbox: &Path,
) -> Result<(), AppError> {
    let paused = delivery.pause_for_policy()?;
    settings.begin_apply()?;
    let state = desktop_policy::read_authority(inbox)?;
    desktop_policy::apply_projection(db, settings, &state)?;
    if let Err(error) = delivery.resume(paused) {
        settings.begin_apply()?;
        return Err(error);
    }
    Ok(())
}

fn save_policy_local(
    db: &Db,
    settings: &SettingsStore,
    delivery: &NotificationRuntimeState,
    inbox: &Path,
    policy: &CapturePolicy,
    expected_revision: i64,
) -> Result<PolicySaveReceipt, AppError> {
    // A later edit may not skip an unapplied revocation. Apply the committed
    // revision (including its cancellations) before accepting the next one.
    if settings.pending() {
        recover_policy(db, settings, delivery, inbox)?;
    }
    let current = match desktop_policy::read_authority(inbox) {
        Ok(state) => state,
        Err(error) => {
            settings.begin_apply()?;
            delivery.pause_for_policy()?;
            return Err(error);
        }
    };
    if current.policy == *policy && current.revision == expected_revision && !settings.pending() {
        return Ok(PolicySaveReceipt {
            status: PolicySaveStatus::Unchanged,
            state: current,
            error_code: None,
        });
    }
    let paused = delivery.pause_for_policy()?;
    settings.begin_apply()?;
    let mut receipt = match desktop_policy::commit(inbox, policy, expected_revision) {
        Ok(receipt) => receipt,
        Err(error) => {
            // A replacement can become durable even if a final platform sync fails.
            if let Ok(actual) = desktop_policy::read_authority(inbox) {
                if actual.revision > expected_revision && actual.policy == *policy {
                    return Ok(PolicySaveReceipt {
                        status: PolicySaveStatus::SavedPendingApply,
                        state: actual,
                        error_code: Some(error.code.into()),
                    });
                }
                if desktop_policy::apply_projection(db, settings, &actual).is_ok()
                    && delivery.resume(paused).is_err()
                {
                    let _ = settings.begin_apply();
                }
            }
            return Err(error);
        }
    };
    if let Err(error) = desktop_policy::apply_projection(db, settings, &receipt.state) {
        receipt.status = PolicySaveStatus::SavedPendingApply;
        receipt.error_code = Some(error.code.into());
        return Ok(receipt);
    }
    if let Err(error) = delivery.resume(paused) {
        settings.begin_apply()?;
        receipt.status = PolicySaveStatus::SavedPendingApply;
        receipt.error_code = Some(error.code.into());
    }
    Ok(receipt)
}

pub(crate) fn content_mode_name(mode: crate::model::ResultContentMode) -> &'static str {
    match mode {
        crate::model::ResultContentMode::StatusOnly => "status_only",
        crate::model::ResultContentMode::RedactedExcerpt => "redacted_excerpt",
        crate::model::ResultContentMode::FullFinal => "full_final",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AgentCorrelationV2, AgentEventEnvelopeV2, AgentEventPayloadV2, CaptureMetadata,
        RunStartedV2, SourceClassification,
    };
    use crate::event_runtime::AgentEventProcessor;
    use crate::model::SourceCheckpointUpdate;
    use crate::notification::outbox::{
        DeliveryCancellation, DeliveryResult, DeliveryTransport, OutboxClaim,
    };

    struct NoTransport;
    impl DeliveryTransport for NoTransport {
        fn deliver(&self, _: &OutboxClaim, _: &DeliveryCancellation) -> DeliveryResult {
            panic!("a paused policy may not send");
        }
    }

    fn event(revision: i64, classification: SourceClassification) -> AgentEventEnvelopeV2 {
        AgentEventEnvelopeV2 {
            schema_version: agent::AGENT_EVENT_SCHEMA_VERSION,
            source: crate::adapters::codex::hooks::source_descriptor(),
            event_id: "fixture-event".into(),
            occurred_at: 1,
            observed_at: 1,
            correlation: AgentCorrelationV2 {
                conversation_key: Some("conversation".into()),
                run_key: Some("run".into()),
                ..AgentCorrelationV2::default()
            },
            payload: AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Codex".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
            metadata: Some(CaptureMetadata {
                helper_build: "fixture".into(),
                policy_revision: revision,
                source_classification: classification,
                source_reason: "fixture".into(),
            }),
        }
    }
    fn checkpoint(event: &AgentEventEnvelopeV2) -> SourceCheckpointUpdate {
        SourceCheckpointUpdate {
            source: event.source.clone(),
            cursor_json: "{\"offset\":1}".into(),
            source_revision: None,
            status: "active".into(),
            last_error_code: None,
            updated_at: 1,
        }
    }
    fn processor(db: Arc<Db>, settings: SettingsStore, inbox: PathBuf) -> PolicyProcessor {
        PolicyProcessor {
            diagnostics: crate::diagnostics::DiagnosticRing::disabled(),
            inner: agent::AgentEventIngestor::new(
                db,
                settings.clone(),
                Arc::new(|| {}),
                Arc::new(|| {}),
            ),
            settings,
            inbox,
        }
    }
    fn counts(db: &Db) -> (i64, i64, i64) {
        db.with_connection(|conn| Ok(conn.query_row("SELECT (SELECT count(*) FROM agent_events),(SELECT count(*) FROM agent_runs),(SELECT count(*) FROM notification_outbox)",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?)).unwrap()
    }

    #[test]
    fn committed_revision_arriving_before_projection_is_retried_without_checkpoint_loss() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("agent-events.jsonl");
        let db = Arc::new(Db::open_in_memory().unwrap());
        let settings = SettingsStore::new();
        desktop_policy::initialize(&db, &settings, &inbox).unwrap();
        let policy = CapturePolicy {
            notify_started: false,
            ..CapturePolicy::default()
        };
        let saved = desktop_policy::commit(&inbox, &policy, 0).unwrap();
        let processor = processor(db.clone(), settings.clone(), inbox);
        let event = event(saved.state.revision, SourceClassification::UserTurn);
        let checkpoint = checkpoint(&event);
        assert_eq!(
            processor
                .process(event.clone(), &checkpoint)
                .unwrap_err()
                .code,
            "POLICY_PENDING_APPLY"
        );
        assert_eq!(counts(&db), (0, 0, 0));
        assert!(db.source_health(&event.source).unwrap().is_none());
        desktop_policy::apply_projection(&db, &settings, &saved.state).unwrap();
        processor.process(event, &checkpoint).unwrap();
        assert_eq!(counts(&db), (1, 1, 0));
        assert!(db.source_health(&checkpoint.source).unwrap().is_some());
    }

    #[test]
    fn verification_non_target_and_unattributed_facts_never_create_task_notifications() {
        for classification in [
            SourceClassification::Verification,
            SourceClassification::ExplicitNonTarget,
            SourceClassification::Unknown,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let inbox = dir.path().join("agent-events.jsonl");
            let db = Arc::new(Db::open_in_memory().unwrap());
            let settings = SettingsStore::new();
            desktop_policy::initialize(&db, &settings, &inbox).unwrap();
            let processor = processor(db.clone(), settings, inbox);
            let event = event(0, classification);
            processor
                .process(event.clone(), &checkpoint(&event))
                .unwrap();
            assert_eq!(counts(&db), (1, 0, 0));
            processor
                .process(event.clone(), &checkpoint(&event))
                .unwrap();
            assert_eq!(counts(&db), (1, 0, 0));
        }
    }

    #[test]
    fn real_projection_failure_returns_saved_pending_apply_and_recovers_without_resave() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("agent-events.jsonl");
        let db = Arc::new(Db::open_in_memory().unwrap());
        let settings = SettingsStore::new();
        desktop_policy::initialize(&db, &settings, &inbox).unwrap();
        let delivery = NotificationRuntimeState::start(
            OutboxRepository::new(db.clone()),
            settings.clone(),
            Arc::new(NoTransport),
            Arc::new(|| false),
        )
        .unwrap();
        db.with_connection(|conn| {conn.execute_batch("CREATE TRIGGER fixture_reject_policy BEFORE UPDATE ON desktop_privacy BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).unwrap();
        let policy = CapturePolicy {
            notify_ended: false,
            ..CapturePolicy::default()
        };
        let receipt = save_policy_local(&db, &settings, &delivery, &inbox, &policy, 0).unwrap();
        assert_eq!(receipt.status, PolicySaveStatus::SavedPendingApply);
        assert_eq!(receipt.state.revision, 1);
        assert!(settings.pending());
        assert_eq!(
            desktop_policy::read_authority(&inbox).unwrap().policy,
            policy
        );
        db.with_connection(|conn| {
            conn.execute_batch("DROP TRIGGER fixture_reject_policy;")?;
            Ok(())
        })
        .unwrap();
        recover_policy(&db, &settings, &delivery, &inbox).unwrap();
        assert!(!settings.pending());
        assert!(!settings.get().unwrap().notifications.notify_ended);
        assert_eq!(settings.get().unwrap().notifications.policy_revision, 1);
        delivery.shutdown().unwrap();
    }

    #[test]
    fn a_second_save_cannot_skip_an_unapplied_selection_or_task_revocation() {
        for task_input in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let inbox = dir.path().join("agent-events.jsonl");
            let db = Arc::new(Db::open_in_memory().unwrap());
            let settings = SettingsStore::new();
            desktop_policy::initialize(&db, &settings, &inbox).unwrap();
            let original = CapturePolicy {
                include_task_input: task_input,
                ..CapturePolicy::default()
            };
            let first = desktop_policy::commit(&inbox, &original, 0).unwrap();
            desktop_policy::apply_projection(&db, &settings, &first.state).unwrap();
            let captured = event(first.state.revision, SourceClassification::UserTurn);
            processor(db.clone(), settings.clone(), inbox.clone())
                .process(captured.clone(), &checkpoint(&captured))
                .unwrap();
            assert_eq!(counts(&db).2, 1);
            let delivery = NotificationRuntimeState::start(
                OutboxRepository::new(db.clone()),
                settings.clone(),
                Arc::new(NoTransport),
                Arc::new(|| false),
            )
            .unwrap();
            db.with_connection(|conn| {conn.execute_batch("CREATE TRIGGER fixture_reject_policy BEFORE UPDATE ON desktop_privacy BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).unwrap();
            let mut revoked = original.clone();
            if task_input {
                revoked.include_task_input = false;
            } else {
                revoked.notify_started = false;
            }
            let pending = save_policy_local(
                &db,
                &settings,
                &delivery,
                &inbox,
                &revoked,
                first.state.revision,
            )
            .unwrap();
            assert_eq!(pending.status, PolicySaveStatus::SavedPendingApply);
            assert!(save_policy_local(
                &db,
                &settings,
                &delivery,
                &inbox,
                &original,
                pending.state.revision
            )
            .is_err());
            assert_eq!(
                desktop_policy::read_authority(&inbox).unwrap(),
                pending.state
            );
            assert!(settings.pending());
            db.with_connection(|conn| {
                conn.execute_batch("DROP TRIGGER fixture_reject_policy;")?;
                Ok(())
            })
            .unwrap();
            let restored = save_policy_local(
                &db,
                &settings,
                &delivery,
                &inbox,
                &original,
                pending.state.revision,
            )
            .unwrap();
            assert_eq!(restored.status, PolicySaveStatus::Saved);
            assert!(!settings.pending());
            let status = db
                .with_connection(|conn| {
                    Ok(
                        conn.query_row("SELECT status FROM notification_outbox", [], |row| {
                            row.get::<_, String>(0)
                        })?,
                    )
                })
                .unwrap();
            assert_eq!(status, "cancelled");
            delivery.shutdown().unwrap();
        }
    }
}
