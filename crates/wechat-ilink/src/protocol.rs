use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const REFERENCE_IMPLEMENTATION_VERSION: &str = "2.4.6";
pub const REFERENCE_CLIENT_VERSION: u32 = (2 << 16) | (4 << 8) | 6;
pub const ILINK_APP_ID: &str = "bot";
pub const ILINK_DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
pub const ILINK_BOT_TYPE: &str = "3";

pub const GET_BOT_QRCODE_PATH: &str = "ilink/bot/get_bot_qrcode";
pub const GET_QRCODE_STATUS_PATH: &str = "ilink/bot/get_qrcode_status";
pub const GET_UPDATES_PATH: &str = "ilink/bot/getupdates";
pub const SEND_MESSAGE_PATH: &str = "ilink/bot/sendmessage";
pub const NOTIFY_START_PATH: &str = "ilink/bot/msg/notifystart";
pub const NOTIFY_STOP_PATH: &str = "ilink/bot/msg/notifystop";

pub const DEFAULT_QR_TIMEOUT_MS: u64 = 35_000;
pub const DEFAULT_LONG_POLL_TIMEOUT_MS: u64 = 35_000;
pub const DEFAULT_API_TIMEOUT_MS: u64 = 15_000;
pub const DEFAULT_NOTIFY_TIMEOUT_MS: u64 = 10_000;

pub const MESSAGE_TYPE_USER: i32 = 1;
pub const MESSAGE_TYPE_BOT: i32 = 2;
pub const MESSAGE_STATE_FINISH: i32 = 2;
pub const MESSAGE_ITEM_TYPE_TEXT: i32 = 1;
pub const STALE_TOKEN_ERRCODE: i64 = -14;

const DEFAULT_BOT_AGENT: &str = "PromptDock";
const BOT_AGENT_MAX_BYTES: usize = 256;

/// Metadata attached to authenticated CGI request bodies.
///
/// `channel_version` mirrors the pinned Tencent reference package version. It
/// is not a separately versioned public protocol number.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct BaseInfo {
    pub channel_version: String,
    pub bot_agent: String,
}

impl BaseInfo {
    pub fn for_prompt_dock(bot_agent: &str) -> Self {
        Self {
            channel_version: REFERENCE_IMPLEMENTATION_VERSION.to_owned(),
            bot_agent: sanitize_bot_agent(bot_agent),
        }
    }
}

/// Keep the self-declared upstream identity ASCII-only and bounded.
pub fn sanitize_bot_agent(raw: &str) -> String {
    let candidate = raw.trim();
    if candidate.is_empty()
        || !candidate.is_ascii()
        || candidate.len() > BOT_AGENT_MAX_BYTES
        || candidate.bytes().any(|byte| byte.is_ascii_control())
    {
        return DEFAULT_BOT_AGENT.to_owned();
    }

    let product = candidate
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default();
    let Some((name, version)) = product.split_once('/') else {
        return DEFAULT_BOT_AGENT.to_owned();
    };
    let valid_name = !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    let valid_version = !version.is_empty()
        && version.len() <= 32
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b'-'));
    if valid_name && valid_version {
        candidate.to_owned()
    } else {
        DEFAULT_BOT_AGENT.to_owned()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct GetBotQrCodeRequest {
    pub local_token_list: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct GetBotQrCodeResponse {
    pub qrcode: String,
    pub qrcode_img_content: String,
}

impl fmt::Debug for GetBotQrCodeResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GetBotQrCodeResponse")
            .field("qrcode", &"<redacted>")
            .field("qrcode_img_content", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub enum QrCodeStatus {
    Wait,
    Scaned,
    Confirmed,
    Expired,
    ScanedButRedirect,
    NeedVerifyCode,
    VerifyCodeBlocked,
    BindedRedirect,
    Unknown(String),
}

impl QrCodeStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Wait => "wait",
            Self::Scaned => "scaned",
            Self::Confirmed => "confirmed",
            Self::Expired => "expired",
            Self::ScanedButRedirect => "scaned_but_redirect",
            Self::NeedVerifyCode => "need_verifycode",
            Self::VerifyCodeBlocked => "verify_code_blocked",
            Self::BindedRedirect => "binded_redirect",
            Self::Unknown(value) => value,
        }
    }
}

impl fmt::Debug for QrCodeStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(_) => formatter.write_str("unknown"),
            _ => formatter.write_str(self.as_str()),
        }
    }
}

