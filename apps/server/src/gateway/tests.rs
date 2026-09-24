use std::{net::SocketAddr, time::Duration};

use futures_util::{SinkExt as _, StreamExt as _};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest as _,
        http::{HeaderValue, header::AUTHORIZATION},
    },
};

use super::{
    RemoteDesiredStateV5, RemoteRunConditionV5, RemoteRunFilterV5, RemoteRunOutcomeV5,
    RemoteRunPageV5, RemoteRunPhaseV5, RemoteRunSummaryV5, RunQueryActionV5, RunQueryResultV5,
    protocol::{
        CLOSE_CODE_FRAME_TOO_LARGE, CLOSE_CODE_POLICY_REVOKED, CLOSE_CODE_PROTOCOL_ERROR,
        CLOSE_CODE_SERVER_SHUTDOWN, CLOSE_CODE_SUPERSEDED, GATEWAY_PROTOCOL_VERSION,
        GatewayCapabilityV5, GatewayClientFrameV5, GatewayServerFrameV5, GetDeviceInfoResultV5,
        HelloV5, MAX_GATEWAY_FRAME_BYTES, RemoteErrorCodeV5, RemoteErrorV5, RemoteResponseResultV5,
        RemoteResponseV5, RequestAckV5, decode_server_frame, encode_client_frame,
    },
    runtime::GatewayHandshakeError,
};
use crate::{
    api,
    auth::{DeviceScope, PlaintextDeviceToken},
    config::{DatabaseConfig, ServerConfig},
    shutdown::TaskSupervisor,
    state::AppState,
};

type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct TestServer {
    _directory: tempfile::TempDir,
    state: AppState,
    supervisor: TaskSupervisor,
    address: SocketAddr,
    server: JoinHandle<()>,
}

impl TestServer {
    async fn start() -> Self {
        let directory = tempfile::tempdir().expect("temporary gateway database");
        let pool = crate::db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("gateway test database");
        let supervisor = TaskSupervisor::new();
        let state = AppState::new(pool, supervisor.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("gateway listener");
        let address = listener.local_addr().expect("gateway address");
        let app = api::router(state.clone(), &ServerConfig::default());
        let shutdown = supervisor.cancellation_token();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await;
        });
        Self {
            _directory: directory,
            state,
            supervisor,
            address,
            server,
        }
    }

    async fn device(&self, scopes: &[DeviceScope]) -> crate::auth::CreatedDevice {
        self.state
            .device_auth
            .create_device("GATEWAY TEST", scopes)
            .await
            .expect("gateway device")
    }

    async fn connect(&self, token: Option<&str>) -> Result<ClientSocket, u16> {
        self.connect_path("/v5/gateway/ws", token).await
    }

    async fn connect_path(&self, path: &str, token: Option<&str>) -> Result<ClientSocket, u16> {
        let mut request = format!("ws://{}{path}", self.address)
            .into_client_request()
            .expect("gateway request");
        if let Some(token) = token {
            request.headers_mut().insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}")).expect("authorization header"),
            );
        }
        connect_async(request)
            .await
            .map(|(socket, _)| socket)
            .map_err(|error| match error {
                tokio_tungstenite::tungstenite::Error::Http(response) => response.status().as_u16(),
                other => panic!("unexpected websocket connection error: {other}"),
            })
    }

    async fn connect_ready(&self, token: &PlaintextDeviceToken) -> (ClientSocket, u64) {
        self.connect_ready_with_capabilities(token, vec![GatewayCapabilityV5::DeviceInfoV5])
            .await
    }

    async fn connect_ready_with_capabilities(
        &self,
        token: &PlaintextDeviceToken,
        capabilities: Vec<GatewayCapabilityV5>,
    ) -> (ClientSocket, u64) {
        let mut socket = self
            .connect(Some(token.expose()))
            .await
            .expect("gateway websocket");
        send_client(
            &mut socket,
            GatewayClientFrameV5::Hello(HelloV5 {
                protocol_version: GATEWAY_PROTOCOL_VERSION,
                client_version: "gateway-test/1".to_owned(),
                capabilities,
            }),
        )
        .await;
        let GatewayServerFrameV5::Welcome(welcome) = recv_server(&mut socket).await else {
            panic!("expected welcome frame");
        };
        (socket, welcome.generation)
    }
}

