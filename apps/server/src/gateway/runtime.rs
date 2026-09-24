use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use tokio::{
    sync::{Mutex, oneshot},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    RunQueryActionV5, RunQueryResultV5,
    protocol::{
        CLOSE_CODE_FRAME_TOO_LARGE, CLOSE_CODE_POLICY_REVOKED, CLOSE_CODE_PROTOCOL_ERROR,
        CLOSE_CODE_SERVER_SHUTDOWN, CLOSE_CODE_SLOW_CONSUMER, CLOSE_CODE_SUPERSEDED,
        CLOSE_REASON_FRAME_TOO_LARGE, CLOSE_REASON_POLICY_REVOKED, CLOSE_REASON_PROTOCOL_ERROR,
        CLOSE_REASON_SERVER_SHUTDOWN, CLOSE_REASON_SLOW_CONSUMER, CLOSE_REASON_SUPERSEDED,
        CatalogQueryActionV5, CatalogQueryResultV5, ControlActionV5, ControlResultV5,
        GATEWAY_HEARTBEAT_SECONDS, GATEWAY_PONG_TIMEOUT_SECONDS, GATEWAY_PROTOCOL_VERSION,
        GatewayClientFrameV5, GatewayProtocolError, GatewayServerFrameV5, HelloV5,
        MAX_GATEWAY_FRAME_BYTES, MAX_GATEWAY_PENDING_REQUESTS, PingV5, PolicyRevocationCodeV5,
        PolicyRevokedV5, RemoteRequestActionV5, RemoteRequestV5, RemoteResponseResultV5,
        SupersededV5, WelcomeV5, decode_client_frame, encode_server_frame,
    },
    registry::{
        GatewayCommand, GatewayConnectionSnapshot, GatewayControl, GatewayRegistration,
        GatewayRegistrationAuthorization, GatewayRegistry, GatewayRegistryError,
        GatewayRequestError,
    },
};
use crate::{
    auth::{
        AuthenticatedDevice, DeviceAuthService, DeviceScope, GatewayAuthorizationLease,
        GatewayAuthorizationState,
    },
    shutdown::TaskSupervisor,
};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const AUTHORIZATION_REVALIDATE: Duration = Duration::from_secs(30);
const PENDING_SCAN: Duration = Duration::from_millis(250);
const SOCKET_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const SOCKET_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct GatewayRuntime {
    registry: GatewayRegistry,
    auth: DeviceAuthService,
    supervisor: TaskSupervisor,
    shutdown: CancellationToken,
}

impl GatewayRuntime {
    pub fn new(auth: DeviceAuthService, supervisor: TaskSupervisor) -> Self {
        Self {
            registry: GatewayRegistry::new(),
            auth,
            shutdown: supervisor.cancellation_token(),
            supervisor,
        }
    }

    pub fn registry(&self) -> GatewayRegistry {
        self.registry.clone()
    }

    pub async fn connections(&self) -> Vec<GatewayConnectionSnapshot> {
        self.registry.connections().await
    }

    pub async fn request_job(
        &self,
        device_id: Uuid,
        action: RunQueryActionV5,
    ) -> Result<RunQueryResultV5, GatewayRequestError> {
        self.request_job_with_revision(device_id, action, None)
            .await
    }

    pub async fn request_job_with_revision(
        &self,
        device_id: Uuid,
        action: RunQueryActionV5,
        expected_revision: Option<&str>,
    ) -> Result<RunQueryResultV5, GatewayRequestError> {
        action
            .validate()
            .map_err(|_| GatewayRequestError::InvalidRequest)?;
        let remote_action = RemoteRequestActionV5::from(action.clone());
        super::protocol::validate_request_action_v5(&remote_action)
            .map_err(|_| GatewayRequestError::InvalidRequest)?;
        let lease = self
            .current_authorization(device_id, DeviceScope::RunQuery, expected_revision)
            .await?;
        let expected = ExpectedResultKind::from(&action);
        let result = self
            .registry
            .request_job_authorized(device_id, remote_action, &lease.credential_revision)
            .await?;
        // The response is untrusted until authorization is checked again. A
        // rotate, revoke, disable, or scope change while the request was in
        // flight must discard an otherwise well-formed client response.
        self.current_authorization(
            device_id,
            DeviceScope::RunQuery,
            Some(&lease.credential_revision),
        )
        .await?;
        if !expected.matches(&result) {
            return Err(GatewayRequestError::ProtocolMismatch);
        }
        RunQueryResultV5::try_from(result).map_err(|()| GatewayRequestError::ProtocolMismatch)
    }

