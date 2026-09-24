use std::{
    collections::{HashMap, VecDeque},
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use relay_provider_wechat::{
    credentials::{
        ConnectionBundle, WECHAT_SECRET_SCHEMA_VERSION, WechatCredentials, WechatSessionSecrets,
    },
    http_client::{WechatHttpClient, validate_production_endpoint},
    protocol::{
        GetBotQrCodeResponse, GetQrCodeStatusResponse, ILINK_DEFAULT_BASE_URL, QrCodeStatus,
    },
};
use serde::{Serialize, Serializer};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::wechat_admin_login::AdminLoginAuthorization;

pub const DEFAULT_LOGIN_TTL: Duration = Duration::from_secs(5 * 60);
pub const DEFAULT_FINISHED_RETENTION: Duration = Duration::from_secs(5 * 60);
pub const MAX_QR_REFRESHES: u8 = 3;
pub const MAX_VERIFY_ATTEMPTS: u8 = 10;
pub const MAX_QR_STARTS_PER_DEVICE: usize = 5;
pub const QR_START_RATE_WINDOW: Duration = Duration::from_secs(10 * 60);

const ERROR_BOUND_WITHOUT_CREDENTIALS: &str = "WECHAT_BOUND_WITHOUT_LOCAL_CREDENTIALS";
const ERROR_INVALID_PROVIDER_RESPONSE: &str = "WECHAT_INVALID_LOGIN_RESPONSE";
const ERROR_VERIFY_BLOCKED: &str = "WECHAT_VERIFY_CODE_BLOCKED";
const ERROR_UNKNOWN_QR_STATUS: &str = "WECHAT_UNKNOWN_QR_STATUS";
const ERROR_CUTOVER_FAILED: &str = "WECHAT_CUTOVER_FAILED";
const ERROR_CUTOVER_DEGRADED: &str = "WECHAT_CUTOVER_ACTIVATED_DEGRADED";
const ERROR_PROVIDER_UNAVAILABLE: &str = "WECHAT_LOGIN_PROVIDER_UNAVAILABLE";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginState {
    FetchingQr,
    WaitingScan,
    Scanned,
    VerifyCodeRequired,
    RefreshingQr,
    Confirmed,
    AlreadyConnected,
    Expired,
    Cancelled,
    Failed,
}

impl LoginState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Confirmed
                | Self::AlreadyConnected
                | Self::Expired
                | Self::Cancelled
                | Self::Failed
        )
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginSnapshot {
    pub login_id: String,
    pub state: LoginState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_content: Option<QrContent>,
    pub expires_at: i64,
    pub can_submit_verify_code: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<&'static str>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct QrContent(Zeroizing<String>);

impl Serialize for QrContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0.as_str())
    }
}

impl fmt::Debug for QrContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("QrContent(<redacted>)")
    }
}

impl fmt::Debug for LoginSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginSnapshot")
            .field("login_id", &self.login_id)
            .field("state", &self.state)
            .field(
                "qr_content",
                &self.qr_content.as_ref().map(|_| "<redacted:present>"),
            )
            .field("expires_at", &self.expires_at)
            .field("can_submit_verify_code", &self.can_submit_verify_code)
            .field("error_code", &self.error_code)
            .finish()
    }
}

pub struct StartOutcome {
    pub snapshot: LoginSnapshot,
    pub created: bool,
}

impl fmt::Debug for StartOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartOutcome")
            .field("snapshot", &self.snapshot)
            .field("created", &self.created)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginError {
    /// Used for both an unknown id and an owner mismatch to avoid an id oracle.
    NotFound,
    CandidateInProgress,
    Terminal,
    VerifyNotAllowed,
    VerifyLimitReached,
    InvalidVerifyCode,
    RateLimited,
    Clock,
}

pub struct PollInput {
    pub generation: u32,
    pub verify_revision: u8,
    pub base_url: Url,
    pub qr_token: Zeroizing<String>,
    pub verify_code: Option<Zeroizing<String>>,
    pub cancellation: CancellationToken,
}

#[derive(Clone, Copy)]
struct PollRevision {
    generation: u32,
    verify: u8,
}

impl fmt::Debug for PollInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PollInput")
            .field("generation", &self.generation)
            .field("verify_revision", &self.verify_revision)
            .field("base_url", &self.base_url)
            .field("qr_token", &"<redacted>")
            .field(
                "verify_code",
                &self.verify_code.as_ref().map(|_| "<redacted:present>"),
            )
            .finish()
    }
}

pub struct PreparedCandidate {
    pub login_id: Uuid,
    pub owner_device_id: Uuid,
    pub bundle: ConnectionBundle,
    cancellation: CancellationToken,
    pub(crate) admin_authorization: Option<AdminLoginAuthorization>,
    pub(crate) operator_authorized: bool,
}

impl fmt::Debug for PreparedCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCandidate")
            .field("login_id", &self.login_id)
            .field("owner_device_id", &self.owner_device_id)
            .field("bundle", &"<redacted>")
            .field(
                "admin_authorization",
                &self.admin_authorization.as_ref().map(|_| "<bound>"),
            )
            .field("operator_authorized", &self.operator_authorized)
            .finish()
    }
}

#[derive(Debug)]
pub enum LoginDirective {
    ContinuePolling,
    IgnoreStale,
    RefreshQr { generation: u32 },
    Terminal(LoginState),
    Cutover(Box<PreparedCandidate>),
}

#[derive(Clone)]
pub struct LoginManager {
    inner: Arc<Mutex<LoginSessions>>,
    ttl: Duration,
    finished_retention: Duration,
}

struct LoginSessions {
    sessions: HashMap<Uuid, LoginSession>,
    candidate_id: Option<Uuid>,
    qr_start_attempts: HashMap<Uuid, VecDeque<Instant>>,
}

struct LoginSession {
    login_id: Uuid,
    owner_device_id: Uuid,
    created_at: Instant,
    expires_at: Instant,
    expires_at_wall_ms: i64,
    finished_at: Option<Instant>,
    cancellation: CancellationToken,
    state: LoginState,
    error_code: Option<&'static str>,
    qr_generation: u32,
    refresh_count: u8,
    verify_attempts: u8,
    verify_revision: u8,
    awaiting_cutover: bool,
    cutover_committing: bool,
    base_url: Url,
    qr_token: Option<Zeroizing<String>>,
    qr_content: Option<Zeroizing<String>>,
    pending_verify_code: Option<Zeroizing<String>>,
    admin_authorization: Option<AdminLoginAuthorization>,
    operator_authorized: bool,
}

#[derive(Default)]
struct LoginAuthority {
    admin_authorization: Option<AdminLoginAuthorization>,
    operator_authorized: bool,
}