#[tokio::test]
async fn legacy_v1_websocket_path_is_not_found_for_an_authenticated_device() {
    let server = TestServer::start().await;
    let device = server.device(&[DeviceScope::GatewayConnect]).await;
    assert!(matches!(
        server
            .connect_path("/v1/gateway/ws", Some(device.token.expose()))
            .await,
        Err(404)
    ));
}

#[tokio::test]
async fn retired_v4_websocket_path_is_not_found_even_with_valid_authentication() {
    let server = TestServer::start().await;
    let device = server.device(&[DeviceScope::GatewayConnect]).await;
    assert!(matches!(
        server
            .connect_path("/v4/gateway/ws", Some(device.token.expose()))
            .await,
        Err(404)
    ));
}

#[tokio::test]
async fn job_dispatch_revalidates_scope_revision_capability_and_generation() {
    let server = TestServer::start().await;
    let missing_scope = server.device(&[DeviceScope::GatewayConnect]).await;
    let (_socket, _) = server
        .connect_ready_with_capabilities(
            &missing_scope.token,
            vec![GatewayCapabilityV5::RemoteRunsReadV2],
        )
        .await;
    assert_eq!(
        server
            .state
            .gateway
            .request_job(
                missing_scope.id,
                RunQueryActionV5::ListRuns {
                    filter: RemoteRunFilterV5::All,
                    page_size: 10,
                    cursor: None,
                },
            )
            .await,
        Err(super::GatewayRequestError::Unauthorized)
    );

    let missing_capability = server
        .device(&[DeviceScope::GatewayConnect, DeviceScope::RunQuery])
        .await;
    let (_socket, _) = server.connect_ready(&missing_capability.token).await;
    assert_eq!(
        server
            .state
            .gateway
            .request_job(
                missing_capability.id,
                RunQueryActionV5::ListRuns {
                    filter: RemoteRunFilterV5::Recent,
                    page_size: 10,
                    cursor: None,
                },
            )
            .await,
        Err(super::GatewayRequestError::UnsupportedCapability)
    );

    let stale_revision = server
        .device(&[DeviceScope::GatewayConnect, DeviceScope::RunQuery])
        .await;
    let (_socket, _) = server
        .connect_ready_with_capabilities(
            &stale_revision.token,
            vec![GatewayCapabilityV5::RemoteRunsReadV2],
        )
        .await;
    server
        .state
        .device_auth
        .rotate_device(
            &stale_revision.id.to_string(),
            &[DeviceScope::GatewayConnect, DeviceScope::RunQuery],
        )
        .await
        .expect("rotate gateway token");
    assert_eq!(
        server
            .state
            .gateway
            .request_job(
                stale_revision.id,
                RunQueryActionV5::ListRuns {
                    filter: RemoteRunFilterV5::Failed,
                    page_size: 10,
                    cursor: None,
                },
            )
            .await,
        Err(super::GatewayRequestError::Unauthorized)
    );
}

