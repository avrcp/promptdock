use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::{Host, Url};
use zeroize::Zeroizing;

use super::credentials::validate_device_token;
use super::protocol::{
    decode_error_code, RelayAcceptedNotification, RelayHealthResponse, RelayNotificationStatus,
    RelayNotificationV1, RelayProbeResult, RelayProtocolError, RelayServerInfo,
};
use super::result_protocol::{
    RelayDeviceStatus, RelayResultLink, RelayResultPublication, RelayResultReceipt,
};

const LIVE_PATH: &str = "health/live";
const READY_PATH: &str = "health/ready";
const SERVER_INFO_PATH: &str = "v1/server-info";
const NOTIFICATIONS_PATH: &str = "v1/notifications";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
enum EndpointPolicy {
    Runtime,
    #[cfg(test)]
    LocalTest(Url),
}

#[derive(Clone)]
pub(crate) struct RelayHttpClient {
    client: Client,
    endpoint_policy: EndpointPolicy,
    request_timeout: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayEndpoint {
    Live,
    Ready,
    ServerInfo,
    Notifications,
    Results,
    DeviceStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayTransportErrorKind {
    Connect,
    Request,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum RelayHttpError {
    #[error("invalid relay endpoint")]
    InvalidEndpoint,
    #[error("invalid relay device token")]
    InvalidDeviceToken,
    #[error("relay HTTP client configuration failed")]
    ClientConfiguration,
    #[error("relay request was cancelled")]
    Cancelled,
    #[error("relay request timed out")]
    Timeout,
    #[error("relay network request failed: {0:?}")]
    Transport(RelayTransportErrorKind),
    #[error("relay authentication was rejected with HTTP {0}")]
    AuthenticationRejected(u16),
    #[error("relay device token lacks permission for this operation")]
    InsufficientScope,
    #[error("relay has no bound notification target")]
    TargetUnavailable,
    #[error("relay requested a bounded retry delay")]
    RetryAfter { delay_ms: i64 },
    #[error("relay is not ready (HTTP {0})")]
    NotReady(u16),
    #[error("relay endpoint {endpoint:?} returned HTTP {status}")]
    HttpStatus {
        endpoint: RelayEndpoint,
        status: u16,
    },
    #[error("relay returned an oversized response")]
    ResponseTooLarge,
    #[error("relay endpoint {0:?} returned invalid JSON")]
    InvalidJson(RelayEndpoint),
    #[error("relay endpoint {0:?} returned an invalid response")]
    InvalidResponse(RelayEndpoint),
    #[error("relay API version {0} is unsupported")]
    UnsupportedApiVersion(u32),
}

impl RelayHttpError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "RELAY_ENDPOINT_INVALID",
            Self::InvalidDeviceToken => "RELAY_TOKEN_INVALID",
            Self::ClientConfiguration => "RELAY_CLIENT_UNAVAILABLE",
            Self::Cancelled => "RELAY_CANCELLED",
            Self::Timeout => "RELAY_TIMEOUT",
            Self::Transport(_) => "RELAY_UNREACHABLE",
            Self::AuthenticationRejected(_) => "RELAY_AUTH_FAILED",
            Self::InsufficientScope => "RELAY_INSUFFICIENT_SCOPE",
            Self::TargetUnavailable => "RELAY_TARGET_UNAVAILABLE",
            Self::RetryAfter { .. } => "RELAY_RETRY_AFTER",
            Self::NotReady(_) => "RELAY_NOT_READY",
            Self::HttpStatus {
                endpoint: RelayEndpoint::Results,
                status: 404,
            } => "RELAY_RESULT_NOT_FOUND",
            Self::HttpStatus { .. } => "RELAY_HTTP_STATUS",
            Self::ResponseTooLarge => "RELAY_RESPONSE_TOO_LARGE",
            Self::InvalidJson(_) | Self::InvalidResponse(_) => "RELAY_RESPONSE_INVALID",
            Self::UnsupportedApiVersion(_) => "RELAY_API_UNSUPPORTED",
        }
    }
}

