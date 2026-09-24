use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use subtle::ConstantTimeEq as _;
use thiserror::Error;
use tokio::sync::Mutex;

const CURSOR_VERSION: u8 = 1;
const CURSOR_TTL_MS: i64 = 15 * 60 * 1_000;
const NONCE_BYTES: usize = 16;
const TAG_BYTES: usize = 32;
const SIGNED_BYTES: usize = 1 + 1 + 8 + NONCE_BYTES;
const TOKEN_BYTES: usize = SIGNED_BYTES + TAG_BYTES;
const TOKEN_CHARS: usize = (TOKEN_BYTES * 4).div_ceil(3);
const MAX_CURSOR_ENTRIES: usize = 4_096;
const MAX_TIE_BREAK_BYTES: usize = 512;

#[derive(Clone)]
pub struct AdminCursorCodec {
    key: Arc<[u8; 32]>,
    entries: Arc<Mutex<HashMap<[u8; NONCE_BYTES], CursorEntry>>>,
}

impl AdminCursorCodec {
    pub(crate) fn new() -> Self {
        Self::with_key(random())
    }

    fn with_key(key: [u8; 32]) -> Self {
        Self {
            key: Arc::new(key),
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn encode(
        &self,
        kind: CursorKind,
        position: CursorPosition,
    ) -> Result<String, CursorError> {
        self.encode_at(kind, position, unix_timestamp_ms()?).await
    }

    async fn encode_at(
        &self,
        kind: CursorKind,
        position: CursorPosition,
        now: i64,
    ) -> Result<String, CursorError> {
        if position.tie_break.is_empty()
            || position.tie_break.len() > MAX_TIE_BREAK_BYTES
            || position.sort_timestamp < 0
        {
            return Err(CursorError::Invalid);
        }
        let expires_at = now.checked_add(CURSOR_TTL_MS).ok_or(CursorError::Clock)?;
        let nonce: [u8; NONCE_BYTES] = random();
        let mut signed = [0_u8; SIGNED_BYTES];
        signed[0] = CURSOR_VERSION;
        signed[1] = kind.as_byte();
        signed[2..10].copy_from_slice(&expires_at.to_be_bytes());
        signed[10..].copy_from_slice(&nonce);
        let tag = blake3::keyed_hash(&self.key, &signed);
        let mut token = [0_u8; TOKEN_BYTES];
        token[..SIGNED_BYTES].copy_from_slice(&signed);
        token[SIGNED_BYTES..].copy_from_slice(tag.as_bytes());

        let mut entries = self.entries.lock().await;
        entries.retain(|_, entry| entry.expires_at > now);
        if entries.len() >= MAX_CURSOR_ENTRIES
            && let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(nonce, _)| *nonce)
        {
            entries.remove(&oldest);
        }
        entries.insert(
            nonce,
            CursorEntry {
                kind,
                position,
                expires_at,
            },
        );
        Ok(URL_SAFE_NO_PAD.encode(token))
    }

    pub async fn decode(
        &self,
        expected_kind: CursorKind,
        token: &str,
    ) -> Result<CursorPosition, CursorError> {
        self.decode_at(expected_kind, token, unix_timestamp_ms()?)
            .await
    }

    async fn decode_at(
        &self,
        expected_kind: CursorKind,
        token: &str,
        now: i64,
    ) -> Result<CursorPosition, CursorError> {
        if token.len() != TOKEN_CHARS {
            return Err(CursorError::Invalid);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| CursorError::Invalid)?;
        if bytes.len() != TOKEN_BYTES
            || bytes[0] != CURSOR_VERSION
            || bytes[1] != expected_kind.as_byte()
        {
            return Err(CursorError::Invalid);
        }
        let expected_tag = blake3::keyed_hash(&self.key, &bytes[..SIGNED_BYTES]);
        if expected_tag
            .as_bytes()
            .ct_eq(&bytes[SIGNED_BYTES..])
            .unwrap_u8()
            != 1
        {
            return Err(CursorError::Invalid);
        }
        let expires_at =
            i64::from_be_bytes(bytes[2..10].try_into().map_err(|_| CursorError::Invalid)?);
        let nonce: [u8; NONCE_BYTES] = bytes[10..SIGNED_BYTES]
            .try_into()
            .map_err(|_| CursorError::Invalid)?;
        let mut entries = self.entries.lock().await;
        if expires_at <= now {
            entries.remove(&nonce);
            return Err(CursorError::Expired);
        }
        let entry = entries.get(&nonce).ok_or(CursorError::Invalid)?;
        if entry.kind != expected_kind || entry.expires_at != expires_at {
            return Err(CursorError::Invalid);
        }
        Ok(entry.position.clone())
    }
}

impl std::fmt::Debug for AdminCursorCodec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdminCursorCodec")
            .field("key", &"[REDACTED]")
            .field("entries", &"[PROCESS_LOCAL]")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorKind {
    Devices,
    WechatEvents,
    Deliveries,
    InteractiveReplies,
    InboundCommands,
    Results,
}

impl CursorKind {
    fn as_byte(self) -> u8 {
        match self {
            Self::Devices => 1,
            Self::WechatEvents => 2,
            Self::Deliveries => 3,
            Self::InteractiveReplies => 4,
            Self::InboundCommands => 5,
            Self::Results => 6,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorPosition {
    pub sort_timestamp: i64,
    pub tie_break: String,
}

#[derive(Clone)]
struct CursorEntry {
    kind: CursorKind,
    position: CursorPosition,
    expires_at: i64,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CursorError {
    #[error("Admin cursor is invalid")]
    Invalid,
    #[error("Admin cursor has expired")]
    Expired,
    #[error("system clock is unavailable")]
    Clock,
}

fn unix_timestamp_ms() -> Result<i64, CursorError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .ok_or(CursorError::Clock)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position() -> CursorPosition {
        CursorPosition {
            sort_timestamp: 1_800_000_000_000,
            tie_break: "internal-sensitive-tie-break".to_owned(),
        }
    }

    #[tokio::test]
    async fn cursor_is_opaque_kind_bound_tamper_evident_and_restart_local() {
        let codec = AdminCursorCodec::with_key([7; 32]);
        let token = codec
            .encode_at(CursorKind::Deliveries, position(), 1_800_000_000_000)
            .await
            .expect("encode cursor");
        assert!(!token.contains("internal-sensitive"));
        assert_eq!(
            codec
                .decode_at(CursorKind::Deliveries, &token, 1_800_000_000_001)
                .await
                .expect("decode cursor"),
            position()
        );
        assert_eq!(
            codec
                .decode_at(CursorKind::Devices, &token, 1_800_000_000_001)
                .await,
            Err(CursorError::Invalid)
        );

        let mut tampered = token.into_bytes();
        tampered[10] = if tampered[10] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).expect("cursor text");
        assert_eq!(
            codec
                .decode_at(CursorKind::Deliveries, &tampered, 1_800_000_000_001)
                .await,
            Err(CursorError::Invalid)
        );

        let restarted = AdminCursorCodec::with_key([7; 32]);
        let token = codec
            .encode_at(CursorKind::Deliveries, position(), 1_800_000_000_000)
            .await
            .expect("encode cursor");
        assert_eq!(
            restarted
                .decode_at(CursorKind::Deliveries, &token, 1_800_000_000_001)
                .await,
            Err(CursorError::Invalid)
        );
    }

    #[tokio::test]
    async fn cursor_expiry_and_position_bounds_fail_closed() {
        let codec = AdminCursorCodec::with_key([9; 32]);
        assert_eq!(
            codec
                .decode_at(CursorKind::Devices, &"A".repeat(TOKEN_CHARS + 1), 10,)
                .await,
            Err(CursorError::Invalid)
        );
        let token = codec
            .encode_at(CursorKind::InboundCommands, position(), 10)
            .await
            .expect("encode cursor");
        assert_eq!(
            codec
                .decode_at(CursorKind::InboundCommands, &token, 10 + CURSOR_TTL_MS)
                .await,
            Err(CursorError::Expired)
        );
        assert_eq!(
            codec
                .encode_at(
                    CursorKind::Devices,
                    CursorPosition {
                        sort_timestamp: -1,
                        tie_break: String::new(),
                    },
                    10,
                )
                .await,
            Err(CursorError::Invalid)
        );
        let debug = format!("{codec:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains('9'));
    }
}