#[tokio::test]
async fn job_dispatch_is_typed_acknowledged_and_result_matched() {
    let server = TestServer::start().await;
    let device = server
        .device(&[DeviceScope::GatewayConnect, DeviceScope::RunQuery])
        .await;
    let (mut socket, generation) = server
        .connect_ready_with_capabilities(&device.token, vec![GatewayCapabilityV5::RemoteRunsReadV2])
        .await;
    let runtime = server.state.gateway.clone();
    let request = tokio::spawn(async move {
        runtime
            .request_job(
                device.id,
                RunQueryActionV5::ListRuns {
                    filter: RemoteRunFilterV5::Recent,
                    page_size: 10,
                    cursor: Some("cursor_f77a3509a2af4e80a32a5d622ad80b4c".into()),
                },
            )
            .await
    });
    let GatewayServerFrameV5::Request(remote) = recv_server(&mut socket).await else {
        panic!("expected job query request");
    };
    assert_eq!(remote.generation, generation);
    assert_eq!(
        remote.action,
        super::protocol::RemoteRequestActionV5::ListRuns {
            filter: RemoteRunFilterV5::Recent,
            page_size: 10,
            cursor: Some("cursor_f77a3509a2af4e80a32a5d622ad80b4c".into()),
        }
    );
    send_client(
        &mut socket,
        GatewayClientFrameV5::RequestAck(RequestAckV5 {
            request_id: remote.request_id.clone(),
            generation,
            accepted: true,
        }),
    )
    .await;
    let page = job_page();
    send_client(
        &mut socket,
        GatewayClientFrameV5::Response(Box::new(RemoteResponseV5 {
            request_id: remote.request_id,
            generation,
            result: RemoteResponseResultV5::ListRuns(page.clone()),
        })),
    )
    .await;
    assert_eq!(
        request.await.expect("job request task"),
        Ok(RunQueryResultV5::ListRuns(page))
    );
}

#[derive(Clone, Copy, Debug)]
enum InFlightAuthorizationMutation {
    Rotate,
    RemoveRunQuery,
    RemoveGatewayConnect,
    Revoke,
}

#[tokio::test]
async fn in_flight_job_responses_are_discarded_after_authorization_changes() {
    for mutation in [
        InFlightAuthorizationMutation::Rotate,
        InFlightAuthorizationMutation::RemoveRunQuery,
        InFlightAuthorizationMutation::RemoveGatewayConnect,
        InFlightAuthorizationMutation::Revoke,
    ] {
        let server = TestServer::start().await;
        let device = server
            .device(&[DeviceScope::GatewayConnect, DeviceScope::RunQuery])
            .await;
        let (mut socket, generation) = server
            .connect_ready_with_capabilities(
                &device.token,
                vec![GatewayCapabilityV5::RemoteRunsReadV2],
            )
            .await;
        let device_id = device.id;
        let runtime = server.state.gateway.clone();
        let request = tokio::spawn(async move {
            runtime
                .request_job(
                    device_id,
                    RunQueryActionV5::ListRuns {
                        filter: RemoteRunFilterV5::Recent,
                        page_size: 10,
                        cursor: None,
                    },
                )
                .await
        });
        let GatewayServerFrameV5::Request(remote) = recv_server(&mut socket).await else {
            panic!("expected in-flight job query for {mutation:?}");
        };

        match mutation {
            InFlightAuthorizationMutation::Rotate => {
                server
                    .state
                    .device_auth
                    .rotate_device(
                        &device.id.to_string(),
                        &[DeviceScope::GatewayConnect, DeviceScope::RunQuery],
                    )
                    .await
                    .expect("rotate in-flight gateway credential");
            }
            InFlightAuthorizationMutation::RemoveRunQuery => {
                server
                    .state
                    .device_auth
                    .rotate_device(&device.id.to_string(), &[DeviceScope::GatewayConnect])
                    .await
                    .expect("remove in-flight job query scope");
            }
            InFlightAuthorizationMutation::RemoveGatewayConnect => {
                server
                    .state
                    .device_auth
                    .rotate_device(&device.id.to_string(), &[DeviceScope::RunQuery])
                    .await
                    .expect("remove in-flight gateway scope");
            }
            InFlightAuthorizationMutation::Revoke => {
                server
                    .state
                    .device_auth
                    .revoke_device(&device.id.to_string())
                    .await
                    .expect("revoke in-flight gateway device");
            }
        }

        send_client(
            &mut socket,
            GatewayClientFrameV5::RequestAck(RequestAckV5 {
                request_id: remote.request_id.clone(),
                generation,
                accepted: true,
            }),
        )
        .await;
        send_client(
            &mut socket,
            GatewayClientFrameV5::Response(Box::new(RemoteResponseV5 {
                request_id: remote.request_id,
                generation,
                result: RemoteResponseResultV5::ListRuns(job_page()),
            })),
        )
        .await;

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), request)
                .await
                .expect("post-response authorization fence")
                .expect("job request task"),
            Err(super::GatewayRequestError::Unauthorized),
            "response escaped the post-result authorization fence for {mutation:?}"
        );
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.supervisor.begin_shutdown();
        self.server.abort();
    }
}

