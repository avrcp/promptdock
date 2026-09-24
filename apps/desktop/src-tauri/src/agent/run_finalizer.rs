use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::db::{now_ms, Db};
use crate::error::AppError;
use crate::model::NotificationSettings;
use crate::notification::content_builder::{BuildNotificationInput, NotificationContentBuilder};
use crate::notification::outbox::{
    NotificationEventKind, NotificationPayloadV2, OutboxRepository, RunNotificationInsert,
};
use crate::notification::run_notification_policy_fingerprint;
use crate::settings::SettingsStore;
use crate::storage::RunRepository;

use super::run_reducer::finalize_due_run;

const FINALIZER_BATCH_SIZE: u16 = 100;
const FINALIZER_IDLE_RECHECK: Duration = Duration::from_secs(60);
const FINALIZER_MIN_ERROR_RETRY: Duration = Duration::from_secs(1);
const FINALIZER_MAX_ERROR_RETRY: Duration = Duration::from_secs(30);
const PHASE_RUNNING: u8 = 0;
const PHASE_STOPPING: u8 = 1;
const PHASE_STOPPED: u8 = 2;

trait FinalizerClock: Send + Sync + 'static {
    fn now_ms(&self) -> Result<i64, AppError>;
}

struct SystemClock;

impl FinalizerClock for SystemClock {
    fn now_ms(&self) -> Result<i64, AppError> {
        now_ms()
    }
}

#[derive(Clone)]
struct FinalizerPolicy {
    notifications: NotificationSettings,
    result_content_mode: crate::model::ResultContentMode,
}

trait FinalizerSettingsProvider: Send + Sync + 'static {
    fn policy(&self) -> Result<FinalizerPolicy, AppError>;
}

impl FinalizerSettingsProvider for SettingsStore {
    fn policy(&self) -> Result<FinalizerPolicy, AppError> {
        let snapshot = self.get()?;
        Ok(FinalizerPolicy {
            notifications: snapshot.notifications,
            result_content_mode: snapshot.capture.result_content_mode,
        })
    }
}

#[derive(Default)]
struct FinalizerSignal {
    state: Mutex<FinalizerSignalState>,
    changed: Condvar,
}

#[derive(Default)]
struct FinalizerSignalState {
    active: bool,
    cancelled: bool,
    generation: u64,
}

impl FinalizerSignal {
    fn activate(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.cancelled {
            state.active = true;
            state.generation = state.generation.wrapping_add(1);
            self.changed.notify_all();
        }
    }

    fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.cancelled {
            state.cancelled = true;
            state.generation = state.generation.wrapping_add(1);
        }
        self.changed.notify_all();
    }

    fn wait_until_active(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while !state.active && !state.cancelled {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        state.active && !state.cancelled
    }

    fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
    }

    /// Waits only when the observed generation is still current. The
    /// generation check and entering the condvar wait share the state mutex,
    /// so a wake between a database scan and this call cannot be lost.
    fn wait(&self, duration: Duration, observed_generation: u64) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.cancelled {
            return false;
        }
        if state.generation != observed_generation {
            return true;
        }
        let (state, _) = self
            .changed
            .wait_timeout(state, duration)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !state.cancelled
    }

    fn wake(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }
}

struct RunFinalizerInner {
    signal: Arc<FinalizerSignal>,
    worker: Mutex<Option<JoinHandle<()>>>,
    phase: AtomicU8,
}

#[derive(Clone)]
pub(crate) struct RunFinalizer(Arc<RunFinalizerInner>);

impl RunFinalizer {
    pub(crate) fn start(
        db: Arc<Db>,
        settings: SettingsStore,
        quota: crate::codex_quota::CodexQuotaRuntime,
        wake_outbox: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self::start_with_components(
            db,
            Arc::new(settings),
            Arc::new(SystemClock),
            quota,
            wake_outbox,
        )
    }

