use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const WECHAT_SECRET_SCHEMA_VERSION: u16 = 1;
pub const MAX_RECENT_MESSAGE_KEYS: usize = 256;
const MAX_RECENT_MESSAGE_KEY_BYTES: usize = 256;
const MAX_TOKEN_BYTES: usize = 256 * 1024;
const MAX_CURSOR_BYTES: usize = 1024 * 1024;
const MAX_ID_BYTES: usize = 4 * 1024;
const MAX_BASE_URL_BYTES: usize = 2 * 1024;

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop, PartialEq, Eq)]
pub struct WechatCredentials {
    pub schema_version: u16,
    pub bot_token: String,
    pub account_id: String,
    pub user_id: String,
    pub base_url: String,
    pub saved_at: i64,
}

impl WechatCredentials {
    pub fn validate(&self) -> Result<(), CredentialValidationError> {
        validate_schema(self.schema_version)?;
        validate_required("bot_token", &self.bot_token)?;
        validate_required("account_id", &self.account_id)?;
        validate_required("user_id", &self.user_id)?;
        validate_required("base_url", &self.base_url)?;
        validate_max_bytes("bot_token", &self.bot_token, MAX_TOKEN_BYTES)?;
        validate_max_bytes("account_id", &self.account_id, MAX_ID_BYTES)?;
        validate_max_bytes("user_id", &self.user_id, MAX_ID_BYTES)?;
        validate_max_bytes("base_url", &self.base_url, MAX_BASE_URL_BYTES)
    }
}

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop, PartialEq, Eq)]
pub struct WechatSessionSecrets {
    pub schema_version: u16,
    pub get_updates_buf: String,
    pub context_token: Option<String>,
    pub context_token_user_id: Option<String>,
    pub context_token_updated_at: Option<i64>,
    #[serde(default)]
    pub recent_message_keys: Vec<String>,
}

impl Default for WechatSessionSecrets {
    fn default() -> Self {
        Self {
            schema_version: WECHAT_SECRET_SCHEMA_VERSION,
            get_updates_buf: String::new(),
            context_token: None,
            context_token_user_id: None,
            context_token_updated_at: None,
            recent_message_keys: Vec::new(),
        }
    }
}

impl WechatSessionSecrets {
    pub fn remember_message_key(&mut self, key: impl Into<String>) -> bool {
        let key = key.into();
        if key.is_empty() || key.len() > MAX_RECENT_MESSAGE_KEY_BYTES {
            return false;
        }
        if let Some(index) = self
            .recent_message_keys
            .iter()
            .position(|existing| existing == &key)
        {
            self.recent_message_keys.remove(index);
        }
        self.recent_message_keys.push(key);
        let excess = self
            .recent_message_keys
            .len()
            .saturating_sub(MAX_RECENT_MESSAGE_KEYS);
        if excess > 0 {
            self.recent_message_keys.drain(..excess);
        }
        true
    }

    pub fn validate(&self) -> Result<(), CredentialValidationError> {
        validate_schema(self.schema_version)?;
        if self.recent_message_keys.len() > MAX_RECENT_MESSAGE_KEYS {
            return Err(CredentialValidationError::InvalidData(
                "recent message key list exceeds its limit",
            ));
        }
        if self
            .recent_message_keys
            .iter()
            .any(|key| key.is_empty() || key.len() > MAX_RECENT_MESSAGE_KEY_BYTES)
        {
            return Err(CredentialValidationError::InvalidData(
                "recent message key is invalid",
            ));
        }
        if self.context_token.as_deref().is_some_and(str::is_empty)
            || self
                .context_token_user_id
                .as_deref()
                .is_some_and(str::is_empty)
        {
            return Err(CredentialValidationError::InvalidData(
                "context token metadata is invalid",
            ));
        }
        validate_max_bytes("get_updates_buf", &self.get_updates_buf, MAX_CURSOR_BYTES)?;
        if let Some(context_token) = &self.context_token {
            validate_max_bytes("context_token", context_token, MAX_TOKEN_BYTES)?;
        }
        if let Some(user_id) = &self.context_token_user_id {
            validate_max_bytes("context_token_user_id", user_id, MAX_ID_BYTES)?;
        }
        let has_context = self.context_token.is_some();
        if self.context_token_user_id.is_some() != has_context
            || self.context_token_updated_at.is_some() != has_context
        {
            return Err(CredentialValidationError::InvalidData(
                "context token metadata is incomplete",
            ));
        }
        Ok(())
    }
}