async fn send_client(socket: &mut ClientSocket, frame: GatewayClientFrameV5) {
    let text = encode_client_frame(&frame).expect("client frame");
    socket
        .send(Message::Text(text.into()))
        .await
        .expect("send client frame");
}

async fn recv_server(socket: &mut ClientSocket) -> GatewayServerFrameV5 {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await.expect("server websocket message") {
                Ok(Message::Text(text)) => {
                    return decode_server_frame(text.as_str()).expect("server frame");
                }
                Ok(Message::Ping(payload)) => socket
                    .send(Message::Pong(payload))
                    .await
                    .expect("transport pong"),
                other => panic!("unexpected server websocket message: {other:?}"),
            }
        }
    })
    .await
    .expect("server frame timeout")
}

async fn recv_close(socket: &mut ClientSocket) -> u16 {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await.expect("server close message") {
                Ok(Message::Close(Some(frame))) => return u16::from(frame.code),
                Ok(Message::Text(_)) | Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                other => panic!("unexpected websocket close sequence: {other:?}"),
            }
        }
    })
    .await
    .expect("server close timeout")
}

#[tokio::test]
async fn handshake_requires_authentication_and_gateway_scope_and_fences_rotation_race() {
    let server = TestServer::start().await;
    assert!(matches!(server.connect(None).await, Err(401)));
    assert!(matches!(
        server.connect(Some("pdv2.invalid.invalid")).await,
        Err(401)
    ));

    let missing_scope = server.device(&[DeviceScope::NotifyWrite]).await;
    assert!(matches!(
        server.connect(Some(missing_scope.token.expose())).await,
        Err(403)
    ));

    let device = server.device(&[DeviceScope::GatewayConnect]).await;
    let mut wrong_secret = device.token.expose().to_owned();
    let replacement = if wrong_secret.ends_with('A') {
        'B'
    } else {
        'A'
    };
    wrong_secret.pop();
    wrong_secret.push(replacement);
    assert!(matches!(
        server.connect(Some(&wrong_secret)).await,
        Err(401)
    ));
    let authenticated = server
        .state
        .device_auth
        .authenticate(device.token.expose())
        .await
        .expect("authenticated snapshot");
    server
        .state
        .device_auth
        .rotate_device(&device.id.to_string(), &[DeviceScope::GatewayConnect])
        .await
        .expect("rotate device");
    assert_eq!(
        server.state.gateway.initial_lease(&authenticated).await,
        Err(GatewayHandshakeError::Unauthorized)
    );
}

#[tokio::test]
async fn binary_malformed_and_oversized_frames_close_with_stable_codes() {
    let server = TestServer::start().await;
    let device = server.device(&[DeviceScope::GatewayConnect]).await;

    let (mut binary, _) = server.connect_ready(&device.token).await;
    binary
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .expect("binary frame");
    assert_eq!(recv_close(&mut binary).await, CLOSE_CODE_PROTOCOL_ERROR);

    let (mut malformed, _) = server.connect_ready(&device.token).await;
    malformed
        .send(Message::Text("{not-json".into()))
        .await
        .expect("malformed frame");
    assert_eq!(recv_close(&mut malformed).await, CLOSE_CODE_PROTOCOL_ERROR);

    let (mut oversized, _) = server.connect_ready(&device.token).await;
    oversized
        .send(Message::Text(
            "x".repeat(MAX_GATEWAY_FRAME_BYTES + 1).into(),
        ))
        .await
        .expect("oversized frame");
    assert_eq!(recv_close(&mut oversized).await, CLOSE_CODE_FRAME_TOO_LARGE);
}

