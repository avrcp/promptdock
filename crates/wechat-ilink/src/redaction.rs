use serde_json::Value;
use url::Url;

const REDACTED: &str = "<redacted>";
const MAX_SAFE_BUSINESS_MESSAGE_CHARS: usize = 160;

pub fn redact_token(value: Option<&str>) -> &'static str {
    if value.is_some_and(|value| !value.is_empty()) {
        "<redacted:present>"
    } else {
        "<redacted:absent>"
    }
}

pub fn redact_id(value: &str) -> String {
    if value.is_empty() {
        return "<empty>".to_owned();
    }
    let suffix = ["@im.wechat", "@im.bot"]
        .into_iter()
        .find(|suffix| value.ends_with(suffix));
    let body = suffix
        .map(|suffix| &value[..value.len() - suffix.len()])
        .unwrap_or(value);
    let prefix: String = body.chars().take(4).collect();
    match suffix {
        Some(suffix) => format!("{prefix}***{suffix}"),
        None => format!("{prefix}***"),
    }
}

pub fn redact_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return "<redacted-url>".to_owned();
    };
    let had_query = url.query().is_some();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    let mut result = url.to_string();
    if had_query {
        result.push_str("?<redacted>");
    }
    result
}

pub fn redact_json_body(raw: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(raw) else {
        return "<redacted-json>".to_owned();
    };
    redact_value(None, &mut value);
    serde_json::to_string(&value).unwrap_or_else(|_| "<redacted-json>".to_owned())
}

pub(crate) fn safe_business_message(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }

    let mut result = String::new();
    let mut result_chars = 0usize;
    let mut redact_next = false;
    for token in raw.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        let contains_secret = contains_secret_marker(&lower);
        let redacted = if redact_next {
            REDACTED.to_owned()
        } else if lower.starts_with("http://") || lower.starts_with("https://") {
            redact_url(token)
        } else if lower.contains("@im.wechat")
            || lower.contains("@im.bot")
            || contains_secret
            || looks_like_opaque_secret(token)
        {
            REDACTED.to_owned()
        } else {
            token
                .chars()
                .filter(|character| !character.is_control())
                .collect()
        };
        redact_next = contains_secret;

        if redacted.is_empty() {
            continue;
        }
        if !result.is_empty() {
            result.push(' ');
        }
        for character in redacted.chars() {
            if result_chars >= MAX_SAFE_BUSINESS_MESSAGE_CHARS {
                break;
            }
            result.push(character);
            result_chars += 1;
        }
        if result_chars >= MAX_SAFE_BUSINESS_MESSAGE_CHARS {
            break;
        }
    }

    let result = result.trim().to_owned();
    (!result.is_empty()).then_some(result)
}

fn contains_secret_marker(value: &str) -> bool {
    [
        "authorization",
        "bearer",
        "bot_token",
        "bottoken",
        "token=",
        "token:",
        "context_token",
        "contexttoken",
        "context=",
        "context:",
        "access_token",
        "accesstoken",
        "get_updates_buf",
        "getupdatesbuf",
        "sync_buf",
        "syncbuf",
        "qrcode",
        "verify_code",
        "verifycode",
        "user_id",
        "userid",
        "account_id",
        "accountid",
        "ilinkuserid",
        "ilinkbotid",
        "client_id",
        "clientid",
    ]
    .iter()
    .any(|marker| value.contains(marker))
}

fn looks_like_opaque_secret(value: &str) -> bool {
    let trimmed = value.trim_matches(|character: char| !character.is_ascii_alphanumeric());
    trimmed.len() >= 32
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn redact_value(parent_key: Option<&str>, value: &mut Value) {
    if parent_key.is_some_and(is_secret_key) {
        *value = Value::String(REDACTED.to_owned());
        return;
    }
    if parent_key.is_some_and(is_id_key) {
        if let Value::String(id) = value {
            *id = redact_id(id);
        } else {
            *value = Value::String(REDACTED.to_owned());
        }
        return;
    }

    match value {
        Value::Object(map) => {
            for (key, child) in map {
                redact_value(Some(key), child);
            }
        }
        Value::Array(values) => {
            for child in values {
                redact_value(parent_key, child);
            }
        }
        _ => {}
    }
}

fn normalized_key(key: &str) -> String {
    key.bytes()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect()
}

fn is_secret_key(key: &str) -> bool {
    matches!(
        normalized_key(key).as_str(),
        "authorization"
            | "bottoken"
            | "contexttoken"
            | "getupdatesbuf"
            | "syncbuf"
            | "qrcode"
            | "qrcodeimgcontent"
            | "verifycode"
            | "localtokenlist"
            | "aeskey"
            | "typingticket"
            | "text"
            | "textitem"
            | "prompt"
            | "lastassistantmessage"
            | "payloadjson"
            | "errmsg"
    )
}

fn is_id_key(key: &str) -> bool {
    matches!(
        normalized_key(key).as_str(),
        "fromuserid" | "touserid" | "ilinkuserid" | "ilinkbotid" | "accountid"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_removes_tokens_cursors_content_urls_and_full_ids() {
        let raw = r#"{
            "bot_token":"bot-secret",
            "get_updates_buf":"cursor-secret",
            "msg":{"context_token":"context-secret","from_user_id":"o9cq123456@im.wechat"},
            "local_token_list":["one","two"],
            "text_item":{"text":"private inbound body"},
            "errmsg":"server echoed a private value"
        }"#;
        let redacted = redact_json_body(raw);
        for secret in [
            "bot-secret",
            "cursor-secret",
            "context-secret",
            "o9cq123456",
            "private inbound body",
            "server echoed a private value",
        ] {
            assert!(!redacted.contains(secret));
        }
        assert!(redacted.contains("o9cq***@im.wechat"));
        assert_eq!(
            redact_url("https://user:pass@ilinkai.weixin.qq.com/path?q=secret#fragment"),
            "https://ilinkai.weixin.qq.com/path?<redacted>"
        );
        assert_eq!(redact_json_body("secret-not-json"), "<redacted-json>");
    }

    #[test]
    fn business_message_is_bounded_and_removes_secret_shapes() {
        let safe = safe_business_message(Some(
            "context expired bearer short-secret bot_token: another-short https://example.test/path?token=secret user@im.wechat",
        ))
        .expect("safe summary");
        assert!(safe.contains("context expired"));
        for secret in [
            "short-secret",
            "another-short",
            "token=secret",
            "user@im.wechat",
        ] {
            assert!(!safe.contains(secret));
        }
        assert!(safe.chars().count() <= MAX_SAFE_BUSINESS_MESSAGE_CHARS);
        assert_eq!(safe_business_message(Some(" \n\t ")), None);
        assert_eq!(redact_token(Some("secret")), "<redacted:present>");
        assert_eq!(redact_id("o9cq123456@im.wechat"), "o9cq***@im.wechat");
    }
}