    pub async fn request_catalog(
        &self,
        device_id: Uuid,
        action: CatalogQueryActionV5,
    ) -> Result<CatalogQueryResultV5, GatewayRequestError> {
        self.request_catalog_with_revision(device_id, action, None)
            .await
    }

    pub async fn request_catalog_with_revision(
        &self,
        device_id: Uuid,
        action: CatalogQueryActionV5,
        expected_revision: Option<&str>,
    ) -> Result<CatalogQueryResultV5, GatewayRequestError> {
        let remote = action.clone().into();
        super::protocol::validate_request_action_v5(&remote)
            .map_err(|_| GatewayRequestError::InvalidRequest)?;
        let expected = ExpectedResultKind::from(&remote);
        let lease = self
            .current_authorization(device_id, DeviceScope::RunQuery, expected_revision)
            .await?;
        let result = self
            .registry
            .request_catalog_authorized(device_id, remote, &lease.credential_revision)
            .await?;
        self.current_authorization(
            device_id,
            DeviceScope::RunQuery,
            Some(&lease.credential_revision),
        )
        .await?;
        if !expected.matches(&result) {
            return Err(GatewayRequestError::ProtocolMismatch);
        }
        CatalogQueryResultV5::try_from(result).map_err(|()| GatewayRequestError::ProtocolMismatch)
    }

    pub async fn request_control(
        &self,
        device_id: Uuid,
        action: ControlActionV5,
    ) -> Result<ControlResultV5, GatewayRequestError> {
        self.request_control_with_revision(device_id, action, None)
            .await
    }

    pub async fn request_control_with_revision(
        &self,
        device_id: Uuid,
        action: ControlActionV5,
        expected_revision: Option<&str>,
    ) -> Result<ControlResultV5, GatewayRequestError> {
        let remote = action.clone().into();
        super::protocol::validate_request_action_v5(&remote)
            .map_err(|_| GatewayRequestError::InvalidRequest)?;
        let expected = ExpectedResultKind::from(&remote);
        let lease = self
            .current_authorization(device_id, DeviceScope::RunControl, expected_revision)
            .await?;
        let result = self
            .registry
            .request_control_authorized(device_id, remote, &lease.credential_revision)
            .await?;
        self.current_authorization(
            device_id,
            DeviceScope::RunControl,
            Some(&lease.credential_revision),
        )
        .await?;
        if !expected.matches(&result) {
            return Err(GatewayRequestError::ProtocolMismatch);
        }
        ControlResultV5::try_from(result).map_err(|()| GatewayRequestError::ProtocolMismatch)
    }

    async fn current_authorization(
        &self,
        device_id: Uuid,
        required_scope: DeviceScope,
        expected_revision: Option<&str>,
    ) -> Result<GatewayAuthorizationLease, GatewayRequestError> {
        match self
            .auth
            .gateway_authorization_state(device_id)
            .await
            .map_err(|_| GatewayRequestError::AuthorizationUnavailable)?
        {
            GatewayAuthorizationState::Active(lease)
                if lease.scopes.contains(&DeviceScope::GatewayConnect)
                    && lease.scopes.contains(&required_scope)
                    && expected_revision
                        .is_none_or(|revision| revision == lease.credential_revision) =>
            {
                Ok(lease)
            }
            GatewayAuthorizationState::Active(_)
            | GatewayAuthorizationState::DeviceRevoked
            | GatewayAuthorizationState::DeviceDisabled
            | GatewayAuthorizationState::Missing => Err(GatewayRequestError::Unauthorized),
        }
    }

