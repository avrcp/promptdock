use std::{fmt, time::Duration};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, RequestBuilder};
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::protocol::{
    BaseInfo, DEFAULT_API_TIMEOUT_MS, DEFAULT_LONG_POLL_TIMEOUT_MS, DEFAULT_NOTIFY_TIMEOUT_MS,
    DEFAULT_QR_TIMEOUT_MS, GET_BOT_QRCODE_PATH, GET_QRCODE_STATUS_PATH, GET_UPDATES_PATH,
    GetBotQrCodeRequest, GetBotQrCodeResponse, GetQrCodeStatusResponse, GetUpdatesRequest,
    GetUpdatesResponse, ILINK_APP_ID, ILINK_BOT_TYPE, ILINK_DEFAULT_BASE_URL, NOTIFY_START_PATH,
    NOTIFY_STOP_PATH, NotifyRequest, NotifyResponse, REFERENCE_CLIENT_VERSION, SEND_MESSAGE_PATH,
    SendMessageRequest, SendMessageResponse,
};
use crate::redaction::safe_business_message;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

const HEADER_AUTHORIZATION_TYPE: HeaderName = HeaderName::from_static("authorizationtype");
const HEADER_WECHAT_UIN: HeaderName = HeaderName::from_static("x-wechat-uin");
const HEADER_ILINK_APP_ID: HeaderName = HeaderName::from_static("ilink-app-id");
const HEADER_ILINK_CLIENT_VERSION: HeaderName = HeaderName::from_static("ilink-app-clientversion");

#[derive(Clone)]
enum EndpointPolicy {
    Production,
    #[cfg(test)]
    LocalTest(Url),
}

#[derive(Clone)]
pub struct WechatHttpClient {
    client: Client,
    bot_agent: String,
    endpoint_policy: EndpointPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportErrorKind {
    Connect,
    Request,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WechatBusinessErrorCategory {
    AuthenticationExpired,
    ContextRejected,
    RateLimited,
    PolicyRejected,
    AmbiguousMinusTwo,
    InvalidRequest,
    Unknown,
}

#[derive(Error, PartialEq, Eq)]
pub enum WechatHttpError {
    #[error("invalid WeChat endpoint")]
    InvalidEndpoint,
    #[error("invalid request header")]
    InvalidHeader,
    #[error("HTTP client configuration failed")]
    ClientConfiguration,
    #[error("request cancelled")]
    Cancelled,
    #[error("request timed out")]
    Timeout,
    #[error("network request failed: {0:?}")]
    Transport(TransportErrorKind),
    #[error("WeChat service returned HTTP {0}")]
    HttpStatus(u16),
    #[error("WeChat service returned an oversized response")]
    ResponseTooLarge,
    #[error("WeChat service returned invalid JSON")]
    InvalidJson,
    #[error("WeChat service response is missing required fields")]
    InvalidResponse,
    #[error(
        "WeChat service rejected the request (ret={ret:?}, errcode={errcode:?}, category={category:?})"
    )]
    ApiRejected {
        ret: Option<i64>,
        errcode: Option<i64>,
        category: WechatBusinessErrorCategory,
        safe_message: Option<String>,
    },
}

impl fmt::Debug for WechatHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => formatter.write_str("InvalidEndpoint"),
            Self::InvalidHeader => formatter.write_str("InvalidHeader"),
            Self::ClientConfiguration => formatter.write_str("ClientConfiguration"),
            Self::Cancelled => formatter.write_str("Cancelled"),
            Self::Timeout => formatter.write_str("Timeout"),
            Self::Transport(kind) => formatter.debug_tuple("Transport").field(kind).finish(),
            Self::HttpStatus(status) => formatter.debug_tuple("HttpStatus").field(status).finish(),
            Self::ResponseTooLarge => formatter.write_str("ResponseTooLarge"),
            Self::InvalidJson => formatter.write_str("InvalidJson"),
            Self::InvalidResponse => formatter.write_str("InvalidResponse"),
            Self::ApiRejected {
                ret,
                errcode,
                category,
                safe_message,
            } => formatter
                .debug_struct("ApiRejected")
                .field("ret", ret)
                .field("errcode", errcode)
                .field("category", category)
                .field(
                    "safe_message",
                    &safe_message.as_ref().map(|_| "<redacted:present>"),
                )
                .finish(),
        }
    }
}

impl WechatHttpError {
    pub fn is_stale_token(&self) -> bool {
        matches!(
            self,
            Self::ApiRejected {
                category: WechatBusinessErrorCategory::AuthenticationExpired,
                ..
            }
        )
    }