    fn start_with_components(
        db: Arc<Db>,
        settings: Arc<dyn FinalizerSettingsProvider>,
        clock: Arc<dyn FinalizerClock>,
        quota: crate::codex_quota::CodexQuotaRuntime,
        wake_outbox: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let signal = Arc::new(FinalizerSignal::default());
        let worker_signal = Arc::clone(&signal);
        let worker = thread::spawn(move || {
            if !worker_signal.wait_until_active() {
                return;
            }
            let mut error_retry = FINALIZER_MIN_ERROR_RETRY;
            loop {
                let observed_generation = worker_signal.generation();
                let now = match clock.now_ms() {
                    Ok(now) => now,
                    Err(error) => {
                        tracing::warn!(code = error.code, "run finalizer clock failed");
                        if !worker_signal.wait(error_retry, observed_generation) {
                            return;
                        }
                        error_retry = double_capped(error_retry, FINALIZER_MAX_ERROR_RETRY);
                        continue;
                    }
                };
                let policy = match settings.policy() {
                    Ok(policy) if policy.notifications.validate() => policy,
                    Ok(_) => {
                        tracing::warn!(
                            code = "INVALID_NOTIFICATION_SETTINGS",
                            "run finalizer is waiting for valid notification settings"
                        );
                        if !worker_signal.wait(error_retry, observed_generation) {
                            return;
                        }
                        error_retry = double_capped(error_retry, FINALIZER_MAX_ERROR_RETRY);
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(code = error.code, "run finalizer settings read failed");
                        if !worker_signal.wait(error_retry, observed_generation) {
                            return;
                        }
                        error_retry = double_capped(error_retry, FINALIZER_MAX_ERROR_RETRY);
                        continue;
                    }
                };

                match finalize_batch(&db, &policy, &quota, now) {
                    Ok(outcome) => {
                        error_retry = FINALIZER_MIN_ERROR_RETRY;
                        if outcome.outbox_changed {
                            wake_outbox();
                        }
                        if outcome.finalized == usize::from(FINALIZER_BATCH_SIZE) {
                            continue;
                        }
                        let wait = outcome
                            .next_deadline
                            .map(|deadline| duration_until(clock.now_ms().unwrap_or(now), deadline))
                            .unwrap_or(FINALIZER_IDLE_RECHECK);
                        if !worker_signal.wait(wait, observed_generation) {
                            return;
                        }
                    }
                    Err(error) => {
                        tracing::error!(code = error.code, "run finalizer transaction failed");
                        if !worker_signal.wait(error_retry, observed_generation) {
                            return;
                        }
                        error_retry = double_capped(error_retry, FINALIZER_MAX_ERROR_RETRY);
                    }
                }
            }
        });
        Self(Arc::new(RunFinalizerInner {
            signal,
            worker: Mutex::new(Some(worker)),
            phase: AtomicU8::new(PHASE_RUNNING),
        }))
    }

    /// Enables database scans only after the durable inbox has reached EOF.
    pub(crate) fn activate(&self) {
        if self.0.phase.load(Ordering::Acquire) == PHASE_RUNNING {
            self.0.signal.activate();
        }
    }

    pub(crate) fn wake(&self) {
        self.0.signal.wake();
    }

    pub(crate) fn begin_shutdown(&self) -> bool {
        if self
            .0
            .phase
            .compare_exchange(
                PHASE_RUNNING,
                PHASE_STOPPING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.0.signal.cancel();
            true
        } else {
            false
        }
    }

    pub(crate) fn shutdown(&self) -> bool {
        self.begin_shutdown();
        let worker = self
            .0
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let panicked = worker.is_some_and(|worker| worker.join().is_err());
        if panicked {
            tracing::error!("run finalizer task panicked during shutdown");
        }
        self.0.phase.store(PHASE_STOPPED, Ordering::Release);
        !panicked
    }
}

struct FinalizeBatchOutcome {
    finalized: usize,
    next_deadline: Option<i64>,
    outbox_changed: bool,
}