    pub(super) async fn initial_lease(
        &self,
        authenticated: &AuthenticatedDevice,
    ) -> Result<GatewayAuthorizationLease, GatewayHandshakeError> {
        match self
            .auth
            .gateway_authorization_state(authenticated.id)
            .await
            .map_err(|_| GatewayHandshakeError::Database)?
        {
            GatewayAuthorizationState::Active(lease)
                if lease.credential_revision == authenticated.credential_revision
                    && lease.scopes.contains(&DeviceScope::GatewayConnect) =>
            {
                Ok(lease)
            }
            GatewayAuthorizationState::Active(lease)
                if !lease.scopes.contains(&DeviceScope::GatewayConnect) =>
            {
                Err(GatewayHandshakeError::InsufficientScope)
            }
            GatewayAuthorizationState::Active(_) => Err(GatewayHandshakeError::Unauthorized),
            GatewayAuthorizationState::DeviceRevoked | GatewayAuthorizationState::Missing => {
                Err(GatewayHandshakeError::Revoked)
            }
            GatewayAuthorizationState::DeviceDisabled => Err(GatewayHandshakeError::Disabled),
        }
    }

    pub(super) async fn prepare_initial_lease(
        &self,
        authenticated: &AuthenticatedDevice,
    ) -> Result<PreparedGatewayLease, GatewayHandshakeError> {
        let invalidation_generation = self
            .registry
            .invalidation_generation(authenticated.id)
            .await;
        let authorization = self.initial_lease(authenticated).await?;
        Ok(PreparedGatewayLease {
            authorization,
            invalidation_generation,
        })
    }

    pub(super) async fn run_tracked(
        &self,
        socket: WebSocket,
        authenticated: AuthenticatedDevice,
        lease: PreparedGatewayLease,
    ) {
        let cleanup = Arc::new(Mutex::new(None));
        let task_cleanup = Arc::clone(&cleanup);
        let runtime = self.clone();
        let handle = self.supervisor.spawn(async move {
            runtime
                .run_session(socket, authenticated, lease, task_cleanup)
                .await;
        });
        let result = handle.await;
        if let Some(identity) = *cleanup.lock().await {
            self.registry
                .cleanup(
                    identity.device_id,
                    identity.connection_id,
                    identity.generation,
                )
                .await;
        }
        if let Err(error) = result {
            tracing::error!(
                operation = "gateway.session",
                task_error = %error,
                "gateway session task terminated unexpectedly"
            );
        }
    }

    async fn run_session(
        &self,
        mut socket: WebSocket,
        authenticated: AuthenticatedDevice,
        lease: PreparedGatewayLease,
        cleanup: Arc<Mutex<Option<SessionIdentity>>>,
    ) {
        let hello = match self.receive_hello(&mut socket).await {
            Ok(hello) => hello,
            Err(stop) => {
                stop.close(&mut socket, &self.shutdown).await;
                return;
            }
        };
        let connected_at = unix_timestamp_ms();
        let capabilities = hello.capabilities.iter().cloned().collect::<BTreeSet<_>>();
        let registration = match self
            .registry
            .register(
                authenticated.id,
                connected_at,
                hello.client_version.clone(),
                capabilities.clone(),
                GatewayRegistrationAuthorization::new(
                    lease.authorization.credential_revision.clone(),
                    lease.invalidation_generation,
                ),
                &self.shutdown,
            )
            .await
        {
            Ok(registration) => registration,
            Err(error) => {
                let stop = match error {
                    GatewayRegistryError::PolicyInvalidated => SessionStop::Policy {
                        code: self
                            .authorization_policy(authenticated.id, &lease.authorization)
                            .await
                            .err()
                            .unwrap_or(PolicyRevocationCodeV5::DeviceRevoked),
                        generation: 0,
                    },
                    GatewayRegistryError::GenerationExhausted => SessionStop::Protocol,
                };
                stop.close(&mut socket, &self.shutdown).await;
                return;
            }
        };
        let connection_id = registration.connection_id;
        let generation = registration.generation;
        *cleanup.lock().await = Some(SessionIdentity {
            device_id: authenticated.id,
            connection_id,
            generation,
        });
        let welcome = GatewayServerFrameV5::Welcome(WelcomeV5 {
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            connection_id: connection_id.to_string(),
            generation,
            heartbeat_seconds: GATEWAY_HEARTBEAT_SECONDS,
            max_frame_bytes: MAX_GATEWAY_FRAME_BYTES,
            server_time: connected_at,
        });
        if send_frame(&mut socket, &welcome).await.is_err() {
            return;
        }
        let stop = self
            .session_loop(
                &mut socket,
                authenticated.id,
                &lease.authorization,
                registration,
            )
            .await;
        stop.close(&mut socket, &self.shutdown).await;
    }