    pub fn business_category(&self) -> Option<WechatBusinessErrorCategory> {
        match self {
            Self::ApiRejected { category, .. } => Some(*category),
            _ => None,
        }
    }
}

impl WechatHttpClient {
    pub fn production(bot_agent: impl Into<String>) -> Result<Self, WechatHttpError> {
        Ok(Self {
            client: build_client(true)?,
            bot_agent: bot_agent.into(),
            endpoint_policy: EndpointPolicy::Production,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        base_url: Url,
        bot_agent: impl Into<String>,
    ) -> Result<Self, WechatHttpError> {
        validate_local_test_endpoint(&base_url)?;
        Ok(Self {
            client: build_client(false)?,
            bot_agent: bot_agent.into(),
            endpoint_policy: EndpointPolicy::LocalTest(base_url),
        })
    }

    pub async fn get_bot_qrcode(
        &self,
        local_token_list: &[String],
        cancellation: &CancellationToken,
    ) -> Result<GetBotQrCodeResponse, WechatHttpError> {
        let base_url = self.login_base_url()?;
        let mut endpoint = self.endpoint(&base_url, GET_BOT_QRCODE_PATH)?;
        endpoint
            .query_pairs_mut()
            .append_pair("bot_type", ILINK_BOT_TYPE);
        let request_body = GetBotQrCodeRequest {
            local_token_list: local_token_list
                .iter()
                .filter(|token| !token.trim().is_empty())
                .take(10)
                .cloned()
                .collect(),
        };
        let response: GetBotQrCodeResponse = self
            .post_json(
                endpoint,
                None,
                &request_body,
                Duration::from_millis(DEFAULT_QR_TIMEOUT_MS),
                cancellation,
            )
            .await?;
        if response.qrcode.is_empty() || response.qrcode_img_content.is_empty() {
            return Err(WechatHttpError::InvalidResponse);
        }
        Ok(response)
    }

    pub async fn get_qrcode_status(
        &self,
        base_url: &Url,
        qrcode: &str,
        verify_code: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<GetQrCodeStatusResponse, WechatHttpError> {
        self.validate_endpoint(base_url)?;
        let mut endpoint = self.endpoint(base_url, GET_QRCODE_STATUS_PATH)?;
        {
            let mut query = endpoint.query_pairs_mut();
            query.append_pair("qrcode", qrcode);
            if let Some(verify_code) = verify_code.filter(|code| !code.is_empty()) {
                query.append_pair("verify_code", verify_code);
            }
        }
        let request = self
            .client
            .get(endpoint)
            .headers(common_headers()?)
            .timeout(Duration::from_millis(DEFAULT_QR_TIMEOUT_MS));
        self.execute_json(request, cancellation).await
    }

    pub async fn get_updates(
        &self,
        base_url: &Url,
        token: &str,
        get_updates_buf: &str,
        timeout: Option<Duration>,
        cancellation: &CancellationToken,
    ) -> Result<GetUpdatesResponse, WechatHttpError> {
        self.validate_endpoint(base_url)?;
        let endpoint = self.endpoint(base_url, GET_UPDATES_PATH)?;
        let body = GetUpdatesRequest {
            get_updates_buf: get_updates_buf.to_owned(),
            base_info: BaseInfo::for_prompt_dock(&self.bot_agent),
        };
        let response: GetUpdatesResponse = match self
            .post_json(
                endpoint,
                Some(token),
                &body,
                timeout.unwrap_or(Duration::from_millis(DEFAULT_LONG_POLL_TIMEOUT_MS)),
                cancellation,
            )
            .await
        {
            // A held long poll reaching its request deadline is ordinary control flow.
            Err(WechatHttpError::Timeout) => GetUpdatesResponse {
                ret: Some(0),
                errcode: None,
                errmsg: None,
                msgs: Some(Vec::new()),
                sync_buf: None,
                get_updates_buf: Some(get_updates_buf.to_owned()),
                longpolling_timeout_ms: None,
            },
            other => other?,
        };
        if response.has_business_error() {
            return Err(api_rejected(
                response.ret,
                response.errcode,
                response.errmsg.as_deref(),
            ));
        }
        Ok(response)
    }

    pub async fn send_message(
        &self,
        base_url: &Url,
        token: &str,
        body: &SendMessageRequest,
        cancellation: &CancellationToken,
    ) -> Result<SendMessageResponse, WechatHttpError> {
        self.validate_endpoint(base_url)?;
        let endpoint = self.endpoint(base_url, SEND_MESSAGE_PATH)?;
        let response: SendMessageResponse = self
            .post_json(
                endpoint,
                Some(token),
                body,
                Duration::from_millis(DEFAULT_API_TIMEOUT_MS),
                cancellation,
            )
            .await?;
        if response.ret.is_some_and(|value| value != 0)
            || response.errcode.is_some_and(|value| value != 0)
        {
            return Err(api_rejected(
                response.ret,
                response.errcode,
                response.errmsg.as_deref(),
            ));
        }
        Ok(response)
    }

    pub async fn notify_start(
        &self,
        base_url: &Url,
        token: &str,
        cancellation: &CancellationToken,
    ) -> Result<NotifyResponse, WechatHttpError> {
        self.notify(base_url, token, NOTIFY_START_PATH, cancellation)
            .await
    }

    pub async fn notify_stop(
        &self,
        base_url: &Url,
        token: &str,
        cancellation: &CancellationToken,
    ) -> Result<NotifyResponse, WechatHttpError> {
        self.notify(base_url, token, NOTIFY_STOP_PATH, cancellation)
            .await
    }

    async fn notify(
        &self,
        base_url: &Url,
        token: &str,
        path: &str,
        cancellation: &CancellationToken,
    ) -> Result<NotifyResponse, WechatHttpError> {
        self.validate_endpoint(base_url)?;
        let endpoint = self.endpoint(base_url, path)?;
        let body = NotifyRequest {
            base_info: BaseInfo::for_prompt_dock(&self.bot_agent),
        };
        let response: NotifyResponse = self
            .post_json(
                endpoint,
                Some(token),
                &body,
                Duration::from_millis(DEFAULT_NOTIFY_TIMEOUT_MS),
                cancellation,
            )
            .await?;
        if response.ret.is_some_and(|value| value != 0) {
            return Err(api_rejected(response.ret, None, response.errmsg.as_deref()));
        }
        Ok(response)
    }

    async fn post_json<TRequest, TResponse>(
        &self,
        endpoint: Url,
        token: Option<&str>,
        body: &TRequest,
        timeout: Duration,
        cancellation: &CancellationToken,
    ) -> Result<TResponse, WechatHttpError>
    where
        TRequest: Serialize + ?Sized,
        TResponse: DeserializeOwned,
    {
        let bytes = serde_json::to_vec(body).map_err(|_| WechatHttpError::InvalidJson)?;
        let request = self
            .client
            .post(endpoint)
            .headers(post_headers(token)?)
            .body(bytes)
            .timeout(timeout);
        self.execute_json(request, cancellation).await
    }

    async fn execute_json<TResponse>(
        &self,
        request: RequestBuilder,
        cancellation: &CancellationToken,
    ) -> Result<TResponse, WechatHttpError>
    where
        TResponse: DeserializeOwned,
    {
        let operation = async {
            let mut response = request.send().await.map_err(map_reqwest_error)?;
            let status = response.status();
            if !status.is_success() {
                return Err(WechatHttpError::HttpStatus(status.as_u16()));
            }
            if response
                .content_length()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
            {
                return Err(WechatHttpError::ResponseTooLarge);
            }
            let mut bytes = Vec::with_capacity(
                response
                    .content_length()
                    .unwrap_or_default()
                    .min(MAX_RESPONSE_BYTES as u64) as usize,
            );
            while let Some(chunk) = response.chunk().await.map_err(map_reqwest_error)? {
                if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                    return Err(WechatHttpError::ResponseTooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|_| WechatHttpError::InvalidJson)
        };
        cancellation
            .run_until_cancelled(operation)
            .await
            .ok_or(WechatHttpError::Cancelled)?
    }

    fn login_base_url(&self) -> Result<Url, WechatHttpError> {
        match &self.endpoint_policy {
            EndpointPolicy::Production => {
                Url::parse(ILINK_DEFAULT_BASE_URL).map_err(|_| WechatHttpError::InvalidEndpoint)
            }
            #[cfg(test)]
            EndpointPolicy::LocalTest(base_url) => Ok(base_url.clone()),
        }
    }

    fn validate_endpoint(&self, url: &Url) -> Result<(), WechatHttpError> {
        match &self.endpoint_policy {
            EndpointPolicy::Production => validate_production_endpoint(url),
            #[cfg(test)]
            EndpointPolicy::LocalTest(_) => validate_local_test_endpoint(url),
        }
    }

    fn endpoint(&self, base_url: &Url, path: &str) -> Result<Url, WechatHttpError> {
        self.validate_endpoint(base_url)?;
        base_url
            .join(path)
            .map_err(|_| WechatHttpError::InvalidEndpoint)
    }
}

fn api_rejected(ret: Option<i64>, errcode: Option<i64>, message: Option<&str>) -> WechatHttpError {
    WechatHttpError::ApiRejected {
        ret,
        errcode,
        category: classify_business_error(ret, errcode, message),
        safe_message: safe_business_message(message),
    }
}

fn classify_business_error(
    ret: Option<i64>,
    errcode: Option<i64>,
    message: Option<&str>,
) -> WechatBusinessErrorCategory {
    let code_is = |expected| ret == Some(expected) || errcode == Some(expected);
    if code_is(crate::protocol::STALE_TOKEN_ERRCODE) {
        return WechatBusinessErrorCategory::AuthenticationExpired;
    }

    let message = message.unwrap_or_default().trim().to_ascii_lowercase();
    let contains_any = |needles: &[&str]| needles.iter().any(|needle| message.contains(needle));
    if contains_any(&[
        "rate limit",
        "ratelimit",
        "too many",
        "quota",
        "频率",
        "频控",
        "限流",
    ]) {
        return WechatBusinessErrorCategory::RateLimited;
    }
    if contains_any(&["context", "上下文"])
        && contains_any(&[
            "expired", "invalid", "reject", "stale", "missing", "过期", "无效", "拒绝", "缺失",
        ])
    {
        return WechatBusinessErrorCategory::ContextRejected;
    }
    if contains_any(&[
        "unauthorized",
        "authentication",
        "auth expired",
        "登录过期",
        "凭据失效",
    ]) {
        return WechatBusinessErrorCategory::AuthenticationExpired;
    }
    if contains_any(&[
        "policy",
        "not allowed",
        "prohibited",
        "主动消息",
        "策略",
        "额度",
    ]) {
        return WechatBusinessErrorCategory::PolicyRejected;
    }
    if code_is(-2) {
        return WechatBusinessErrorCategory::AmbiguousMinusTwo;
    }
    if matches!(ret.or(errcode), Some(400 | 404 | 422))
        || contains_any(&["invalid request", "bad request", "参数", "格式"])
    {
        return WechatBusinessErrorCategory::InvalidRequest;
    }
    WechatBusinessErrorCategory::Unknown
}

pub fn validate_production_endpoint(url: &Url) -> Result<(), WechatHttpError> {
    let host = url.host_str().ok_or(WechatHttpError::InvalidEndpoint)?;
    let valid_host = host.eq_ignore_ascii_case("ilinkai.weixin.qq.com")
        || host
            .to_ascii_lowercase()
            .strip_suffix(".weixin.qq.com")
            .is_some_and(|prefix| !prefix.is_empty() && !prefix.ends_with('.'));
    let valid = url.scheme() == "https"
        && valid_host
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.port(), None | Some(443))
        && matches!(url.path(), "" | "/")
        && url.query().is_none()
        && url.fragment().is_none();
    if valid {
        Ok(())
    } else {
        Err(WechatHttpError::InvalidEndpoint)
    }
}

#[cfg(test)]
fn validate_local_test_endpoint(url: &Url) -> Result<(), WechatHttpError> {
    let host = url.host_str().ok_or(WechatHttpError::InvalidEndpoint)?;
    let valid = matches!(url.scheme(), "http" | "https")
        && matches!(host, "127.0.0.1" | "localhost" | "::1")
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.path(), "" | "/")
        && url.query().is_none()
        && url.fragment().is_none();
    if valid {
        Ok(())
    } else {
        Err(WechatHttpError::InvalidEndpoint)
    }
}

fn build_client(https_only: bool) -> Result<Client, WechatHttpError> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(https_only)
        .build()
        .map_err(|_| WechatHttpError::ClientConfiguration)
}

fn common_headers() -> Result<HeaderMap, WechatHttpError> {
    let mut headers = HeaderMap::new();
    headers.insert(HEADER_ILINK_APP_ID, HeaderValue::from_static(ILINK_APP_ID));
    headers.insert(
        HEADER_ILINK_CLIENT_VERSION,
        HeaderValue::from_str(&REFERENCE_CLIENT_VERSION.to_string())
            .map_err(|_| WechatHttpError::InvalidHeader)?,
    );
    Ok(headers)
}

fn post_headers(token: Option<&str>) -> Result<HeaderMap, WechatHttpError> {
    let mut headers = common_headers()?;
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HEADER_AUTHORIZATION_TYPE,
        HeaderValue::from_static("ilink_bot_token"),
    );
    let random_uin = rand::random::<u32>().to_string();
    let encoded_uin = BASE64_STANDARD.encode(random_uin.as_bytes());
    headers.insert(
        HEADER_WECHAT_UIN,
        HeaderValue::from_str(&encoded_uin).map_err(|_| WechatHttpError::InvalidHeader)?,
    );
    if let Some(token) = token.map(str::trim).filter(|token| !token.is_empty()) {
        let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| WechatHttpError::InvalidHeader)?;
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
    }
    Ok(headers)
}

