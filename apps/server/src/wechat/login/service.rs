use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use relay_provider_wechat::{
    credentials::ConnectionBundle,
    http_client::{WechatHttpClient, WechatHttpError, validate_production_endpoint},
};
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;
use url::Url;
use uuid::Uuid;

use crate::{
    auth::{DeviceAuthService, DeviceScope},
    config::WechatConfig,
    shutdown::TaskSupervisor,
    wechat::{
        monitor::WechatMonitorRuntime,
        secret_store::{
            EncryptedFileSecretStore, SecretStore as _, SecretStoreError, StagedSecret,
        },
    },
    wechat_admin_login::AdminLoginAuthorization,
};

use super::qr::{
    BlueGreenBackend, BlueGreenCoordinator, CutoverBackendError, CutoverOutcome, LoginDirective,
    LoginError, LoginManager, LoginSnapshot, LoginState, PreparedCandidate, QrLoginProvider,
    StartOutcome,
};

const QR_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub struct WechatBootstrap {
    pub(crate) store: EncryptedFileSecretStore,
    pub(crate) bundle: Option<ConnectionBundle>,
    pub(crate) status: WechatRuntimeStatus,
}

pub async fn bootstrap_wechat(
    config: &WechatConfig,
) -> Result<Option<WechatBootstrap>, SecretStoreError> {
    if !config.enabled {
        return Ok(None);
    }
    let store = EncryptedFileSecretStore::from_systemd_credential(&config.connection_file).await?;
    match store.load().await {
        Ok(Some(bundle)) if persisted_bundle_is_production_safe(&bundle) => {
            Ok(Some(WechatBootstrap {
                status: WechatRuntimeStatus::ConnectedAwaitingActivation,
                store,
                bundle: Some(bundle),
            }))
        }
        Ok(None) => Ok(Some(WechatBootstrap {
            status: WechatRuntimeStatus::Disconnected,
            store,
            bundle: None,
        })),
        Ok(Some(_)) | Err(_) => {
            tracing::warn!(
                operation = "wechat.secret.load",
                error_code = "WECHAT_CREDENTIALS_UNREADABLE",
                "encrypted WeChat credentials are unreadable"
            );
            Ok(Some(WechatBootstrap {
                store,
                bundle: None,
                status: WechatRuntimeStatus::CredentialsUnreadable,
            }))
        }
    }
}