    async fn receive_hello(&self, socket: &mut WebSocket) -> Result<HelloV5, SessionStop> {
        let message = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => return Err(SessionStop::Shutdown),
            result = tokio::time::timeout(HELLO_TIMEOUT, socket.recv()) => {
                result.map_err(|_| SessionStop::Protocol)?
            }
        };
        match message {
            Some(Ok(Message::Text(text))) => match decode_client_frame(text.as_str()) {
                Ok(GatewayClientFrameV5::Hello(hello)) => Ok(hello),
                Err(GatewayProtocolError::FrameTooLarge) => Err(SessionStop::FrameTooLarge),
                _ => Err(SessionStop::Protocol),
            },
            Some(Ok(Message::Binary(_))) => Err(SessionStop::Protocol),
            Some(Ok(Message::Close(_))) | None => Err(SessionStop::Silent),
            Some(Ok(Message::Ping(_))) => Err(SessionStop::Protocol),
            Some(Ok(Message::Pong(_))) => Err(SessionStop::Protocol),
            // Tungstenite enforces the explicit 64 KiB upgrade limit before a
            // `Message` is yielded. Treat read-side failures as a size fence;
            // ordinary peer disconnects arrive as `None` or `Close`.
            Some(Err(_)) => Err(SessionStop::FrameTooLarge),
        }
    }

    async fn session_loop(
        &self,
        socket: &mut WebSocket,
        device_id: Uuid,
        initial_lease: &GatewayAuthorizationLease,
        mut registration: GatewayRegistration,
    ) -> SessionStop {
        let connection_id = registration.connection_id;
        let generation = registration.generation;
        let session = ClientSessionFence {
            device_id,
            connection_id,
            generation,
            registry: &self.registry,
        };
        let mut pending = HashMap::<String, PendingRequest>::new();
        let mut ping_ids = VecDeque::<String>::new();
        let mut last_pong = Instant::now();
        let mut heartbeat = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(GATEWAY_HEARTBEAT_SECONDS.into()),
            Duration::from_secs(GATEWAY_HEARTBEAT_SECONDS.into()),
        );
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut authorization = tokio::time::interval_at(
            tokio::time::Instant::now() + AUTHORIZATION_REVALIDATE,
            AUTHORIZATION_REVALIDATE,
        );
        authorization.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pending_scan = tokio::time::interval(PENDING_SCAN);
        pending_scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        pending_scan.tick().await;

        let stop = loop {
            tokio::select! {
                biased;
                control = registration.control.recv() => match control {
                    Some(GatewayControl::Superseded { generation }) => {
                        let _ = send_frame(
                            socket,
                            &GatewayServerFrameV5::Superseded(SupersededV5 { generation }),
                        ).await;
                        break SessionStop::Superseded;
                    }
                    Some(GatewayControl::PolicyRevoked { generation, code }) => {
                        break SessionStop::Policy { code, generation };
                    }
                    Some(GatewayControl::SlowConsumer) => break SessionStop::SlowConsumer,
                    None => break SessionStop::Silent,
                },
                () = registration.cancellation.cancelled() => {
                    break if self.shutdown.is_cancelled() {
                        SessionStop::Shutdown
                    } else {
                        match self.authorization_policy(device_id, initial_lease).await {
                            Ok(()) => SessionStop::Superseded,
                            Err(code) => SessionStop::Policy { code, generation },
                        }
                    };
                }
                incoming = socket.recv() => match incoming {
                    Some(Ok(message)) => match handle_client_message(
                        socket,
                        message,
                        &session,
                        &mut pending,
                        &mut ping_ids,
                        &mut last_pong,
                    ).await {
                        Ok(()) => {}
                        Err(stop) => break stop,
                    },
                    // The configured WebSocket size fence rejects an oversized
                    // frame before it can be decoded into a `Message`.
                    Some(Err(_)) => break SessionStop::FrameTooLarge,
                    None => break SessionStop::Silent,
                },
                command = registration.commands.recv() => match command {
                    Some(command) => {
                        if let Err(stop) = handle_command(
                            socket,
                            generation,
                            command,
                            &mut pending,
                        ).await {
                            break stop;
                        }
                    }
                    None => break SessionStop::Silent,
                },
                _ = heartbeat.tick() => {
                    if last_pong.elapsed()
                        >= Duration::from_secs(GATEWAY_PONG_TIMEOUT_SECONDS.into())
                    {
                        break SessionStop::Protocol;
                    }
                    let ping_id = Uuid::new_v4().to_string();
                    while ping_ids.len() >= 3 {
                        ping_ids.pop_front();
                    }
                    ping_ids.push_back(ping_id.clone());
                    if send_frame(socket, &GatewayServerFrameV5::Ping(PingV5 {
                        ping_id,
                        generation,
                        sent_at: unix_timestamp_ms(),
                    })).await.is_err() {
                        break SessionStop::Silent;
                    }
                },
                _ = authorization.tick() => {
                    match self.authorization_policy(device_id, initial_lease).await {
                        Ok(()) => {}
                        Err(code) => break SessionStop::Policy { code, generation },
                    }
                },
                _ = pending_scan.tick() => expire_pending(&mut pending),
            }
        };
        for (_, request) in pending {
            let _ = request
                .response
                .send(Err(GatewayRequestError::Disconnected));
        }
        stop
    }

    pub(super) async fn authorization_policy(
        &self,
        device_id: Uuid,
        initial: &GatewayAuthorizationLease,
    ) -> Result<(), PolicyRevocationCodeV5> {
        let state = self
            .auth
            .gateway_authorization_state(device_id)
            .await
            .map_err(|_| PolicyRevocationCodeV5::DeviceRevoked)?;
        match state {
            GatewayAuthorizationState::Active(current)
                if current.credential_revision == initial.credential_revision
                    && current.scopes.contains(&DeviceScope::GatewayConnect) =>
            {
                Ok(())
            }
            GatewayAuthorizationState::Active(current)
                if current.scopes != initial.scopes
                    || !current.scopes.contains(&DeviceScope::GatewayConnect) =>
            {
                Err(PolicyRevocationCodeV5::ScopeChanged)
            }
            GatewayAuthorizationState::Active(_) => Err(PolicyRevocationCodeV5::TokenRotated),
            GatewayAuthorizationState::DeviceDisabled => {
                Err(PolicyRevocationCodeV5::DeviceDisabled)
            }
            GatewayAuthorizationState::DeviceRevoked | GatewayAuthorizationState::Missing => {
                Err(PolicyRevocationCodeV5::DeviceRevoked)
            }
        }
    }
}