impl RelayHttpClient {
    pub(crate) fn new() -> Result<Self, RelayHttpError> {
        Ok(Self {
            client: build_client(CONNECT_TIMEOUT)?,
            endpoint_policy: EndpointPolicy::Runtime,
            request_timeout: REQUEST_TIMEOUT,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        base_url: Url,
        request_timeout: Duration,
    ) -> Result<Self, RelayHttpError> {
        validate_base_url(&base_url)?;
        Ok(Self {
            client: build_client(CONNECT_TIMEOUT)?,
            endpoint_policy: EndpointPolicy::LocalTest(base_url),
            request_timeout,
        })
    }

    pub(crate) fn canonical_base_url(&self, raw: &str) -> Result<String, RelayHttpError> {
        Ok(self.parse_base_url(raw)?.to_string())
    }

    pub(crate) async fn probe(
        &self,
        base_url: &str,
        device_token: &str,
        cancellation: &CancellationToken,
    ) -> Result<RelayProbeResult, RelayHttpError> {
        let base_url = self.parse_base_url(base_url)?;
        let authorization = bearer_header(device_token)?;

        self.probe_health(
            &base_url,
            LIVE_PATH,
            RelayEndpoint::Live,
            "ok",
            cancellation,
        )
        .await?;
        self.probe_health(
            &base_url,
            READY_PATH,
            RelayEndpoint::Ready,
            "ready",
            cancellation,
        )
        .await?;

        let endpoint = join_endpoint(&base_url, SERVER_INFO_PATH)?;
        let request = self
            .client
            .get(endpoint)
            .header(AUTHORIZATION, authorization)
            .timeout(self.request_timeout);
        let response = self
            .execute(request, RelayEndpoint::ServerInfo, cancellation)
            .await?;
        if response.status != StatusCode::OK {
            inspect_error_envelope(&response.bytes);
            return if matches!(
                response.status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) {
                Err(classify_auth_rejection(response.status, &response.bytes))
            } else {
                Err(RelayHttpError::HttpStatus {
                    endpoint: RelayEndpoint::ServerInfo,
                    status: response.status.as_u16(),
                })
            };
        }
        ensure_json_content_type(&response, RelayEndpoint::ServerInfo)?;

        let server_info: RelayServerInfo = serde_json::from_slice(&response.bytes)
            .map_err(|_| RelayHttpError::InvalidJson(RelayEndpoint::ServerInfo))?;
        let server_info = server_info.validate().map_err(map_protocol_error)?;
        let mut probe = RelayProbeResult::from_server_info(server_info);
        if probe
            .server_info
            .features
            .iter()
            .any(|feature| feature == "device_status_v1")
        {
            let status: RelayDeviceStatus = self
                .authenticated_json(
                    self.client
                        .get(join_endpoint(&base_url, "v1/device-status")?),
                    device_token,
                    RelayEndpoint::DeviceStatus,
                    StatusCode::OK,
                    cancellation,
                )
                .await?;
            if super::credentials::device_id_from_token(device_token).ok()
                != Some(status.device_id.as_str())
            {
                return Err(RelayHttpError::InvalidResponse(RelayEndpoint::DeviceStatus));
            }
            probe.device_status = Some(status);
        }
        Ok(probe)
    }

    pub(crate) async fn create_notification(
        &self,
        base_url: &str,
        device_token: &str,
        notification: &RelayNotificationV1,
        cancellation: &CancellationToken,
    ) -> Result<RelayAcceptedNotification, RelayHttpError> {
        let endpoint = join_endpoint(&self.parse_base_url(base_url)?, NOTIFICATIONS_PATH)?;
        let accepted: RelayAcceptedNotification = self
            .authenticated_json(
                self.client.post(endpoint).json(notification),
                device_token,
                RelayEndpoint::Notifications,
                StatusCode::ACCEPTED,
                cancellation,
            )
            .await?;
        if !accepted.is_valid_for(Some(&notification.notification_id)) {
            return Err(RelayHttpError::InvalidResponse(
                RelayEndpoint::Notifications,
            ));
        }
        Ok(accepted)
    }

    pub(crate) async fn notification_status(
        &self,
        base_url: &str,
        device_token: &str,
        notification_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<RelayNotificationStatus, RelayHttpError> {
        let endpoint = resource_endpoint(
            &self.parse_base_url(base_url)?,
            NOTIFICATIONS_PATH,
            notification_id,
        )?;
        let status: RelayNotificationStatus = self
            .authenticated_json(
                self.client.get(endpoint),
                device_token,
                RelayEndpoint::Notifications,
                StatusCode::OK,
                cancellation,
            )
            .await?;
        if !status.is_valid_for(notification_id) {
            return Err(RelayHttpError::InvalidResponse(
                RelayEndpoint::Notifications,
            ));
        }
        Ok(status)
    }

    pub(crate) async fn create_result(
        &self,
        base_url: &str,
        device_token: &str,
        result: &RelayResultPublication,
        cancellation: &CancellationToken,
    ) -> Result<RelayResultReceipt, RelayHttpError> {
        let endpoint = join_endpoint(&self.parse_base_url(base_url)?, "v1/results")?;
        let status: RelayResultReceipt = self
            .authenticated_json(
                self.client.post(endpoint).json(result),
                device_token,
                RelayEndpoint::Results,
                StatusCode::ACCEPTED,
                cancellation,
            )
            .await?;
        if !status.is_valid_for(&result.result_id, &result.source_hash) {
            return Err(RelayHttpError::InvalidResponse(RelayEndpoint::Results));
        }
        Ok(status)
    }

    pub(crate) async fn result_status(
        &self,
        base_url: &str,
        device_token: &str,
        result_id: &str,
        source_hash: &str,
        cancellation: &CancellationToken,
    ) -> Result<RelayResultReceipt, RelayHttpError> {
        let endpoint = resource_endpoint(&self.parse_base_url(base_url)?, "v1/results", result_id)?;
        let status: RelayResultReceipt = self
            .authenticated_json(
                self.client.get(endpoint),
                device_token,
                RelayEndpoint::Results,
                StatusCode::OK,
                cancellation,
            )
            .await?;
        if !status.is_valid_for(result_id, source_hash) {
            return Err(RelayHttpError::InvalidResponse(RelayEndpoint::Results));
        }
        Ok(status)
    }

    pub(crate) async fn result_link(
        &self,
        base_url: &str,
        device_token: &str,
        result_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<RelayResultLink, RelayHttpError> {
        let endpoint = result_action_endpoint(&self.parse_base_url(base_url)?, result_id, "link")?;
        self.authenticated_json(
            self.client.get(endpoint),
            device_token,
            RelayEndpoint::Results,
            StatusCode::OK,
            cancellation,
        )
        .await
    }

    pub(crate) async fn result_action(
        &self,
        base_url: &str,
        device_token: &str,
        result_id: &str,
        action: &str,
        request_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<RelayResultReceipt, RelayHttpError> {
        let endpoint = result_action_endpoint(&self.parse_base_url(base_url)?, result_id, action)?;
        self.authenticated_json(
            self.client
                .post(endpoint)
                .json(&serde_json::json!({"requestId":request_id})),
            device_token,
            RelayEndpoint::Results,
            StatusCode::OK,
            cancellation,
        )
        .await
    }

    async fn authenticated_json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        device_token: &str,
        endpoint: RelayEndpoint,
        expected_status: StatusCode,
        cancellation: &CancellationToken,
    ) -> Result<T, RelayHttpError> {
        let response = self
            .execute(
                request
                    .header(AUTHORIZATION, bearer_header(device_token)?)
                    .timeout(self.request_timeout),
                endpoint,
                cancellation,
            )
            .await?;
        if response.status != expected_status {
            inspect_error_envelope(&response.bytes);
            if endpoint == RelayEndpoint::Results
                && response.status == StatusCode::CONFLICT
                && decode_error_code(&response.bytes).as_deref() == Some("TARGET_UNAVAILABLE")
            {
                return Err(RelayHttpError::TargetUnavailable);
            }
            if matches!(response.status.as_u16(), 429 | 503) {
                if let Some(delay_ms) = response
                    .headers
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(retry_after_ms)
                {
                    return Err(RelayHttpError::RetryAfter { delay_ms });
                }
            }
            return if matches!(
                response.status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) {
                Err(classify_auth_rejection(response.status, &response.bytes))
            } else {
                Err(RelayHttpError::HttpStatus {
                    endpoint,
                    status: response.status.as_u16(),
                })
            };
        }
        ensure_json_content_type(&response, endpoint)?;
        serde_json::from_slice(&response.bytes).map_err(|_| RelayHttpError::InvalidJson(endpoint))
    }

    async fn probe_health(
        &self,
        base_url: &Url,
        path: &str,
        kind: RelayEndpoint,
        expected_status: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), RelayHttpError> {
        let endpoint = join_endpoint(base_url, path)?;
        // Health endpoints are deliberately built without the authorization map.
        let request = self.client.get(endpoint).timeout(self.request_timeout);
        let response = self.execute(request, kind, cancellation).await?;
        if response.status != StatusCode::OK {
            inspect_error_envelope(&response.bytes);
            return if kind == RelayEndpoint::Ready {
                Err(RelayHttpError::NotReady(response.status.as_u16()))
            } else {
                Err(RelayHttpError::HttpStatus {
                    endpoint: kind,
                    status: response.status.as_u16(),
                })
            };
        }
        ensure_json_content_type(&response, kind)?;
        let health: RelayHealthResponse = serde_json::from_slice(&response.bytes)
            .map_err(|_| RelayHttpError::InvalidJson(kind))?;
        if !health.is_status(expected_status) {
            return Err(RelayHttpError::InvalidResponse(kind));
        }
        Ok(())
    }

    async fn execute(
        &self,
        request: RequestBuilder,
        endpoint: RelayEndpoint,
        cancellation: &CancellationToken,
    ) -> Result<BufferedResponse, RelayHttpError> {
        let operation = async {
            let mut response = request.send().await.map_err(map_reqwest_error)?;
            if response
                .content_length()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
            {
                return Err(RelayHttpError::ResponseTooLarge);
            }
            let status = response.status();
            let headers = response.headers().clone();
            let mut bytes = Vec::with_capacity(
                response
                    .content_length()
                    .unwrap_or_default()
                    .min(MAX_RESPONSE_BYTES as u64) as usize,
            );
            while let Some(chunk) = response.chunk().await.map_err(map_reqwest_error)? {
                if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                    return Err(RelayHttpError::ResponseTooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(BufferedResponse {
                status,
                headers,
                bytes,
            })
        };
        cancellation
            .run_until_cancelled(operation)
            .await
            .ok_or(RelayHttpError::Cancelled)?
            .map_err(|error| match error {
                RelayHttpError::InvalidJson(_) | RelayHttpError::InvalidResponse(_) => {
                    RelayHttpError::InvalidResponse(endpoint)
                }
                other => other,
            })
    }

    fn parse_base_url(&self, raw: &str) -> Result<Url, RelayHttpError> {
        if raw.trim() != raw {
            return Err(RelayHttpError::InvalidEndpoint);
        }
        let url = Url::parse(raw).map_err(|_| RelayHttpError::InvalidEndpoint)?;
        match &self.endpoint_policy {
            EndpointPolicy::Runtime => validate_base_url(&url)?,
            #[cfg(test)]
            EndpointPolicy::LocalTest(expected) => {
                validate_base_url(&url)?;
                if &url != expected {
                    return Err(RelayHttpError::InvalidEndpoint);
                }
            }
        }
        Ok(url)
    }
}

struct BufferedResponse {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
}

fn retry_after_ms(value: &str) -> Option<i64> {
    let seconds = if let Ok(seconds) = value.parse::<u64>() {
        seconds.min(86_400) as i64
    } else {
        let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
        date.timestamp()
            .saturating_sub(chrono::Utc::now().timestamp())
            .clamp(0, 86_400)
    };
    Some(seconds.saturating_mul(1000).max(1000))
}

#[cfg(test)]
mod retry_after_tests {
    use super::*;
    #[test]
    fn delay_supports_seconds_and_http_date_without_unbounded_waits() {
        assert_eq!(retry_after_ms("37"), Some(37_000));
        assert_eq!(retry_after_ms("999999999"), Some(86_400_000));
        assert_eq!(retry_after_ms("-2"), None);
        assert_eq!(retry_after_ms("invalid"), None);
        let later = (chrono::Utc::now() + chrono::Duration::seconds(90)).to_rfc2822();
        assert!((88_000..=90_000).contains(&retry_after_ms(&later).unwrap()));
    }
}

fn ensure_json_content_type(
    response: &BufferedResponse,
    endpoint: RelayEndpoint,
) -> Result<(), RelayHttpError> {
    let is_json = response
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
    if is_json {
        Ok(())
    } else {
        Err(RelayHttpError::InvalidResponse(endpoint))
    }
}

fn build_client(connect_timeout: Duration) -> Result<Client, RelayHttpError> {
    Client::builder()
        .connect_timeout(connect_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| RelayHttpError::ClientConfiguration)
}

fn validate_base_url(url: &Url) -> Result<(), RelayHttpError> {
    let host = url.host().ok_or(RelayHttpError::InvalidEndpoint)?;
    let scheme_allowed =
        url.scheme() == "https" || (url.scheme() == "http" && is_numeric_loopback(host));
    let valid = scheme_allowed
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.path(), "" | "/")
        && url.query().is_none()
        && url.fragment().is_none();
    if valid {
        Ok(())
    } else {
        Err(RelayHttpError::InvalidEndpoint)
    }
}

fn is_numeric_loopback(host: Host<&str>) -> bool {
    match host {
        Host::Domain(_) => false,
        Host::Ipv4(value) => value == Ipv4Addr::LOCALHOST,
        Host::Ipv6(value) => value == Ipv6Addr::LOCALHOST,
    }
}

fn join_endpoint(base_url: &Url, path: &str) -> Result<Url, RelayHttpError> {
    base_url
        .join(path)
        .map_err(|_| RelayHttpError::InvalidEndpoint)
}

fn resource_endpoint(base_url: &Url, path: &str, resource: &str) -> Result<Url, RelayHttpError> {
    if resource.is_empty() || resource.len() > 256 || resource.chars().any(char::is_control) {
        return Err(RelayHttpError::InvalidEndpoint);
    }
    let mut url = join_endpoint(base_url, path)?;
    url.path_segments_mut()
        .map_err(|_| RelayHttpError::InvalidEndpoint)?
        .push(resource);
    Ok(url)
}

fn result_action_endpoint(
    base_url: &Url,
    result_id: &str,
    action: &str,
) -> Result<Url, RelayHttpError> {
    if !matches!(action, "link" | "revoke" | "resend") {
        return Err(RelayHttpError::InvalidEndpoint);
    }
    let mut url = resource_endpoint(base_url, "v1/results", result_id)?;
    url.path_segments_mut()
        .map_err(|_| RelayHttpError::InvalidEndpoint)?
        .push(action);
    Ok(url)
}

fn bearer_header(device_token: &str) -> Result<HeaderValue, RelayHttpError> {
    validate_device_token(device_token).map_err(|_| RelayHttpError::InvalidDeviceToken)?;
    let authorization = Zeroizing::new(format!("Bearer {device_token}"));
    let mut header =
        HeaderValue::from_str(&authorization).map_err(|_| RelayHttpError::InvalidDeviceToken)?;
    header.set_sensitive(true);
    Ok(header)
}

fn inspect_error_envelope(bytes: &[u8]) {
    // Parse only the bounded code. Message and requestId are intentionally ignored so
    // hostile response data cannot flow into Debug, logs, status, or diagnostics.
    let _ = decode_error_code(bytes);
}

fn classify_auth_rejection(status: StatusCode, bytes: &[u8]) -> RelayHttpError {
    if status == StatusCode::FORBIDDEN {
        return match decode_error_code(bytes).as_deref() {
            Some("INSUFFICIENT_SCOPE") => RelayHttpError::InsufficientScope,
            _ => RelayHttpError::AuthenticationRejected(status.as_u16()),
        };
    }
    RelayHttpError::AuthenticationRejected(status.as_u16())
}

fn map_protocol_error(error: RelayProtocolError) -> RelayHttpError {
    match error {
        RelayProtocolError::InvalidServerInfo => {
            RelayHttpError::InvalidResponse(RelayEndpoint::ServerInfo)
        }
        RelayProtocolError::UnsupportedApiVersion(version) => {
            RelayHttpError::UnsupportedApiVersion(version)
        }
    }
}

fn map_reqwest_error(error: reqwest::Error) -> RelayHttpError {
    if error.is_connect() {
        RelayHttpError::Transport(RelayTransportErrorKind::Connect)
    } else if error.is_timeout() {
        RelayHttpError::Timeout
    } else {
        RelayHttpError::Transport(RelayTransportErrorKind::Request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TEST_DEVICE_TOKEN: &str =
        "pdv2.123e4567-e89b-12d3-a456-426614174000.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    fn run_async(test: impl std::future::Future<Output = ()>) {
        tauri::async_runtime::block_on(test);
    }

    fn client(server: &MockServer) -> RelayHttpClient {
        RelayHttpClient::new_for_test(Url::parse(&server.uri()).unwrap(), Duration::from_secs(2))
            .unwrap()
    }

    async fn mount_live(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/health/live"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"ok"})))
            .mount(server)
            .await;
    }

    async fn mount_ready(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/health/ready"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"ready"})))
            .mount(server)
            .await;
    }

    #[test]
    fn successful_probe_is_get_only_and_authenticates_only_server_info() {
        run_async(async {
            let server = MockServer::start().await;
            mount_live(&server).await;
            mount_ready(&server).await;
            Mock::given(method("GET"))
                .and(path("/v1/server-info"))
                .and(header(
                    "authorization",
                    format!("Bearer {TEST_DEVICE_TOKEN}"),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "apiVersion": 1,
                    "serverVersion": "3.0.0",
                    "features": ["device_scopes_v1", "notifications", "future-feature"],
                    "futureField": true
                })))
                .mount(&server)
                .await;

            let result = client(&server)
                .probe(&server.uri(), TEST_DEVICE_TOKEN, &CancellationToken::new())
                .await
                .unwrap();
            assert!(result.supports_notifications);
            assert!(result.supports_device_scopes);
            assert_eq!(result.server_info.features.len(), 3);

            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 3);
            assert!(requests
                .iter()
                .all(|request| request.method.as_str() == "GET"));
            assert!(requests[0].headers.get("authorization").is_none());
            assert!(requests[1].headers.get("authorization").is_none());
            assert_eq!(
                requests[2]
                    .headers
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                format!("Bearer {TEST_DEVICE_TOKEN}").as_str()
            );
        });
    }

    #[test]
    fn bearer_header_is_sensitive() {
        let header = bearer_header(TEST_DEVICE_TOKEN).unwrap();
        assert!(header.is_sensitive());
        assert_eq!(header, format!("Bearer {TEST_DEVICE_TOKEN}"));
        assert_eq!(
            bearer_header(&format!(" {TEST_DEVICE_TOKEN}")).unwrap_err(),
            RelayHttpError::InvalidDeviceToken
        );
        assert_eq!(
            bearer_header(&format!("{TEST_DEVICE_TOKEN} ")).unwrap_err(),
            RelayHttpError::InvalidDeviceToken
        );
    }

    #[test]
    fn unavailable_result_target_is_not_an_idempotency_conflict() {
        run_async(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/results"))
                .respond_with(
                    ResponseTemplate::new(409)
                        .set_body_json(json!({"error":{"code":"TARGET_UNAVAILABLE"}})),
                )
                .mount(&server)
                .await;
            let client = client(&server);
            let error = client
                .authenticated_json::<serde_json::Value>(
                    client
                        .client
                        .post(format!("{}/v1/results", server.uri()))
                        .json(&json!({})),
                    TEST_DEVICE_TOKEN,
                    RelayEndpoint::Results,
                    StatusCode::ACCEPTED,
                    &CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert_eq!(error, RelayHttpError::TargetUnavailable);
        });
    }

    #[test]
    fn device_permissions_are_read_without_attempting_a_notification() {
        run_async(async {
            let server = MockServer::start().await;
            mount_live(&server).await;
            mount_ready(&server).await;
            Mock::given(method("GET")).and(path("/v1/server-info"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "apiVersion":1,"serverVersion":"test","features":["notifications","device_scopes_v1","device_status_v1"]
                }))).mount(&server).await;
            Mock::given(method("GET")).and(path("/v1/device-status"))
                .and(header("authorization", format!("Bearer {TEST_DEVICE_TOKEN}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "deviceId": super::super::credentials::device_id_from_token(TEST_DEVICE_TOKEN).unwrap(),
                    "canSubmit":true,"canReadOwn":false,"channelState":"unknown"
                }))).mount(&server).await;
            let result = client(&server)
                .probe(&server.uri(), TEST_DEVICE_TOKEN, &CancellationToken::new())
                .await
                .unwrap();
            let permissions = result.device_status.unwrap();
            assert!(permissions.can_submit);
            assert!(!permissions.can_read_own);
            assert_eq!(server.received_requests().await.unwrap().len(), 4);
        });
    }

