use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::error::AppError;
use crate::model::NotificationBackendKind;
use crate::notification::hold::{HoldController, HoldDuration, HoldReceipt, UserHoldStatus};
use crate::notification::outbox::{
    DeliveryTransport, NotificationDeliveryWorker, NotificationOutboxStats, OutboxRepository,
    RunNotificationPolicyProvider,
};
use crate::settings::SettingsStore;

type ReadinessProbe = Arc<dyn Fn() -> bool + Send + Sync>;

struct RuntimeControl {
    worker: Option<NotificationDeliveryWorker>,
    paused: bool,
}

struct NotificationRuntimeInner {
    outbox: OutboxRepository,
    hold: HoldController,
    transport: Arc<dyn DeliveryTransport>,
    policy: Arc<dyn RunNotificationPolicyProvider>,
    ready: ReadinessProbe,
    control: Mutex<RuntimeControl>,
    stopped: AtomicBool,
    last_hold_resume_wake: Mutex<Option<Instant>>,
}

/// Owns the sole Relay outbox worker. Readiness gates startup and reconciliation;
/// the durable outbox remains authoritative while Relay is unavailable.
#[derive(Clone)]
pub(crate) struct NotificationRuntimeState(Arc<NotificationRuntimeInner>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PauseReceipt;

impl NotificationRuntimeState {
    pub(crate) fn start(
        outbox: OutboxRepository,
        settings: SettingsStore,
        transport: Arc<dyn DeliveryTransport>,
        ready: ReadinessProbe,
    ) -> Result<Self, AppError> {
        let state = Self(Arc::new(NotificationRuntimeInner {
            hold: outbox.hold_controller(),
            outbox,
            transport,
            policy: Arc::new(settings),
            ready,
            control: Mutex::new(RuntimeControl {
                worker: None,
                paused: false,
            }),
            stopped: AtomicBool::new(false),
            last_hold_resume_wake: Mutex::new(None),
        }));
        state.reconcile()?;
        Ok(state)
    }

    pub(crate) fn wake(&self) -> Result<bool, AppError> {
        let control = self.control()?;
        if let Some(worker) = control.worker.as_ref() {
            worker.wake();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn pause(&self) -> Result<PauseReceipt, AppError> {
        let mut control = self.control()?;
        if control.paused {
            return Err(AppError::new(
                "NOTIFICATION_RUNTIME_ALREADY_PAUSED",
                "通知投递后台已暂停",
            ));
        }
        let worker_was_running = control.worker.is_some();
        control.paused = true;
        if let Err(error) = stop_worker(&mut control) {
            control.paused = false;
            if worker_was_running && (self.0.ready)() {
                let _ = start_worker(&self.0, &mut control);
            }
            return Err(error);
        }
        Ok(PauseReceipt)
    }

    /// Policy recovery shares the application's operation lock with UI mutations.
    /// A previously failed apply keeps this pause until its committed revision is applied.
    pub(crate) fn pause_for_policy(&self) -> Result<PauseReceipt, AppError> {
        if self.control()?.paused {
            return Ok(PauseReceipt);
        }
        self.pause()
    }

    pub(crate) fn resume(&self, _receipt: PauseReceipt) -> Result<(), AppError> {
        let mut control = self.control()?;
        control.paused = false;
        if (self.0.ready)() {
            start_worker(&self.0, &mut control)?;
        }
        Ok(())
    }

    pub(crate) fn reconcile(&self) -> Result<bool, AppError> {
        if self.0.stopped.load(Ordering::Acquire) {
            return Ok(false);
        }
        let mut control = self.control()?;
        if control.paused {
            return Ok(false);
        }
        if !(self.0.ready)() {
            let changed = control.worker.is_some();
            stop_worker(&mut control)?;
            return Ok(changed);
        }
        if control.worker.is_some() {
            return Ok(false);
        }
        start_worker(&self.0, &mut control)?;
        Ok(true)
    }

    pub(crate) fn stats(&self) -> Result<NotificationOutboxStats, AppError> {
        self.0.outbox.stats()
    }

    pub(crate) fn user_hold_status(&self, now: i64) -> Result<UserHoldStatus, AppError> {
        self.0.hold.status_at(now)
    }

    pub(crate) fn require_notification_not_held(&self, now: i64) -> Result<(), AppError> {
        self.0.hold.require_not_held(now)
    }

    pub(crate) fn set_user_hold(
        &self,
        expected_revision: u64,
        duration: HoldDuration,
        now: i64,
    ) -> Result<HoldReceipt, AppError> {
        self.0.hold.hold(expected_revision, duration, now)
    }

    /// Resume does not rewrite, cancel, or re-create any outbox row. It only
    /// wakes the existing worker at most once per second, so the row's original
    /// TTL and Relay identity remain intact.
    pub(crate) fn resume_user_hold(
        &self,
        expected_revision: u64,
        now: i64,
    ) -> Result<HoldReceipt, AppError> {
        let receipt = self.0.hold.resume(expected_revision, now)?;
        if receipt.status == "applied" {
            let mut last = self.0.last_hold_resume_wake.lock().map_err(|_| {
                AppError::new("NOTIFICATION_RUNTIME_UNAVAILABLE", "通知投递暂时不可用")
            })?;
            if last.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
                *last = Some(Instant::now());
                let _ = self.wake()?;
            }
        }
        Ok(receipt)
    }

    pub(crate) fn begin_shutdown(&self) -> bool {
        if self.0.stopped.swap(true, Ordering::AcqRel) {
            return false;
        }
        if let Ok(control) = self.0.control.try_lock() {
            if let Some(worker) = control.worker.as_ref() {
                worker.begin_shutdown();
            }
        }
        true
    }

    pub(crate) fn shutdown(&self) -> Result<(), AppError> {
        self.begin_shutdown();
        let mut control = self.control()?;
        stop_worker(&mut control)
    }

    fn control(&self) -> Result<MutexGuard<'_, RuntimeControl>, AppError> {
        self.0
            .control
            .lock()
            .map_err(|_| AppError::new("NOTIFICATION_RUNTIME_UNAVAILABLE", "通知投递暂时不可用"))
    }
}

fn start_worker(
    inner: &NotificationRuntimeInner,
    control: &mut RuntimeControl,
) -> Result<(), AppError> {
    if inner.stopped.load(Ordering::Acquire) {
        return Ok(());
    }
    if control.worker.is_none() {
        control.worker = Some(NotificationDeliveryWorker::start_for_backend(
            inner.outbox.clone(),
            Arc::clone(&inner.transport),
            Arc::clone(&inner.policy),
            NotificationBackendKind::Relay,
            inner.hold.clone(),
        )?);
    }
    Ok(())
}

fn stop_worker(control: &mut RuntimeControl) -> Result<(), AppError> {
    if let Some(mut worker) = control.worker.take() {
        worker.shutdown()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification::outbox::{DeliveryCancellation, DeliveryResult, OutboxClaim};

    struct NeverTransport;

    impl DeliveryTransport for NeverTransport {
        fn deliver(
            &self,
            _claim: &OutboxClaim,
            _cancellation: &DeliveryCancellation,
        ) -> DeliveryResult {
            unreachable!()
        }
    }

    #[test]
    fn begin_shutdown_does_not_wait_for_reconcile_control_lock() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::Db::open_in_memory().unwrap());
        let settings = SettingsStore::open(directory.path().join("settings.json")).unwrap();
        let state = NotificationRuntimeState::start(
            OutboxRepository::new(db),
            settings,
            Arc::new(NeverTransport),
            Arc::new(|| false),
        )
        .unwrap();
        let held = state.control().unwrap();
        let shutdown = state.clone();
        let (send, recv) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            send.send(shutdown.begin_shutdown()).unwrap();
        });
        let result = recv.recv_timeout(std::time::Duration::from_millis(200));
        drop(held);
        worker.join().unwrap();
        assert!(result.unwrap());
        assert!(!state.reconcile().unwrap());
        state.shutdown().unwrap();
    }

    #[test]
    fn failed_worker_join_restores_pause_state_and_allows_retry() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::Db::open_in_memory().unwrap());
        let settings = SettingsStore::open(directory.path().join("settings.json")).unwrap();
        let outbox = OutboxRepository::new(db);
        let hold = outbox.hold_controller();
        let state = NotificationRuntimeState(Arc::new(NotificationRuntimeInner {
            outbox,
            hold,
            transport: Arc::new(NeverTransport),
            policy: Arc::new(settings),
            ready: Arc::new(|| true),
            control: Mutex::new(RuntimeControl {
                worker: Some(NotificationDeliveryWorker::panicked_for_test()),
                paused: false,
            }),
            stopped: AtomicBool::new(false),
            last_hold_resume_wake: Mutex::new(None),
        }));

        assert_eq!(
            state.pause().unwrap_err().code,
            "NOTIFICATION_WORKER_JOIN_FAILED"
        );
        assert!(!state.control().unwrap().paused);
        assert!(state.control().unwrap().worker.is_some());
        let receipt = state.pause().expect("a failed pause must remain retryable");
        state.resume(receipt).unwrap();
        assert!(!state.control().unwrap().paused);
        assert!(state.control().unwrap().worker.is_some());
        state.shutdown().unwrap();
    }

    #[test]
    fn shutdown_fences_resume_and_reconcile_from_restarting_the_worker() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::Db::open_in_memory().unwrap());
        let settings = SettingsStore::open(directory.path().join("settings.json")).unwrap();
        let state = NotificationRuntimeState::start(
            OutboxRepository::new(db),
            settings,
            Arc::new(NeverTransport),
            Arc::new(|| true),
        )
        .unwrap();

        let receipt = state.pause().unwrap();
        state.begin_shutdown();
        state.resume(receipt).unwrap();
        assert!(state.control().unwrap().worker.is_none());
        assert!(!state.reconcile().unwrap());
        assert!(state.control().unwrap().worker.is_none());
    }

    #[test]
    fn resume_repairs_a_missing_worker_when_relay_is_ready() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::Db::open_in_memory().unwrap());
        let settings = SettingsStore::open(directory.path().join("settings.json")).unwrap();
        let outbox = OutboxRepository::new(db);
        let hold = outbox.hold_controller();
        let state = NotificationRuntimeState(Arc::new(NotificationRuntimeInner {
            outbox,
            hold,
            transport: Arc::new(NeverTransport),
            policy: Arc::new(settings),
            ready: Arc::new(|| true),
            control: Mutex::new(RuntimeControl {
                worker: None,
                paused: true,
            }),
            stopped: AtomicBool::new(false),
            last_hold_resume_wake: Mutex::new(None),
        }));

        state.resume(PauseReceipt).unwrap();
        assert!(state.control().unwrap().worker.is_some());
        state.shutdown().unwrap();
    }

    #[test]
    fn startup_exposes_corrupt_and_clock_rollback_holds_as_uncertain() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::Db::open_in_memory().unwrap());
        let settings = SettingsStore::open(directory.path().join("settings.json")).unwrap();
        let outbox = OutboxRepository::new(Arc::clone(&db));
        let state = NotificationRuntimeState::start(
            outbox,
            settings,
            Arc::new(NeverTransport),
            Arc::new(|| false),
        )
        .unwrap();
        let held = state
            .set_user_hold(0, HoldDuration::Minutes15, 10_000)
            .unwrap();
        assert_eq!(
            state.user_hold_status(9_999).unwrap().state,
            crate::notification::hold::UserHoldState::Uncertain
        );
        state.resume_user_hold(held.state.revision, 10_001).unwrap();
        db.with_connection(|conn| {
            conn.execute(
                "UPDATE app_metadata SET value = 'not-json' WHERE key = 'notification_user_hold_v1'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            state.user_hold_status(10_002).unwrap().state,
            crate::notification::hold::UserHoldState::Uncertain
        );
        assert!(state.0.hold.acquire_send_permit(10_002).unwrap().is_none());
        state.shutdown().unwrap();
    }
}