async fn handle_client_message(
    socket: &mut WebSocket,
    message: Message,
    session: &ClientSessionFence<'_>,
    pending: &mut HashMap<String, PendingRequest>,
    ping_ids: &mut VecDeque<String>,
    last_pong: &mut Instant,
) -> Result<(), SessionStop> {
    let frame = match message {
        Message::Text(text) => decode_client_frame(text.as_str()).map_err(|error| match error {
            GatewayProtocolError::FrameTooLarge | GatewayProtocolError::ResponseTooLarge => {
                SessionStop::FrameTooLarge
            }
            GatewayProtocolError::Malformed | GatewayProtocolError::Validation => {
                SessionStop::Protocol
            }
        })?,
        Message::Binary(_) => return Err(SessionStop::Protocol),
        Message::Ping(payload) => {
            tokio::time::timeout(SOCKET_SEND_TIMEOUT, socket.send(Message::Pong(payload)))
                .await
                .map_err(|_| SessionStop::SlowConsumer)?
                .map_err(|_| SessionStop::Silent)?;
            return Ok(());
        }
        Message::Pong(_) => return Ok(()),
        Message::Close(_) => return Err(SessionStop::Silent),
    };
    match frame {
        GatewayClientFrameV5::Hello(_) => Err(SessionStop::Protocol),
        GatewayClientFrameV5::Pong(pong) => {
            if pong.generation != session.generation
                || !ping_ids.iter().any(|ping_id| ping_id == &pong.ping_id)
            {
                return Err(SessionStop::Protocol);
            }
            while let Some(ping_id) = ping_ids.pop_front() {
                if ping_id == pong.ping_id {
                    break;
                }
            }
            *last_pong = Instant::now();
            session
                .registry
                .mark_pong(
                    session.device_id,
                    session.connection_id,
                    session.generation,
                    unix_timestamp_ms(),
                )
                .await;
            Ok(())
        }
        GatewayClientFrameV5::RequestAck(ack) => {
            if ack.generation != session.generation {
                return Err(SessionStop::Protocol);
            }
            let Some(request) = pending.get_mut(&ack.request_id) else {
                return Err(SessionStop::Protocol);
            };
            if request.is_expired() {
                let request = pending
                    .remove(&ack.request_id)
                    .expect("pending request exists");
                let _ = request.response.send(Err(GatewayRequestError::Expired));
                return Err(SessionStop::Protocol);
            }
            if !ack.accepted {
                let request = pending
                    .remove(&ack.request_id)
                    .expect("pending request exists");
                let _ = request.response.send(Err(GatewayRequestError::Rejected));
            } else {
                request.acknowledged = true;
            }
            Ok(())
        }
        GatewayClientFrameV5::Response(response) => {
            if response.generation != session.generation {
                return Err(SessionStop::Protocol);
            }
            let Some(request) = pending.remove(&response.request_id) else {
                return Err(SessionStop::Protocol);
            };
            if !request.acknowledged || request.is_expired() {
                let _ = request.response.send(Err(GatewayRequestError::Expired));
                return Err(SessionStop::Protocol);
            }
            if !request.expected.matches(&response.result) {
                let _ = request
                    .response
                    .send(Err(GatewayRequestError::ProtocolMismatch));
                return Err(SessionStop::Protocol);
            }
            let _ = request.response.send(Ok(response.result));
            Ok(())
        }
        GatewayClientFrameV5::Error(error) => {
            if error.generation != session.generation {
                return Err(SessionStop::Protocol);
            }
            let Some(request) = pending.remove(&error.request_id) else {
                return Err(SessionStop::Protocol);
            };
            if !request.acknowledged || request.is_expired() {
                let _ = request.response.send(Err(GatewayRequestError::Expired));
                return Err(SessionStop::Protocol);
            }
            let _ = request.response.send(Err(GatewayRequestError::Rejected));
            Ok(())
        }
    }
}