fn finalize_batch(
    db: &Db,
    policy: &FinalizerPolicy,
    quota: &crate::codex_quota::CodexQuotaRuntime,
    now: i64,
) -> Result<FinalizeBatchOutcome, AppError> {
    db.with_transaction(|tx| {
        let due = RunRepository::find_due_settling(tx, now, FINALIZER_BATCH_SIZE)?;
        crate::desktop_policy::require_applied(tx, policy.notifications.policy_revision)?;
        let mut finalized = 0;
        let mut outbox_changed = false;
        for current in due {
            let Some(run) = finalize_due_run(current, now) else {
                continue;
            };
            RunRepository::upsert(tx, &run)?;
            crate::storage::activity_projection::advance(tx, &run.run_key, now)?;
            crate::storage::activity_projection::cancel_unsent_attention(tx, &run.run_key, now)?;
            OutboxRepository::cancel_pending_run_started_tx(
                tx,
                &run.run_key,
                now,
                "RUN_FINALIZED",
                "Run completed before start notification delivery",
            )?;
            if policy.notifications.enabled && policy.notifications.notify_ended {
                let dedupe_key = format!(
                    "run_completed:{}:settle:{}",
                    run.run_key, run.settle_generation
                );
                let payload = build_notification_payload(tx, policy, quota, &run)?;
                let content_policy_hash = run_notification_policy_fingerprint(
                    &policy.notifications,
                    policy.result_content_mode.captures_text(),
                );
                outbox_changed |= OutboxRepository::insert_tx(
                    tx,
                    RunNotificationInsert {
                        agent_run_key: &run.run_key,
                        kind: NotificationEventKind::RunCompleted,
                        dedupe_key: &dedupe_key,
                        payload: &payload,
                        content_policy_hash: &content_policy_hash,
                        created_at: now,
                        not_before: now,
                    },
                )?;
            }
            finalized += 1;
        }
        Ok(FinalizeBatchOutcome {
            finalized,
            next_deadline: RunRepository::next_settle_deadline(tx)?,
            outbox_changed,
        })
    })
}

fn build_notification_payload(
    tx: &rusqlite::Transaction<'_>,
    policy: &FinalizerPolicy,
    quota: &crate::codex_quota::CodexQuotaRuntime,
    run: &crate::storage::AgentRunRecord,
) -> Result<NotificationPayloadV2, AppError> {
    let output = latest_result_for_run(tx, &run.run_key)?;
    let usage = quota.snapshot().and_then(|snapshot| {
        snapshot
            .primary
            .map(|window| crate::agent::CodexUsageSnapshot {
                used_percent: Some(window.used_percent),
                window_duration_mins: Some(window.window_duration_mins),
                resets_at: Some(window.resets_at),
            })
    });
    Ok(NotificationContentBuilder::build(BuildNotificationInput {
        event_kind: NotificationEventKind::RunCompleted,
        run,
        output: output.as_ref(),
        start_context: None,
        usage: usage.as_ref(),
        settings: &policy.notifications,
    })?
    .into_safe_payload())
}