#[tokio::test]
async fn token_rotation_is_revalidated_within_thirty_seconds_and_closes_with_current_generation() {
    let server = TestServer::start().await;
    let device = server.device(&[DeviceScope::GatewayConnect]).await;
    let (mut socket, generation) = server.connect_ready(&device.token).await;
    server
        .state
        .device_auth
        .rotate_device(&device.id.to_string(), &[DeviceScope::GatewayConnect])
        .await
        .expect("rotate active gateway credential");

    // Do not pause Tokio time across the asynchronous SQLite mutation. A
    // paused runtime may auto-advance while the database future is pending,
    // allowing the 60-second pong deadline to win before this test explicitly
    // advances the 30-second authorization-revalidation window.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::time::resume();
    tokio::task::yield_now().await;
    loop {
        match recv_server(&mut socket).await {
            GatewayServerFrameV5::PolicyRevoked(frame) => {
                assert_eq!(frame.generation, generation);
                assert_eq!(
                    frame.code,
                    super::protocol::PolicyRevocationCodeV5::TokenRotated
                );
                break;
            }
            GatewayServerFrameV5::Ping(_) => {}
            other => panic!("unexpected frame before policy revocation: {other:?}"),
        }
    }
    assert_eq!(recv_close(&mut socket).await, CLOSE_CODE_POLICY_REVOKED);
}

#[tokio::test]
async fn runtime_policy_classifies_scope_revocation_and_disable_fail_closed() {
    let server = TestServer::start().await;

    let scope_device = server.device(&[DeviceScope::GatewayConnect]).await;
    let scope_auth = server
        .state
        .device_auth
        .authenticate(scope_device.token.expose())
        .await
        .expect("scope device auth");
    let scope_lease = server
        .state
        .gateway
        .initial_lease(&scope_auth)
        .await
        .expect("scope lease");
    sqlx::query("DELETE FROM device_scopes WHERE device_id=?1 AND scope='gateway:connect'")
        .bind(scope_device.id.to_string())
        .execute(&server.state.db)
        .await
        .expect("remove gateway scope");
    assert_eq!(
        server
            .state
            .gateway
            .authorization_policy(scope_device.id, &scope_lease)
            .await,
        Err(super::protocol::PolicyRevocationCodeV5::ScopeChanged)
    );

    let disabled = server.device(&[DeviceScope::GatewayConnect]).await;
    let disabled_auth = server
        .state
        .device_auth
        .authenticate(disabled.token.expose())
        .await
        .expect("disabled device auth");
    let disabled_lease = server
        .state
        .gateway
        .initial_lease(&disabled_auth)
        .await
        .expect("disabled lease");
    sqlx::query("UPDATE devices SET enabled=0 WHERE id=?1")
        .bind(disabled.id.to_string())
        .execute(&server.state.db)
        .await
        .expect("disable gateway device");
    assert_eq!(
        server
            .state
            .gateway
            .authorization_policy(disabled.id, &disabled_lease)
            .await,
        Err(super::protocol::PolicyRevocationCodeV5::DeviceDisabled)
    );

    let revoked = server.device(&[DeviceScope::GatewayConnect]).await;
    let revoked_auth = server
        .state
        .device_auth
        .authenticate(revoked.token.expose())
        .await
        .expect("revoked device auth");
    let revoked_lease = server
        .state
        .gateway
        .initial_lease(&revoked_auth)
        .await
        .expect("revoked lease");
    server
        .state
        .device_auth
        .revoke_device(&revoked.id.to_string())
        .await
        .expect("revoke gateway device");
    assert_eq!(
        server
            .state
            .gateway
            .authorization_policy(revoked.id, &revoked_lease)
            .await,
        Err(super::protocol::PolicyRevocationCodeV5::DeviceRevoked)
    );
}

#[tokio::test]
async fn missing_pong_closes_the_session_at_sixty_seconds() {
    let server = TestServer::start().await;
    let device = server.device(&[DeviceScope::GatewayConnect]).await;
    let (mut socket, _) = server.connect_ready(&device.token).await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    tokio::time::pause();

    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::time::resume();
    tokio::task::yield_now().await;
    assert_eq!(recv_close(&mut socket).await, CLOSE_CODE_PROTOCOL_ERROR);
}