async fn handle_command(
    socket: &mut WebSocket,
    generation: u64,
    command: GatewayCommand,
    pending: &mut HashMap<String, PendingRequest>,
) -> Result<(), SessionStop> {
    match command {
        GatewayCommand::Request {
            generation: command_generation,
            issued_at,
            expires_at,
            deadline,
            action,
            response,
        } => {
            if command_generation != generation {
                let _ = response.send(Err(GatewayRequestError::Disconnected));
                return Ok(());
            }
            if expires_at <= unix_timestamp_ms() || deadline <= Instant::now() {
                let _ = response.send(Err(GatewayRequestError::Expired));
                return Ok(());
            }
            if pending.len() >= MAX_GATEWAY_PENDING_REQUESTS {
                let _ = response.send(Err(GatewayRequestError::PendingLimit));
                return Ok(());
            }
            let request_id = Uuid::new_v4().to_string();
            let frame = GatewayServerFrameV5::Request(RemoteRequestV5 {
                request_id: request_id.clone(),
                generation,
                issued_at,
                expires_at,
                action: action.clone(),
            });
            send_frame(socket, &frame).await?;
            pending.insert(
                request_id,
                PendingRequest {
                    expires_at,
                    deadline,
                    acknowledged: false,
                    expected: ExpectedResultKind::from(&action),
                    response,
                },
            );
            Ok(())
        }
    }
}

