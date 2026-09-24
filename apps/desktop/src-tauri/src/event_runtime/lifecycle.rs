use super::*;

const CONTROL_CAPACITY: usize = 4;
const PHASE_RUNNING: u8 = 0;
const PHASE_STOPPING: u8 = 1;
const PHASE_STOPPED: u8 = 2;
#[derive(Default)]
pub(super) struct Cancellation {
    state: Mutex<CancellationState>,
    changed: Condvar,
}

#[derive(Default)]
struct CancellationState {
    cancelled: bool,
}

impl Cancellation {
    fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.cancelled = true;
        drop(state);
        self.changed.notify_all();
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cancelled
    }

    /// Waits only while cancellation has not been requested. Both the
    /// predicate and notification use the same mutex, so a cancellation that
    /// arrives immediately before this call cannot be lost.
    pub(super) fn wait(&self, duration: Duration) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (state, _) = self
            .changed
            .wait_timeout_while(state, duration, |state| !state.cancelled)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.cancelled
    }
}

struct DurableEventRuntimeInner {
    cancellation: Arc<Cancellation>,
    source: Mutex<Option<JoinHandle<()>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    control: mpsc::SyncSender<RuntimeCommand>,
    phase: AtomicU8,
}

#[derive(Clone)]
pub struct DurableEventRuntime(Arc<DurableEventRuntimeInner>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeShutdownReport {
    pub source_panicked: bool,
    pub worker_panicked: bool,
}

impl RuntimeShutdownReport {
    pub const fn completed_cleanly(self) -> bool {
        !self.source_panicked && !self.worker_panicked
    }
}

impl DurableEventRuntime {
    pub fn start(
        db: Arc<Db>,
        processor: Arc<dyn AgentEventProcessor>,
        app: tauri::AppHandle,
        inbox_path: PathBuf,
        source_descriptor: SourceDescriptor,
        on_caught_up: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self::start_with_emitter_and_caught_up(
            db,
            processor,
            Arc::new(TauriRuntimeEventEmitter(app)),
            inbox_path,
            source_descriptor,
            on_caught_up,
        )
    }

    pub(super) fn start_with_emitter_and_caught_up(
        db: Arc<Db>,
        processor: Arc<dyn AgentEventProcessor>,
        events: Arc<dyn RuntimeEventEmitter>,
        inbox_path: PathBuf,
        source_descriptor: SourceDescriptor,
        on_caught_up: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let cancellation = Arc::new(Cancellation::default());
        // A rendezvous channel plus the per-item ack below makes the invariant
        // explicit: at most one fact can be in flight before its checkpoint.
        let (sender, receiver) = mpsc::sync_channel::<WorkItem>(0);
        let (control, control_receiver) = mpsc::sync_channel::<RuntimeCommand>(CONTROL_CAPACITY);
        let worker_events = Arc::clone(&events);
        let worker_inbox_path = inbox_path.clone();
        let worker = thread::spawn(move || {
            while let Ok(work) = receiver.recv() {
                let WorkItem {
                    record,
                    checkpoint,
                    ack,
                } = work;
                let succeeded = match process_record(processor.as_ref(), record, &checkpoint) {
                    Ok(processed) => {
                        if let ProcessedRecord::Rejected(code) = processed {
                            record_rejection(&worker_inbox_path, code, &checkpoint);
                            let rejected = SourceCheckpointUpdate {
                                source: checkpoint.source.clone(),
                                cursor_json: checkpoint.cursor_json.clone(),
                                source_revision: checkpoint.source_revision.clone(),
                                status: "degraded".into(),
                                last_error_code: Some(code.into()),
                                updated_at: checkpoint.updated_at,
                            };
                            worker_events.capture_health_changed(&rejected);
                        } else {
                            worker_events.capture_health_changed(&checkpoint);
                        }
                        true
                    }
                    Err(error) => {
                        tracing::error!(code = error.code, "agent event transaction failed");
                        false
                    }
                };
                let _ = ack.send(succeeded);
            }
        });

        let source_cancellation = Arc::clone(&cancellation);
        let source = thread::spawn(move || {
            run_inbox_source(
                InboxSourceContext {
                    db,
                    events,
                    paths: InboxPaths::new(inbox_path),
                    source: source_descriptor,
                    on_caught_up,
                },
                sender,
                control_receiver,
                source_cancellation,
            );
        });

        Self(Arc::new(DurableEventRuntimeInner {
            cancellation,
            source: Mutex::new(Some(source)),
            worker: Mutex::new(Some(worker)),
            control,
            phase: AtomicU8::new(PHASE_RUNNING),
        }))
    }

    pub fn begin_shutdown(&self) -> bool {
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
            self.0.cancellation.cancel();
            let _ = self.0.control.try_send(RuntimeCommand::Wake);
            true
        } else {
            false
        }
    }

    pub fn shutdown(&self) -> RuntimeShutdownReport {
        self.0.cancellation.cancel();
        let _ = self.0.control.try_send(RuntimeCommand::Wake);
        let source_panicked = join_once("inbox-source", &self.0.source);
        let worker_panicked = join_once("event-worker", &self.0.worker);
        self.0.phase.store(PHASE_STOPPED, Ordering::Release);
        RuntimeShutdownReport {
            source_panicked,
            worker_panicked,
        }
    }
}

fn join_once(component: &'static str, slot: &Mutex<Option<JoinHandle<()>>>) -> bool {
    let handle = slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(handle) = handle {
        tracing::info!(%component, "runtime shutdown phase started");
        if handle.join().is_err() {
            tracing::error!(%component, "runtime task panicked during shutdown");
            return true;
        }
        tracing::info!(%component, "runtime shutdown phase completed");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_before_wait_does_not_sleep_for_the_fallback_interval() {
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let started = Instant::now();

        assert!(cancellation.wait(Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