#[tokio::test]
async fn second_connection_supersedes_first_with_new_generation_and_shutdown_is_joined() {
    let server = TestServer::start().await;
    let device = server.device(&[DeviceScope::GatewayConnect]).await;
    let (mut first, first_generation) = server.connect_ready(&device.token).await;
    assert_eq!(first_generation, 1);
    let (mut second, second_generation) = server.connect_ready(&device.token).await;
    assert_eq!(second_generation, 2);

    assert_eq!(
        recv_server(&mut first).await,
        GatewayServerFrameV5::Superseded(super::protocol::SupersededV5 {
            generation: second_generation,
        })
    );
    let close = first.next().await.expect("supersede close").expect("close");
    assert!(
        matches!(close, Message::Close(Some(frame)) if u16::from(frame.code) == CLOSE_CODE_SUPERSEDED)
    );

    server.supervisor.begin_shutdown();
    let close = tokio::time::timeout(Duration::from_secs(2), second.next())
        .await
        .expect("shutdown close timeout")
        .expect("shutdown close message")
        .expect("shutdown close");
    assert!(
        matches!(close, Message::Close(Some(frame)) if u16::from(frame.code) == CLOSE_CODE_SERVER_SHUTDOWN)
    );
    tokio::time::timeout(Duration::from_secs(2), server.supervisor.wait())
        .await
        .expect("tracked gateway session joined");
}

#[tokio::test]
async fn device_info_is_closed_typed_and_pending_is_bounded_at_sixteen() {
    let server = TestServer::start().await;
    let device = server.device(&[DeviceScope::GatewayConnect]).await;
    let (mut socket, generation) = server.connect_ready(&device.token).await;
    let registry = server.state.gateway.registry();

    let mut requests = Vec::new();
    for _ in 0..16 {
        let registry = registry.clone();
        requests.push(tokio::spawn(async move {
            registry.request_device_info(device.id).await
        }));
    }
    let mut request_ids = Vec::new();
    for _ in 0..16 {
        let GatewayServerFrameV5::Request(request) = recv_server(&mut socket).await else {
            panic!("expected get_device_info request");
        };
        assert_eq!(request.generation, generation);
        assert_eq!(
            request.action,
            super::protocol::RemoteRequestActionV5::GetDeviceInfo
        );
        request_ids.push(request.request_id);
    }

    assert_eq!(
        registry.request_device_info(device.id).await,
        Err(super::GatewayRequestError::PendingLimit)
    );

    let request_id = request_ids.remove(0);
    send_client(
        &mut socket,
        GatewayClientFrameV5::RequestAck(RequestAckV5 {
            request_id: request_id.clone(),
            generation,
            accepted: true,
        }),
    )
    .await;
    let expected = GetDeviceInfoResultV5 {
        schema_version: GATEWAY_PROTOCOL_VERSION,
        device_name: "GATEWAY TEST".to_owned(),
        client_version: "gateway-test/1".to_owned(),
        capabilities: vec![GatewayCapabilityV5::DeviceInfoV5],
    };
    send_client(
        &mut socket,
        GatewayClientFrameV5::Response(Box::new(RemoteResponseV5 {
            request_id,
            generation,
            result: RemoteResponseResultV5::GetDeviceInfo(expected.clone()),
        })),
    )
    .await;
    assert_eq!(
        requests.remove(0).await.expect("request task"),
        Ok(expected)
    );

    socket.close(None).await.expect("close gateway test socket");
    for request in requests {
        assert_eq!(
            request.await.expect("pending request task"),
            Err(super::GatewayRequestError::Disconnected)
        );
    }
}