impl LoginManager {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_LOGIN_TTL, DEFAULT_FINISHED_RETENTION)
    }

    pub fn with_limits(ttl: Duration, finished_retention: Duration) -> Self {
        assert!(!ttl.is_zero(), "login TTL must be non-zero");
        Self {
            inner: Arc::new(Mutex::new(LoginSessions {
                sessions: HashMap::new(),
                candidate_id: None,
                qr_start_attempts: HashMap::new(),
            })),
            ttl,
            finished_retention,
        }
    }

    /// Starts a session for an authenticated channel administrator. Detail,
    /// verify, and cancellation remain owner-scoped after this point.
    pub fn start(
        &self,
        owner_device_id: Uuid,
        force_fresh: bool,
        has_active_connection: bool,
    ) -> Result<StartOutcome, LoginError> {
        self.start_at(
            owner_device_id,
            force_fresh,
            has_active_connection,
            LoginAuthority::default(),
            Instant::now(),
            wall_now_ms()?,
        )
    }

    pub(crate) fn start_admin(
        &self,
        owner_device_id: Uuid,
        force_fresh: bool,
        has_active_connection: bool,
        authorization: AdminLoginAuthorization,
    ) -> Result<StartOutcome, LoginError> {
        if authorization.owner_device_id() != owner_device_id {
            return Err(LoginError::NotFound);
        }
        self.start_at(
            owner_device_id,
            force_fresh,
            has_active_connection,
            LoginAuthority {
                admin_authorization: Some(authorization),
                operator_authorized: false,
            },
            Instant::now(),
            wall_now_ms()?,
        )
    }

    /// Starts a process-owned login session for the authenticated Admin
    /// operator. Unlike the legacy device handoff flow, this authority is not
    /// coupled to any Dock device credential or lifecycle.
    pub(crate) fn start_operator(
        &self,
        owner_id: Uuid,
        force_fresh: bool,
        has_active_connection: bool,
    ) -> Result<StartOutcome, LoginError> {
        self.start_at(
            owner_id,
            force_fresh,
            has_active_connection,
            LoginAuthority {
                admin_authorization: None,
                operator_authorized: true,
            },
            Instant::now(),
            wall_now_ms()?,
        )
    }

    fn start_at(
        &self,
        owner_device_id: Uuid,
        force_fresh: bool,
        has_active_connection: bool,
        authority: LoginAuthority,
        now: Instant,
        wall_now_ms: i64,
    ) -> Result<StartOutcome, LoginError> {
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        if let Some(candidate_id) = inner.candidate_id {
            let existing = inner
                .sessions
                .get_mut(&candidate_id)
                .expect("candidate id always references a session");
            if existing.owner_device_id == owner_device_id {
                // Once confirmation has produced a PreparedCandidate, rebinding
                // only the session would leave the worker carrying an older
                // authorization. Fail closed instead of creating that split view.
                if existing.awaiting_cutover && authority.admin_authorization.is_some() {
                    return Err(LoginError::Terminal);
                }
                if let Some(authorization) = authority.admin_authorization {
                    existing.admin_authorization = Some(authorization);
                }
                existing.operator_authorized |= authority.operator_authorized;
                return Ok(StartOutcome {
                    snapshot: existing.snapshot(),
                    created: false,
                });
            }
            return Err(LoginError::CandidateInProgress);
        }

        let login_id = Uuid::new_v4();
        let expires_at = now.checked_add(self.ttl).ok_or(LoginError::Clock)?;
        let ttl_ms = i64::try_from(self.ttl.as_millis()).map_err(|_| LoginError::Clock)?;
        let expires_at_wall_ms = wall_now_ms.checked_add(ttl_ms).ok_or(LoginError::Clock)?;
        let state = if has_active_connection && !force_fresh {
            LoginState::AlreadyConnected
        } else {
            LoginState::FetchingQr
        };
        if state == LoginState::FetchingQr {
            inner.record_qr_start(owner_device_id, now)?;
        }
        let session = LoginSession {
            login_id,
            owner_device_id,
            created_at: now,
            expires_at,
            expires_at_wall_ms,
            finished_at: state.is_terminal().then_some(now),
            cancellation: CancellationToken::new(),
            state,
            error_code: None,
            qr_generation: 0,
            refresh_count: 0,
            verify_attempts: 0,
            verify_revision: 0,
            awaiting_cutover: false,
            cutover_committing: false,
            base_url: production_base_url().map_err(|_| LoginError::Clock)?,
            qr_token: None,
            qr_content: None,
            pending_verify_code: None,
            admin_authorization: authority.admin_authorization,
            operator_authorized: authority.operator_authorized,
        };
        let snapshot = session.snapshot();
        if !state.is_terminal() {
            inner.candidate_id = Some(login_id);
        }
        inner.sessions.insert(login_id, session);
        Ok(StartOutcome {
            snapshot,
            created: true,
        })
    }

    pub fn snapshot(
        &self,
        owner_device_id: Uuid,
        login_id: Uuid,
    ) -> Result<LoginSnapshot, LoginError> {
        self.snapshot_at(owner_device_id, login_id, Instant::now())
    }

    fn snapshot_at(
        &self,
        owner_device_id: Uuid,
        login_id: Uuid,
        now: Instant,
    ) -> Result<LoginSnapshot, LoginError> {
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        let session = inner.owner_session(owner_device_id, login_id)?;
        Ok(session.snapshot())
    }

    pub fn submit_verify_code(
        &self,
        owner_device_id: Uuid,
        login_id: Uuid,
        code: &str,
    ) -> Result<LoginSnapshot, LoginError> {
        if code.is_empty() || code.len() > 16 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(LoginError::InvalidVerifyCode);
        }
        let now = Instant::now();
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        let session = inner.owner_session_mut(owner_device_id, login_id)?;
        if session.state.is_terminal() || session.awaiting_cutover {
            return Err(LoginError::Terminal);
        }
        if session.state != LoginState::VerifyCodeRequired {
            return Err(LoginError::VerifyNotAllowed);
        }
        if session.verify_attempts >= MAX_VERIFY_ATTEMPTS {
            return Err(LoginError::VerifyLimitReached);
        }
        session.verify_attempts += 1;
        session.verify_revision += 1;
        session.pending_verify_code = Some(Zeroizing::new(code.to_owned()));
        Ok(session.snapshot())
    }

    pub fn cancel(
        &self,
        owner_device_id: Uuid,
        login_id: Uuid,
    ) -> Result<LoginSnapshot, LoginError> {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        let snapshot = {
            let session = inner.owner_session_mut(owner_device_id, login_id)?;
            if session.cutover_committing {
                return Err(LoginError::Terminal);
            }
            if !session.state.is_terminal() {
                session.finish(LoginState::Cancelled, None, now);
            }
            session.snapshot()
        };
        inner.release_candidate(login_id);
        Ok(snapshot)
    }

    /// Cancels the process-wide login candidate during an authenticated
    /// channel disconnect. Unlike the public login cancellation operation,
    /// disconnect is intentionally not owner-scoped: any authenticated device
    /// may tear down the single shared WeChat channel.
    pub(crate) fn cancel_candidate_for_disconnect(&self) -> Option<Uuid> {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        let login_id = inner.candidate_id?;
        let session = inner
            .sessions
            .get_mut(&login_id)
            .expect("candidate id always references a session");
        debug_assert!(!session.cutover_committing);
        if !session.state.is_terminal() {
            session.finish(LoginState::Cancelled, None, now);
        }
        inner.release_candidate(login_id);
        Some(login_id)
    }

    /// Installs a freshly fetched QR. This is worker-only and deliberately has
    /// no device credential parameter.
    pub fn install_qr(
        &self,
        login_id: Uuid,
        response: GetBotQrCodeResponse,
    ) -> Result<u32, LoginError> {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        let session = inner
            .sessions
            .get_mut(&login_id)
            .ok_or(LoginError::NotFound)?;
        if session.state.is_terminal() || session.awaiting_cutover {
            return Err(LoginError::Terminal);
        }
        if response.qrcode.is_empty() || response.qrcode_img_content.is_empty() {
            session.finish(
                LoginState::Failed,
                Some(ERROR_INVALID_PROVIDER_RESPONSE),
                now,
            );
            inner.release_candidate(login_id);
            return Err(LoginError::Terminal);
        }
        session.qr_generation = session.qr_generation.saturating_add(1);
        session.qr_token = Some(Zeroizing::new(response.qrcode.clone()));
        session.qr_content = Some(Zeroizing::new(response.qrcode_img_content.clone()));
        session.pending_verify_code = None;
        session.state = LoginState::WaitingScan;
        Ok(session.qr_generation)
    }

    pub fn poll_input(&self, login_id: Uuid) -> Result<PollInput, LoginError> {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        let session = inner.sessions.get(&login_id).ok_or(LoginError::NotFound)?;
        if session.state.is_terminal() || session.awaiting_cutover {
            return Err(LoginError::Terminal);
        }
        let qr_token = session.qr_token.as_ref().ok_or(LoginError::Terminal)?;
        Ok(PollInput {
            generation: session.qr_generation,
            verify_revision: session.verify_revision,
            base_url: session.base_url.clone(),
            qr_token: Zeroizing::new(qr_token.as_str().to_owned()),
            verify_code: session
                .pending_verify_code
                .as_ref()
                .map(|code| Zeroizing::new(code.as_str().to_owned())),
            cancellation: session.cancellation.clone(),
        })
    }

    pub fn worker_cancellation(&self, login_id: Uuid) -> Result<CancellationToken, LoginError> {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        let session = inner.sessions.get(&login_id).ok_or(LoginError::NotFound)?;
        if session.state.is_terminal() {
            return Err(LoginError::Terminal);
        }
        Ok(session.cancellation.clone())
    }

    pub fn apply_provider_status(
        &self,
        login_id: Uuid,
        generation: u32,
        verify_revision: u8,
        response: GetQrCodeStatusResponse,
        has_active_credentials: bool,
    ) -> Result<LoginDirective, LoginError> {
        self.apply_provider_status_at(
            login_id,
            PollRevision {
                generation,
                verify: verify_revision,
            },
            response,
            has_active_credentials,
            Instant::now(),
            wall_now_ms()?,
        )
    }

    fn apply_provider_status_at(
        &self,
        login_id: Uuid,
        poll_revision: PollRevision,
        mut response: GetQrCodeStatusResponse,
        has_active_credentials: bool,
        now: Instant,
        wall_now_ms: i64,
    ) -> Result<LoginDirective, LoginError> {
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        let session = inner
            .sessions
            .get_mut(&login_id)
            .ok_or(LoginError::NotFound)?;
        if session.state.is_terminal() || session.awaiting_cutover {
            return Err(LoginError::Terminal);
        }
        if session.qr_generation != poll_revision.generation {
            return Ok(LoginDirective::IgnoreStale);
        }
        // A verify code is retained until a current-generation response arrives,
        // so a transport failure does not silently consume an attempt.
        if session.verify_revision == poll_revision.verify {
            session.pending_verify_code = None;
        }

        let directive = match response.status {
            QrCodeStatus::Wait => {
                session.state = LoginState::WaitingScan;
                LoginDirective::ContinuePolling
            }
            QrCodeStatus::Unknown(_) => {
                session.finish(LoginState::Failed, Some(ERROR_UNKNOWN_QR_STATUS), now);
                LoginDirective::Terminal(LoginState::Failed)
            }
            QrCodeStatus::Scaned => {
                session.state = LoginState::Scanned;
                LoginDirective::ContinuePolling
            }
            QrCodeStatus::NeedVerifyCode => {
                session.state = LoginState::VerifyCodeRequired;
                LoginDirective::ContinuePolling
            }
            QrCodeStatus::VerifyCodeBlocked => {
                session.finish(LoginState::Failed, Some(ERROR_VERIFY_BLOCKED), now);
                LoginDirective::Terminal(LoginState::Failed)
            }
            QrCodeStatus::Expired => {
                if session.refresh_count < MAX_QR_REFRESHES {
                    session.refresh_count += 1;
                    session.state = LoginState::RefreshingQr;
                    session.clear_qr_secrets();
                    LoginDirective::RefreshQr {
                        generation: session.qr_generation,
                    }
                } else {
                    session.finish(LoginState::Expired, None, now);
                    LoginDirective::Terminal(LoginState::Expired)
                }
            }
            QrCodeStatus::ScanedButRedirect => {
                let Some(redirect_host) = response.redirect_host.take() else {
                    session.finish(
                        LoginState::Failed,
                        Some(ERROR_INVALID_PROVIDER_RESPONSE),
                        now,
                    );
                    inner.release_candidate(login_id);
                    return Ok(LoginDirective::Terminal(LoginState::Failed));
                };
                match redirect_base_url(&redirect_host) {
                    Ok(base_url) => {
                        session.base_url = base_url;
                        session.state = LoginState::Scanned;
                        LoginDirective::ContinuePolling
                    }
                    Err(_) => {
                        session.finish(
                            LoginState::Failed,
                            Some(ERROR_INVALID_PROVIDER_RESPONSE),
                            now,
                        );
                        LoginDirective::Terminal(LoginState::Failed)
                    }
                }
            }
            QrCodeStatus::BindedRedirect => {
                if has_active_credentials {
                    session.finish(LoginState::AlreadyConnected, None, now);
                    LoginDirective::Terminal(LoginState::AlreadyConnected)
                } else {
                    session.finish(
                        LoginState::Failed,
                        Some(ERROR_BOUND_WITHOUT_CREDENTIALS),
                        now,
                    );
                    LoginDirective::Terminal(LoginState::Failed)
                }
            }
            QrCodeStatus::Confirmed => {
                match confirmed_bundle(response, &session.base_url, wall_now_ms) {
                    Ok(bundle) => {
                        session.awaiting_cutover = true;
                        session.clear_qr_secrets();
                        LoginDirective::Cutover(Box::new(PreparedCandidate {
                            login_id,
                            owner_device_id: session.owner_device_id,
                            bundle,
                            cancellation: session.cancellation.clone(),
                            admin_authorization: session.admin_authorization.clone(),
                            operator_authorized: session.operator_authorized,
                        }))
                    }
                    Err(()) => {
                        session.finish(
                            LoginState::Failed,
                            Some(ERROR_INVALID_PROVIDER_RESPONSE),
                            now,
                        );
                        LoginDirective::Terminal(LoginState::Failed)
                    }
                }
            }
        };
        if matches!(directive, LoginDirective::Terminal(_)) {
            inner.release_candidate(login_id);
        }
        Ok(directive)
    }

    pub fn finish_cutover(
        &self,
        login_id: Uuid,
        outcome: CutoverOutcome,
    ) -> Result<LoginSnapshot, LoginError> {
        let now = Instant::now();
        let mut inner = self.lock();
        let snapshot = {
            let session = inner
                .sessions
                .get_mut(&login_id)
                .ok_or(LoginError::NotFound)?;
            if !session.awaiting_cutover {
                // Owner cancellation or monotonic expiry may win while the
                // staged candidate is waiting for the cutover mutex. The
                // worker can safely report its Cancelled outcome idempotently.
                if session.state.is_terminal() && outcome == CutoverOutcome::Cancelled {
                    return Ok(session.snapshot());
                }
                return Err(LoginError::Terminal);
            }
            match outcome {
                CutoverOutcome::Activated => {
                    session.finish(LoginState::Confirmed, None, now);
                }
                CutoverOutcome::ActivatedDegraded => {
                    session.finish(LoginState::Confirmed, Some(ERROR_CUTOVER_DEGRADED), now);
                }
                CutoverOutcome::Rejected { .. } | CutoverOutcome::Cancelled => {
                    session.finish(LoginState::Failed, Some(ERROR_CUTOVER_FAILED), now);
                }
            }
            session.snapshot()
        };
        inner.release_candidate(login_id);
        Ok(snapshot)
    }

    pub fn fail_provider(&self, login_id: Uuid) -> Result<LoginSnapshot, LoginError> {
        self.fail_provider_inner(login_id)
            .map(|(snapshot, _)| snapshot)
    }

    pub fn fail_unfinished_worker(&self, login_id: Uuid) -> Result<bool, LoginError> {
        self.fail_provider_inner(login_id)
            .map(|(_, transitioned)| transitioned)
    }

    fn fail_provider_inner(&self, login_id: Uuid) -> Result<(LoginSnapshot, bool), LoginError> {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        let (snapshot, transitioned) = {
            let session = inner
                .sessions
                .get_mut(&login_id)
                .ok_or(LoginError::NotFound)?;
            let transitioned = !session.state.is_terminal();
            if !session.state.is_terminal() {
                session.finish(LoginState::Failed, Some(ERROR_PROVIDER_UNAVAILABLE), now);
            }
            (session.snapshot(), transitioned)
        };
        inner.release_candidate(login_id);
        Ok((snapshot, transitioned))
    }

    pub fn candidate_is_current(&self, login_id: Uuid, owner_device_id: Uuid) -> bool {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        inner.candidate_id == Some(login_id)
            && inner.sessions.get(&login_id).is_some_and(|session| {
                session.owner_device_id == owner_device_id
                    && session.awaiting_cutover
                    && !session.cutover_committing
                    && !session.state.is_terminal()
            })
    }

    pub fn begin_cutover(&self, login_id: Uuid, owner_device_id: Uuid) -> bool {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.expire_and_purge(now, self.finished_retention);
        if inner.candidate_id != Some(login_id) {
            return false;
        }
        let Some(session) = inner.sessions.get_mut(&login_id) else {
            return false;
        };
        if session.owner_device_id != owner_device_id
            || !session.awaiting_cutover
            || session.cutover_committing
            || session.state.is_terminal()
        {
            return false;
        }
        session.cutover_committing = true;
        true
    }

    pub fn cutover_is_committing(&self, login_id: Uuid, owner_device_id: Uuid) -> bool {
        let inner = self.lock();
        inner.candidate_id == Some(login_id)
            && inner.sessions.get(&login_id).is_some_and(|session| {
                session.owner_device_id == owner_device_id
                    && session.awaiting_cutover
                    && session.cutover_committing
                    && !session.state.is_terminal()
            })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LoginSessions> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for LoginManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginSessions {
    fn record_qr_start(&mut self, owner_device_id: Uuid, now: Instant) -> Result<(), LoginError> {
        let attempts = self.qr_start_attempts.entry(owner_device_id).or_default();
        while attempts.front().is_some_and(|attempt| {
            now.checked_duration_since(*attempt)
                .is_some_and(|age| age >= QR_START_RATE_WINDOW)
        }) {
            attempts.pop_front();
        }
        if attempts.len() >= MAX_QR_STARTS_PER_DEVICE {
            return Err(LoginError::RateLimited);
        }
        attempts.push_back(now);
        Ok(())
    }

    fn owner_session(
        &self,
        owner_device_id: Uuid,
        login_id: Uuid,
    ) -> Result<&LoginSession, LoginError> {
        self.sessions
            .get(&login_id)
            .filter(|session| session.owner_device_id == owner_device_id)
            .ok_or(LoginError::NotFound)
    }

    fn owner_session_mut(
        &mut self,
        owner_device_id: Uuid,
        login_id: Uuid,
    ) -> Result<&mut LoginSession, LoginError> {
        self.sessions
            .get_mut(&login_id)
            .filter(|session| session.owner_device_id == owner_device_id)
            .ok_or(LoginError::NotFound)
    }

    fn release_candidate(&mut self, login_id: Uuid) {
        if self.candidate_id == Some(login_id) {
            self.candidate_id = None;
        }
    }

    fn expire_and_purge(&mut self, now: Instant, finished_retention: Duration) {
        let expired: Vec<Uuid> = self
            .sessions
            .iter_mut()
            .filter_map(|(id, session)| {
                if !session.state.is_terminal()
                    && !session.cutover_committing
                    && now >= session.expires_at
                {
                    session.finish(LoginState::Expired, None, now);
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        for id in expired {
            self.release_candidate(id);
        }
        self.sessions.retain(|_, session| {
            !session.finished_at.is_some_and(|finished_at| {
                now.checked_duration_since(finished_at)
                    .is_some_and(|age| age >= finished_retention)
            })
        });
        self.qr_start_attempts.retain(|_, attempts| {
            attempts.retain(|attempt| {
                now.checked_duration_since(*attempt)
                    .is_none_or(|age| age < QR_START_RATE_WINDOW)
            });
            !attempts.is_empty()
        });
    }
}

impl LoginSession {
    fn snapshot(&self) -> LoginSnapshot {
        debug_assert!(self.expires_at >= self.created_at);
        LoginSnapshot {
            login_id: self.login_id.to_string(),
            state: self.state,
            qr_content: self
                .qr_content
                .as_ref()
                .map(|content| QrContent(Zeroizing::new(content.as_str().to_owned()))),
            expires_at: self.expires_at_wall_ms,
            can_submit_verify_code: self.state == LoginState::VerifyCodeRequired
                && self.verify_attempts < MAX_VERIFY_ATTEMPTS,
            error_code: self.error_code,
        }
    }

    fn clear_qr_secrets(&mut self) {
        self.qr_token = None;
        self.qr_content = None;
        self.pending_verify_code = None;
    }

    fn finish(&mut self, state: LoginState, error_code: Option<&'static str>, now: Instant) {
        debug_assert!(state.is_terminal());
        self.state = state;
        self.error_code = error_code;
        self.finished_at = Some(now);
        self.awaiting_cutover = false;
        self.cutover_committing = false;
        self.clear_qr_secrets();
        self.cancellation.cancel();
        if let Some(authorization) = &self.admin_authorization {
            authorization.remove_access(self.login_id);
        }
    }
}

fn wall_now_ms() -> Result<i64, LoginError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LoginError::Clock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| LoginError::Clock)
}

fn production_base_url() -> Result<Url, InvalidProviderEndpoint> {
    let url = Url::parse(ILINK_DEFAULT_BASE_URL).map_err(|_| InvalidProviderEndpoint)?;
    validate_production_endpoint(&url).map_err(|_| InvalidProviderEndpoint)?;
    Ok(url)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidProviderEndpoint;

/// Accepts a provider-supplied host only. A scheme, userinfo, port, path,
/// query, fragment, IP literal, or deceptive suffix is rejected.
pub fn redirect_base_url(redirect_host: &str) -> Result<Url, InvalidProviderEndpoint> {
    if redirect_host.is_empty()
        || redirect_host.trim() != redirect_host
        || redirect_host.contains(['/', '\\', '@', ':', '?', '#'])
    {
        return Err(InvalidProviderEndpoint);
    }
    let url =
        Url::parse(&format!("https://{redirect_host}/")).map_err(|_| InvalidProviderEndpoint)?;
    validate_production_endpoint(&url).map_err(|_| InvalidProviderEndpoint)?;
    Ok(url)
}

fn confirmed_bundle(
    mut response: GetQrCodeStatusResponse,
    current_base_url: &Url,
    saved_at: i64,
) -> Result<ConnectionBundle, ()> {
    let bot_token = response
        .bot_token
        .take()
        .filter(|value| !value.is_empty())
        .ok_or(())?;
    let account_id = response
        .ilink_bot_id
        .take()
        .filter(|value| !value.is_empty())
        .ok_or(())?;
    let user_id = response
        .ilink_user_id
        .take()
        .filter(|value| !value.is_empty())
        .ok_or(())?;
    let base_url = match response.baseurl.take().filter(|value| !value.is_empty()) {
        Some(value) => {
            let url = Url::parse(&value).map_err(|_| ())?;
            validate_production_endpoint(&url).map_err(|_| ())?;
            url
        }
        None => current_base_url.clone(),
    };
    ConnectionBundle::new(
        WechatCredentials {
            schema_version: WECHAT_SECRET_SCHEMA_VERSION,
            bot_token,
            account_id,
            user_id,
            base_url: base_url.to_string(),
            saved_at,
        },
        WechatSessionSecrets::default(),
    )
    .map_err(|_| ())
}

/// Injectable network seam for the supervised login worker. Implementations
/// must keep their error type out of API responses and secret-bearing logs.
#[async_trait]
pub trait QrLoginProvider: Send + Sync {
    async fn fetch_qr(
        &self,
        local_token_list: &[String],
        cancellation: &CancellationToken,
    ) -> Result<GetBotQrCodeResponse, QrLoginProviderError>;

    async fn poll_status(
        &self,
        input: &PollInput,
    ) -> Result<GetQrCodeStatusResponse, QrLoginProviderError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QrLoginProviderError;

#[async_trait]
impl QrLoginProvider for WechatHttpClient {
    async fn fetch_qr(
        &self,
        local_token_list: &[String],
        cancellation: &CancellationToken,
    ) -> Result<GetBotQrCodeResponse, QrLoginProviderError> {
        self.get_bot_qrcode(local_token_list, cancellation)
            .await
            .map_err(|_| QrLoginProviderError)
    }

    async fn poll_status(
        &self,
        input: &PollInput,
    ) -> Result<GetQrCodeStatusResponse, QrLoginProviderError> {
        self.get_qrcode_status(
            &input.base_url,
            input.qr_token.as_str(),
            input.verify_code.as_ref().map(|code| code.as_str()),
            &input.cancellation,
        )
        .await
        .map_err(|_| QrLoginProviderError)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutoverStep {
    Stage,
    ValidateCurrent,
    StopActive,
    Promote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CutoverBackendError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutoverOutcome {
    Activated,
    /// Promotion succeeded, so the new bundle remains source of truth, but the
    /// new monitor failed to start and the channel must become degraded.
    ActivatedDegraded,
    Rejected {
        step: CutoverStep,
    },
    Cancelled,
}

#[async_trait]
pub trait BlueGreenBackend: Send + Sync {
    /// Encrypt, create_new, fsync the candidate temp file. Must not alter the
    /// active secret or monitor.
    async fn stage_candidate(
        &self,
        candidate: &PreparedCandidate,
    ) -> Result<(), CutoverBackendError>;
    /// Runs under the cutover mutex. It must reject a stale login/generation
    /// or an unexpected active-connection revision before the old monitor is
    /// stopped.
    async fn validate_candidate_current(
        &self,
        candidate: &PreparedCandidate,
    ) -> Result<(), CutoverBackendError>;
    /// Revalidates immediately before durable promotion. Production holds the
    /// same Admin authorization fence from this check through promotion.
    async fn validate_before_promote(
        &self,
        _candidate: &PreparedCandidate,
    ) -> Result<(), CutoverBackendError> {
        Ok(())
    }
    async fn stop_and_join_active(&self) -> Result<(), CutoverBackendError>;
    async fn promote_staged(&self, login_id: Uuid) -> Result<(), CutoverBackendError>;
    async fn start_promoted(&self) -> Result<(), CutoverBackendError>;
    async fn discard_staged(&self, login_id: Uuid);
    async fn resume_active_after_failed_cutover(&self);
    async fn mark_promoted_degraded(&self);
}

pub struct BlueGreenCoordinator<B> {
    backend: B,
    cutover: AsyncMutex<()>,
}

impl<B> BlueGreenCoordinator<B>
where
    B: BlueGreenBackend,
{
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            cutover: AsyncMutex::new(()),
        }
    }

    /// Acquires the same process-wide mutation boundary used by blue/green
    /// promotion. Active disconnect uses this guard so it cannot interleave
    /// stop/clear with a candidate promotion.
    pub(crate) async fn lock_cutover(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.cutover.lock().await
    }

    pub async fn cutover(&self, candidate: &PreparedCandidate) -> CutoverOutcome {
        // Staging is intentionally outside the mutex: the old monitor remains
        // live during encryption, write and fsync.
        if self.backend.stage_candidate(candidate).await.is_err() {
            self.backend.discard_staged(candidate.login_id).await;
            return CutoverOutcome::Rejected {
                step: CutoverStep::Stage,
            };
        }
        let _guard = self.cutover.lock().await;
        let _authorization_guard = match &candidate.admin_authorization {
            Some(authorization) => Some(authorization.lifecycle_guard().await),
            None => None,
        };
        if candidate.cancellation.is_cancelled() {
            self.backend.discard_staged(candidate.login_id).await;
            return CutoverOutcome::Cancelled;
        }
        if self
            .backend
            .validate_candidate_current(candidate)
            .await
            .is_err()
        {
            self.backend.discard_staged(candidate.login_id).await;
            return CutoverOutcome::Rejected {
                step: CutoverStep::ValidateCurrent,
            };
        }
        if self.backend.stop_and_join_active().await.is_err() {
            self.backend.discard_staged(candidate.login_id).await;
            self.backend.resume_active_after_failed_cutover().await;
            return CutoverOutcome::Rejected {
                step: CutoverStep::StopActive,
            };
        }
        if self
            .backend
            .validate_before_promote(candidate)
            .await
            .is_err()
        {
            self.backend.discard_staged(candidate.login_id).await;
            self.backend.resume_active_after_failed_cutover().await;
            return CutoverOutcome::Rejected {
                step: CutoverStep::ValidateCurrent,
            };
        }
        if self
            .backend
            .promote_staged(candidate.login_id)
            .await
            .is_err()
        {
            self.backend.discard_staged(candidate.login_id).await;
            self.backend.resume_active_after_failed_cutover().await;
            return CutoverOutcome::Rejected {
                step: CutoverStep::Promote,
            };
        }
        if self.backend.start_promoted().await.is_err() {
            // Never restore the old credential after promotion. The atomic
            // rename made the new bundle authoritative.
            self.backend.mark_promoted_degraded().await;
            return CutoverOutcome::ActivatedDegraded;
        }
        CutoverOutcome::Activated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn qr_response(suffix: &str) -> GetBotQrCodeResponse {
        GetBotQrCodeResponse {
            qrcode: format!("qr-token-{suffix}"),
            qrcode_img_content: format!("qr-content-{suffix}"),
        }
    }

    fn status(status: QrCodeStatus) -> GetQrCodeStatusResponse {
        GetQrCodeStatusResponse {
            status,
            bot_token: None,
            ilink_bot_id: None,
            baseurl: None,
            ilink_user_id: None,
            redirect_host: None,
        }
    }

    fn confirmed_status() -> GetQrCodeStatusResponse {
        GetQrCodeStatusResponse {
            status: QrCodeStatus::Confirmed,
            bot_token: Some("bot-secret".into()),
            ilink_bot_id: Some("bot-id".into()),
            baseurl: Some("https://ilinkai.weixin.qq.com/".into()),
            ilink_user_id: Some("user@im.wechat".into()),
            redirect_host: None,
        }
    }

    fn manager() -> LoginManager {
        LoginManager::with_limits(Duration::from_secs(300), Duration::from_secs(300))
    }

    fn start(manager: &LoginManager, owner: Uuid) -> Uuid {
        let outcome = manager.start(owner, false, false).expect("start login");
        assert!(outcome.created);
        Uuid::parse_str(&outcome.snapshot.login_id).expect("login id")
    }

    #[test]
    fn owner_scope_hides_session_and_single_candidate_prevents_takeover() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let login_id = start(&manager, owner);
        assert_eq!(manager.snapshot(other, login_id), Err(LoginError::NotFound));
        assert_eq!(manager.cancel(other, login_id), Err(LoginError::NotFound));
        assert_eq!(
            manager.submit_verify_code(other, login_id, "123456"),
            Err(LoginError::NotFound)
        );
        assert!(matches!(
            manager.start(other, true, true),
            Err(LoginError::CandidateInProgress)
        ));
        let repeated = manager
            .start(owner, true, true)
            .expect("same owner resumes");
        assert!(!repeated.created);
        assert_eq!(repeated.snapshot.login_id, login_id.to_string());
    }

    #[test]
    fn active_connection_requires_force_fresh_and_candidate_does_not_replace_it() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let existing = manager
            .start(owner, false, true)
            .expect("already connected");
        assert_eq!(existing.snapshot.state, LoginState::AlreadyConnected);
        let candidate = manager.start(owner, true, true).expect("candidate");
        assert_eq!(candidate.snapshot.state, LoginState::FetchingQr);
    }

    #[test]
    fn monotonic_expiry_wins_even_when_wall_clock_snapshot_is_unchanged() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let now = Instant::now();
        let started = manager
            .start_at(owner, false, false, LoginAuthority::default(), now, 1_000)
            .expect("start");
        let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("id");
        assert_eq!(started.snapshot.expires_at, 301_000);
        let expired = manager
            .snapshot_at(owner, login_id, now + Duration::from_secs(301))
            .expect("retained expired snapshot");
        assert_eq!(expired.state, LoginState::Expired);
        assert_eq!(expired.expires_at, 301_000);
    }

    #[test]
    fn qr_start_rate_limit_is_per_device_and_uses_monotonic_window() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let now = Instant::now();
        for attempt in 0..MAX_QR_STARTS_PER_DEVICE {
            let started = manager
                .start_at(owner, false, false, LoginAuthority::default(), now, 1_000)
                .expect("within owner limit");
            let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("login id");
            if attempt == 0 {
                for _ in 0..20 {
                    let repeated = manager
                        .start_at(owner, false, false, LoginAuthority::default(), now, 1_000)
                        .expect("idempotent retry");
                    assert!(!repeated.created);
                    assert_eq!(repeated.snapshot.login_id, started.snapshot.login_id);
                }
            }
            manager.cancel(owner, login_id).expect("cancel candidate");
        }
        assert!(matches!(
            manager.start_at(owner, false, false, LoginAuthority::default(), now, 1_000),
            Err(LoginError::RateLimited)
        ));
        let other_start = manager
            .start_at(other, false, false, LoginAuthority::default(), now, 1_000)
            .expect("another device has an independent bucket");
        manager
            .cancel(
                other,
                Uuid::parse_str(&other_start.snapshot.login_id).expect("other login id"),
            )
            .expect("cancel other candidate");
        assert!(
            manager
                .start_at(
                    owner,
                    false,
                    false,
                    LoginAuthority::default(),
                    now + QR_START_RATE_WINDOW,
                    1_000,
                )
                .is_ok(),
            "the monotonic window refills"
        );
    }

    #[test]
    fn verify_is_state_gated_validated_and_limited_per_login() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let login_id = start(&manager, owner);
        let generation = manager
            .install_qr(login_id, qr_response("one"))
            .expect("QR");
        assert_eq!(
            manager.submit_verify_code(owner, login_id, "123456"),
            Err(LoginError::VerifyNotAllowed)
        );
        manager
            .apply_provider_status(
                login_id,
                generation,
                0,
                status(QrCodeStatus::NeedVerifyCode),
                false,
            )
            .expect("verify state");
        for attempt in 0..MAX_VERIFY_ATTEMPTS {
            let code = format!("{attempt:06}");
            manager
                .submit_verify_code(owner, login_id, &code)
                .expect("allowed verify attempt");
        }
        assert_eq!(
            manager.submit_verify_code(owner, login_id, "999999"),
            Err(LoginError::VerifyLimitReached)
        );
        assert_eq!(
            manager.submit_verify_code(owner, login_id, "12 34"),
            Err(LoginError::InvalidVerifyCode)
        );
    }

    #[test]
    fn older_poll_response_does_not_consume_a_newer_verify_code() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let login_id = start(&manager, owner);
        let generation = manager
            .install_qr(login_id, qr_response("verify-race"))
            .expect("QR");
        manager
            .apply_provider_status(
                login_id,
                generation,
                0,
                status(QrCodeStatus::NeedVerifyCode),
                false,
            )
            .expect("verify state");
        manager
            .submit_verify_code(owner, login_id, "111111")
            .expect("first code");
        let older_poll = manager.poll_input(login_id).expect("older poll");
        manager
            .submit_verify_code(owner, login_id, "222222")
            .expect("newer code");

        manager
            .apply_provider_status(
                login_id,
                older_poll.generation,
                older_poll.verify_revision,
                status(QrCodeStatus::NeedVerifyCode),
                false,
            )
            .expect("older response");

        let next_poll = manager.poll_input(login_id).expect("next poll");
        assert_eq!(next_poll.verify_revision, 2);
        assert_eq!(
            next_poll.verify_code.as_deref().map(String::as_str),
            Some("222222")
        );
    }

    #[test]
    fn refresh_is_bounded_and_stale_generation_is_ignored() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let login_id = start(&manager, owner);
        let first = manager.install_qr(login_id, qr_response("0")).expect("QR");
        for refresh in 0..MAX_QR_REFRESHES {
            assert!(matches!(
                manager
                    .apply_provider_status(
                        login_id,
                        first + u32::from(refresh),
                        0,
                        status(QrCodeStatus::Expired),
                        false,
                    )
                    .expect("expired"),
                LoginDirective::RefreshQr { .. }
            ));
            let next = manager
                .install_qr(login_id, qr_response(&format!("{}", refresh + 1)))
                .expect("refreshed QR");
            assert_eq!(next, first + u32::from(refresh) + 1);
            assert!(matches!(
                manager
                    .apply_provider_status(
                        login_id,
                        next - 1,
                        0,
                        status(QrCodeStatus::Scaned),
                        false,
                    )
                    .expect("stale response"),
                LoginDirective::IgnoreStale
            ));
        }
        let current = manager.poll_input(login_id).expect("poll").generation;
        assert!(matches!(
            manager
                .apply_provider_status(login_id, current, 0, status(QrCodeStatus::Expired), false,)
                .expect("limit"),
            LoginDirective::Terminal(LoginState::Expired)
        ));
        assert!(manager.poll_input(login_id).is_err());
    }

    #[test]
    fn redirect_host_is_ssrf_safe() {
        for valid in [
            "ilinkai.weixin.qq.com",
            "edge.weixin.qq.com",
            "a.b.weixin.qq.com",
        ] {
            assert!(redirect_base_url(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "weixin.qq.com",
            "evilweixin.qq.com",
            "weixin.qq.com.evil.test",
            "127.0.0.1",
            "[::1]",
            "edge.weixin.qq.com:443",
            "https://edge.weixin.qq.com",
            "user@edge.weixin.qq.com",
            "edge.weixin.qq.com/path",
            "edge.weixin.qq.com?token=secret",
            " edge.weixin.qq.com",
        ] {
            assert!(redirect_base_url(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn binded_redirect_never_invents_missing_credentials() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let login_id = start(&manager, owner);
        let generation = manager
            .install_qr(login_id, qr_response("one"))
            .expect("QR");
        assert!(matches!(
            manager
                .apply_provider_status(
                    login_id,
                    generation,
                    0,
                    status(QrCodeStatus::BindedRedirect),
                    false,
                )
                .expect("status"),
            LoginDirective::Terminal(LoginState::Failed)
        ));
        assert_eq!(
            manager
                .snapshot(owner, login_id)
                .expect("snapshot")
                .error_code,
            Some(ERROR_BOUND_WITHOUT_CREDENTIALS)
        );
    }

    #[test]
    fn unknown_qr_status_fails_closed() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let login_id = start(&manager, owner);
        let generation = manager
            .install_qr(login_id, qr_response("unknown"))
            .expect("QR");
        assert!(matches!(
            manager
                .apply_provider_status(
                    login_id,
                    generation,
                    0,
                    status(QrCodeStatus::Unknown("future-secret-state".into())),
                    false,
                )
                .expect("status"),
            LoginDirective::Terminal(LoginState::Failed)
        ));
        let snapshot = manager.snapshot(owner, login_id).expect("snapshot");
        assert_eq!(snapshot.error_code, Some(ERROR_UNKNOWN_QR_STATUS));
        assert!(!format!("{snapshot:?}").contains("future-secret-state"));
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Failure {
        None,
        Stage,
        Validate,
        Stop,
        Promote,
        Start,
    }

    struct FakeBackend {
        events: StdMutex<Vec<&'static str>>,
        failure: Failure,
    }

    impl FakeBackend {
        fn new(failure: Failure) -> Self {
            Self {
                events: StdMutex::new(Vec::new()),
                failure,
            }
        }

        fn push(&self, event: &'static str) {
            self.events.lock().expect("events").push(event);
        }

        fn events(&self) -> Vec<&'static str> {
            self.events.lock().expect("events").clone()
        }
    }

    #[async_trait]
    impl BlueGreenBackend for Arc<FakeBackend> {
        async fn stage_candidate(
            &self,
            _candidate: &PreparedCandidate,
        ) -> Result<(), CutoverBackendError> {
            self.push("stage");
            (self.failure != Failure::Stage)
                .then_some(())
                .ok_or(CutoverBackendError)
        }

        async fn stop_and_join_active(&self) -> Result<(), CutoverBackendError> {
            self.push("stop");
            (self.failure != Failure::Stop)
                .then_some(())
                .ok_or(CutoverBackendError)
        }

        async fn validate_candidate_current(
            &self,
            _candidate: &PreparedCandidate,
        ) -> Result<(), CutoverBackendError> {
            self.push("validate");
            (self.failure != Failure::Validate)
                .then_some(())
                .ok_or(CutoverBackendError)
        }

        async fn promote_staged(&self, _login_id: Uuid) -> Result<(), CutoverBackendError> {
            self.push("promote");
            (self.failure != Failure::Promote)
                .then_some(())
                .ok_or(CutoverBackendError)
        }

        async fn start_promoted(&self) -> Result<(), CutoverBackendError> {
            self.push("start");
            (self.failure != Failure::Start)
                .then_some(())
                .ok_or(CutoverBackendError)
        }

        async fn discard_staged(&self, _login_id: Uuid) {
            self.push("discard");
        }

        async fn resume_active_after_failed_cutover(&self) {
            self.push("resume-old");
        }

        async fn mark_promoted_degraded(&self) {
            self.push("degraded");
        }
    }

    fn prepared_candidate(manager: &LoginManager, owner: Uuid) -> PreparedCandidate {
        let login_id = start(manager, owner);
        let generation = manager
            .install_qr(login_id, qr_response("one"))
            .expect("QR");
        match manager
            .apply_provider_status(login_id, generation, 0, confirmed_status(), true)
            .expect("confirmed")
        {
            LoginDirective::Cutover(candidate) => *candidate,
            other => panic!("unexpected directive: {other:?}"),
        }
    }

    #[tokio::test]
    async fn blue_green_orders_stage_stop_promote_start() {
        let manager2 = manager();
        let candidate = prepared_candidate(&manager2, Uuid::new_v4());
        let backend = Arc::new(FakeBackend::new(Failure::None));
        let coordinator = BlueGreenCoordinator::new(backend.clone());
        assert_eq!(
            coordinator.cutover(&candidate).await,
            CutoverOutcome::Activated
        );
        assert_eq!(
            backend.events(),
            ["stage", "validate", "stop", "promote", "start"]
        );
    }

    #[tokio::test]
    async fn pre_promotion_failure_resumes_old_but_post_promotion_failure_never_rolls_back() {
        let manager = manager();
        let candidate = prepared_candidate(&manager, Uuid::new_v4());
        let promote_failure = Arc::new(FakeBackend::new(Failure::Promote));
        let coordinator = BlueGreenCoordinator::new(promote_failure.clone());
        assert_eq!(
            coordinator.cutover(&candidate).await,
            CutoverOutcome::Rejected {
                step: CutoverStep::Promote
            }
        );
        assert_eq!(
            promote_failure.events(),
            [
                "stage",
                "validate",
                "stop",
                "promote",
                "discard",
                "resume-old"
            ]
        );

        let manager_after_promote =
            LoginManager::with_limits(Duration::from_secs(300), Duration::from_secs(300));
        let candidate = prepared_candidate(&manager_after_promote, Uuid::new_v4());
        let start_failure = Arc::new(FakeBackend::new(Failure::Start));
        let coordinator = BlueGreenCoordinator::new(start_failure.clone());
        assert_eq!(
            coordinator.cutover(&candidate).await,
            CutoverOutcome::ActivatedDegraded
        );
        assert_eq!(
            start_failure.events(),
            ["stage", "validate", "stop", "promote", "start", "degraded"]
        );
        assert!(!start_failure.events().contains(&"resume-old"));
    }

    #[tokio::test]
    async fn stale_candidate_is_rejected_inside_mutex_before_old_monitor_is_touched() {
        let manager = manager();
        let candidate = prepared_candidate(&manager, Uuid::new_v4());
        let backend = Arc::new(FakeBackend::new(Failure::Validate));
        let coordinator = BlueGreenCoordinator::new(backend.clone());
        assert_eq!(
            coordinator.cutover(&candidate).await,
            CutoverOutcome::Rejected {
                step: CutoverStep::ValidateCurrent
            }
        );
        assert_eq!(backend.events(), ["stage", "validate", "discard"]);
        assert!(!backend.events().contains(&"stop"));
    }

    #[tokio::test]
    async fn cancelled_or_expired_candidate_never_stops_old_monitor() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let candidate = prepared_candidate(&manager, owner);
        manager
            .cancel(owner, candidate.login_id)
            .expect("cancel candidate");
        let backend = Arc::new(FakeBackend::new(Failure::None));
        let coordinator = BlueGreenCoordinator::new(backend.clone());
        assert_eq!(
            coordinator.cutover(&candidate).await,
            CutoverOutcome::Cancelled
        );
        assert_eq!(backend.events(), ["stage", "discard"]);
        assert_eq!(
            manager
                .finish_cutover(candidate.login_id, CutoverOutcome::Cancelled)
                .expect("cancelled worker completion is idempotent")
                .state,
            LoginState::Cancelled
        );

        let manager =
            LoginManager::with_limits(Duration::from_millis(50), Duration::from_secs(300));
        let owner = Uuid::new_v4();
        let candidate = prepared_candidate(&manager, owner);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            manager
                .snapshot(owner, candidate.login_id)
                .expect("expired snapshot")
                .state,
            LoginState::Expired
        );
        let backend = Arc::new(FakeBackend::new(Failure::None));
        let coordinator = BlueGreenCoordinator::new(backend.clone());
        assert_eq!(
            coordinator.cutover(&candidate).await,
            CutoverOutcome::Cancelled
        );
        assert_eq!(backend.events(), ["stage", "discard"]);
    }

    #[test]
    fn cutover_commit_claim_serializes_late_cancel_and_expiry() {
        let manager =
            LoginManager::with_limits(Duration::from_millis(50), Duration::from_secs(300));
        let owner = Uuid::new_v4();
        let candidate = prepared_candidate(&manager, owner);
        assert!(manager.begin_cutover(candidate.login_id, owner));
        assert_eq!(
            manager.cancel(owner, candidate.login_id),
            Err(LoginError::Terminal)
        );
        std::thread::sleep(Duration::from_millis(60));
        assert_ne!(
            manager
                .snapshot(owner, candidate.login_id)
                .expect("commit-claimed snapshot")
                .state,
            LoginState::Expired
        );
    }

    #[test]
    fn upstream_terminal_failure_does_not_invoke_cutover_backend() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let login_id = start(&manager, owner);
        let generation = manager
            .install_qr(login_id, qr_response("blocked"))
            .expect("QR");
        let backend = Arc::new(FakeBackend::new(Failure::None));
        manager
            .apply_provider_status(
                login_id,
                generation,
                0,
                status(QrCodeStatus::VerifyCodeBlocked),
                true,
            )
            .expect("failed status");
        assert!(backend.events().is_empty());
    }

    #[tokio::test]
    async fn activated_degraded_is_visible_in_safe_login_snapshot() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let candidate = prepared_candidate(&manager, owner);
        let backend = Arc::new(FakeBackend::new(Failure::Start));
        let coordinator = BlueGreenCoordinator::new(backend);
        let outcome = coordinator.cutover(&candidate).await;
        let snapshot = manager
            .finish_cutover(candidate.login_id, outcome)
            .expect("finish cutover");
        assert_eq!(snapshot.state, LoginState::Confirmed);
        assert_eq!(snapshot.error_code, Some(ERROR_CUTOVER_DEGRADED));
    }

    #[test]
    fn snapshot_debug_redacts_qr_content_and_poll_debug_redacts_inputs() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let login_id = start(&manager, owner);
        manager
            .install_qr(login_id, qr_response("sentinel"))
            .expect("QR");
        let snapshot = manager.snapshot(owner, login_id).expect("snapshot");
        let poll = manager.poll_input(login_id).expect("poll");
        assert!(!format!("{snapshot:?}").contains("qr-content-sentinel"));
        assert!(!format!("{poll:?}").contains("qr-token-sentinel"));
        let json = serde_json::to_value(&snapshot).expect("serialize snapshot");
        assert_eq!(json["qrContent"], "qr-content-sentinel");
    }

    #[test]
    fn confirmed_rejects_an_untrusted_base_url() {
        let manager = manager();
        let owner = Uuid::new_v4();
        let login_id = start(&manager, owner);
        let generation = manager
            .install_qr(login_id, qr_response("one"))
            .expect("QR");
        let mut response = confirmed_status();
        response.baseurl = Some("http://127.0.0.1:8080/".into());
        assert!(matches!(
            manager
                .apply_provider_status(login_id, generation, 0, response, false)
                .expect("status"),
            LoginDirective::Terminal(LoginState::Failed)
        ));
    }
}