    #[test]
    fn result_round_trip_preserves_body_and_rejects_wrong_receipt_hash() {
        run_async(async {
            let server = MockServer::start().await;
            let hash = "a".repeat(64);
            let body = "  中文\r\n```rust\r\n  let x = \\\"🧪\\\";\r\n```\n\n";
            let result = RelayResultPublication {
                schema_version: 1,
                result_id: "result-1".into(),
                dedupe_key: "dedupe-1".into(),
                kind: "run_completed".into(),
                content_mode: "full_final".into(),
                source: "codex_stop".into(),
                correlation_key: "run-1".into(),
                result_revision: "1".into(),
                title: "Completed".into(),
                body: body.into(),
                source_hash: hash.clone(),
                created_at: 1,
                notification_expires_at: 100,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            };
            Mock::given(method("POST"))
                .and(path("/v1/results"))
                .respond_with(ResponseTemplate::new(202).set_body_json(json!({
                    "schemaVersion":1,"resultId":"result-1","sourceHash":hash,"acceptedAt":2,"updatedAt":2,
                    "pageState":"available","pageExpiresAt":100,"notificationId":"notification-1","notificationStatus":"pending_channel"
                })))
                .mount(&server)
                .await;
            client(&server)
                .create_result(
                    &server.uri(),
                    TEST_DEVICE_TOKEN,
                    &result,
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            let requests = server.received_requests().await.unwrap();
            let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert_eq!(sent["body"].as_str().unwrap().as_bytes(), body.as_bytes());
            Mock::given(method("GET")).and(path("/v1/results/result-1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "schemaVersion":1,"resultId":"result-1","sourceHash":"b".repeat(64),"acceptedAt":2,"updatedAt":3,
                    "pageState":"available","pageExpiresAt":100,"notificationId":"notification-1","notificationStatus":"provider_accepted"
                }))).mount(&server).await;
            assert_eq!(
                client(&server)
                    .result_status(
                        &server.uri(),
                        TEST_DEVICE_TOKEN,
                        "result-1",
                        &hash,
                        &CancellationToken::new()
                    )
                    .await
                    .unwrap_err(),
                RelayHttpError::InvalidResponse(RelayEndpoint::Results)
            );
        });
    }

    #[test]
    fn result_actions_keep_the_result_id_in_the_path() {
        run_async(async {
            let server = MockServer::start().await;
            let receipt = json!({"schemaVersion":1,"resultId":"result-1","sourceHash":"a".repeat(64),"acceptedAt":2,"updatedAt":2,"pageState":"available","pageExpiresAt":100,"notificationId":"notification-1","notificationStatus":"pending_channel"});
            Mock::given(method("GET"))
                .and(path("/v1/results/result-1/link"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({"url":"https://relay.example.test/r/token","expiresAt":100}),
                ))
                .mount(&server)
                .await;
            for action in ["revoke", "resend"] {
                Mock::given(method("POST"))
                    .and(path(format!("/v1/results/result-1/{action}")))
                    .respond_with(ResponseTemplate::new(200).set_body_json(receipt.clone()))
                    .mount(&server)
                    .await;
            }
            let client = client(&server);
            client
                .result_link(
                    &server.uri(),
                    TEST_DEVICE_TOKEN,
                    "result-1",
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            for action in ["revoke", "resend"] {
                client
                    .result_action(
                        &server.uri(),
                        TEST_DEVICE_TOKEN,
                        "result-1",
                        action,
                        "123e4567-e89b-12d3-a456-426614174000",
                        &CancellationToken::new(),
                    )
                    .await
                    .unwrap();
            }
        });
    }

    #[test]
    fn result_action_not_found_is_a_stable_permanent_error() {
        run_async(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/results/missing/resend"))
                .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                    "error":{"code":"NOT_FOUND","message":"private detail","requestId":"req-secret"}
                })))
                .mount(&server)
                .await;

            let error = client(&server)
                .result_action(
                    &server.uri(),
                    TEST_DEVICE_TOKEN,
                    "missing",
                    "resend",
                    "123e4567-e89b-12d3-a456-426614174000",
                    &CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert_eq!(
                error,
                RelayHttpError::HttpStatus {
                    endpoint: RelayEndpoint::Results,
                    status: 404,
                }
            );
            assert_eq!(error.code(), "RELAY_RESULT_NOT_FOUND");
            let debug = format!("{error:?}");
            assert!(!debug.contains("private detail"));
            assert!(!debug.contains("req-secret"));
        });
    }

    #[test]
    fn server_info_401_and_403_are_stable_authentication_errors() {
        for status in [401, 403] {
            run_async(async move {
                let server = MockServer::start().await;
                mount_live(&server).await;
                mount_ready(&server).await;
                Mock::given(method("GET"))
                    .and(path("/v1/server-info"))
                    .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                        "error":{"code":"AUTH_FAILED","message":"token=device-secret","requestId":"req-secret"}
                    })))
                    .mount(&server)
                    .await;
                let error = client(&server)
                    .probe(&server.uri(), TEST_DEVICE_TOKEN, &CancellationToken::new())
                    .await
                    .unwrap_err();
                assert_eq!(error, RelayHttpError::AuthenticationRejected(status));
                assert_eq!(error.code(), "RELAY_AUTH_FAILED");
                let debug = format!("{error:?}");
                assert!(!debug.contains("device-secret"));
                assert!(!debug.contains("req-secret"));
            });
        }
    }

    #[test]
    fn only_exact_bounded_insufficient_scope_403_gets_the_scope_variant() {
        let insufficient = br#"{"error":{"code":"INSUFFICIENT_SCOPE","message":"token=secret","requestId":"req-secret"}}"#;
        assert_eq!(
            classify_auth_rejection(StatusCode::FORBIDDEN, insufficient),
            RelayHttpError::InsufficientScope
        );
        assert_eq!(
            classify_auth_rejection(StatusCode::UNAUTHORIZED, insufficient),
            RelayHttpError::AuthenticationRejected(401)
        );
        for body in [
            br#"{"error":{"code":"DEVICE_REVOKED"}}"#.as_slice(),
            br#"{"error":{"code":"insufficient_scope"}}"#.as_slice(),
            br#"{"error":{"code":"INSUFFICIENT_SCOPE ","requestId":"req"}}"#.as_slice(),
            b"not-json".as_slice(),
        ] {
            assert_eq!(
                classify_auth_rejection(StatusCode::FORBIDDEN, body),
                RelayHttpError::AuthenticationRejected(403)
            );
        }
        let debug = format!(
            "{:?}",
            classify_auth_rejection(StatusCode::FORBIDDEN, insufficient)
        );
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("req-secret"));
    }

    #[test]
    fn ready_503_is_not_ready_and_does_not_reach_server_info() {
        run_async(async {
            let server = MockServer::start().await;
            mount_live(&server).await;
            Mock::given(method("GET"))
                .and(path("/health/ready"))
                .respond_with(ResponseTemplate::new(503).set_body_json(json!({
                    "error":{"code":"NOT_READY","message":"private detail","requestId":"req-1"}
                })))
                .mount(&server)
                .await;
            let error = client(&server)
                .probe(&server.uri(), TEST_DEVICE_TOKEN, &CancellationToken::new())
                .await
                .unwrap_err();
            assert_eq!(error, RelayHttpError::NotReady(503));
            assert_eq!(server.received_requests().await.unwrap().len(), 2);
        });
    }

    #[test]
    fn redirects_are_not_followed() {
        run_async(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/health/live"))
                .respond_with(ResponseTemplate::new(302).insert_header("location", "/redirected"))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/redirected"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"ok"})))
                .mount(&server)
                .await;
            let error = client(&server)
                .probe(&server.uri(), TEST_DEVICE_TOKEN, &CancellationToken::new())
                .await
                .unwrap_err();
            assert_eq!(
                error,
                RelayHttpError::HttpStatus {
                    endpoint: RelayEndpoint::Live,
                    status: 302
                }
            );
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        });
    }

    #[test]
    fn invalid_json_and_wrong_health_status_are_rejected() {
        run_async(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/health/live"))
                .respond_with(ResponseTemplate::new(200).set_body_raw("not-json", "text/plain"))
                .mount(&server)
                .await;
            assert_eq!(
                client(&server)
                    .probe(&server.uri(), TEST_DEVICE_TOKEN, &CancellationToken::new())
                    .await
                    .unwrap_err(),
                RelayHttpError::InvalidResponse(RelayEndpoint::Live)
            );
        });

        run_async(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/health/live"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"ready"})))
                .mount(&server)
                .await;
            assert_eq!(
                client(&server)
                    .probe(&server.uri(), TEST_DEVICE_TOKEN, &CancellationToken::new())
                    .await
                    .unwrap_err(),
                RelayHttpError::InvalidResponse(RelayEndpoint::Live)
            );
        });
    }

    #[test]
    fn oversized_response_is_rejected() {
        run_async(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/health/live"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_bytes(vec![b'x'; MAX_RESPONSE_BYTES + 1]),
                )
                .mount(&server)
                .await;
            assert_eq!(
                client(&server)
                    .probe(&server.uri(), TEST_DEVICE_TOKEN, &CancellationToken::new())
                    .await
                    .unwrap_err(),
                RelayHttpError::ResponseTooLarge
            );
        });
    }

    #[test]
    fn total_timeout_and_cancellation_are_stable() {
        run_async(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/health/live"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_millis(200))
                        .set_body_json(json!({"status":"ok"})),
                )
                .mount(&server)
                .await;
            let timeout_client = RelayHttpClient::new_for_test(
                Url::parse(&server.uri()).unwrap(),
                Duration::from_millis(25),
            )
            .unwrap();
            assert_eq!(
                timeout_client
                    .probe(&server.uri(), TEST_DEVICE_TOKEN, &CancellationToken::new())
                    .await
                    .unwrap_err(),
                RelayHttpError::Timeout
            );

            let cancellation = CancellationToken::new();
            cancellation.cancel();
            assert_eq!(
                client(&server)
                    .probe(&server.uri(), TEST_DEVICE_TOKEN, &cancellation)
                    .await
                    .unwrap_err(),
                RelayHttpError::Cancelled
            );
        });
    }

    #[test]
    fn runtime_url_rules_allow_only_https_or_numeric_loopback_http() {
        assert_eq!(
            RelayHttpClient::new()
                .unwrap()
                .canonical_base_url(" https://relay.example.com")
                .unwrap_err(),
            RelayHttpError::InvalidEndpoint
        );
        for accepted in [
            "https://relay.example.com",
            "https://relay.example.com:8443/",
        ] {
            assert!(validate_base_url(&Url::parse(accepted).unwrap()).is_ok());
        }
        for rejected in [
            "http://relay.example.com",
            "https://user@relay.example.com",
            "https://relay.example.com/base",
            "https://relay.example.com/?query=secret",
            "https://relay.example.com/#fragment",
        ] {
            assert_eq!(
                validate_base_url(&Url::parse(rejected).unwrap()),
                Err(RelayHttpError::InvalidEndpoint),
                "unexpectedly accepted {rejected}"
            );
        }
        for accepted in ["http://127.0.0.1:3000", "http://[::1]:3000"] {
            assert!(validate_base_url(&Url::parse(accepted).unwrap()).is_ok());
        }
        for rejected in [
            "http://localhost:3000",
            "http://localhost.evil.test:3000",
            "http://127.0.0.2:3000",
            "http://0.0.0.0:3000",
        ] {
            assert_eq!(
                validate_base_url(&Url::parse(rejected).unwrap()),
                Err(RelayHttpError::InvalidEndpoint),
                "unexpectedly accepted {rejected}"
            );
        }
    }

    #[test]
    fn authenticated_notification_post_and_receipt_get_are_exact() {
        run_async(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/notifications"))
                .and(header(
                    "authorization",
                    format!("Bearer {TEST_DEVICE_TOKEN}"),
                ))
                .and(body_json(json!({
                    "schemaVersion":1,"notificationId":"notification-1","dedupeKey":"dedupe-1",
                    "kind":"test","priority":90,"title":"测试","body":"安全正文",
                    "createdAt":100,"expiresAt":1000
                })))
                .respond_with(ResponseTemplate::new(202).set_body_json(json!({
                    "notificationId":"notification-1","relayStatus":"accepted",
                    "existing":false,"acceptedAt":200
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/v1/notifications/notification-1"))
                .and(header(
                    "authorization",
                    format!("Bearer {TEST_DEVICE_TOKEN}"),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "notificationId":"notification-1","status":"provider_accepted",
                    "attemptCount":1,"lastErrorCode":null,"providerMessageId":"provider-1",
                    "updatedAt":210,"providerAcceptedAt":210,"futureField":true
                })))
                .mount(&server)
                .await;

            let client = client(&server);
            let notification = RelayNotificationV1 {
                schema_version: 1,
                notification_id: "notification-1".into(),
                dedupe_key: "dedupe-1".into(),
                kind: "test".into(),
                priority: 90,
                title: "测试".into(),
                body: "安全正文".into(),
                correlation_key: None,
                created_at: 100,
                expires_at: 1_000,
            };
            let accepted = client
                .create_notification(
                    &server.uri(),
                    TEST_DEVICE_TOKEN,
                    &notification,
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(accepted.accepted_at, 200);
            let remote = client
                .notification_status(
                    &server.uri(),
                    TEST_DEVICE_TOKEN,
                    "notification-1",
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                remote.status,
                crate::relay::protocol::RelayRemoteNotificationState::ProviderAccepted
            );
        });
    }

    #[test]
    fn notification_acceptance_requires_exact_202_and_json_content_type() {
        run_async(async {
            let notification = RelayNotificationV1 {
                schema_version: 1,
                notification_id: "notification-1".into(),
                dedupe_key: "dedupe-1".into(),
                kind: "test".into(),
                priority: 90,
                title: "test".into(),
                body: "body".into(),
                correlation_key: None,
                created_at: 100,
                expires_at: 1_000,
            };
            for (status, content_type) in [(200, "application/json"), (202, "text/plain")] {
                let server = MockServer::start().await;
                Mock::given(method("POST"))
                    .and(path("/v1/notifications"))
                    .respond_with(
                        ResponseTemplate::new(status)
                            .set_body_raw(
                                r#"{"notificationId":"notification-1","relayStatus":"accepted","existing":false,"acceptedAt":200}"#,
                                content_type,
                            ),
                    )
                    .mount(&server)
                    .await;
                let error = client(&server)
                    .create_notification(
                        &server.uri(),
                        TEST_DEVICE_TOKEN,
                        &notification,
                        &CancellationToken::new(),
                    )
                    .await
                    .unwrap_err();
                assert!(matches!(
                    error,
                    RelayHttpError::HttpStatus {
                        endpoint: RelayEndpoint::Notifications,
                        status: 200
                    } | RelayHttpError::InvalidResponse(RelayEndpoint::Notifications)
                ));
            }
        });
    }

    #[test]
    #[ignore = "requires PROMPTDOCK_RELAY_URL and PROMPTDOCK_RELAY_TOKEN"]
    fn manual_real_relay_contract_smoke() {
        let base_url = std::env::var("PROMPTDOCK_RELAY_URL")
            .expect("PROMPTDOCK_RELAY_URL must be set for the ignored integration test");
        let device_token = Zeroizing::new(
            std::env::var("PROMPTDOCK_RELAY_TOKEN")
                .expect("PROMPTDOCK_RELAY_TOKEN must be set for the ignored integration test"),
        );
        run_async(async {
            let client = RelayHttpClient::new().expect("build Relay HTTP client");
            let result = client
                .probe(&base_url, &device_token, &CancellationToken::new())
                .await
                .expect("probe live, ready and authenticated server-info");
            assert_eq!(result.server_info.api_version, 1);
            assert!(result
                .server_info
                .features
                .iter()
                .any(|item| item == "notifications"));

            let now = chrono::Utc::now().timestamp_millis().max(0);
            let notification_id = Uuid::new_v4().hyphenated().to_string();
            let notification = RelayNotificationV1 {
                schema_version: 1,
                notification_id: notification_id.clone(),
                dedupe_key: format!("live-contract:{notification_id}"),
                kind: "test".into(),
                priority: 90,
                title: "PromptDock Relay contract smoke".into(),
                body: "Automated non-provider integration smoke".into(),
                correlation_key: None,
                created_at: now,
                expires_at: now + 300_000,
            };
            let accepted = client
                .create_notification(
                    &base_url,
                    &device_token,
                    &notification,
                    &CancellationToken::new(),
                )
                .await
                .expect("server durably accepts the notification");
            assert_eq!(accepted.notification_id, notification_id);
            assert!(!accepted.existing);

            let replayed = client
                .create_notification(
                    &base_url,
                    &device_token,
                    &notification,
                    &CancellationToken::new(),
                )
                .await
                .expect("exact replay remains accepted");
            assert_eq!(replayed.notification_id, notification_id);
            assert!(replayed.existing);
            assert_eq!(replayed.accepted_at, accepted.accepted_at);

            let remote = client
                .notification_status(
                    &base_url,
                    &device_token,
                    &notification_id,
                    &CancellationToken::new(),
                )
                .await
                .expect("accepted notification has a readable status");
            assert_eq!(remote.notification_id, notification_id);
            assert!(matches!(
                remote.status,
                crate::relay::protocol::RelayRemoteNotificationState::PendingChannel
                    | crate::relay::protocol::RelayRemoteNotificationState::SendingChannel
                    | crate::relay::protocol::RelayRemoteNotificationState::RetryWait
                    | crate::relay::protocol::RelayRemoteNotificationState::BlockedActivation
                    | crate::relay::protocol::RelayRemoteNotificationState::BlockedReconnect
                    | crate::relay::protocol::RelayRemoteNotificationState::ProviderAccepted
            ));
        });
    }
}