/// A Stop-derived assistant candidate is eligible as a result excerpt after
/// the quiet window, but remains a candidate in storage. The notification copy
/// must not promote it to an authoritative/final output fact.
fn latest_result_for_run(
    tx: &rusqlite::Transaction<'_>,
    run_key: &str,
) -> Result<Option<crate::storage::StoredResultContent>, AppError> {
    let floor = tx.query_row(
        "SELECT excerpt_floor FROM desktop_privacy WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    crate::storage::OutputRepository::latest_result_for_run(tx, run_key, floor)
}

fn duration_until(now: i64, deadline: i64) -> Duration {
    let millis = deadline.saturating_sub(now).max(0) as u64;
    Duration::from_millis(millis).min(FINALIZER_IDLE_RECHECK)
}

fn double_capped(current: Duration, maximum: Duration) -> Duration {
    current.saturating_mul(2).min(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AgentCorrelationV2, AgentEventEnvelopeV2, AgentEventPayloadV2, OutputKind,
        OutputProducedV2, AGENT_EVENT_SCHEMA_VERSION,
    };
    use crate::source::{AgentKind, SourceDescriptor};
    use crate::storage::{AgentRunRecord, OutputRepository, SourceRepository};
    use std::sync::atomic::{AtomicI64, AtomicUsize};
    use std::sync::Barrier;
    use std::time::Instant;

    struct FixedSettings(FinalizerPolicy);

    impl FinalizerSettingsProvider for FixedSettings {
        fn policy(&self) -> Result<FinalizerPolicy, AppError> {
            Ok(self.0.clone())
        }
    }

    struct FakeClock(AtomicI64);

    impl FinalizerClock for FakeClock {
        fn now_ms(&self) -> Result<i64, AppError> {
            Ok(self.0.load(Ordering::Acquire))
        }
    }

    fn settings(enabled: bool) -> FinalizerPolicy {
        FinalizerPolicy {
            notifications: NotificationSettings {
                enabled,
                result_content_mode: crate::model::ResultContentMode::RedactedExcerpt,
                ..NotificationSettings::default()
            },
            result_content_mode: crate::model::ResultContentMode::RedactedExcerpt,
        }
    }

    fn settling_run(deadline: i64) -> AgentRunRecord {
        AgentRunRecord {
            run_key: "run-1".into(),
            agent_kind: "fixture-agent".into(),
            instance_id: "default".into(),
            conversation_key: Some("conversation-1".into()),
            parent_run_key: None,
            agent_id: None,
            agent_role: None,
            agent_label: "Fixture Agent".into(),
            status: "settling".into(),
            outcome: "unknown".into(),
            completion_confidence: "provisional".into(),
            started_at: Some(10),
            settling_at: Some(80),
            settle_not_before: Some(deadline),
            settle_generation: 1,
            completed_at: None,
            failed_at: None,
            interrupted_at: None,
            cancelled_at: None,
            last_event_at: 90,
        }
    }

    fn insert_candidate(db: &Db) {
        db.with_transaction(|tx| {
            let source = SourceDescriptor::new(
                AgentKind::new("fixture-agent").unwrap(),
                "events",
                "jsonl",
                "default",
            )
            .unwrap();
            let source_key = SourceRepository::ensure(tx, &source, 90)?;
            let envelope = AgentEventEnvelopeV2 {
                metadata: None,
                schema_version: AGENT_EVENT_SCHEMA_VERSION,
                source,
                event_id: "output-1".into(),
                occurred_at: 90,
                observed_at: 90,
                correlation: AgentCorrelationV2 {
                    run_key: Some("run-1".into()),
                    ..AgentCorrelationV2::default()
                },
                payload: AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                    output_kind: OutputKind::AssistantCandidate,
                    text: "private result".into(),
                    content_mode: crate::model::ResultContentMode::RedactedExcerpt,
                    content_available: true,
                    source_hash: Some(crate::agent::source_hash_sha256("private result")),
                    capture_generation: Some(0),
                }),
            };
            OutputRepository::append(
                tx,
                &source_key,
                &envelope,
                match &envelope.payload {
                    AgentEventPayloadV2::OutputProduced(output) => output,
                    _ => unreachable!(),
                },
            )?;
            Ok(())
        })
        .unwrap();
    }

    fn insert_run(db: &Db, deadline: i64) {
        db.with_transaction(|tx| RunRepository::upsert(tx, &settling_run(deadline)))
            .unwrap();
    }

    fn test_quota_runtime() -> crate::codex_quota::CodexQuotaRuntime {
        crate::codex_quota::CodexQuotaRuntime::new()
    }

    #[test]
    fn finalizes_only_at_the_deadline_without_promoting_the_result_candidate() {
        let db = Db::open_in_memory().unwrap();
        insert_run(&db, 100);
        insert_candidate(&db);
        let quota = test_quota_runtime();

        assert_eq!(
            finalize_batch(&db, &settings(true), &quota, 99)
                .unwrap()
                .finalized,
            0
        );
        let outcome = finalize_batch(&db, &settings(true), &quota, 100).unwrap();
        assert_eq!(outcome.finalized, 1);
        assert!(outcome.outbox_changed);

        db.with_connection(|connection| {
            let projection = connection.query_row(
                "SELECT status, outcome, completion_confidence, completed_at
                 FROM agent_runs WHERE run_key = 'run-1'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )?;
            assert_eq!(
                projection,
                ("completed".into(), "unknown".into(), "inferred".into(), 90)
            );
            let final_count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM agent_outputs WHERE is_final = 1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(final_count, 0);
            let outbox_count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_completed'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(outbox_count, 1);
            let payload: String = connection.query_row(
                "SELECT payload_json FROM notification_outbox WHERE event_kind = 'run_completed'",
                [],
                |row| row.get(0),
            )?;
            assert!(!payload.contains("private result"));
            let payload: NotificationPayloadV2 = crate::content_crypto::unprotect_json(
                crate::content_crypto::ContentPurpose::NotificationPayload,
                &payload,
            )?;
            assert!(payload.body.contains("private result"));
            assert_eq!(payload.schema_version, 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn notify_ended_false_still_finalizes_without_creating_an_outbox_item() {
        let db = Db::open_in_memory().unwrap();
        insert_run(&db, 100);
        insert_candidate(&db);
        let mut policy = settings(true);
        policy.notifications.notify_ended = false;
        let quota = test_quota_runtime();

        let outcome = finalize_batch(&db, &policy, &quota, 100).unwrap();

        assert_eq!(outcome.finalized, 1);
        assert!(!outcome.outbox_changed);
        db.with_connection(|connection| {
            let status: String = connection.query_row(
                "SELECT status FROM agent_runs WHERE run_key = 'run-1'",
                [],
                |row| row.get(0),
            )?;
            let outbox_count: i64 =
                connection.query_row("SELECT COUNT(*) FROM notification_outbox", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(status, "completed");
            assert_eq!(outbox_count, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn finalizer_enqueues_only_for_relay_with_enabled_result_excerpt() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        insert_run(&db, 100);
        insert_candidate(&db);
        let quota = test_quota_runtime();

        assert_eq!(
            finalize_batch(&db, &settings(true), &quota, 100)
                .unwrap()
                .finalized,
            1
        );
        let (backend, payload): (String, String) = db
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT delivery_backend, payload_json FROM notification_outbox",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(backend, "relay");
        assert!(!payload.contains("private result"));
        let payload: NotificationPayloadV2 = crate::content_crypto::unprotect_json(
            crate::content_crypto::ContentPurpose::NotificationPayload,
            &payload,
        )
        .unwrap();
        assert!(payload.body.contains("private result"));
    }

    #[test]
    fn wake_between_scan_and_wait_is_not_lost() {
        let signal = Arc::new(FinalizerSignal::default());
        signal.activate();
        let scan_complete = Arc::new(Barrier::new(2));
        let wake_complete = Arc::new(Barrier::new(2));
        let worker_signal = Arc::clone(&signal);
        let worker_scan_complete = Arc::clone(&scan_complete);
        let worker_wake_complete = Arc::clone(&wake_complete);
        let worker = thread::spawn(move || {
            let observed_generation = worker_signal.generation();
            worker_scan_complete.wait();
            worker_wake_complete.wait();
            worker_signal.wait(Duration::from_secs(30), observed_generation)
        });

        scan_complete.wait();
        signal.wake();
        wake_complete.wait();

        assert!(worker.join().unwrap());
    }

    #[test]
    fn cancellation_wins_over_activation_and_interrupts_wait() {
        let signal = Arc::new(FinalizerSignal::default());
        signal.cancel();
        signal.activate();

        assert!(!signal.wait_until_active());
        let observed_generation = signal.generation();
        assert!(!signal.wait(Duration::from_secs(30), observed_generation));
    }

    #[test]
    fn concurrent_wakes_advance_generation_without_losing_notification() {
        const WAKE_THREADS: usize = 8;
        let signal = Arc::new(FinalizerSignal::default());
        signal.activate();
        let observed_generation = signal.generation();
        let start = Arc::new(Barrier::new(WAKE_THREADS + 1));
        let mut workers = Vec::with_capacity(WAKE_THREADS);
        for _ in 0..WAKE_THREADS {
            let worker_signal = Arc::clone(&signal);
            let worker_start = Arc::clone(&start);
            workers.push(thread::spawn(move || {
                worker_start.wait();
                worker_signal.wake();
            }));
        }
        start.wait();
        for worker in workers {
            worker.join().unwrap();
        }

        assert_eq!(
            signal.generation(),
            observed_generation.wrapping_add(WAKE_THREADS as u64)
        );
        assert!(signal.wait(Duration::from_secs(30), observed_generation));
    }

    #[test]
    fn paused_worker_waits_for_inbox_catch_up_before_restart_recovery() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        insert_run(&db, 100);
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake_probe = Arc::clone(&wake_count);
        let finalizer = RunFinalizer::start_with_components(
            Arc::clone(&db),
            Arc::new(FixedSettings(settings(false))),
            Arc::new(FakeClock(AtomicI64::new(100))),
            test_quota_runtime(),
            Arc::new(move || {
                wake_probe.fetch_add(1, Ordering::AcqRel);
            }),
        );

        thread::sleep(Duration::from_millis(30));
        let status_before = db
            .with_transaction(|tx| RunRepository::find(tx, "run-1"))
            .unwrap()
            .unwrap()
            .status;
        assert_eq!(status_before, "settling");

        finalizer.activate();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status = db
                .with_transaction(|tx| RunRepository::find(tx, "run-1"))
                .unwrap()
                .unwrap()
                .status;
            if status == "completed" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "finalizer did not recover due run"
            );
            thread::sleep(Duration::from_millis(10));
        }
        finalizer.shutdown();
        assert_eq!(wake_count.load(Ordering::Acquire), 0);
    }

    fn insert_settling_event_no_usage(db: &Db, run_key: &str, event_id: &str) {
        db.with_transaction(|tx| {
            let source = SourceDescriptor::new(
                AgentKind::new("fixture-agent").unwrap(),
                "events",
                "jsonl",
                "default",
            )
            .unwrap();
            let _source_key = SourceRepository::ensure(tx, &source, 90)?;
            let envelope = AgentEventEnvelopeV2 {
                metadata: None,
                schema_version: AGENT_EVENT_SCHEMA_VERSION,
                source,
                event_id: event_id.into(),
                occurred_at: 90,
                observed_at: 90,
                correlation: AgentCorrelationV2 {
                    run_key: Some(run_key.into()),
                    ..AgentCorrelationV2::default()
                },
                payload: AgentEventPayloadV2::RunSettling(crate::agent::RunSettlingV2 {
                    reason: crate::agent::SettlingReason::AgentStopHook,
                    codex_usage: None,
                }),
            };
            crate::storage::EventStore::append(tx, &envelope)?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q09_finalizer_produces_outbox_when_settling_had_no_codex_usage() {
        let db = Db::open_in_memory().unwrap();
        insert_run(&db, 100);
        insert_settling_event_no_usage(&db, "run-1", "stop-no-usage");
        let quota = test_quota_runtime();

        let outcome = finalize_batch(&db, &settings(true), &quota, 100).unwrap();
        assert_eq!(outcome.finalized, 1);
        assert!(outcome.outbox_changed);

        db.with_connection(|connection| {
            let status: String = connection.query_row(
                "SELECT status FROM agent_runs WHERE run_key = 'run-1'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(status, "completed");
            let outbox_count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_completed'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(outbox_count, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q12_finalizer_produces_outbox_without_any_output() {
        let db = Db::open_in_memory().unwrap();
        insert_run(&db, 100);
        insert_settling_event_no_usage(&db, "run-1", "stop-no-output");
        let quota = test_quota_runtime();

        let outcome = finalize_batch(&db, &settings(true), &quota, 100).unwrap();
        assert_eq!(outcome.finalized, 1);
        assert!(outcome.outbox_changed);

        db.with_connection(|connection| {
            let outbox_count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_completed'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(outbox_count, 1);
            let payload: String = connection.query_row(
                "SELECT payload_json FROM notification_outbox WHERE event_kind = 'run_completed'",
                [],
                |row| row.get(0),
            )?;
            let payload: NotificationPayloadV2 = crate::content_crypto::unprotect_json(
                crate::content_crypto::ContentPurpose::NotificationPayload,
                &payload,
            )?;
            assert!(!payload.body.is_empty());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q14_two_parallel_runs_without_usage_both_finalize_without_batch_rollback() {
        let db = Db::open_in_memory().unwrap();
        insert_run(&db, 100);
        insert_settling_event_no_usage(&db, "run-1", "stop-run-1");

        let mut run_2 = settling_run(100);
        run_2.run_key = "run-2".into();
        run_2.conversation_key = Some("conversation-2".into());
        db.with_transaction(|tx| RunRepository::upsert(tx, &run_2))
            .unwrap();
        insert_settling_event_no_usage(&db, "run-2", "stop-run-2");
        let quota = test_quota_runtime();

        let outcome = finalize_batch(&db, &settings(true), &quota, 100).unwrap();
        assert_eq!(outcome.finalized, 2);
        assert!(outcome.outbox_changed);

        db.with_connection(|connection| {
            let outbox_count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_completed'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(outbox_count, 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q10_redacted_excerpt_payload_carries_content_mode_and_revision() {
        let db = Db::open_in_memory().unwrap();
        insert_run(&db, 100);
        insert_candidate(&db);
        insert_settling_event_no_usage(&db, "run-1", "stop-run-1");
        let quota = test_quota_runtime();

        let outcome = finalize_batch(&db, &settings(true), &quota, 100).unwrap();
        assert_eq!(outcome.finalized, 1);

        db.with_connection(|connection| {
            let payload_json: String = connection.query_row(
                "SELECT payload_json FROM notification_outbox WHERE event_kind = 'run_completed'",
                [],
                |row| row.get(0),
            )?;
            let payload: NotificationPayloadV2 = crate::content_crypto::unprotect_json(
                crate::content_crypto::ContentPurpose::NotificationPayload,
                &payload_json,
            )?;
            assert_eq!(
                payload.content_mode,
                crate::model::ResultContentMode::RedactedExcerpt
            );
            assert!(payload.result_revision.is_some());
            assert!(payload.body.contains("private result"));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q15_replaying_finalize_batch_does_not_create_duplicate_outbox() {
        let db = Db::open_in_memory().unwrap();
        insert_run(&db, 100);
        insert_settling_event_no_usage(&db, "run-1", "stop-run-1");
        let quota = test_quota_runtime();

        let first = finalize_batch(&db, &settings(true), &quota, 100).unwrap();
        assert_eq!(first.finalized, 1);
        assert!(first.outbox_changed);

        let second = finalize_batch(&db, &settings(true), &quota, 100).unwrap();
        assert_eq!(second.finalized, 0);
        assert!(!second.outbox_changed);

        db.with_connection(|connection| {
            let outbox_count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_completed' AND agent_run_key = 'run-1'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(outbox_count, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q11_full_final_payload_carries_full_content_mode_and_complete_text() {
        let db = Db::open_in_memory().unwrap();
        insert_run(&db, 100);
        db.with_transaction(|tx| {
            let source = SourceDescriptor::new(
                AgentKind::new("fixture-agent").unwrap(),
                "events",
                "jsonl",
                "default",
            )
            .unwrap();
            let source_key = SourceRepository::ensure(tx, &source, 90)?;
            let envelope = AgentEventEnvelopeV2 {
                metadata: None,
                schema_version: AGENT_EVENT_SCHEMA_VERSION,
                source,
                event_id: "full-final-output".into(),
                occurred_at: 90,
                observed_at: 90,
                correlation: AgentCorrelationV2 {
                    run_key: Some("run-1".into()),
                    ..AgentCorrelationV2::default()
                },
                payload: AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                    output_kind: OutputKind::AssistantCandidate,
                    text: "完整原文：包含中文、emoji 🎉、代码 `fn main()` 和路径 C:\\Users".into(),
                    content_mode: crate::model::ResultContentMode::FullFinal,
                    content_available: true,
                    source_hash: Some(crate::agent::source_hash_sha256(
                        "完整原文：包含中文、emoji 🎉、代码 `fn main()` 和路径 C:\\Users",
                    )),
                    capture_generation: Some(0),
                }),
            };
            OutputRepository::append(
                tx,
                &source_key,
                &envelope,
                match &envelope.payload {
                    AgentEventPayloadV2::OutputProduced(output) => output,
                    _ => unreachable!(),
                },
            )?;
            Ok(())
        })
        .unwrap();
        insert_settling_event_no_usage(&db, "run-1", "stop-full-final");

        let mut policy = settings(true);
        policy.result_content_mode = crate::model::ResultContentMode::FullFinal;
        policy.notifications.result_content_mode = crate::model::ResultContentMode::FullFinal;
        let quota = test_quota_runtime();

        let outcome = finalize_batch(&db, &policy, &quota, 100).unwrap();
        assert_eq!(outcome.finalized, 1);

        db.with_connection(|connection| {
            let payload_json: String = connection.query_row(
                "SELECT payload_json FROM notification_outbox WHERE event_kind = 'run_completed'",
                [],
                |row| row.get(0),
            )?;
            let payload: NotificationPayloadV2 = crate::content_crypto::unprotect_json(
                crate::content_crypto::ContentPurpose::NotificationPayload,
                &payload_json,
            )?;
            assert_eq!(
                payload.content_mode,
                crate::model::ResultContentMode::FullFinal
            );
            assert!(payload.result_revision.is_some());
            assert!(payload.source_hash.is_some());
            assert!(payload.body.contains("完整原文"));
            assert!(payload.body.contains("🎉"));
            assert!(payload.body.contains("C:\\Users"));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q16_settling_run_survives_database_close_and_reopen() {
        let temp = tempfile::tempdir().unwrap();
        {
            let db = Db::open(temp.path()).unwrap();
            insert_run(&db, 100);
            insert_settling_event_no_usage(&db, "run-1", "stop-restart");
        }

        let reopened = Db::open(temp.path()).unwrap();
        let quota = test_quota_runtime();
        let outcome = finalize_batch(&reopened, &settings(true), &quota, 100).unwrap();
        assert_eq!(outcome.finalized, 1);
        assert!(outcome.outbox_changed);

        reopened
            .with_connection(|connection| {
                let status: String = connection.query_row(
                    "SELECT status FROM agent_runs WHERE run_key = 'run-1'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(status, "completed");
                let outbox_count: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM notification_outbox WHERE event_kind = 'run_completed'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(outbox_count, 1);
                Ok(())
            })
            .unwrap();
    }

    fn insert_settling_event_with_corrupt_usage(db: &Db, run_key: &str, event_id: &str) {
        db.with_transaction(|tx| {
            let source = SourceDescriptor::new(
                AgentKind::new("fixture-agent").unwrap(),
                "events",
                "jsonl",
                "default",
            )
            .unwrap();
            let _source_key = SourceRepository::ensure(tx, &source, 90)?;
            let envelope = AgentEventEnvelopeV2 {
                metadata: None,
                schema_version: AGENT_EVENT_SCHEMA_VERSION,
                source,
                event_id: event_id.into(),
                occurred_at: 90,
                observed_at: 90,
                correlation: AgentCorrelationV2 {
                    run_key: Some(run_key.into()),
                    ..AgentCorrelationV2::default()
                },
                payload: AgentEventPayloadV2::RunSettling(crate::agent::RunSettlingV2 {
                    reason: crate::agent::SettlingReason::AgentStopHook,
                    codex_usage: None,
                }),
            };
            crate::storage::EventStore::append(tx, &envelope)?;
            let corrupt_metadata = serde_json::json!({
                "reason": "agent_stop_hook",
                "codexUsage": "corrupted-not-an-object"
            });
            let protected = crate::content_crypto::protect_json(
                crate::content_crypto::ContentPurpose::AgentEventMetadata,
                &corrupt_metadata,
            )?;
            tx.execute(
                "UPDATE agent_events SET metadata_json = ? WHERE event_id = ?",
                rusqlite::params![protected, event_id],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q21_corrupt_metadata_in_database_does_not_affect_finalizer() {
        let db = Db::open_in_memory().unwrap();
        insert_run(&db, 100);
        insert_settling_event_with_corrupt_usage(&db, "run-1", "stop-corrupt");
        let quota = test_quota_runtime();

        // The finalizer reads usage from the independent quota cache, not the database,
        // so corrupt usage data in the database does not affect finalization.
        let outcome = finalize_batch(&db, &settings(true), &quota, 100).unwrap();
        assert_eq!(outcome.finalized, 1);

        db.with_connection(|connection| {
            let status: String = connection.query_row(
                "SELECT status FROM agent_runs WHERE run_key = 'run-1'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                status, "completed",
                "finalizer should complete the run regardless of corrupt usage data"
            );
            let outbox_count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM notification_outbox WHERE agent_run_key = 'run-1'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                outbox_count, 1,
                "finalizer should create outbox entry even with corrupt usage data in database"
            );
            Ok(())
        })
        .unwrap();
    }
}