fn map_reqwest_error(error: reqwest::Error) -> WechatHttpError {
    // Connect timeouts satisfy both predicates; classify them as transport
    // failures so a long-poll caller backs off instead of spinning.
    if error.is_connect() {
        WechatHttpError::Transport(TransportErrorKind::Connect)
    } else if error.is_timeout() {
        WechatHttpError::Timeout
    } else {
        WechatHttpError::Transport(TransportErrorKind::Request)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::thread;

    use serde_json::{Value, json};
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::protocol::{QrCodeStatus, SendMessageRequest};

    fn test_client(server: &MockServer) -> WechatHttpClient {
        WechatHttpClient::new_for_test(
            Url::parse(&server.uri()).expect("mock URL"),
            "PromptDockRelay/0.1.0",
        )
        .expect("test client")
    }

    fn raw_server(response: Vec<u8>) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").expect("raw listener");
        let address = listener.local_addr().expect("raw address");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("raw request");
            let mut request = [0_u8; 16 * 1024];
            let _ = stream.read(&mut request);
            stream.write_all(&response).expect("raw response");
            stream.flush().expect("raw flush");
        });
        Url::parse(&format!("http://{address}/")).expect("raw URL")
    }

    fn fixed_length_response(content_length: usize, body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn chunked_response(body: &[u8], chunk_size: usize) -> Vec<u8> {
        let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        for chunk in body.chunks(chunk_size) {
            response.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            response.extend_from_slice(chunk);
            response.extend_from_slice(b"\r\n");
        }
        response.extend_from_slice(b"0\r\n\r\n");
        response
    }

    async fn raw_json<T: DeserializeOwned>(
        client: &WechatHttpClient,
        url: Url,
    ) -> Result<T, WechatHttpError> {
        client
            .execute_json(client.client.get(url), &CancellationToken::new())
            .await
    }

    #[test]
    fn endpoint_policies_reject_ssrf_shapes_and_accept_exact_boundaries() {
        for raw in [
            "https://ilinkai.weixin.qq.com",
            "https://ilinkai.weixin.qq.com:443/",
            "https://edge.weixin.qq.com/",
        ] {
            assert!(
                validate_production_endpoint(&Url::parse(raw).expect("production URL")).is_ok(),
                "rejected {raw}"
            );
        }
        for raw in [
            "http://ilinkai.weixin.qq.com",
            "https://evilweixin.qq.com",
            "https://weixin.qq.com.evil.test",
            "https://user@ilinkai.weixin.qq.com",
            "https://ilinkai.weixin.qq.com:8443",
            "https://ilinkai.weixin.qq.com/prefix",
            "https://ilinkai.weixin.qq.com/?query=secret",
            "https://ilinkai.weixin.qq.com/#fragment",
            "https://weixin.qq.com",
        ] {
            assert_eq!(
                validate_production_endpoint(&Url::parse(raw).expect("production URL")),
                Err(WechatHttpError::InvalidEndpoint),
                "accepted {raw}"
            );
        }
        for raw in ["http://127.0.0.1:1/", "http://localhost:1/"] {
            assert!(validate_local_test_endpoint(&Url::parse(raw).expect("local URL")).is_ok());
        }
        for raw in [
            "http://example.test/",
            "http://127.0.0.1:1/path",
            "http://user@127.0.0.1:1/",
            "ftp://127.0.0.1:1/",
        ] {
            assert_eq!(
                validate_local_test_endpoint(&Url::parse(raw).expect("local URL")),
                Err(WechatHttpError::InvalidEndpoint)
            );
        }
    }

    #[test]
    fn bearer_header_is_sensitive_and_optional() {
        let with_token = post_headers(Some(" bot-secret ")).expect("token headers");
        let authorization = with_token.get(AUTHORIZATION).expect("authorization");
        assert!(authorization.is_sensitive());
        assert_eq!(authorization, "Bearer bot-secret");
        assert!(post_headers(None).unwrap().get(AUTHORIZATION).is_none());
        assert!(
            post_headers(Some("  "))
                .unwrap()
                .get(AUTHORIZATION)
                .is_none()
        );
        assert_eq!(
            post_headers(Some("secret\r\ninjected")),
            Err(WechatHttpError::InvalidHeader)
        );
    }

    #[tokio::test]
    async fn qr_post_and_status_get_use_distinct_headers_and_encoded_parameters() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/get_bot_qrcode"))
            .and(query_param("bot_type", "3"))
            .and(body_json(json!({"local_token_list":["existing-token"]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "qrcode":"qr-secret",
                "qrcode_img_content":"qr-content-secret"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ilink/bot/get_qrcode_status"))
            .and(query_param("qrcode", "qr + secret"))
            .and(query_param("verify_code", "12 34"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"scaned"})))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let qr = client
            .get_bot_qrcode(
                &["".to_owned(), "existing-token".to_owned()],
                &CancellationToken::new(),
            )
            .await
            .expect("QR response");
        assert_eq!(qr.qrcode, "qr-secret");
        let status = client
            .get_qrcode_status(
                &Url::parse(&server.uri()).unwrap(),
                "qr + secret",
                Some("12 34"),
                &CancellationToken::new(),
            )
            .await
            .expect("status response");
        assert_eq!(status.status, QrCodeStatus::Scaned);

        let requests = server.received_requests().await.expect("requests");
        let post = requests
            .iter()
            .find(|request| request.url.path() == "/ilink/bot/get_bot_qrcode")
            .expect("QR POST");
        assert_eq!(post.headers[CONTENT_TYPE], "application/json");
        assert_eq!(post.headers[HEADER_AUTHORIZATION_TYPE], "ilink_bot_token");
        assert_eq!(post.headers[HEADER_ILINK_APP_ID], "bot");
        assert_eq!(post.headers[HEADER_ILINK_CLIENT_VERSION], "132102");
        assert!(post.headers.get(AUTHORIZATION).is_none());
        let decimal = String::from_utf8(
            BASE64_STANDARD
                .decode(post.headers[HEADER_WECHAT_UIN].as_bytes())
                .expect("base64 UIN"),
        )
        .expect("UTF-8 UIN");
        decimal.parse::<u32>().expect("decimal UIN");

        let get = requests
            .iter()
            .find(|request| request.url.path() == "/ilink/bot/get_qrcode_status")
            .expect("QR status GET");
        assert_eq!(get.headers[HEADER_ILINK_APP_ID], "bot");
        assert!(get.headers.get(AUTHORIZATION).is_none());
        assert!(get.headers.get(HEADER_AUTHORIZATION_TYPE).is_none());
        assert!(get.headers.get(HEADER_WECHAT_UIN).is_none());
        assert!(get.headers.get(CONTENT_TYPE).is_none());
    }

    #[tokio::test]
    async fn get_updates_sends_cursor_and_bearer_and_timeout_is_empty_poll() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ret":0,"get_updates_buf":"next-cursor","msgs":[]
            })))
            .mount(&server)
            .await;
        let client = test_client(&server);
        let result = client
            .get_updates(
                &Url::parse(&server.uri()).unwrap(),
                "bot-secret",
                "cursor-secret",
                None,
                &CancellationToken::new(),
            )
            .await
            .expect("updates");
        assert_eq!(result.get_updates_buf.as_deref(), Some("next-cursor"));
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests[0].headers[AUTHORIZATION], "Bearer bot-secret");
        let body: Value = serde_json::from_slice(&requests[0].body).expect("request JSON");
        assert_eq!(body["get_updates_buf"], "cursor-secret");
        assert_eq!(body["base_info"]["bot_agent"], "PromptDockRelay/0.1.0");

        let slow = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(100))
                    .set_body_json(json!({"ret":0})),
            )
            .mount(&slow)
            .await;
        let response = test_client(&slow)
            .get_updates(
                &Url::parse(&slow.uri()).unwrap(),
                "bot-secret",
                "cursor-secret",
                Some(Duration::from_millis(10)),
                &CancellationToken::new(),
            )
            .await
            .expect("timeout becomes empty poll");
        assert_eq!(response.ret, Some(0));
        assert!(response.msgs.as_ref().is_some_and(Vec::is_empty));
        assert_eq!(response.get_updates_buf.as_deref(), Some("cursor-secret"));
    }

    #[tokio::test]
    async fn cancellation_is_reported_without_request_secrets() {
        let server = MockServer::start().await;
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = test_client(&server)
            .get_updates(
                &Url::parse(&server.uri()).unwrap(),
                "SENTINEL_BOT_SECRET_DO_NOT_EXPOSE",
                "SENTINEL_CURSOR_SECRET_DO_NOT_EXPOSE",
                None,
                &cancellation,
            )
            .await;
        let error = match result {
            Ok(_) => panic!("cancelled request unexpectedly succeeded"),
            Err(error) => error,
        };
        assert_eq!(error, WechatHttpError::Cancelled);
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains("SENTINEL"));

        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(json!({"ret":0})),
            )
            .mount(&server)
            .await;
        let active_cancellation = CancellationToken::new();
        let task_cancellation = active_cancellation.clone();
        let active_client = test_client(&server);
        let active_url = Url::parse(&server.uri()).unwrap();
        let task = tokio::spawn(async move {
            active_client
                .get_updates(
                    &active_url,
                    "SENTINEL_ACTIVE_BOT_SECRET_DO_NOT_EXPOSE",
                    "SENTINEL_ACTIVE_CURSOR_SECRET_DO_NOT_EXPOSE",
                    None,
                    &task_cancellation,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if server
                    .received_requests()
                    .await
                    .is_some_and(|requests| !requests.is_empty())
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("request reached the mock before cancellation");
        active_cancellation.cancel();
        let active_result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("active request cancellation was prompt")
            .expect("request task did not panic");
        let active_error = match active_result {
            Ok(_) => panic!("active cancelled request unexpectedly succeeded"),
            Err(error) => error,
        };
        assert_eq!(active_error, WechatHttpError::Cancelled);
        assert!(!format!("{active_error:?} {active_error}").contains("SENTINEL"));
    }

    #[tokio::test]
    async fn send_and_notify_use_expected_paths_bodies_and_bearer() {
        let server = MockServer::start().await;
        for endpoint in [
            "/ilink/bot/sendmessage",
            "/ilink/bot/msg/notifystart",
            "/ilink/bot/msg/notifystop",
        ] {
            Mock::given(method("POST"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ret":0})))
                .mount(&server)
                .await;
        }
        let base_url = Url::parse(&server.uri()).unwrap();
        let client = test_client(&server);
        let request = SendMessageRequest::text(
            "user@im.wechat".into(),
            "notification".into(),
            "stable-client-id".into(),
            Some("context-secret".into()),
            None,
            "PromptDockRelay/0.1.0",
        );
        client
            .send_message(&base_url, "bot-secret", &request, &CancellationToken::new())
            .await
            .expect("send");
        client
            .notify_start(&base_url, "bot-secret", &CancellationToken::new())
            .await
            .expect("notify start");
        client
            .notify_stop(&base_url, "bot-secret", &CancellationToken::new())
            .await
            .expect("notify stop");
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 3);
        for request in &requests {
            assert_eq!(request.headers[AUTHORIZATION], "Bearer bot-secret");
            assert_eq!(request.headers[CONTENT_TYPE], "application/json");
        }
        let notify = requests
            .iter()
            .find(|request| request.url.path().ends_with("notifystart"))
            .expect("notify request");
        let body: Value = serde_json::from_slice(&notify.body).expect("notify JSON");
        assert_eq!(body["base_info"]["channel_version"], "2.4.6");
    }

    #[tokio::test]
    async fn redirect_http_status_and_invalid_json_are_safe_failures() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/target", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/target"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"followed":true})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(503).set_body_string("SENTINEL_HTTP_BODY"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/invalid"))
            .respond_with(ResponseTemplate::new(200).set_body_string("SENTINEL_INVALID_JSON"))
            .mount(&server)
            .await;
        let client = test_client(&server);
        for (path, expected) in [
            ("redirect", WechatHttpError::HttpStatus(302)),
            ("status", WechatHttpError::HttpStatus(503)),
            ("invalid", WechatHttpError::InvalidJson),
        ] {
            let url = Url::parse(&format!("{}/{path}", server.uri())).unwrap();
            let error = raw_json::<Value>(&client, url)
                .await
                .expect_err("safe failure");
            assert_eq!(error, expected);
            assert!(!format!("{error:?} {error}").contains("SENTINEL"));
        }
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.path() == "/target")
                .count(),
            0,
            "redirect was followed"
        );
    }

    #[tokio::test]
    async fn content_length_and_streaming_limits_enforce_two_mib_boundary() {
        let oversized_length = raw_server(fixed_length_response(MAX_RESPONSE_BYTES + 1, b"{}"));
        let client = WechatHttpClient::new_for_test(oversized_length.clone(), "Test/1").unwrap();
        assert_eq!(
            raw_json::<Value>(&client, oversized_length).await,
            Err(WechatHttpError::ResponseTooLarge)
        );

        let exact_body = format!("\"{}\"", "x".repeat(MAX_RESPONSE_BYTES - 2)).into_bytes();
        assert_eq!(exact_body.len(), MAX_RESPONSE_BYTES);
        let exact_url = raw_server(chunked_response(&exact_body, 64 * 1024));
        let client = WechatHttpClient::new_for_test(exact_url.clone(), "Test/1").unwrap();
        let exact: String = raw_json(&client, exact_url)
            .await
            .expect("exact 2 MiB JSON");
        assert_eq!(exact.len(), MAX_RESPONSE_BYTES - 2);

        let oversized_body = vec![b' '; MAX_RESPONSE_BYTES + 1];
        let oversized_url = raw_server(chunked_response(&oversized_body, 64 * 1024));
        let client = WechatHttpClient::new_for_test(oversized_url.clone(), "Test/1").unwrap();
        assert_eq!(
            raw_json::<Value>(&client, oversized_url).await,
            Err(WechatHttpError::ResponseTooLarge)
        );
    }

    #[tokio::test]
    async fn stale_token_minus_two_evidence_and_safe_business_messages_are_preserved() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/sendmessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errcode":-14,
                "errmsg":"bot_token: SENTINEL_BUSINESS_SECRET_DO_NOT_EXPOSE"
            })))
            .mount(&server)
            .await;
        let request = SendMessageRequest::text(
            "user@im.wechat".into(),
            "body".into(),
            "client".into(),
            None,
            None,
            "PromptDockRelay/0.1.0",
        );
        let result = test_client(&server)
            .send_message(
                &Url::parse(&server.uri()).unwrap(),
                "bot-secret",
                &request,
                &CancellationToken::new(),
            )
            .await;
        let error = match result {
            Ok(_) => panic!("stale token unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.is_stale_token());
        assert_eq!(
            error.business_category(),
            Some(WechatBusinessErrorCategory::AuthenticationExpired)
        );
        assert!(!format!("{error:?} {error}").contains("SENTINEL"));

        for (message, expected) in [
            (
                "rate limit exceeded",
                WechatBusinessErrorCategory::RateLimited,
            ),
            (
                "context token expired",
                WechatBusinessErrorCategory::ContextRejected,
            ),
            (
                "policy prohibited",
                WechatBusinessErrorCategory::PolicyRejected,
            ),
            ("", WechatBusinessErrorCategory::AmbiguousMinusTwo),
        ] {
            assert_eq!(
                classify_business_error(Some(-2), None, Some(message)),
                expected
            );
        }

        let fixture: Value = serde_json::from_str(include_str!("../fixtures/protocol-v2.4.6.json"))
            .expect("valid protocol fixture");
        for vector in fixture["classificationVectors"]
            .as_array()
            .expect("classification vectors")
        {
            let expected = match vector["category"].as_str().expect("category") {
                "AuthenticationExpired" => WechatBusinessErrorCategory::AuthenticationExpired,
                "ContextRejected" => WechatBusinessErrorCategory::ContextRejected,
                "RateLimited" => WechatBusinessErrorCategory::RateLimited,
                "PolicyRejected" => WechatBusinessErrorCategory::PolicyRejected,
                "AmbiguousMinusTwo" => WechatBusinessErrorCategory::AmbiguousMinusTwo,
                "InvalidRequest" => WechatBusinessErrorCategory::InvalidRequest,
                "Unknown" => WechatBusinessErrorCategory::Unknown,
                category => panic!("unknown fixture category: {category}"),
            };
            assert_eq!(
                classify_business_error(
                    vector["ret"].as_i64(),
                    vector["errcode"].as_i64(),
                    vector["message"].as_str(),
                ),
                expected
            );
        }

        let unmarked = api_rejected(
            Some(-2),
            None,
            Some("private-short-value https://example.test/private-path-value?x=1"),
        );
        let diagnostic = format!("{unmarked:?} {unmarked}");
        assert!(!diagnostic.contains("private-short-value"));
        assert!(!diagnostic.contains("private-path-value"));
    }
}
