use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::error::AppError;

const FRAME_MAGIC: &[u8] = b"promptdock-protected-content";
const FRAME_SCHEMA: u8 = 1;
const TEXT_PREFIX: &str = "pdenc1:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentPurpose {
    InboxEvent,
    AgentEventMetadata,
    AgentOutput,
    NotificationPayload,
}

impl ContentPurpose {
    const fn label(self) -> &'static [u8] {
        match self {
            Self::InboxEvent => b"inbox-event",
            Self::AgentEventMetadata => b"agent-event-metadata",
            Self::AgentOutput => b"agent-output",
            Self::NotificationPayload => b"notification-payload",
        }
    }
}

pub(crate) fn protect_text(purpose: ContentPurpose, plaintext: &[u8]) -> Result<String, AppError> {
    let frame = frame(purpose, plaintext)?;
    let ciphertext = crate::platform::dpapi::protect_bytes(&frame).map_err(crypto_error)?;
    Ok(format!(
        "{TEXT_PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(ciphertext)
    ))
}

pub(crate) fn unprotect_text(
    purpose: ContentPurpose,
    protected: &str,
) -> Result<Zeroizing<Vec<u8>>, AppError> {
    let encoded = protected
        .strip_prefix(TEXT_PREFIX)
        .ok_or_else(|| AppError::new("PROTECTED_CONTENT_INVALID", "受保护内容格式无效"))?;
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| AppError::new("PROTECTED_CONTENT_INVALID", "受保护内容格式无效"))?;
    let frame = crate::platform::dpapi::unprotect_bytes(&ciphertext).map_err(crypto_error)?;
    unframe(purpose, frame)
}

pub(crate) fn protect_json<T: Serialize>(
    purpose: ContentPurpose,
    value: &T,
) -> Result<String, AppError> {
    let plaintext = Zeroizing::new(serde_json::to_vec(value).map_err(|error| {
        AppError::internal(
            "PROTECTED_CONTENT_SERIALIZE_FAILED",
            "受保护内容无法序列化",
            error.to_string(),
        )
    })?);
    protect_text(purpose, &plaintext)
}

pub(crate) fn unprotect_json<T: DeserializeOwned>(
    purpose: ContentPurpose,
    protected: &str,
) -> Result<T, AppError> {
    let plaintext = unprotect_text(purpose, protected)?;
    serde_json::from_slice(&plaintext).map_err(|error| {
        AppError::internal(
            "PROTECTED_CONTENT_CORRUPT",
            "受保护内容损坏",
            error.to_string(),
        )
    })
}

fn frame(purpose: ContentPurpose, plaintext: &[u8]) -> Result<Zeroizing<Vec<u8>>, AppError> {
    let purpose = purpose.label();
    let purpose_len = u8::try_from(purpose.len())
        .map_err(|_| AppError::new("PROTECTED_CONTENT_INVALID", "受保护内容用途域无效"))?;
    let mut framed = Zeroizing::new(Vec::with_capacity(
        FRAME_MAGIC.len() + 3 + purpose.len() + plaintext.len(),
    ));
    framed.extend_from_slice(FRAME_MAGIC);
    framed.push(0);
    framed.push(FRAME_SCHEMA);
    framed.push(purpose_len);
    framed.extend_from_slice(purpose);
    framed.extend_from_slice(plaintext);
    Ok(framed)
}

fn unframe(
    purpose: ContentPurpose,
    mut framed: Zeroizing<Vec<u8>>,
) -> Result<Zeroizing<Vec<u8>>, AppError> {
    let header_len = FRAME_MAGIC.len() + 3;
    if framed.len() < header_len
        || &framed[..FRAME_MAGIC.len()] != FRAME_MAGIC
        || framed[FRAME_MAGIC.len()] != 0
        || framed[FRAME_MAGIC.len() + 1] != FRAME_SCHEMA
    {
        return Err(AppError::new(
            "PROTECTED_CONTENT_INVALID",
            "受保护内容版本或格式无效",
        ));
    }
    let purpose_len = usize::from(framed[FRAME_MAGIC.len() + 2]);
    let payload_offset = header_len
        .checked_add(purpose_len)
        .ok_or_else(|| AppError::new("PROTECTED_CONTENT_INVALID", "受保护内容格式无效"))?;
    if framed.len() < payload_offset || &framed[header_len..payload_offset] != purpose.label() {
        return Err(AppError::new(
            "PROTECTED_CONTENT_PURPOSE_MISMATCH",
            "受保护内容用途不匹配",
        ));
    }
    let payload = framed.split_off(payload_offset);
    Ok(Zeroizing::new(payload))
}

fn crypto_error(error: crate::platform::dpapi::DpapiError) -> AppError {
    if dpapi_error_is_invalid_data(&error) {
        return AppError::internal(
            "PROTECTED_CONTENT_CORRUPT",
            "受保护内容损坏",
            error.to_string(),
        );
    }
    AppError::internal(
        "PROTECTED_CONTENT_UNAVAILABLE",
        "无法保护或读取本地正文",
        error.to_string(),
    )
}

fn dpapi_error_is_invalid_data(error: &crate::platform::dpapi::DpapiError) -> bool {
    match error {
        crate::platform::dpapi::DpapiError::InvalidData(_) => true,
        crate::platform::dpapi::DpapiError::DataProtection { source, .. } => {
            source.raw_os_error() == Some(13)
        }
        _ => false,
    }
}

pub(crate) fn is_protected_content_corrupt(error: &AppError) -> bool {
    matches!(
        error.code,
        "PROTECTED_CONTENT_INVALID"
            | "PROTECTED_CONTENT_CORRUPT"
            | "PROTECTED_CONTENT_PURPOSE_MISMATCH"
    )
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_dpapi_round_trip_and_purpose_separation() {
        let sentinel = "DPAPI sentinel 中文 👩🏽‍💻\r\n```rust\n  let x = 1;\n```";
        let protected = protect_text(ContentPurpose::AgentOutput, sentinel.as_bytes()).unwrap();
        assert!(!protected.contains(sentinel));
        assert_eq!(
            &*unprotect_text(ContentPurpose::AgentOutput, &protected).unwrap(),
            sentinel.as_bytes()
        );
        assert_eq!(
            unprotect_text(ContentPurpose::NotificationPayload, &protected)
                .unwrap_err()
                .code,
            "PROTECTED_CONTENT_PURPOSE_MISMATCH"
        );
    }

    #[test]
    fn only_explicit_invalid_data_is_classified_as_corrupt() {
        let invalid = crate::platform::dpapi::DpapiError::InvalidData("fixture");
        assert_eq!(crypto_error(invalid).code, "PROTECTED_CONTENT_CORRUPT");
        let windows_invalid = crate::platform::dpapi::DpapiError::DataProtection {
            operation: "unprotect",
            source: std::io::Error::from_raw_os_error(13),
        };
        assert_eq!(
            crypto_error(windows_invalid).code,
            "PROTECTED_CONTENT_CORRUPT"
        );
        let unavailable = crate::platform::dpapi::DpapiError::DataProtection {
            operation: "unprotect",
            source: std::io::Error::from_raw_os_error(5),
        };
        assert_eq!(
            crypto_error(unavailable).code,
            "PROTECTED_CONTENT_UNAVAILABLE"
        );
    }
}