fn expire_pending(pending: &mut HashMap<String, PendingRequest>) {
    let expired = pending
        .iter()
        .filter(|(_, request)| request.is_expired())
        .map(|(request_id, _)| request_id.clone())
        .collect::<Vec<_>>();
    for request_id in expired {
        if let Some(request) = pending.remove(&request_id) {
            let _ = request.response.send(Err(GatewayRequestError::Expired));
        }
    }
}

struct PendingRequest {
    expires_at: i64,
    deadline: Instant,
    acknowledged: bool,
    expected: ExpectedResultKind,
    response: oneshot::Sender<Result<RemoteResponseResultV5, GatewayRequestError>>,
}

#[derive(Clone, Copy)]
enum ExpectedResultKind {
    GetDeviceInfo,
    ListRuns,
    GetRunDetail,
    GetRunTree,
    ListRuntimes,
    ListWorkspaces,
    ListHarnessProfiles,
    ListTaskPresets,
    StartRun,
    CancelRun,
}

impl ExpectedResultKind {
    fn matches(self, result: &RemoteResponseResultV5) -> bool {
        matches!(
            (self, result),
            (
                Self::GetDeviceInfo,
                RemoteResponseResultV5::GetDeviceInfo(_)
            ) | (Self::ListRuns, RemoteResponseResultV5::ListRuns(_))
                | (Self::GetRunDetail, RemoteResponseResultV5::GetRunDetail(_))
                | (Self::GetRunTree, RemoteResponseResultV5::GetRunTree(_))
                | (Self::ListRuntimes, RemoteResponseResultV5::ListRuntimes(_))
                | (
                    Self::ListWorkspaces,
                    RemoteResponseResultV5::ListWorkspaces(_)
                )
                | (
                    Self::ListHarnessProfiles,
                    RemoteResponseResultV5::ListHarnessProfiles(_)
                )
                | (
                    Self::ListTaskPresets,
                    RemoteResponseResultV5::ListTaskPresets(_)
                )
                | (Self::StartRun, RemoteResponseResultV5::StartRun(_))
                | (Self::CancelRun, RemoteResponseResultV5::CancelRun(_))
        )
    }
}

impl From<&RemoteRequestActionV5> for ExpectedResultKind {
    fn from(action: &RemoteRequestActionV5) -> Self {
        match action {
            RemoteRequestActionV5::GetDeviceInfo => Self::GetDeviceInfo,
            RemoteRequestActionV5::ListRuns { .. } => Self::ListRuns,
            RemoteRequestActionV5::GetRunDetail { .. } => Self::GetRunDetail,
            RemoteRequestActionV5::GetRunTree { .. } => Self::GetRunTree,
            RemoteRequestActionV5::ListRuntimes => Self::ListRuntimes,
            RemoteRequestActionV5::ListWorkspaces => Self::ListWorkspaces,
            RemoteRequestActionV5::ListHarnessProfiles { .. } => Self::ListHarnessProfiles,
            RemoteRequestActionV5::ListTaskPresets { .. } => Self::ListTaskPresets,
            RemoteRequestActionV5::StartRun { .. } => Self::StartRun,
            RemoteRequestActionV5::CancelRun { .. } => Self::CancelRun,
        }
    }
}