impl Serialize for QrCodeStatus {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QrCodeStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "wait" => Self::Wait,
            "scaned" => Self::Scaned,
            "confirmed" => Self::Confirmed,
            "expired" => Self::Expired,
            "scaned_but_redirect" => Self::ScanedButRedirect,
            "need_verifycode" => Self::NeedVerifyCode,
            "verify_code_blocked" => Self::VerifyCodeBlocked,
            "binded_redirect" => Self::BindedRedirect,
            _ => Self::Unknown(value),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct GetQrCodeStatusResponse {
    pub status: QrCodeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilink_bot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseurl: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilink_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_host: Option<String>,
}

impl fmt::Debug for GetQrCodeStatusResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GetQrCodeStatusResponse")
            .field("status", &self.status)
            .field("has_bot_token", &self.bot_token.is_some())
            .field("has_bot_id", &self.ilink_bot_id.is_some())
            .field("has_baseurl", &self.baseurl.is_some())
            .field("has_user_id", &self.ilink_user_id.is_some())
            .field("has_redirect_host", &self.redirect_host.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct GetUpdatesRequest {
    pub get_updates_buf: String,
    pub base_info: BaseInfo,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct TextItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct MessageItem {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub item_type: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_completed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_item: Option<TextItem>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct WeixinMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_type: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_state: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_list: Option<Vec<MessageItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct GetUpdatesResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ret: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errcode: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errmsg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msgs: Option<Vec<WeixinMessage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_buf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub get_updates_buf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longpolling_timeout_ms: Option<u64>,
}

impl GetUpdatesResponse {
    pub fn has_business_error(&self) -> bool {
        self.ret.is_some_and(|value| value != 0) || self.errcode.is_some_and(|value| value != 0)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct OutboundTextItem {
    pub text: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct OutboundMessageItem {
    #[serde(rename = "type")]
    pub item_type: i32,
    pub text_item: OutboundTextItem,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct OutboundWeixinMessage {
    pub from_user_id: String,
    pub to_user_id: String,
    pub client_id: String,
    pub message_type: i32,
    pub message_state: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_list: Option<Vec<OutboundMessageItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct SendMessageRequest {
    pub msg: OutboundWeixinMessage,
    pub base_info: BaseInfo,
}

impl SendMessageRequest {
    pub fn text(
        to_user_id: String,
        text: String,
        client_id: String,
        context_token: Option<String>,
        run_id: Option<String>,
        bot_agent: &str,
    ) -> Self {
        let item_list = if text.is_empty() {
            None
        } else {
            Some(vec![OutboundMessageItem {
                item_type: MESSAGE_ITEM_TYPE_TEXT,
                text_item: OutboundTextItem { text },
            }])
        };
        Self {
            msg: OutboundWeixinMessage {
                from_user_id: String::new(),
                to_user_id,
                client_id,
                message_type: MESSAGE_TYPE_BOT,
                message_state: MESSAGE_STATE_FINISH,
                item_list,
                context_token,
                run_id,
            },
            base_info: BaseInfo::for_prompt_dock(bot_agent),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMessageResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ret: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errcode: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errmsg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<serde_json::Value>,
}

impl Drop for SendMessageResponse {
    fn drop(&mut self) {
        self.errmsg.zeroize();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct NotifyRequest {
    pub base_info: BaseInfo,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ret: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errmsg: Option<String>,
}

impl Drop for NotifyResponse {
    fn drop(&mut self) {
        self.errmsg.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../fixtures/protocol-v2.4.6.json"))
            .expect("valid protocol fixture")
    }

    #[test]
    fn fixture_locks_reference_constants_paths_headers_and_timeouts() {
        let fixture = fixture();
        assert_eq!(
            fixture["reference"]["implementationVersion"],
            REFERENCE_IMPLEMENTATION_VERSION
        );
        assert_eq!(
            fixture["reference"]["clientVersion"],
            REFERENCE_CLIENT_VERSION
        );
        assert_eq!(fixture["reference"]["appId"], ILINK_APP_ID);
        assert_eq!(fixture["reference"]["botType"], ILINK_BOT_TYPE);
        assert_eq!(
            fixture["reference"]["defaultBaseUrl"],
            ILINK_DEFAULT_BASE_URL
        );
        assert_eq!(
            fixture["paths"],
            json!({
                "getBotQrCode": GET_BOT_QRCODE_PATH,
                "getQrCodeStatus": GET_QRCODE_STATUS_PATH,
                "getUpdates": GET_UPDATES_PATH,
                "sendMessage": SEND_MESSAGE_PATH,
                "notifyStart": NOTIFY_START_PATH,
                "notifyStop": NOTIFY_STOP_PATH
            })
        );
        assert_eq!(
            fixture["timeoutsMs"],
            json!({
                "qr": DEFAULT_QR_TIMEOUT_MS,
                "longPoll": DEFAULT_LONG_POLL_TIMEOUT_MS,
                "send": DEFAULT_API_TIMEOUT_MS,
                "notify": DEFAULT_NOTIFY_TIMEOUT_MS,
                "connect": 10_000
            })
        );
        assert_eq!(
            fixture["headers"],
            json!({
                "contentType": "Content-Type",
                "contentTypeValue": "application/json",
                "authorizationType": "AuthorizationType",
                "authorizationTypeValue": "ilink_bot_token",
                "wechatUin": "X-WECHAT-UIN",
                "appId": "iLink-App-Id",
                "clientVersion": "iLink-App-ClientVersion",
                "authorization": "Authorization",
                "authorizationScheme": "Bearer"
            })
        );
    }

    #[test]
    fn fixture_locks_qr_json_and_unknown_status_compatibility() {
        let fixture = fixture();
        let request: GetBotQrCodeRequest =
            serde_json::from_value(fixture["examples"]["getBotQrCodeRequest"].clone())
                .expect("QR request");
        assert_eq!(
            serde_json::to_value(request).expect("serialize QR request"),
            fixture["examples"]["getBotQrCodeRequest"]
        );

        let response: GetBotQrCodeResponse =
            serde_json::from_value(fixture["examples"]["getBotQrCodeResponse"].clone())
                .expect("QR response");
        let debug = format!("{response:?}");
        assert!(!debug.contains("fixture-qr-secret"));
        assert!(debug.contains("redacted"));

        for status in fixture["qrStatuses"].as_array().expect("QR statuses") {
            let status: QrCodeStatus =
                serde_json::from_value(status.clone()).expect("known QR status");
            assert!(!matches!(status, QrCodeStatus::Unknown(_)));
        }
        let unknown_wire = fixture["examples"]["unknownQrStatus"]["status"]
            .as_str()
            .expect("unknown status");
        let unknown: GetQrCodeStatusResponse =
            serde_json::from_value(fixture["examples"]["unknownQrStatus"].clone())
                .expect("forward-compatible status");
        assert_eq!(unknown.status.as_str(), unknown_wire);
        assert_eq!(format!("{:?}", unknown.status), "unknown");
        assert!(!format!("{unknown:?}").contains(unknown_wire));
    }

    #[test]
    fn fixture_locks_getupdates_sendmessage_and_notify_json() {
        let fixture = fixture();
        let examples = &fixture["examples"];

        let updates_request: GetUpdatesRequest =
            serde_json::from_value(examples["getUpdatesRequest"].clone())
                .expect("getupdates request");
        assert_eq!(
            serde_json::to_value(updates_request).expect("serialize getupdates request"),
            examples["getUpdatesRequest"]
        );
        let updates_response: GetUpdatesResponse =
            serde_json::from_value(examples["getUpdatesResponse"].clone())
                .expect("getupdates response");
        assert!(!updates_response.has_business_error());
        assert_eq!(
            serde_json::to_value(updates_response).expect("serialize getupdates response"),
            examples["getUpdatesResponse"]
        );

        let send_request = SendMessageRequest::text(
            "fixture-user@im.wechat".to_owned(),
            "fixture notification".to_owned(),
            "fixture-client-id".to_owned(),
            Some("fixture-context-token".to_owned()),
            Some("fixture-run-id".to_owned()),
            "PromptDockRelay/0.1.0",
        );
        assert_eq!(
            serde_json::to_value(send_request).expect("serialize send request"),
            examples["sendMessageRequest"]
        );
        for response in examples["sendMessageResponses"]
            .as_array()
            .expect("send responses")
        {
            serde_json::from_value::<SendMessageResponse>(response.clone())
                .expect("compatible send response");
        }

        let notify_request: NotifyRequest =
            serde_json::from_value(examples["notifyRequest"].clone()).expect("notify request");
        assert_eq!(
            serde_json::to_value(notify_request).expect("serialize notify request"),
            examples["notifyRequest"]
        );
        let notify_response: NotifyResponse =
            serde_json::from_value(examples["notifyResponse"].clone()).expect("notify response");
        assert_eq!(
            serde_json::to_value(notify_response).expect("serialize notify response"),
            examples["notifyResponse"]
        );
    }

    #[test]
    fn fixture_locks_business_classification_vectors_for_http_tests() {
        let fixture = fixture();
        assert_eq!(
            fixture["classificationVectors"],
            json!([
                {"ret": -14, "errcode": null, "message": null, "category": "AuthenticationExpired"},
                {"ret": -2, "errcode": null, "message": "rate limit exceeded", "category": "RateLimited"},
                {"ret": -2, "errcode": null, "message": "context token expired", "category": "ContextRejected"},
                {"ret": -2, "errcode": null, "message": "authentication expired", "category": "AuthenticationExpired"},
                {"ret": -2, "errcode": null, "message": "prohibited by policy", "category": "PolicyRejected"},
                {"ret": -2, "errcode": null, "message": "rejected", "category": "AmbiguousMinusTwo"},
                {"ret": 400, "errcode": null, "message": "bad request", "category": "InvalidRequest"},
                {"ret": 999, "errcode": null, "message": "unclassified", "category": "Unknown"}
            ])
        );
    }

    #[test]
    fn optional_fields_are_omitted_and_bot_agent_is_bounded() {
        let request = SendMessageRequest::text(
            "user@im.wechat".to_owned(),
            "hello".to_owned(),
            "stable-client-id".to_owned(),
            None,
            None,
            "PromptDockRelay/0.1.0",
        );
        let value = serde_json::to_value(request).expect("serialize request");
        assert!(value["msg"].get("context_token").is_none());
        assert!(value["msg"].get("run_id").is_none());
        assert_eq!(
            sanitize_bot_agent("PromptDockRelay/0.1.0"),
            "PromptDockRelay/0.1.0"
        );
        assert_eq!(sanitize_bot_agent("PromptDock\n/0.1.0"), DEFAULT_BOT_AGENT);
        assert_eq!(REFERENCE_CLIENT_VERSION, 132_102);
    }
}