/// Credentials and mutable session state form one durable document. Phase 5
/// owns its encrypted persistence; this type only freezes the DTO/invariants.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop, PartialEq, Eq)]
pub struct ConnectionBundle {
    pub schema_version: u16,
    pub credentials: WechatCredentials,
    pub session: WechatSessionSecrets,
}

impl ConnectionBundle {
    pub fn new(
        credentials: WechatCredentials,
        session: WechatSessionSecrets,
    ) -> Result<Self, CredentialValidationError> {
        let bundle = Self {
            schema_version: WECHAT_SECRET_SCHEMA_VERSION,
            credentials,
            session,
        };
        bundle.validate()?;
        Ok(bundle)
    }

    pub fn validate(&self) -> Result<(), CredentialValidationError> {
        validate_schema(self.schema_version)?;
        self.credentials.validate()?;
        self.session.validate()?;
        if self
            .session
            .context_token_user_id
            .as_deref()
            .is_some_and(|user_id| user_id != self.credentials.user_id)
        {
            return Err(CredentialValidationError::InvalidData(
                "session context belongs to a different connection",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CredentialValidationError {
    #[error("WeChat credential data is invalid: {0}")]
    InvalidData(&'static str),
}

fn validate_schema(schema_version: u16) -> Result<(), CredentialValidationError> {
    if schema_version == WECHAT_SECRET_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(CredentialValidationError::InvalidData(
            "unsupported secret schema version",
        ))
    }
}

fn validate_required(name: &'static str, value: &str) -> Result<(), CredentialValidationError> {
    if value.trim().is_empty() {
        Err(CredentialValidationError::InvalidData(name))
    } else {
        Ok(())
    }
}

fn validate_max_bytes(
    name: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), CredentialValidationError> {
    if value.len() <= maximum {
        Ok(())
    } else {
        Err(CredentialValidationError::InvalidData(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials() -> WechatCredentials {
        WechatCredentials {
            schema_version: WECHAT_SECRET_SCHEMA_VERSION,
            bot_token: "bot-secret".into(),
            account_id: "account@im.bot".into(),
            user_id: "user@im.wechat".into(),
            base_url: "https://ilinkai.weixin.qq.com".into(),
            saved_at: 1_700_000_000_000,
        }
    }

    #[test]
    fn bundle_requires_complete_context_owned_by_credentials() {
        let mut session = WechatSessionSecrets::default();
        session.get_updates_buf = "cursor-secret".into();
        session.context_token = Some("context-secret".into());
        session.context_token_user_id = Some("user@im.wechat".into());
        session.context_token_updated_at = Some(1_700_000_000_100);
        assert!(session.remember_message_key("message-1"));
        ConnectionBundle::new(credentials(), session.clone()).expect("valid bundle");

        session.context_token_user_id = Some("other@im.wechat".into());
        assert!(matches!(
            ConnectionBundle::new(credentials(), session),
            Err(CredentialValidationError::InvalidData(
                "session context belongs to a different connection"
            ))
        ));
    }

    #[test]
    fn session_keys_are_deduplicated_bounded_and_validated() {
        let mut session = WechatSessionSecrets::default();
        for index in 0..=MAX_RECENT_MESSAGE_KEYS {
            assert!(session.remember_message_key(format!("message-{index}")));
        }
        assert_eq!(session.recent_message_keys.len(), MAX_RECENT_MESSAGE_KEYS);
        assert_eq!(session.recent_message_keys[0], "message-1");
        assert!(session.remember_message_key("message-1"));
        assert_eq!(
            session.recent_message_keys.last().map(String::as_str),
            Some("message-1")
        );
        assert!(!session.remember_message_key(""));
        assert!(!session.remember_message_key("x".repeat(MAX_RECENT_MESSAGE_KEY_BYTES + 1)));

        session.context_token = Some("context-secret".into());
        assert_eq!(
            session.validate(),
            Err(CredentialValidationError::InvalidData(
                "context token metadata is incomplete"
            ))
        );
    }

    #[test]
    fn documents_reject_unknown_schema_and_unbounded_values() {
        let mut invalid = credentials();
        invalid.schema_version += 1;
        assert!(invalid.validate().is_err());

        let mut oversized = credentials();
        oversized.bot_token = "x".repeat(MAX_TOKEN_BYTES + 1);
        assert_eq!(
            oversized.validate(),
            Err(CredentialValidationError::InvalidData("bot_token"))
        );
    }
}
