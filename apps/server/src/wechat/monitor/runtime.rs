use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use relay_provider_wechat::{
    credentials::{ConnectionBundle, CredentialValidationError},
    http_client::WechatHttpClient,
};
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{inbound::InboundMessageSink, wechat::secret_store::SecretStore};

use super::{
    poll::{run_monitor, set_worker_failure},
    state::{
        ERROR_MONITOR_WORKER_EXITED, MonitorActivationHook, MonitorState, MonitorStatus, Shared,
        SharedState, WechatMonitorError, WechatMonitorHandle, WechatMonitorTransport,
    },
};

const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// Owns the process-wide getupdates lifecycle. All lifecycle mutations are
/// serialized, and replacement always cancels and joins the old worker before
/// installing the next connection.
#[derive(Clone)]
pub struct WechatMonitorRuntime {
    pub(super) shared: Arc<Shared>,
    lifecycle: Arc<AsyncMutex<Lifecycle>>,
}

#[derive(Default)]
struct Lifecycle {
    worker: Option<Worker>,
}

struct Worker {
    cancellation: CancellationToken,
    join: JoinHandle<()>,
}

impl WechatMonitorRuntime {
    pub fn new(
        client: WechatHttpClient,
        store: Arc<dyn SecretStore>,
        sink: Arc<dyn InboundMessageSink>,
        hook: Arc<dyn MonitorActivationHook>,
    ) -> Self {
        Self::with_transport_and_timing(
            Arc::new(client),
            store,
            sink,
            hook,
            DEFAULT_RECONNECT_DELAY,
            MAX_RECONNECT_DELAY,
        )
    }

    /// Constructs the runtime from a bundle already read during application
    /// bootstrap, avoiding a second secret-store read. Call `start_installed`
    /// after the surrounding server lifecycle is ready to own background work.
    pub fn new_with_bundle(
        client: WechatHttpClient,
        store: Arc<dyn SecretStore>,
        sink: Arc<dyn InboundMessageSink>,
        hook: Arc<dyn MonitorActivationHook>,
        bundle: Option<ConnectionBundle>,
    ) -> Result<Self, CredentialValidationError> {
        if let Some(bundle) = &bundle {
            bundle.validate()?;
        }
        let runtime = Self::new(client, store, sink, hook);
        if let Some(bundle) = bundle {
            let mut state = runtime.shared.lock_state();
            state.bundle = Some(bundle);
            state.revision = 1;
        }
        Ok(runtime)
    }

    pub(super) fn with_transport_and_timing(
        transport: Arc<dyn WechatMonitorTransport>,
        store: Arc<dyn SecretStore>,
        sink: Arc<dyn InboundMessageSink>,
        hook: Arc<dyn MonitorActivationHook>,
        reconnect_delay: Duration,
        max_reconnect_delay: Duration,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(SharedState {
                    bundle: None,
                    revision: 0,
                    status: MonitorStatus::default(),
                    activation_required: false,
                }),
                transport,
                store,
                sink,
                hook,
                reconnect_delay,
                max_reconnect_delay,
            }),
            lifecycle: Arc::new(AsyncMutex::new(Lifecycle::default())),
        }
    }

    pub fn handle(&self) -> WechatMonitorHandle {
        WechatMonitorHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Loads the encrypted source of truth and starts monitoring it. A missing
    /// connection is a valid stopped state.
    pub async fn start_loaded(&self) -> Result<bool, WechatMonitorError> {
        match self.shared.store.load().await? {
            Some(bundle) => {
                self.replace_bundle(bundle).await?;
                Ok(true)
            }
            None => {
                self.stop().await?;
                Ok(false)
            }
        }
    }

    pub async fn start_installed(&self) -> Result<bool, WechatMonitorError> {
        let bundle = self.shared.lock_state().bundle.clone();
        match bundle {
            Some(bundle) => {
                self.replace_bundle(bundle).await?;
                Ok(true)
            }
            None => {
                self.stop().await?;
                Ok(false)
            }
        }
    }

    /// Replaces the in-process bundle after the caller has promoted it in the
    /// encrypted store. Mutable cursor/context changes made by the worker are
    /// subsequently saved as complete atomic bundles.
    pub async fn replace_bundle(&self, bundle: ConnectionBundle) -> Result<(), WechatMonitorError> {
        bundle.validate()?;
        let mut lifecycle = self.lifecycle.lock().await;
        stop_worker(&mut lifecycle).await?;

        {
            let mut state = self.shared.lock_state();
            state.bundle = Some(bundle.clone());
            state.revision = state.revision.saturating_add(1);
            state.status = MonitorStatus {
                monitor: MonitorState::Starting,
                ..MonitorStatus::default()
            };
            state.activation_required = false;
        }

        let cancellation = CancellationToken::new();
        let worker_shared = Arc::clone(&self.shared);
        let worker_cancellation = cancellation.clone();
        let observer_shared = Arc::clone(&self.shared);
        let observer_cancellation = cancellation.clone();
        let monitor = tokio::spawn(async move {
            run_monitor(worker_shared, bundle, worker_cancellation).await;
        });
        let join = tokio::spawn(async move {
            let _ = monitor.await;
            if !observer_cancellation.is_cancelled() {
                set_worker_failure(&observer_shared, ERROR_MONITOR_WORKER_EXITED);
            }
        });
        lifecycle.worker = Some(Worker { cancellation, join });
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), WechatMonitorError> {
        let mut lifecycle = self.lifecycle.lock().await;
        stop_worker(&mut lifecycle).await?;
        let mut state = self.shared.lock_state();
        state.status.activated = false;
        state.status.monitor = MonitorState::Stopped;
        state.status.error_code = None;
        Ok(())
    }

    /// Stops the single long-poll owner and removes all in-process connection
    /// material. State is cleared even when joining an already-failed worker
    /// reports an error, keeping disconnect fail-closed.
    pub(crate) async fn stop_and_clear(&self) -> Result<(), WechatMonitorError> {
        let mut lifecycle = self.lifecycle.lock().await;
        let stopped = stop_worker(&mut lifecycle).await;
        let mut state = self.shared.lock_state();
        state.bundle = None;
        state.revision = state.revision.saturating_add(1);
        state.status = MonitorStatus::default();
        state.activation_required = false;
        stopped
    }
}

async fn stop_worker(lifecycle: &mut Lifecycle) -> Result<(), WechatMonitorError> {
    let Some(worker) = lifecycle.worker.take() else {
        return Ok(());
    };
    worker.cancellation.cancel();
    worker.join.await.map_err(|_| WechatMonitorError::Worker)
}
