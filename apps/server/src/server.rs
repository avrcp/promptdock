use std::{future::Future, future::IntoFuture as _, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::Router;
use thiserror::Error;
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
};

use crate::{
    admin::{AdminState, RuntimeHealthRegistry, WorkerKey, WorkerState},
    api,
    auth::DeviceAuthService,
    config::{Config, ConfigError},
    db,
    inbound::{
        ConfirmationCredentialError, ConfirmationVerifier, DiscardingInboundMessageSink,
        DurableInboundMessageSink, InboundCommandService, InboundCommandWorker, InboundMessageSink,
    },
    outbox::{
        BundleContentCipher, ChannelMessage, ChannelOutcome, NotificationChannel, OutboxError,
        OutboxService, OutboxWorker,
    },
    retention::RetentionService,
    secret_store::{SecretStore, SecretStoreError},
    shutdown::{
        ShutdownTimeout, SignalError, TaskSupervisor, first_signal, os_signal, wait_with_deadline,
    },
    state::AppState,
    wechat_channel::WechatChannel,
    wechat_login::{WechatLoginService, WechatRuntimeStatus, bootstrap_wechat},
    wechat_monitor::{MonitorActivationHook, WechatMonitorError, WechatMonitorRuntime},
};

struct OutboxActivationHook {
    outbox: OutboxService,
}

struct UnavailableChannel;

#[async_trait]
impl NotificationChannel for UnavailableChannel {
    async fn send(
        &self,
        _message: &ChannelMessage,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> ChannelOutcome {
        ChannelOutcome::BlockedReconnect
    }
}

#[async_trait]
impl MonitorActivationHook for OutboxActivationHook {
    async fn activated(&self) {
        let activation = self.outbox.unblock_activation().await;
        let reconnect = self.outbox.unblock_reconnect().await;
        if activation.is_err() || reconnect.is_err() {
            tracing::error!(
                operation = "wechat.activation.unblock",
                error_code = "OUTBOX_UNAVAILABLE",
                "could not unblock WeChat-ready notifications"
            );
        }
    }