impl From<&RunQueryActionV5> for ExpectedResultKind {
    fn from(action: &RunQueryActionV5) -> Self {
        match action {
            RunQueryActionV5::ListRuns { .. } => Self::ListRuns,
            RunQueryActionV5::GetRunDetail { .. } => Self::GetRunDetail,
            RunQueryActionV5::GetRunTree { .. } => Self::GetRunTree,
        }
    }
}

struct ClientSessionFence<'a> {
    device_id: Uuid,
    connection_id: Uuid,
    generation: u64,
    registry: &'a GatewayRegistry,
}

#[derive(Clone, Copy)]
struct SessionIdentity {
    device_id: Uuid,
    connection_id: Uuid,
    generation: u64,
}

pub(super) struct PreparedGatewayLease {
    authorization: GatewayAuthorizationLease,
    invalidation_generation: u64,
}

impl PendingRequest {
    fn is_expired(&self) -> bool {
        self.expires_at <= unix_timestamp_ms() || self.deadline <= Instant::now()
    }
}

async fn send_frame(
    socket: &mut WebSocket,
    frame: &GatewayServerFrameV5,
) -> Result<(), SessionStop> {
    let text = encode_server_frame(frame).map_err(|_| SessionStop::Protocol)?;
    tokio::time::timeout(SOCKET_SEND_TIMEOUT, socket.send(Message::Text(text.into())))
        .await
        .map_err(|_| SessionStop::SlowConsumer)?
        .map_err(|_| SessionStop::Silent)
}

#[derive(Clone, Copy)]
enum SessionStop {
    Silent,
    Protocol,
    FrameTooLarge,
    Superseded,
    Policy {
        code: PolicyRevocationCodeV5,
        generation: u64,
    },
    SlowConsumer,
    Shutdown,
}

impl SessionStop {
    async fn close(self, socket: &mut WebSocket, cancellation: &CancellationToken) {
        let (code, reason) = match self {
            Self::Silent => return,
            Self::Protocol => (CLOSE_CODE_PROTOCOL_ERROR, CLOSE_REASON_PROTOCOL_ERROR),
            Self::FrameTooLarge => (CLOSE_CODE_FRAME_TOO_LARGE, CLOSE_REASON_FRAME_TOO_LARGE),
            Self::Superseded => (CLOSE_CODE_SUPERSEDED, CLOSE_REASON_SUPERSEDED),
            Self::Policy { .. } => (CLOSE_CODE_POLICY_REVOKED, CLOSE_REASON_POLICY_REVOKED),
            Self::SlowConsumer => (CLOSE_CODE_SLOW_CONSUMER, CLOSE_REASON_SLOW_CONSUMER),
            Self::Shutdown => (CLOSE_CODE_SERVER_SHUTDOWN, CLOSE_REASON_SERVER_SHUTDOWN),
        };
        if let Self::Policy { code, generation } = self {
            // Best effort: the policy frame carries the stable reason before the close frame.
            // No sensitive authorization material is included.
            let frame = GatewayServerFrameV5::PolicyRevoked(PolicyRevokedV5 { generation, code });
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                _ = send_frame(socket, &frame) => {}
            }
        }
        let send = tokio::time::timeout(
            SOCKET_CLOSE_TIMEOUT,
            socket.send(Message::Close(Some(CloseFrame {
                code,
                reason: reason.into(),
            }))),
        );
        if matches!(self, Self::Shutdown) {
            let _ = send.await;
        } else {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {}
                _ = send => {}
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GatewayHandshakeError {
    Unauthorized,
    Revoked,
    Disabled,
    InsufficientScope,
    Database,
}

fn unix_timestamp_ms() -> i64 {
    let millis = SystemTime::UNIX_EPOCH
        .elapsed()
        .map_or(0, |elapsed| elapsed.as_millis());
    i64::try_from(millis).unwrap_or(i64::MAX)
}