fn persisted_bundle_is_production_safe(bundle: &ConnectionBundle) -> bool {
    Url::parse(&bundle.credentials.base_url)
        .ok()
        .is_some_and(|url| validate_production_endpoint(&url).is_ok())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WechatRuntimeStatus {
    Disconnected,
    ConnectedAwaitingActivation,
    CredentialsUnreadable,
    Degraded,
}

struct RuntimeConnection {
    inner: Mutex<RuntimeConnectionInner>,
}

struct RuntimeConnectionInner {
    bundle: Option<ConnectionBundle>,
    status: WechatRuntimeStatus,
}

impl RuntimeConnection {
    fn new(bundle: Option<ConnectionBundle>, status: WechatRuntimeStatus) -> Self {
        Self {
            inner: Mutex::new(RuntimeConnectionInner { bundle, status }),
        }
    }

    fn has_active_credentials(&self) -> bool {
        self.lock().bundle.is_some()
    }

    fn snapshot_bundle(&self) -> Option<ConnectionBundle> {
        self.lock().bundle.clone()
    }

    fn activate(&self, bundle: ConnectionBundle) {
        let mut inner = self.lock();
        inner.bundle = Some(bundle);
        inner.status = WechatRuntimeStatus::ConnectedAwaitingActivation;
    }

    fn mark_degraded(&self) {
        self.lock().status = WechatRuntimeStatus::Degraded;
    }

    fn restore_safe_status(&self) {
        let mut inner = self.lock();
        inner.status = if inner.bundle.is_some() {
            WechatRuntimeStatus::ConnectedAwaitingActivation
        } else {
            WechatRuntimeStatus::Disconnected
        };
    }

    fn disconnect(&self, durable_secret_cleared: bool) {
        let mut inner = self.lock();
        inner.bundle = None;
        inner.status = if durable_secret_cleared {
            WechatRuntimeStatus::Disconnected
        } else {
            WechatRuntimeStatus::CredentialsUnreadable
        };
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RuntimeConnectionInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct StagedCandidate {
    secret: StagedSecret,
    bundle: ConnectionBundle,
}

struct RuntimeBackend {
    store: EncryptedFileSecretStore,
    runtime: Arc<RuntimeConnection>,
    manager: LoginManager,
    device_auth: DeviceAuthService,
    staging_gate: AsyncMutex<()>,
    staged: AsyncMutex<HashMap<Uuid, StagedCandidate>>,
    monitor: Option<WechatMonitorRuntime>,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("WeChat disconnect could not durably clear all connection state")]
pub struct WechatDisconnectError;

impl RuntimeBackend {
    async fn candidate_authorization_is_current(&self, candidate: &PreparedCandidate) -> bool {
        if candidate.operator_authorized {
            return true;
        }
        let Some(authorization) = &candidate.admin_authorization else {
            return self
                .device_auth
                .is_device_enabled(candidate.owner_device_id)
                .await
                .unwrap_or(false);
        };
        if authorization.owner_device_id() != candidate.owner_device_id
            || !authorization.generation_is_current()
        {
            return false;
        }
        match self
            .device_auth
            .gateway_authorization_lease(candidate.owner_device_id)
            .await
        {
            Ok(Some(lease)) => {
                lease.credential_revision == authorization.authorization_revision()
                    && lease.scopes.contains(&DeviceScope::ChannelRead)
                    && lease.scopes.contains(&DeviceScope::ChannelManage)
                    && authorization.generation_is_current()
            }
            Ok(None) | Err(_) => false,
        }
    }

    async fn disconnect(&self) -> Result<(), WechatDisconnectError> {
        // Candidate staging intentionally happens outside the cutover mutex.
        // This second, narrowly-scoped gate lets disconnect wait for any stage
        // writer before draining it, without weakening blue/green availability.
        let _staging_guard = self.staging_gate.lock().await;
        let _ = self.manager.cancel_candidate_for_disconnect();

        let staged: Vec<StagedSecret> = {
            let mut staged = self.staged.lock().await;
            staged
                .drain()
                .map(|(_, candidate)| candidate.secret)
                .collect()
        };
        let mut failed = false;
        for secret in staged {
            if self.store.discard(secret).await.is_err() {
                failed = true;
            }
        }

        if let Some(monitor) = &self.monitor
            && monitor.stop_and_clear().await.is_err()
        {
            failed = true;
        }

        let durable_secret_cleared = self.store.clear().await.is_ok();
        if !durable_secret_cleared {
            failed = true;
        }
        // Always remove secret-bearing process state. If durable deletion was
        // uncertain, expose a fail-closed unreadable state rather than claiming
        // a clean disconnect.
        self.runtime.disconnect(durable_secret_cleared);

        if failed {
            Err(WechatDisconnectError)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl BlueGreenBackend for Arc<RuntimeBackend> {
    async fn stage_candidate(
        &self,
        candidate: &PreparedCandidate,
    ) -> Result<(), CutoverBackendError> {
        let _staging_guard = self.staging_gate.lock().await;
        if !persisted_bundle_is_production_safe(&candidate.bundle) {
            return Err(CutoverBackendError);
        }
        let secret = self
            .store
            .stage(&candidate.bundle)
            .await
            .map_err(|_| CutoverBackendError)?;
        let mut staged = self.staged.lock().await;
        if staged.contains_key(&candidate.login_id) {
            drop(staged);
            let _ = self.store.discard(secret).await;
            return Err(CutoverBackendError);
        }
        staged.insert(
            candidate.login_id,
            StagedCandidate {
                secret,
                bundle: candidate.bundle.clone(),
            },
        );
        Ok(())
    }

    async fn validate_candidate_current(
        &self,
        candidate: &PreparedCandidate,
    ) -> Result<(), CutoverBackendError> {
        if self.candidate_authorization_is_current(candidate).await
            && self
                .manager
                .begin_cutover(candidate.login_id, candidate.owner_device_id)
        {
            Ok(())
        } else {
            Err(CutoverBackendError)
        }
    }

    async fn validate_before_promote(
        &self,
        candidate: &PreparedCandidate,
    ) -> Result<(), CutoverBackendError> {
        if self.candidate_authorization_is_current(candidate).await
            && self
                .manager
                .cutover_is_committing(candidate.login_id, candidate.owner_device_id)
        {
            Ok(())
        } else {
            Err(CutoverBackendError)
        }
    }

    async fn stop_and_join_active(&self) -> Result<(), CutoverBackendError> {
        if let Some(monitor) = &self.monitor {
            monitor.stop().await.map_err(|_| CutoverBackendError)?;
        }
        Ok(())
    }

    async fn promote_staged(&self, login_id: Uuid) -> Result<(), CutoverBackendError> {
        let Some(staged) = self.staged.lock().await.remove(&login_id) else {
            return Err(CutoverBackendError);
        };
        let committed = match self.store.commit(staged.secret).await {
            Ok(()) | Err(SecretStoreError::CommitDurabilityUncertain(_)) => true,
            Err(_) => false,
        };
        if !committed {
            return Err(CutoverBackendError);
        }
        self.runtime.activate(staged.bundle);
        Ok(())
    }

    async fn start_promoted(&self) -> Result<(), CutoverBackendError> {
        if let Some(monitor) = &self.monitor {
            let bundle = self.runtime.snapshot_bundle().ok_or(CutoverBackendError)?;
            monitor
                .replace_bundle(bundle)
                .await
                .map_err(|_| CutoverBackendError)?;
        }
        Ok(())
    }

    async fn discard_staged(&self, login_id: Uuid) {
        if let Some(staged) = self.staged.lock().await.remove(&login_id) {
            let _ = self.store.discard(staged.secret).await;
        }
    }

    async fn resume_active_after_failed_cutover(&self) {
        self.runtime.restore_safe_status();
        if let Some(monitor) = &self.monitor {
            let bundle = monitor
                .handle()
                .snapshot_bundle()
                .or_else(|| self.runtime.snapshot_bundle());
            if let Some(bundle) = bundle {
                let _ = monitor.replace_bundle(bundle).await;
            }
        }
    }

    async fn mark_promoted_degraded(&self) {
        self.runtime.mark_degraded();
        if let Some(monitor) = &self.monitor {
            monitor
                .handle()
                .mark_reconnect_required("WECHAT_CUTOVER_ACTIVATED_DEGRADED");
        }
    }
}

#[derive(Clone)]
pub struct WechatLoginService {
    manager: LoginManager,
    provider: Arc<dyn QrLoginProvider>,
    coordinator: Arc<BlueGreenCoordinator<Arc<RuntimeBackend>>>,
    backend: Arc<RuntimeBackend>,
    runtime: Arc<RuntimeConnection>,
    supervisor: TaskSupervisor,
    shutdown: tokio_util::sync::CancellationToken,
}

impl WechatLoginService {
    pub fn production(
        bootstrap: WechatBootstrap,
        device_auth: DeviceAuthService,
        supervisor: TaskSupervisor,
        monitor: WechatMonitorRuntime,
    ) -> Result<Self, WechatHttpError> {
        let provider = Arc::new(WechatHttpClient::production(format!(
            "PromptDockRelay/{}",
            env!("CARGO_PKG_VERSION")
        ))?);
        Ok(Self::new(
            bootstrap,
            device_auth,
            supervisor,
            provider,
            Some(monitor),
        ))
    }

    fn new(
        bootstrap: WechatBootstrap,
        device_auth: DeviceAuthService,
        supervisor: TaskSupervisor,
        provider: Arc<dyn QrLoginProvider>,
        monitor: Option<WechatMonitorRuntime>,
    ) -> Self {
        let manager = LoginManager::new();
        let runtime = Arc::new(RuntimeConnection::new(bootstrap.bundle, bootstrap.status));
        let backend = Arc::new(RuntimeBackend {
            store: bootstrap.store,
            runtime: Arc::clone(&runtime),
            manager: manager.clone(),
            device_auth,
            staging_gate: AsyncMutex::new(()),
            staged: AsyncMutex::new(HashMap::new()),
            monitor,
        });
        let coordinator = Arc::new(BlueGreenCoordinator::new(Arc::clone(&backend)));
        Self {
            manager,
            provider,
            coordinator,
            backend,
            runtime,
            shutdown: supervisor.cancellation_token(),
            supervisor,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        store: EncryptedFileSecretStore,
        bundle: Option<ConnectionBundle>,
        device_auth: DeviceAuthService,
        supervisor: TaskSupervisor,
        provider: Arc<dyn QrLoginProvider>,
    ) -> Self {
        let status = if bundle.is_some() {
            WechatRuntimeStatus::ConnectedAwaitingActivation
        } else {
            WechatRuntimeStatus::Disconnected
        };
        Self::new(
            WechatBootstrap {
                store,
                bundle,
                status,
            },
            device_auth,
            supervisor,
            provider,
            None,
        )
    }

    pub fn start(
        &self,
        owner_device_id: Uuid,
        force_fresh: bool,
    ) -> Result<StartOutcome, LoginError> {
        let outcome = self.manager.start(
            owner_device_id,
            force_fresh,
            self.runtime.has_active_credentials(),
        )?;
        self.spawn_login_worker_if_created(&outcome)?;
        Ok(outcome)
    }

    fn spawn_login_worker_if_created(&self, outcome: &StartOutcome) -> Result<(), LoginError> {
        if outcome.created && outcome.snapshot.state == LoginState::FetchingQr {
            let login_id =
                Uuid::parse_str(&outcome.snapshot.login_id).map_err(|_| LoginError::Clock)?;
            let service = self.clone();
            let worker = self.supervisor.spawn(async move {
                service.run_login(login_id).await;
            });
            let manager = self.manager.clone();
            let shutdown = self.shutdown.clone();
            self.supervisor.spawn(async move {
                match worker.await {
                    Ok(()) if !shutdown.is_cancelled() => {
                        if manager.fail_unfinished_worker(login_id).unwrap_or(false) {
                            tracing::error!(
                                operation = "wechat.login.worker",
                                error_code = "WECHAT_LOGIN_WORKER_EXITED",
                                "WeChat login worker exited before reaching a terminal state"
                            );
                        }
                    }
                    Err(error) if !error.is_cancelled() => {
                        let _ = manager.fail_unfinished_worker(login_id);
                        tracing::error!(
                            operation = "wechat.login.worker",
                            error_code = "WECHAT_LOGIN_WORKER_PANICKED",
                            "WeChat login worker panicked"
                        );
                    }
                    Ok(()) | Err(_) => {}
                }
            });
        }
        Ok(())
    }

    pub fn start_admin(
        &self,
        owner_device_id: Uuid,
        force_fresh: bool,
        authorization: AdminLoginAuthorization,
    ) -> Result<StartOutcome, LoginError> {
        let outcome = self.manager.start_admin(
            owner_device_id,
            force_fresh,
            self.runtime.has_active_credentials(),
            authorization,
        )?;
        self.spawn_login_worker_if_created(&outcome)?;
        Ok(outcome)
    }

    pub fn start_operator(&self, force_fresh: bool) -> Result<StartOutcome, LoginError> {
        let outcome = self.manager.start_operator(
            admin_operator_owner_id(),
            force_fresh,
            self.runtime.has_active_credentials(),
        )?;
        self.spawn_login_worker_if_created(&outcome)?;
        Ok(outcome)
    }

    pub fn operator_snapshot(&self, login_id: Uuid) -> Result<LoginSnapshot, LoginError> {
        self.snapshot(admin_operator_owner_id(), login_id)
    }

    pub fn operator_verify(&self, login_id: Uuid, code: &str) -> Result<LoginSnapshot, LoginError> {
        self.verify(admin_operator_owner_id(), login_id, code)
    }

    pub fn operator_cancel(&self, login_id: Uuid) -> Result<LoginSnapshot, LoginError> {
        self.cancel(admin_operator_owner_id(), login_id)
    }

    pub fn snapshot(
        &self,
        owner_device_id: Uuid,
        login_id: Uuid,
    ) -> Result<LoginSnapshot, LoginError> {
        self.manager.snapshot(owner_device_id, login_id)
    }

    pub fn verify(
        &self,
        owner_device_id: Uuid,
        login_id: Uuid,
        code: &str,
    ) -> Result<LoginSnapshot, LoginError> {
        self.manager
            .submit_verify_code(owner_device_id, login_id, code)
    }

    pub fn cancel(
        &self,
        owner_device_id: Uuid,
        login_id: Uuid,
    ) -> Result<LoginSnapshot, LoginError> {
        self.manager.cancel(owner_device_id, login_id)
    }

    /// Disconnects the process-wide WeChat channel. Authorization is enforced
    /// by the caller; this operation deliberately has no login-owner check, so
    /// any authenticated device can tear down the shared channel.
    pub async fn disconnect(&self) -> Result<(), WechatDisconnectError> {
        let _cutover = self.coordinator.lock_cutover().await;
        self.backend.disconnect().await
    }

    async fn run_login(self, login_id: Uuid) {
        let cancellation = match self.manager.worker_cancellation(login_id) {
            Ok(token) => token,
            Err(_) => return,
        };
        loop {
            let fetched = tokio::select! {
                _ = self.shutdown.cancelled() => return,
                result = self.provider.fetch_qr(&[], &cancellation) => result,
            };
            let response = match fetched {
                Ok(response) => response,
                Err(_) if cancellation.is_cancelled() || self.shutdown.is_cancelled() => return,
                Err(_) => {
                    let _ = self.manager.fail_provider(login_id);
                    return;
                }
            };
            if self.manager.install_qr(login_id, response).is_err() {
                return;
            }

            loop {
                let input = match self.manager.poll_input(login_id) {
                    Ok(input) => input,
                    Err(_) => return,
                };
                let polled = tokio::select! {
                    _ = self.shutdown.cancelled() => return,
                    result = self.provider.poll_status(&input) => result,
                };
                let response = match polled {
                    Ok(response) => response,
                    Err(_) if input.cancellation.is_cancelled() || self.shutdown.is_cancelled() => {
                        return;
                    }
                    Err(_) => {
                        let _ = self.manager.fail_provider(login_id);
                        return;
                    }
                };
                let directive = match self.manager.apply_provider_status(
                    login_id,
                    input.generation,
                    input.verify_revision,
                    response,
                    self.runtime.has_active_credentials(),
                ) {
                    Ok(directive) => directive,
                    Err(_) => return,
                };
                match directive {
                    LoginDirective::ContinuePolling | LoginDirective::IgnoreStale => {
                        tokio::select! {
                            _ = self.shutdown.cancelled() => return,
                            _ = input.cancellation.cancelled() => return,
                            _ = tokio::time::sleep(QR_POLL_INTERVAL) => {}
                        }
                    }
                    LoginDirective::RefreshQr { .. } => break,
                    LoginDirective::Terminal(_) => return,
                    LoginDirective::Cutover(candidate) => {
                        let outcome: CutoverOutcome = self.coordinator.cutover(&candidate).await;
                        let _ = self.manager.finish_cutover(login_id, outcome);
                        return;
                    }
                }
            }
        }
    }
}

fn admin_operator_owner_id() -> Uuid {
    Uuid::from_u128(0x7f4c_6bc4_4491_4d48_9c49_6c86_0f27_5001)
}

#[cfg(test)]
mod tests {
    use relay_provider_wechat::protocol::{
        GetBotQrCodeResponse, GetQrCodeStatusResponse, QrCodeStatus,
    };
    use tokio::sync::Notify;

    use super::*;
    use crate::{
        auth::DeviceScope,
        config::DatabaseConfig,
        outbox::{OutboxService, RelayNotificationV1},
        qr_login::{CutoverStep, PollInput, QrLoginProviderError},
        state::AppState,
        wechat_admin_login::{
            AdminLoginAccessRegistry, AdminLoginAuthorization, WechatAdminLoginGrantRegistry,
        },
        wechat_monitor::{NoopMonitorActivationHook, WechatMonitorRuntime},
    };

    struct PanicProvider;

    #[derive(Default)]
    struct BlockingProvider {
        fetch_started: Notify,
    }

    #[async_trait]
    impl QrLoginProvider for BlockingProvider {
        async fn fetch_qr(
            &self,
            _local_token_list: &[String],
            cancellation: &tokio_util::sync::CancellationToken,
        ) -> Result<GetBotQrCodeResponse, QrLoginProviderError> {
            self.fetch_started.notify_one();
            cancellation.cancelled().await;
            Err(QrLoginProviderError)
        }

        async fn poll_status(
            &self,
            _input: &PollInput,
        ) -> Result<GetQrCodeStatusResponse, QrLoginProviderError> {
            unreachable!("blocking provider never installs a QR")
        }
    }

    #[async_trait]
    impl QrLoginProvider for PanicProvider {
        async fn fetch_qr(
            &self,
            _local_token_list: &[String],
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> Result<GetBotQrCodeResponse, QrLoginProviderError> {
            panic!("provider panic sentinel")
        }

        async fn poll_status(
            &self,
            _input: &PollInput,
        ) -> Result<GetQrCodeStatusResponse, QrLoginProviderError> {
            unreachable!("fetch always panics")
        }
    }

    fn confirmed_candidate(manager: &LoginManager, owner: Uuid) -> Box<PreparedCandidate> {
        let started = manager.start(owner, false, false).expect("start");
        let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("login id");
        let generation = manager
            .install_qr(
                login_id,
                GetBotQrCodeResponse {
                    qrcode: "qr-secret".into(),
                    qrcode_img_content: "qr-content".into(),
                },
            )
            .expect("install QR");
        match manager
            .apply_provider_status(
                login_id,
                generation,
                0,
                GetQrCodeStatusResponse {
                    status: QrCodeStatus::Confirmed,
                    bot_token: Some("bot-secret".into()),
                    ilink_bot_id: Some("bot-id".into()),
                    baseurl: Some("https://ilinkai.weixin.qq.com/".into()),
                    ilink_user_id: Some("user-id".into()),
                    redirect_host: None,
                },
                false,
            )
            .expect("confirmed")
        {
            LoginDirective::Cutover(candidate) => candidate,
            other => panic!("unexpected directive: {other:?}"),
        }
    }

    fn confirmed_admin_candidate(
        manager: &LoginManager,
        owner: Uuid,
        authorization: AdminLoginAuthorization,
    ) -> Box<PreparedCandidate> {
        let started = manager
            .start_admin(owner, false, false, authorization)
            .expect("start authorized login");
        let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("login id");
        let generation = manager
            .install_qr(
                login_id,
                GetBotQrCodeResponse {
                    qrcode: "admin-qr-secret".into(),
                    qrcode_img_content: "admin-qr-content".into(),
                },
            )
            .expect("install QR");
        match manager
            .apply_provider_status(
                login_id,
                generation,
                0,
                GetQrCodeStatusResponse {
                    status: QrCodeStatus::Confirmed,
                    bot_token: Some("admin-bot-secret".into()),
                    ilink_bot_id: Some("admin-bot-id".into()),
                    baseurl: Some("https://ilinkai.weixin.qq.com/".into()),
                    ilink_user_id: Some("admin-user-id".into()),
                    redirect_host: None,
                },
                false,
            )
            .expect("confirmed")
        {
            LoginDirective::Cutover(candidate) => candidate,
            other => panic!("unexpected directive: {other:?}"),
        }
    }

    fn admin_authorization(
        grants: &WechatAdminLoginGrantRegistry,
        access: &AdminLoginAccessRegistry,
        owner: Uuid,
        revision: String,
    ) -> AdminLoginAuthorization {
        let receipt = grants
            .issue(owner, revision, Duration::from_secs(600))
            .expect("grant");
        let consumption = grants
            .begin_consume(receipt.grant_id, receipt.grant_token())
            .expect("consume grant");
        assert!(grants.finish_consumption(&consumption, true));
        AdminLoginAuthorization::new(grants.clone(), access.clone(), &consumption)
    }

    #[test]
    fn admin_authorization_cannot_rebind_an_already_prepared_legacy_candidate() {
        let manager = LoginManager::new();
        let owner = Uuid::new_v4();
        let started = manager.start(owner, false, false).expect("legacy start");
        let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("login id");
        let generation = manager
            .install_qr(
                login_id,
                GetBotQrCodeResponse {
                    qrcode: "legacy-qr-secret".into(),
                    qrcode_img_content: "legacy-qr-content".into(),
                },
            )
            .expect("install QR");
        let directive = manager
            .apply_provider_status(
                login_id,
                generation,
                0,
                GetQrCodeStatusResponse {
                    status: QrCodeStatus::Confirmed,
                    bot_token: Some("legacy-bot-secret".into()),
                    ilink_bot_id: Some("legacy-bot-id".into()),
                    baseurl: Some("https://ilinkai.weixin.qq.com/".into()),
                    ilink_user_id: Some("legacy-user-id".into()),
                    redirect_host: None,
                },
                false,
            )
            .expect("confirmed");
        assert!(matches!(directive, LoginDirective::Cutover(_)));

        let (grants, access) = WechatAdminLoginGrantRegistry::new_pair();
        assert!(matches!(
            manager.start_admin(
                owner,
                false,
                false,
                admin_authorization(&grants, &access, owner, "revision".to_owned()),
            ),
            Err(LoginError::Terminal)
        ));
    }

    #[test]
    fn persisted_bundle_must_revalidate_the_production_endpoint() {
        let manager = LoginManager::new();
        let owner = Uuid::new_v4();
        let mut candidate = confirmed_candidate(&manager, owner);
        assert!(persisted_bundle_is_production_safe(&candidate.bundle));

        candidate.bundle.credentials.base_url = "http://127.0.0.1/private".into();
        assert!(!persisted_bundle_is_production_safe(&candidate.bundle));
    }

    async fn test_backend(
        directory: &tempfile::TempDir,
        auth: DeviceAuthService,
        manager: LoginManager,
    ) -> (Arc<RuntimeBackend>, EncryptedFileSecretStore) {
        let store = EncryptedFileSecretStore::new_for_test(
            directory.path().join("wechat-connection.enc"),
            [9; 32],
        );
        let runtime = Arc::new(RuntimeConnection::new(
            None,
            WechatRuntimeStatus::Disconnected,
        ));
        (
            Arc::new(RuntimeBackend {
                store: store.clone(),
                runtime,
                manager,
                device_auth: auth,
                staging_gate: AsyncMutex::new(()),
                staged: AsyncMutex::new(HashMap::new()),
                monitor: None,
            }),
            store,
        )
    }

    fn unix_timestamp_ms() -> i64 {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_millis(),
        )
        .expect("timestamp")
    }

    #[tokio::test]
    async fn concrete_cutover_promotes_encrypted_bundle_and_revalidates_owner() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = crate::db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let auth = DeviceAuthService::new(pool.clone());
        let owner = auth
            .create_device("CUTOVER OWNER", &crate::auth::DeviceScope::ALL)
            .await
            .expect("device");
        let manager = LoginManager::new();
        let candidate = confirmed_candidate(&manager, owner.id);
        let (backend, store) = test_backend(&directory, auth.clone(), manager.clone()).await;
        let coordinator = BlueGreenCoordinator::new(backend);
        assert_eq!(
            coordinator.cutover(&candidate).await,
            CutoverOutcome::Activated
        );
        let loaded = store.load().await.expect("load").expect("bundle");
        assert_eq!(loaded.credentials.user_id, "user-id");

        let manager = LoginManager::new();
        let candidate = confirmed_candidate(&manager, owner.id);
        auth.revoke_device(&owner.id.to_string())
            .await
            .expect("revoke");
        let second_directory = tempfile::tempdir().expect("second directory");
        let (backend, second_store) = test_backend(&second_directory, auth, manager).await;
        let coordinator = BlueGreenCoordinator::new(backend);
        assert_eq!(
            coordinator.cutover(&candidate).await,
            CutoverOutcome::Rejected {
                step: CutoverStep::ValidateCurrent
            }
        );
        assert!(second_store.load().await.expect("load").is_none());
        pool.close().await;
    }

    #[derive(Clone, Copy, Debug)]
    enum AuthorizationMutation {
        Rotate,
        Disable,
        Revoke,
        RemoveScope,
        InvalidateGeneration,
    }

    #[tokio::test]
    async fn authorization_mutations_while_candidate_is_staged_never_promote() {
        for mutation in [
            AuthorizationMutation::Rotate,
            AuthorizationMutation::Disable,
            AuthorizationMutation::Revoke,
            AuthorizationMutation::RemoveScope,
            AuthorizationMutation::InvalidateGeneration,
        ] {
            let directory = tempfile::tempdir().expect("directory");
            let pool = crate::db::open(&DatabaseConfig {
                path: directory.path().join("relay.db"),
                ..DatabaseConfig::default()
            })
            .await
            .expect("database");
            let auth = DeviceAuthService::new(pool.clone());
            let owner = auth
                .create_device(
                    "ADMIN CUTOVER OWNER",
                    &[DeviceScope::ChannelRead, DeviceScope::ChannelManage],
                )
                .await
                .expect("owner");
            let lease = auth
                .gateway_authorization_lease(owner.id)
                .await
                .expect("lease query")
                .expect("lease");
            let (grants, access) = WechatAdminLoginGrantRegistry::new_pair();
            let manager = LoginManager::new();
            let candidate = confirmed_admin_candidate(
                &manager,
                owner.id,
                admin_authorization(&grants, &access, owner.id, lease.credential_revision),
            );
            let login_id = candidate.login_id;
            let (backend, store) = test_backend(&directory, auth.clone(), manager.clone()).await;
            let coordinator = Arc::new(BlueGreenCoordinator::new(backend.clone()));
            let cutover_guard = coordinator.lock_cutover().await;
            let cutover = tokio::spawn({
                let coordinator = coordinator.clone();
                async move { coordinator.cutover(&candidate).await }
            });
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if backend.staged.lock().await.contains_key(&login_id) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("candidate staged");

            match mutation {
                AuthorizationMutation::Rotate => {
                    auth.rotate_device(
                        &owner.id.to_string(),
                        &[DeviceScope::ChannelRead, DeviceScope::ChannelManage],
                    )
                    .await
                    .expect("rotate");
                }
                AuthorizationMutation::Disable => {
                    auth.set_device_enabled(&owner.id.to_string(), false)
                        .await
                        .expect("disable");
                }
                AuthorizationMutation::Revoke => {
                    auth.revoke_device(&owner.id.to_string())
                        .await
                        .expect("revoke");
                }
                AuthorizationMutation::RemoveScope => {
                    auth.rotate_device(&owner.id.to_string(), &[DeviceScope::ChannelRead])
                        .await
                        .expect("remove manage scope");
                }
                AuthorizationMutation::InvalidateGeneration => grants.invalidate_device(owner.id),
            }
            drop(cutover_guard);

            assert_eq!(
                cutover.await.expect("cutover task"),
                CutoverOutcome::Rejected {
                    step: CutoverStep::ValidateCurrent
                },
                "{mutation:?}"
            );
            assert!(store.load().await.expect("load").is_none(), "{mutation:?}");
            assert!(!backend.runtime.has_active_credentials(), "{mutation:?}");
            assert!(backend.staged.lock().await.is_empty(), "{mutation:?}");
            pool.close().await;
        }
    }

    #[tokio::test]
    async fn authorization_mutations_after_begin_cutover_fail_the_prepromote_recheck() {
        for mutation in [
            AuthorizationMutation::Rotate,
            AuthorizationMutation::Disable,
            AuthorizationMutation::Revoke,
            AuthorizationMutation::RemoveScope,
            AuthorizationMutation::InvalidateGeneration,
        ] {
            let directory = tempfile::tempdir().expect("directory");
            let pool = crate::db::open(&DatabaseConfig {
                path: directory.path().join("relay.db"),
                ..DatabaseConfig::default()
            })
            .await
            .expect("database");
            let auth = DeviceAuthService::new(pool.clone());
            let owner = auth
                .create_device(
                    "ADMIN PREPROMOTE OWNER",
                    &[DeviceScope::ChannelRead, DeviceScope::ChannelManage],
                )
                .await
                .expect("owner");
            let lease = auth
                .gateway_authorization_lease(owner.id)
                .await
                .expect("lease query")
                .expect("lease");
            let (grants, access) = WechatAdminLoginGrantRegistry::new_pair();
            let manager = LoginManager::new();
            let candidate = confirmed_admin_candidate(
                &manager,
                owner.id,
                admin_authorization(&grants, &access, owner.id, lease.credential_revision),
            );
            let (backend, store) = test_backend(&directory, auth.clone(), manager).await;
            backend
                .stage_candidate(&candidate)
                .await
                .expect("stage candidate");
            backend
                .validate_candidate_current(&candidate)
                .await
                .expect("initial authorization");

            match mutation {
                AuthorizationMutation::Rotate => {
                    auth.rotate_device(
                        &owner.id.to_string(),
                        &[DeviceScope::ChannelRead, DeviceScope::ChannelManage],
                    )
                    .await
                    .expect("rotate");
                }
                AuthorizationMutation::Disable => {
                    auth.set_device_enabled(&owner.id.to_string(), false)
                        .await
                        .expect("disable");
                }
                AuthorizationMutation::Revoke => {
                    auth.revoke_device(&owner.id.to_string())
                        .await
                        .expect("revoke");
                }
                AuthorizationMutation::RemoveScope => {
                    auth.rotate_device(&owner.id.to_string(), &[DeviceScope::ChannelRead])
                        .await
                        .expect("remove manage scope");
                }
                AuthorizationMutation::InvalidateGeneration => grants.invalidate_device(owner.id),
            }
            assert_eq!(
                backend.validate_before_promote(&candidate).await,
                Err(CutoverBackendError),
                "{mutation:?}"
            );
            backend.discard_staged(candidate.login_id).await;
            assert!(store.load().await.expect("load").is_none(), "{mutation:?}");
            assert!(!backend.runtime.has_active_credentials(), "{mutation:?}");
            pool.close().await;
        }
    }

    #[tokio::test]
    async fn shutdown_after_staging_invalidates_the_process_epoch_before_promotion() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = crate::db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let supervisor = TaskSupervisor::new();
        let app = AppState::new(pool.clone(), supervisor.clone());
        let owner = app
            .device_auth
            .create_device(
                "ADMIN SHUTDOWN OWNER",
                &[DeviceScope::ChannelRead, DeviceScope::ChannelManage],
            )
            .await
            .expect("owner");
        let lease = app
            .device_auth
            .gateway_authorization_lease(owner.id)
            .await
            .expect("lease query")
            .expect("lease");
        let receipt = app
            .wechat_admin_login_grants
            .issue(
                owner.id,
                lease.credential_revision,
                Duration::from_secs(600),
            )
            .expect("grant");
        let consumption = app
            .wechat_admin_login_grants
            .begin_consume(receipt.grant_id, receipt.grant_token())
            .expect("consume");
        assert!(
            app.wechat_admin_login_grants
                .finish_consumption(&consumption, true)
        );
        let manager = LoginManager::new();
        let candidate = confirmed_admin_candidate(
            &manager,
            owner.id,
            AdminLoginAuthorization::new(
                app.wechat_admin_login_grants.clone(),
                app.admin_login_access.clone(),
                &consumption,
            ),
        );
        let login_id = candidate.login_id;
        app.admin_login_access.insert(login_id, &consumption);
        let (backend, store) = test_backend(&directory, app.device_auth.clone(), manager).await;
        let coordinator = Arc::new(BlueGreenCoordinator::new(backend.clone()));
        let cutover_guard = coordinator.lock_cutover().await;
        let cutover = tokio::spawn({
            let coordinator = coordinator.clone();
            async move { coordinator.cutover(&candidate).await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if backend.staged.lock().await.contains_key(&login_id) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("candidate staged");

        app.mark_not_ready().await;
        supervisor.begin_shutdown();
        assert!(app.admin_login_access.resolve(login_id).is_none());
        assert!(!app.wechat_admin_login_grants.authorization_matches(
            owner.id,
            consumption.generation,
            consumption.process_epoch,
        ));
        drop(cutover_guard);

        assert_eq!(
            cutover.await.expect("cutover task"),
            CutoverOutcome::Rejected {
                step: CutoverStep::ValidateCurrent
            }
        );
        assert!(store.load().await.expect("load").is_none());
        assert!(!backend.runtime.has_active_credentials());
        assert!(backend.staged.lock().await.is_empty());
        pool.close().await;
    }

    #[test]
    fn admin_access_is_removed_on_async_terminal_legacy_cancel_expiry_and_purge() {
        for transition in ["provider", "legacy-cancel", "expiry"] {
            let manager = if transition == "expiry" {
                LoginManager::with_limits(Duration::from_millis(5), Duration::from_millis(5))
            } else {
                LoginManager::new()
            };
            let owner = Uuid::new_v4();
            let (grants, access) = WechatAdminLoginGrantRegistry::new_pair();
            let authorization = admin_authorization(
                &grants,
                &access,
                owner,
                "test-authorization-revision".to_owned(),
            );
            let started = manager
                .start_admin(owner, false, false, authorization)
                .expect("start");
            let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("login id");
            let receipt = grants
                .issue(owner, "second-revision".to_owned(), Duration::from_secs(60))
                .expect("access fixture grant");
            let consumption = grants
                .begin_consume(receipt.grant_id, receipt.grant_token())
                .expect("access fixture consume");
            access.insert(login_id, &consumption);
            assert!(access.resolve(login_id).is_some());

            match transition {
                "provider" => {
                    manager.fail_provider(login_id).expect("provider terminal");
                }
                "legacy-cancel" => {
                    manager
                        .cancel(owner, login_id)
                        .expect("legacy owner cancel");
                }
                "expiry" => {
                    std::thread::sleep(Duration::from_millis(7));
                    let snapshot = manager.snapshot(owner, login_id).expect("expired snapshot");
                    assert_eq!(snapshot.state, LoginState::Expired);
                    std::thread::sleep(Duration::from_millis(7));
                    assert_eq!(manager.snapshot(owner, login_id), Err(LoginError::NotFound));
                }
                _ => unreachable!(),
            }
            assert!(access.resolve(login_id).is_none(), "{transition}");
        }
    }

    #[tokio::test]
    async fn worker_panic_is_observed_and_fails_the_session_closed() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = crate::db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let auth = DeviceAuthService::new(pool.clone());
        let supervisor = TaskSupervisor::new();
        let service = WechatLoginService::for_test(
            EncryptedFileSecretStore::new_for_test(
                directory.path().join("wechat-connection.enc"),
                [7; 32],
            ),
            None,
            auth,
            supervisor.clone(),
            Arc::new(PanicProvider),
        );
        let owner = Uuid::new_v4();
        let started = service.start(owner, false).expect("start");
        let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("login id");

        let mut snapshot = started.snapshot;
        for _ in 0..100 {
            snapshot = service.snapshot(owner, login_id).expect("snapshot");
            if snapshot.state == LoginState::Failed {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(snapshot.state, LoginState::Failed);
        assert_eq!(
            snapshot.error_code,
            Some("WECHAT_LOGIN_PROVIDER_UNAVAILABLE")
        );

        supervisor.begin_shutdown();
        supervisor.wait().await;
        pool.close().await;
    }

    #[tokio::test]
    async fn disconnect_cancels_candidate_clears_all_connection_state_and_keeps_outbox() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = crate::db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let auth = DeviceAuthService::new(pool.clone());
        let device = auth
            .create_device("DISCONNECT DEVICE", &crate::auth::DeviceScope::ALL)
            .await
            .expect("device");
        let active = confirmed_candidate(&LoginManager::new(), device.id)
            .bundle
            .clone();
        let store = EncryptedFileSecretStore::new_for_test(
            directory.path().join("wechat-connection.enc"),
            [4; 32],
        );
        store.save(&active).await.expect("active secret");

        let monitor = WechatMonitorRuntime::new_with_bundle(
            WechatHttpClient::production("PromptDockRelay/test").expect("client"),
            Arc::new(store.clone()),
            Arc::new(crate::inbound::DurableInboundMessageSink::new(
                crate::inbound::InboundCommandService::new(pool.clone()),
            )),
            Arc::new(NoopMonitorActivationHook),
            Some(active.clone()),
        )
        .expect("monitor");
        let monitor_handle = monitor.handle();
        let supervisor = TaskSupervisor::new();
        let provider = Arc::new(BlockingProvider::default());
        let service = WechatLoginService::new(
            WechatBootstrap {
                store: store.clone(),
                bundle: Some(active),
                status: WechatRuntimeStatus::ConnectedAwaitingActivation,
            },
            auth,
            supervisor.clone(),
            provider.clone(),
            Some(monitor),
        );

        let now = unix_timestamp_ms();
        let outbox = OutboxService::new(pool.clone());
        outbox
            .enqueue_device(
                device.id,
                RelayNotificationV1 {
                    schema_version: 1,
                    notification_id: "disconnect-keeps-outbox".to_owned(),
                    dedupe_key: "disconnect-keeps-outbox".to_owned(),
                    kind: "test".to_owned(),
                    priority: 100,
                    title: "safe title".to_owned(),
                    body: "safe body".to_owned(),
                    correlation_key: None,
                    created_at: now - 1,
                    expires_at: now + 60_000,
                },
            )
            .await
            .expect("enqueue");

        let started = service.start(device.id, true).expect("candidate");
        let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("login id");
        tokio::time::timeout(Duration::from_secs(1), provider.fetch_started.notified())
            .await
            .expect("login worker started");

        service.disconnect().await.expect("disconnect");

        assert_eq!(
            service
                .snapshot(device.id, login_id)
                .expect("snapshot")
                .state,
            LoginState::Cancelled
        );
        assert!(!service.runtime.has_active_credentials());
        assert_eq!(
            service.runtime.lock().status,
            WechatRuntimeStatus::Disconnected
        );
        assert!(monitor_handle.snapshot_bundle().is_none());
        assert!(store.load().await.expect("load").is_none());
        assert_eq!(
            outbox
                .status(device.id, "disconnect-keeps-outbox")
                .await
                .expect("outbox status")
                .status,
            "pending_channel"
        );

        supervisor.begin_shutdown();
        supervisor.wait().await;
        pool.close().await;
    }

    #[tokio::test]
    async fn disconnect_is_serialized_with_cutover_and_final_state_wins() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = crate::db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let auth = DeviceAuthService::new(pool.clone());
        let owner = auth
            .create_device("SERIAL OWNER", &crate::auth::DeviceScope::ALL)
            .await
            .expect("device");
        let store = EncryptedFileSecretStore::new_for_test(
            directory.path().join("wechat-connection.enc"),
            [5; 32],
        );
        let supervisor = TaskSupervisor::new();
        let service = WechatLoginService::for_test(
            store.clone(),
            None,
            auth,
            supervisor.clone(),
            Arc::new(PanicProvider),
        );
        let candidate = confirmed_candidate(&service.manager, owner.id);
        let login_id = candidate.login_id;

        let guard = service.coordinator.lock_cutover().await;
        let cutover_service = service.clone();
        let cutover = tokio::spawn(async move {
            let outcome = cutover_service.coordinator.cutover(&candidate).await;
            cutover_service
                .manager
                .finish_cutover(login_id, outcome)
                .expect("finish cutover");
            outcome
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !service.backend.staged.lock().await.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("candidate staged before cutover lock");
        let disconnect_service = service.clone();
        let disconnect = tokio::spawn(async move { disconnect_service.disconnect().await });
        assert!(!cutover.is_finished());
        assert!(!disconnect.is_finished());
        drop(guard);

        assert_eq!(
            cutover.await.expect("cutover task"),
            CutoverOutcome::Activated
        );
        disconnect
            .await
            .expect("disconnect task")
            .expect("disconnect");
        assert!(!service.runtime.has_active_credentials());
        assert!(store.load().await.expect("load").is_none());

        supervisor.begin_shutdown();
        supervisor.wait().await;
        pool.close().await;
    }

    #[tokio::test]
    async fn disconnect_failure_still_clears_process_credentials_fail_closed() {
        let directory = tempfile::tempdir().expect("directory");
        let pool = crate::db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let auth = DeviceAuthService::new(pool.clone());
        let active = confirmed_candidate(&LoginManager::new(), Uuid::new_v4())
            .bundle
            .clone();
        // A directory cannot be removed with remove_file, deterministically
        // injecting durable clear failure without a platform-specific hook.
        let store = EncryptedFileSecretStore::new_for_test(directory.path().to_owned(), [6; 32]);
        let supervisor = TaskSupervisor::new();
        let service = WechatLoginService::for_test(
            store,
            Some(active),
            auth,
            supervisor.clone(),
            Arc::new(PanicProvider),
        );

        assert_eq!(service.disconnect().await, Err(WechatDisconnectError));
        assert!(!service.runtime.has_active_credentials());
        assert_eq!(
            service.runtime.lock().status,
            WechatRuntimeStatus::CredentialsUnreadable
        );

        supervisor.begin_shutdown();
        supervisor.wait().await;
        pool.close().await;
    }
}