    async fn reconnected(&self) {
        if self.outbox.unblock_reconnect().await.is_err() {
            tracing::error!(
                operation = "wechat.reconnect.unblock",
                error_code = "OUTBOX_UNAVAILABLE",
                "could not unblock reconnect-waiting notifications"
            );
        }
    }
}

pub async fn serve(config: Config) -> Result<(), ServerError> {
    // The master key is a startup prerequisite only when WeChat is enabled.
    // Keep the store alive for the process lifetime so key material is not
    // repeatedly read from the credential directory.
    let bundle_cipher = if config.wechat.enabled || config.results.enabled {
        Some(BundleContentCipher::from_systemd_credential().await?)
    } else {
        None
    };
    let wechat_bootstrap = bootstrap_wechat(&config.wechat).await?;
    let confirmation_verifier = if config.wechat.enabled {
        ConfirmationVerifier::from_systemd_credential().await?
    } else {
        ConfirmationVerifier::disabled_runtime()
    };
    let wechat_credentials_unreadable = wechat_bootstrap
        .as_ref()
        .is_some_and(|bootstrap| bootstrap.status == WechatRuntimeStatus::CredentialsUnreadable);
    let pool = db::open(&config.database).await?;
    let supervisor = TaskSupervisor::new();
    let mut outbox = OutboxService::new(pool.clone())
        .with_bundle_content_retention_days(config.retention.bundle_content_days);
    if let Some(cipher) = bundle_cipher.clone() {
        outbox = outbox.with_bundle_cipher(cipher);
    }
    let inbound = InboundCommandService::new(pool.clone());
    let device_auth = DeviceAuthService::new(pool.clone());
    let (wechat_login, wechat_monitor) = match wechat_bootstrap {
        Some(bootstrap) => {
            let monitor_client = relay_provider_wechat::ProductionWechatProvider::new(format!(
                "PromptDockRelay/{}",
                env!("CARGO_PKG_VERSION")
            ))?
            .into_client();
            let monitor = WechatMonitorRuntime::new_with_bundle(
                monitor_client,
                Arc::new(bootstrap.store.clone()) as Arc<dyn SecretStore>,
                inbound_message_sink(
                    config.server.notification_only,
                    inbound.clone(),
                    confirmation_verifier.clone(),
                ),
                Arc::new(OutboxActivationHook {
                    outbox: outbox.clone(),
                }),
                bootstrap.bundle.clone(),
            )
            .map_err(|_| ServerError::MonitorConfiguration)?;
            let login = WechatLoginService::production(
                bootstrap,
                device_auth.clone(),
                supervisor.clone(),
                monitor.clone(),
            )?;
            (Some(login), Some(monitor))
        }
        None => (None, None),
    };
    let monitor_handle = wechat_monitor.as_ref().map(WechatMonitorRuntime::handle);
    let mut state = AppState::new_with_services(
        pool,
        supervisor.clone(),
        outbox,
        wechat_login,
        monitor_handle.clone(),
    );
    state.results =
        crate::results::ResultService::new(state.db.clone(), config.results.clone(), bundle_cipher);
    if state.results.available() {
        state
            .server_info
            .features
            .push("result_pages_v1".to_owned());
    }
    state.outbox.recover_stale_claims().await?;
    let listeners = match BoundListeners::bind(&config).await {
        Ok(listeners) => listeners,
        Err(error) => {
            state.db.close().await;
            return Err(error);
        }
    };
    let health = RuntimeHealthRegistry::default();
    health.set_state(WorkerKey::PublicHttp, WorkerState::Starting);
    if config.admin.enabled {
        health.set_state(WorkerKey::AdminHttp, WorkerState::Starting);
    } else {
        health.set_state(WorkerKey::AdminHttp, WorkerState::Disabled);
    }
    health.set_state(
        WorkerKey::Gateway,
        if config.server.notification_only {
            WorkerState::Disabled
        } else {
            WorkerState::Running
        },
    );
    health.set_state(
        WorkerKey::Inbound,
        if config.server.notification_only {
            WorkerState::Disabled
        } else {
            WorkerState::Starting
        },
    );
    if config.wechat.enabled {
        health.set_state(WorkerKey::WechatMonitor, WorkerState::Starting);
    } else {
        health.set_state(WorkerKey::WechatMonitor, WorkerState::Disabled);
    }
    let retention = RetentionService::new(state.db.clone(), config.retention.clone());
    let admin_state = if config.admin.enabled {
        Some(AdminState::new(
            state.clone(),
            &config,
            retention.clone(),
            health.clone(),
        )?)
    } else {
        None
    };
    let (critical_tx, mut critical_rx) = mpsc::unbounded_channel();
    let channel: Arc<dyn NotificationChannel> = match monitor_handle {
        Some(handle) => Arc::new(WechatChannel::production(Arc::new(handle))?),
        None => Arc::new(UnavailableChannel),
    };
    let outbox_health = health.clone();
    spawn_critical_worker(
        &supervisor,
        "outbox",
        WorkerKey::Outbox,
        "OUTBOX_WORKER_STOPPED",
        health.clone(),
        OutboxWorker::new(state.outbox.clone(), channel)
            .run_with_progress(supervisor.cancellation_token(), move || {
                outbox_health.tick(WorkerKey::Outbox)
            }),
        critical_tx.clone(),
    );
    if !config.server.notification_only {
        let inbound_health = health.clone();
        spawn_critical_worker(
            &supervisor,
            "inbound",
            WorkerKey::Inbound,
            "INBOUND_WORKER_STOPPED",
            health.clone(),
            InboundCommandWorker::new_with_verifier(
                inbound,
                state.outbox.clone(),
                device_auth,
                state.gateway.clone(),
                confirmation_verifier,
            )
            .run_with_progress(supervisor.cancellation_token(), move || {
                inbound_health.tick(WorkerKey::Inbound);
            }),
            critical_tx.clone(),
        );
    }
    let retention_health = health.clone();
    spawn_critical_worker(
        &supervisor,
        "retention",
        WorkerKey::Retention,
        "RETENTION_WORKER_STOPPED",
        health.clone(),
        retention.run_with_progress(supervisor.cancellation_token(), move || {
            retention_health.tick(WorkerKey::Retention);
        }),
        critical_tx,
    );
    if let Some(monitor) = wechat_monitor {
        monitor.start_installed().await?;
        health.set_state(WorkerKey::WechatMonitor, WorkerState::Running);
        if wechat_credentials_unreadable {
            monitor
                .handle()
                .mark_reconnect_required("WECHAT_CREDENTIALS_UNREADABLE");
        }
        let cancellation = supervisor.cancellation_token();
        supervisor.spawn(async move {
            cancellation.cancelled().await;
            if monitor.stop().await.is_err() {
                tracing::error!(
                    operation = "wechat.monitor.stop",
                    error_code = "WECHAT_MONITOR_STOP_FAILED",
                    "WeChat monitor did not stop cleanly"
                );
            }
        });
    }
    let public_app = api::router(state.clone(), &config.server);
    let admin_app = admin_state.map(crate::admin::router);
    state.mark_ready();
    health.set_state(WorkerKey::PublicHttp, WorkerState::Running);
    if config.admin.enabled {
        health.set_state(WorkerKey::AdminHttp, WorkerState::Running);
    }

    tracing::info!(
        operation = "server.listen",
        listener = "public",
        address = %listeners.public_address,
        "relay server listening"
    );
    if let Some(address) = listeners.admin_address {
        tracing::info!(
            operation = "server.listen",
            listener = "admin",
            address = %address,
            "relay Admin listener listening"
        );
    }

    serve_with_listeners(
        listeners.public,
        public_app,
        listeners.admin.zip(admin_app),
        ServerRuntime {
            state,
            supervisor,
            health,
            deadline: config.server.shutdown_timeout(),
        },
        async move {
            first_signal(
                async { os_signal().await.map_err(ServerError::Signal) },
                async {
                    let _ = critical_rx.recv().await;
                    Err(ServerError::CriticalWorker)
                },
            )
            .await
        },
    )
    .await
}

fn inbound_message_sink(
    notification_only: bool,
    service: InboundCommandService,
    verifier: ConfirmationVerifier,
) -> Arc<dyn InboundMessageSink> {
    if notification_only {
        Arc::new(DiscardingInboundMessageSink::new(service))
    } else {
        Arc::new(DurableInboundMessageSink::new_with_verifier(
            service, verifier,
        ))
    }
}

#[derive(Debug)]
struct BoundListeners {
    public: TcpListener,
    public_address: std::net::SocketAddr,
    admin: Option<TcpListener>,
    admin_address: Option<std::net::SocketAddr>,
}

impl BoundListeners {
    async fn bind(config: &Config) -> Result<Self, ServerError> {
        let public = TcpListener::bind(config.server.bind)
            .await
            .map_err(|_| ServerError::PublicBind)?;
        let public_address = public.local_addr().map_err(|_| ServerError::PublicBind)?;
        let (admin, admin_address) = if config.admin.enabled {
            let listener = TcpListener::bind(config.admin.bind)
                .await
                .map_err(|_| ServerError::AdminBind)?;
            let address = listener.local_addr().map_err(|_| ServerError::AdminBind)?;
            (Some(listener), Some(address))
        } else {
            (None, None)
        };
        Ok(Self {
            public,
            public_address,
            admin,
            admin_address,
        })
    }
}

fn spawn_critical_worker<F, E>(
    supervisor: &TaskSupervisor,
    name: &'static str,
    key: WorkerKey,
    failure_code: &'static str,
    health: RuntimeHealthRegistry,
    worker: F,
    failure: mpsc::UnboundedSender<()>,
) where
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: Send + 'static,
{
    let cancellation = supervisor.cancellation_token();
    health.set_state(key, WorkerState::Starting);
    let handle = supervisor.spawn(worker);
    health.set_state(key, WorkerState::Running);
    supervisor.spawn(async move {
        let failed = match handle.await {
            Ok(Ok(())) => !cancellation.is_cancelled(),
            Ok(Err(_)) | Err(_) => true,
        };
        if failed {
            health.fail(key, failure_code);
            tracing::error!(
                operation = "worker.failed",
                worker = name,
                "critical worker stopped"
            );
            let _ = failure.send(());
        } else {
            health.set_state(key, WorkerState::Stopped);
        }
    });
}

pub async fn serve_with_listener<F>(
    listener: TcpListener,
    app: Router,
    state: AppState,
    supervisor: TaskSupervisor,
    shutdown: F,
    deadline: Duration,
) -> Result<(), ServerError>
where
    F: Future<Output = Result<(), ServerError>> + Send,
{
    let health = RuntimeHealthRegistry::default();
    health.set_state(WorkerKey::PublicHttp, WorkerState::Running);
    health.set_state(WorkerKey::AdminHttp, WorkerState::Disabled);
    serve_with_listeners(
        listener,
        app,
        None,
        ServerRuntime {
            state,
            supervisor,
            health,
            deadline,
        },
        shutdown,
    )
    .await
}

struct ServerRuntime {
    state: AppState,
    supervisor: TaskSupervisor,
    health: RuntimeHealthRegistry,
    deadline: Duration,
}

async fn serve_with_listeners<F>(
    public_listener: TcpListener,
    public_app: Router,
    admin: Option<(TcpListener, Router)>,
    runtime: ServerRuntime,
    shutdown: F,
) -> Result<(), ServerError>
where
    F: Future<Output = Result<(), ServerError>> + Send,
{
    let ServerRuntime {
        state,
        supervisor,
        health,
        deadline,
    } = runtime;
    let (public_shutdown_tx, public_shutdown_rx) = oneshot::channel::<()>();
    let public_server = axum::serve(public_listener, public_app)
        .with_graceful_shutdown(async move {
            let _ = public_shutdown_rx.await;
        })
        .into_future();
    let (admin_shutdown_tx, admin_server) = match admin {
        Some((listener, app)) => {
            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
            let server = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .into_future();
            (Some(shutdown_tx), Some(server))
        }
        None => (None, None),
    };
    tokio::pin!(public_server);
    tokio::pin!(admin_server);
    tokio::pin!(shutdown);

    tokio::select! {
        result = &mut public_server => {
            state.mark_not_ready().await;
            health.fail(WorkerKey::PublicHttp, "PUBLIC_HTTP_STOPPED");
            health.set_state(WorkerKey::AdminHttp, WorkerState::Stopping);
            supervisor.begin_shutdown();
            drop(public_shutdown_tx);
            drop(admin_shutdown_tx);
            let tracked = wait_with_deadline(deadline, async {
                if let Some(server) = admin_server.as_mut().as_pin_mut() {
                    server.await.map_err(|_| ServerError::Serve)?;
                }
                supervisor.wait().await;
                state.db.close().await;
                Ok::<(), ServerError>(())
            }).await;
            finish_or_abort(tracked, &supervisor).await??;
            health.set_state(WorkerKey::AdminHttp, WorkerState::Stopped);
            result.map_err(|_| ServerError::Serve)?;
            Err(ServerError::Serve)
        }
        result = async {
            match admin_server.as_mut().as_pin_mut() {
                Some(server) => server.await,
                None => std::future::pending().await,
            }
        } => {
            state.mark_not_ready().await;
            health.fail(WorkerKey::AdminHttp, "ADMIN_HTTP_STOPPED");
            health.set_state(WorkerKey::PublicHttp, WorkerState::Stopping);
            supervisor.begin_shutdown();
            drop(public_shutdown_tx);
            drop(admin_shutdown_tx);
            let tracked = wait_with_deadline(deadline, async {
                public_server.await.map_err(|_| ServerError::Serve)?;
                supervisor.wait().await;
                state.db.close().await;
                Ok::<(), ServerError>(())
            }).await;
            finish_or_abort(tracked, &supervisor).await??;
            health.set_state(WorkerKey::PublicHttp, WorkerState::Stopped);
            result.map_err(|_| ServerError::Serve)?;
            Err(ServerError::Serve)
        }
        shutdown_result = &mut shutdown => {
            tracing::info!(operation = "server.shutdown", "shutdown requested");
            state.mark_not_ready().await;
            health.set_state(WorkerKey::PublicHttp, WorkerState::Stopping);
            if admin_server.is_some() {
                health.set_state(WorkerKey::AdminHttp, WorkerState::Stopping);
            }
            supervisor.begin_shutdown();
            let _ = public_shutdown_tx.send(());
            if let Some(shutdown_tx) = admin_shutdown_tx {
                let _ = shutdown_tx.send(());
            }

            let drain = wait_with_deadline(deadline, async {
                if let Some(server) = admin_server.as_mut().as_pin_mut() {
                    let (public_result, admin_result) = tokio::join!(&mut public_server, server);
                    public_result.map_err(|_| ServerError::Serve)?;
                    admin_result.map_err(|_| ServerError::Serve)?;
                } else {
                    public_server.await.map_err(|_| ServerError::Serve)?;
                }
                supervisor.wait().await;
                state.db.close().await;
                Ok::<(), ServerError>(())
            })
            .await;
            finish_or_abort(drain, &supervisor).await??;
            health.set_state(WorkerKey::PublicHttp, WorkerState::Stopped);
            if admin_server.is_some() {
                health.set_state(WorkerKey::AdminHttp, WorkerState::Stopped);
            }
            shutdown_result?;
            tracing::info!(operation = "server.shutdown", "shutdown complete");
            Ok(())
        }
    }
}

async fn finish_or_abort<T>(
    result: Result<T, ShutdownTimeout>,
    supervisor: &TaskSupervisor,
) -> Result<T, ServerError> {
    match result {
        Ok(value) => Ok(value),
        Err(timeout) => {
            supervisor.abort_all();
            supervisor.wait().await;
            Err(timeout.into())
        }
    }
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    SecretStore(#[from] SecretStoreError),
    #[error(transparent)]
    ConfirmationCredential(#[from] ConfirmationCredentialError),
    #[error("WeChat HTTP client initialization failed")]
    WechatClient(#[from] relay_provider_wechat::http_client::WechatHttpError),
    #[error("WeChat monitor configuration failed")]
    MonitorConfiguration,
    #[error(transparent)]
    Monitor(#[from] WechatMonitorError),
    #[error(transparent)]
    Database(#[from] db::DatabaseError),
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    #[error("public server could not bind its configured address")]
    PublicBind,
    #[error("Admin server could not bind its configured loopback address")]
    AdminBind,
    #[error("HTTP server failed")]
    Serve,
    #[error(transparent)]
    Shutdown(#[from] ShutdownTimeout),
    #[error(transparent)]
    Signal(#[from] SignalError),
    #[error("critical background worker failed")]
    CriticalWorker,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::routing::get;
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::TcpStream,
        sync::Notify,
        time::timeout,
    };

    use super::*;

    async fn test_state(supervisor: &TaskSupervisor) -> (tempfile::TempDir, AppState) {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let database = crate::config::DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..crate::config::DatabaseConfig::default()
        };
        let pool = crate::db::open(&database).await.expect("test database");
        (directory, AppState::new(pool, supervisor.clone()))
    }

    #[tokio::test]
    async fn production_disabled_channel_never_reports_fake_provider_acceptance() {
        let outcome = UnavailableChannel
            .send(
                &ChannelMessage {
                    client_id: "stable-client".into(),
                    body: "body".into(),
                    correlation_key: None,
                    target_account_fingerprint: None,
                },
                tokio_util::sync::CancellationToken::new(),
            )
            .await;
        assert!(matches!(outcome, ChannelOutcome::BlockedReconnect));
    }

    #[tokio::test]
    async fn notification_only_ingress_is_discarded_and_not_replayed_after_normal_restart() {
        let supervisor = TaskSupervisor::new();
        let (_database_directory, state) = test_state(&supervisor).await;
        let service = InboundCommandService::new(state.db.clone());
        let cancellation = tokio_util::sync::CancellationToken::new();
        let message = |message_key: &str| crate::inbound::EphemeralInboundText {
            message_key: message_key.to_owned(),
            sender_fingerprint: format!("wx:{}", "a".repeat(64)),
            text: "help".to_owned().into(),
            received_at: 1_800_000_000_000,
        };

        let notification_sink = inbound_message_sink(
            true,
            service.clone(),
            ConfirmationVerifier::disabled_runtime(),
        );
        assert_eq!(
            notification_sink
                .accept(message("disabled-mode-message"), &cancellation)
                .await
                .expect("notification-only ingress"),
            crate::inbound::InboundAcceptOutcome::Discarded
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM inbound_commands WHERE status = 'received'",
            )
            .fetch_one(&state.db)
            .await
            .expect("disabled-mode queue depth"),
            0
        );

        let normal_sink =
            inbound_message_sink(false, service, ConfirmationVerifier::disabled_runtime());
        assert_eq!(
            normal_sink
                .accept(message("disabled-mode-message"), &cancellation)
                .await
                .expect("disabled-mode replay after normal restart"),
            crate::inbound::InboundAcceptOutcome::ExactReplay
        );
        assert_eq!(
            normal_sink
                .accept(message("normal-mode-message"), &cancellation)
                .await
                .expect("normal ingress"),
            crate::inbound::InboundAcceptOutcome::Fresh
        );
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT message_key, status FROM inbound_commands ORDER BY message_key",
        )
        .fetch_all(&state.db)
        .await
        .expect("normal-mode queue rows");
        assert_eq!(
            rows,
            [
                ("disabled-mode-message".to_owned(), "expired".to_owned()),
                ("normal-mode-message".to_owned(), "received".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_drains_inflight_http_and_joins_tracked_tasks() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let route_started = Arc::clone(&started);
        let route_release = Arc::clone(&release);
        let app = Router::new().route(
            "/slow",
            get(move || {
                let started = Arc::clone(&route_started);
                let release = Arc::clone(&route_release);
                async move {
                    started.notify_one();
                    release.notified().await;
                    "complete"
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("local address");
        let supervisor = TaskSupervisor::new();
        let worker_token = supervisor.cancellation_token();
        let worker_stopped = Arc::new(Notify::new());
        let worker_ack = Arc::clone(&worker_stopped);
        let worker = supervisor.spawn(async move {
            worker_token.cancelled().await;
            worker_ack.notify_one();
        });
        let (_database_directory, state) = test_state(&supervisor).await;
        state.mark_ready();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let server_state = state.clone();
        let server = tokio::spawn(serve_with_listener(
            listener,
            app,
            server_state,
            supervisor,
            async {
                let _ = shutdown_rx.await;
                Ok(())
            },
            Duration::from_secs(2),
        ));
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.expect("connect");
            stream
                .write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .expect("write request");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .await
                .expect("read response");
            response
        });

        timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("slow handler started");
        shutdown_tx.send(()).expect("shutdown receiver");
        timeout(Duration::from_secs(1), worker_stopped.notified())
            .await
            .expect("tracked worker stopped");
        assert!(!state.is_ready());
        assert!(
            !server.is_finished(),
            "server exited before in-flight response drained"
        );

        release.notify_one();
        let response = timeout(Duration::from_secs(1), client)
            .await
            .expect("client deadline")
            .expect("client task");
        assert!(String::from_utf8_lossy(&response).contains("200 OK"));
        timeout(Duration::from_secs(1), server)
            .await
            .expect("server deadline")
            .expect("server task")
            .expect("graceful result");
        worker.await.expect("tracked worker");
    }

    #[tokio::test]
    async fn dual_listener_shutdown_drains_both_and_closes_shared_database() {
        let public_started = Arc::new(Notify::new());
        let public_release = Arc::new(Notify::new());
        let admin_started = Arc::new(Notify::new());
        let admin_release = Arc::new(Notify::new());
        let public_app = Router::new().route(
            "/slow",
            get({
                let started = Arc::clone(&public_started);
                let release = Arc::clone(&public_release);
                move || {
                    let started = Arc::clone(&started);
                    let release = Arc::clone(&release);
                    async move {
                        started.notify_one();
                        release.notified().await;
                        "public complete"
                    }
                }
            }),
        );
        let admin_app = Router::new().route(
            "/slow",
            get({
                let started = Arc::clone(&admin_started);
                let release = Arc::clone(&admin_release);
                move || {
                    let started = Arc::clone(&started);
                    let release = Arc::clone(&release);
                    async move {
                        started.notify_one();
                        release.notified().await;
                        "admin complete"
                    }
                }
            }),
        );
        let public_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("public listener");
        let public_address = public_listener.local_addr().expect("public address");
        let admin_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("admin listener");
        let admin_address = admin_listener.local_addr().expect("admin address");
        let supervisor = TaskSupervisor::new();
        let (_database_directory, state) = test_state(&supervisor).await;
        state.mark_ready();
        let health = RuntimeHealthRegistry::default();
        health.set_state(WorkerKey::PublicHttp, WorkerState::Running);
        health.set_state(WorkerKey::AdminHttp, WorkerState::Running);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server_state = state.clone();
        let server_health = health.clone();
        let server = tokio::spawn(serve_with_listeners(
            public_listener,
            public_app,
            Some((admin_listener, admin_app)),
            ServerRuntime {
                state: server_state,
                supervisor,
                health: server_health,
                deadline: Duration::from_secs(2),
            },
            async {
                let _ = shutdown_rx.await;
                Ok(())
            },
        ));
        let public_client = tokio::spawn(http_get(public_address, "/slow"));
        let admin_client = tokio::spawn(http_get(admin_address, "/slow"));
        timeout(Duration::from_secs(1), public_started.notified())
            .await
            .expect("public handler started");
        timeout(Duration::from_secs(1), admin_started.notified())
            .await
            .expect("admin handler started");

        shutdown_tx.send(()).expect("shutdown receiver");
        tokio::task::yield_now().await;
        assert!(!state.is_ready());
        assert!(!server.is_finished(), "both listeners must drain");
        public_release.notify_one();
        tokio::task::yield_now().await;
        assert!(!server.is_finished(), "Admin listener was not drained");
        admin_release.notify_one();

        for client in [public_client, admin_client] {
            let response = timeout(Duration::from_secs(1), client)
                .await
                .expect("client deadline")
                .expect("client task");
            assert!(String::from_utf8_lossy(&response).contains("200 OK"));
        }
        timeout(Duration::from_secs(1), server)
            .await
            .expect("server deadline")
            .expect("server task")
            .expect("graceful result");
        assert!(state.db.is_closed());
        let snapshot = health.snapshot();
        assert_eq!(
            snapshot
                .get(&WorkerKey::PublicHttp)
                .map(|entry| entry.state),
            Some(WorkerState::Stopped)
        );
        assert_eq!(
            snapshot.get(&WorkerKey::AdminHttp).map(|entry| entry.state),
            Some(WorkerState::Stopped)
        );
    }

    #[tokio::test]
    async fn admin_disabled_skips_bind_and_enabled_conflict_fails_fast() {
        let occupied = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("occupied listener");
        let occupied_address = occupied.local_addr().expect("occupied address");
        let mut config = Config::default();
        config.server.bind = "127.0.0.1:0".parse().expect("public address");
        config.admin.bind = occupied_address;

        let disabled = BoundListeners::bind(&config)
            .await
            .expect("disabled Admin must not bind");
        assert!(disabled.admin.is_none());
        drop(disabled);

        config.admin.enabled = true;
        let error = BoundListeners::bind(&config)
            .await
            .expect_err("Admin bind conflict");
        assert!(matches!(error, ServerError::AdminBind));
    }

    async fn http_get(address: std::net::SocketAddr, path: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(address).await.expect("connect");
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        response
    }

    #[tokio::test]
    async fn shutdown_deadline_aborts_non_cooperative_tracked_tasks() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let supervisor = TaskSupervisor::new();
        let stubborn = supervisor.spawn(std::future::pending::<()>());
        let (_database_directory, state) = test_state(&supervisor).await;
        tokio::time::pause();
        state.mark_ready();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = tokio::spawn(serve_with_listener(
            listener,
            Router::new(),
            state,
            supervisor,
            async {
                let _ = shutdown_rx.await;
                Ok(())
            },
            Duration::from_secs(15),
        ));

        shutdown_tx.send(()).expect("shutdown receiver");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(15)).await;
        let error = server
            .await
            .expect("server task")
            .expect_err("deadline error");
        assert!(matches!(error, ServerError::Shutdown(ShutdownTimeout)));
        assert!(
            stubborn
                .await
                .expect_err("stubborn task aborted")
                .is_cancelled()
        );
    }

    #[tokio::test]
    async fn shutdown_signal_listener_failure_is_propagated() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let supervisor = TaskSupervisor::new();
        let (_database_directory, state) = test_state(&supervisor).await;
        state.mark_ready();
        let error = serve_with_listener(
            listener,
            Router::new(),
            state.clone(),
            supervisor,
            async { Err(ServerError::Signal(SignalError)) },
            Duration::from_secs(1),
        )
        .await
        .expect_err("signal error");
        assert!(matches!(error, ServerError::Signal(SignalError)));
        assert!(!state.is_ready());
    }

    async fn assert_critical_worker_is_observable<F>(worker: F)
    where
        F: Future<Output = Result<(), OutboxError>> + Send + 'static,
    {
        let supervisor = TaskSupervisor::new();
        let (failure_tx, mut failure_rx) = mpsc::unbounded_channel();
        let health = RuntimeHealthRegistry::default();
        spawn_critical_worker(
            &supervisor,
            "test-worker",
            WorkerKey::Outbox,
            "TEST_WORKER_STOPPED",
            health.clone(),
            worker,
            failure_tx,
        );
        timeout(Duration::from_secs(1), failure_rx.recv())
            .await
            .expect("critical worker signal")
            .expect("failure sender");
        assert_eq!(
            health
                .snapshot()
                .get(&WorkerKey::Outbox)
                .map(|item| item.state),
            Some(WorkerState::Failed)
        );
        supervisor.begin_shutdown();
        supervisor.wait().await;
    }

    async fn panicking_worker() -> Result<(), OutboxError> {
        panic!("test worker panic")
    }

    #[tokio::test]
    async fn critical_worker_error_panic_and_unexpected_exit_are_observable() {
        assert_critical_worker_is_observable(async { Err(OutboxError::Database) }).await;
        assert_critical_worker_is_observable(panicking_worker()).await;
        assert_critical_worker_is_observable(async { Ok(()) }).await;
    }
}