#[tokio::test]
async fn stale_generation_response_is_rejected_without_affecting_the_new_connection_entry() {
    let server = TestServer::start().await;
    let device = server.device(&[DeviceScope::GatewayConnect]).await;
    let (mut first, first_generation) = server.connect_ready(&device.token).await;
    let registry = server.state.gateway.registry();
    let request = tokio::spawn({
        let registry = registry.clone();
        async move { registry.request_device_info(device.id).await }
    });
    let GatewayServerFrameV5::Request(old_request) = recv_server(&mut first).await else {
        panic!("expected old-generation request");
    };
    assert_eq!(old_request.generation, first_generation);

    let (mut second, second_generation) = server.connect_ready(&device.token).await;
    assert!(second_generation > first_generation);
    assert_eq!(
        registry
            .connection(device.id)
            .await
            .expect("new connection entry")
            .generation,
        second_generation
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .expect("old request completion")
            .expect("old request task"),
        Err(super::GatewayRequestError::Disconnected)
    );
    send_client(
        &mut second,
        GatewayClientFrameV5::Response(Box::new(RemoteResponseV5 {
            request_id: old_request.request_id,
            generation: first_generation,
            result: RemoteResponseResultV5::GetDeviceInfo(device_info_result()),
        })),
    )
    .await;
    assert_eq!(recv_close(&mut second).await, CLOSE_CODE_PROTOCOL_ERROR);
}

#[tokio::test]
async fn ack_response_and_error_are_all_strictly_fenced_at_the_monotonic_ttl() {
    for late_kind in 0..3 {
        let server = TestServer::start().await;
        let device = server.device(&[DeviceScope::GatewayConnect]).await;
        let (mut socket, generation) = server.connect_ready(&device.token).await;
        let registry = server.state.gateway.registry();
        let request = tokio::spawn(async move { registry.request_device_info(device.id).await });
        let GatewayServerFrameV5::Request(remote) = recv_server(&mut socket).await else {
            panic!("expected expiring request");
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::time::resume();
        tokio::task::yield_now().await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), request)
                .await
                .expect("request TTL completion")
                .expect("request task"),
            Err(super::GatewayRequestError::Expired)
        );

        let late = match late_kind {
            0 => GatewayClientFrameV5::RequestAck(RequestAckV5 {
                request_id: remote.request_id,
                generation,
                accepted: true,
            }),
            1 => GatewayClientFrameV5::Response(Box::new(RemoteResponseV5 {
                request_id: remote.request_id,
                generation,
                result: RemoteResponseResultV5::GetDeviceInfo(device_info_result()),
            })),
            _ => GatewayClientFrameV5::Error(RemoteErrorV5 {
                request_id: remote.request_id,
                generation,
                code: RemoteErrorCodeV5::RequestExpired,
                message: "expired".to_owned(),
                retryable: false,
            }),
        };
        send_client(&mut socket, late).await;
        assert_eq!(recv_close(&mut socket).await, CLOSE_CODE_PROTOCOL_ERROR);
    }
}

fn device_info_result() -> GetDeviceInfoResultV5 {
    GetDeviceInfoResultV5 {
        schema_version: GATEWAY_PROTOCOL_VERSION,
        device_name: "GATEWAY TEST".to_owned(),
        client_version: "gateway-test/1".to_owned(),
        capabilities: vec![GatewayCapabilityV5::DeviceInfoV5],
    }
}

fn job_page() -> RemoteRunPageV5 {
    RemoteRunPageV5 {
        items: vec![RemoteRunSummaryV5 {
            run_handle: "run_8f5d8b82c9124e12b7d9aabbccdd".into(),
            title: "PromptDock".into(),
            runtime_label: "Codex".into(),
            workspace_label: "PromptDock".into(),
            desired_state: RemoteDesiredStateV5::Running,
            phase: RemoteRunPhaseV5::Finished,
            outcome: RemoteRunOutcomeV5::Succeeded,
            conditions: vec![
                RemoteRunConditionV5::Accepted,
                RemoteRunConditionV5::TerminalObserved,
            ],
            attention_count: 0,
            started_at: 1_787_616_000_000,
            updated_at: 1_787_616_060_000,
            child_count: 0,
            active_child_count: 0,
        }],
        next_cursor: Some("cursor_11223344556647889900aabbccdd".into()),
    }
}
