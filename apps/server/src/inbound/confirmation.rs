use std::{path::Path, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use subtle::ConstantTimeEq as _;
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

use super::model::PendingConfirmation;

pub const SYSTEMD_CREDENTIAL_NAME: &str = "relay-confirmation-key";
const CREDENTIALS_DIRECTORY_ENV: &str = "CREDENTIALS_DIRECTORY";
const KEY_BYTES: usize = 32;
const MAX_CREDENTIAL_BYTES: u64 = 1024;
const DIGEST_CONTEXT: &str = "promptdock-relay/control-confirmation/v2";

/// Owns the independent confirmation-verification secret.  It deliberately
/// has no Debug implementation and must never share the WeChat master key.
#[derive(Clone)]
pub struct ConfirmationVerifier(Arc<ConfirmationKey>);

struct ConfirmationKey(Zeroizing<[u8; KEY_BYTES]>);

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ConfirmationCredentialError {
    #[error("confirmation credential is unavailable")]
    Unavailable,
    #[error("confirmation credential is invalid")]
    Invalid,
}

impl ConfirmationVerifier {
    pub async fn from_systemd_credential() -> Result<Self, ConfirmationCredentialError> {
        let directory = std::env::var_os(CREDENTIALS_DIRECTORY_ENV)
            .filter(|value| !value.is_empty())
            .ok_or(ConfirmationCredentialError::Unavailable)?;
        let bytes = tokio::fs::read(Path::new(&directory).join(SYSTEMD_CREDENTIAL_NAME))
            .await
            .map_err(|_| ConfirmationCredentialError::Unavailable)?;
        if bytes.len() > MAX_CREDENTIAL_BYTES as usize {
            return Err(ConfirmationCredentialError::Invalid);
        }
        let encoded = std::str::from_utf8(&bytes)
            .map_err(|_| ConfirmationCredentialError::Invalid)?
            .trim_end_matches(['\r', '\n']);
        if encoded.is_empty() || encoded.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(ConfirmationCredentialError::Invalid);
        }
        let decoded = STANDARD
            .decode(encoded)
            .map_err(|_| ConfirmationCredentialError::Invalid)?;
        if decoded.len() != KEY_BYTES || STANDARD.encode(&decoded) != encoded {
            return Err(ConfirmationCredentialError::Invalid);
        }
        let mut key = Zeroizing::new([0_u8; KEY_BYTES]);
        key.copy_from_slice(&decoded);
        Ok(Self(Arc::new(ConfirmationKey(key))))
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self(Arc::new(ConfirmationKey(Zeroizing::new([0xA5; KEY_BYTES]))))
    }

    /// No inbound WeChat text can be accepted while that runtime is disabled.
    /// This key is intentionally reachable only by the disabled server path;
    /// enabled production startup always loads `relay-confirmation-key`.
    pub(crate) fn disabled_runtime() -> Self {
        Self(Arc::new(ConfirmationKey(Zeroizing::new([0_u8; KEY_BYTES]))))
    }

    pub(super) fn digest(&self, confirmation: &PendingConfirmation, code: &str) -> String {
        digest_parts(
            &self.0.0,
            &confirmation.sender_fingerprint,
            &confirmation.confirmation_id,
            &confirmation.confirmation_nonce,
            confirmation.device_id.as_hyphenated().to_string().as_str(),
            confirmation.action_kind.as_str(),
            confirmation.runtime_handle.as_deref(),
            confirmation.workspace_handle.as_deref(),
            confirmation.harness_profile_handle.as_deref(),
            confirmation.task_preset_handle.as_deref(),
            confirmation.run_handle.as_deref(),
            &confirmation.intent_id,
            confirmation.expires_at,
            code,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn matches(
        &self,
        sender_fingerprint: &str,
        confirmation_id: &str,
        confirmation_nonce: &str,
        device_id: &str,
        action_kind: &str,
        runtime_handle: Option<&str>,
        workspace_handle: Option<&str>,
        harness_profile_handle: Option<&str>,
        task_preset_handle: Option<&str>,
        run_handle: Option<&str>,
        intent_id: &str,
        expires_at: i64,
        code: &str,
        expected: &str,
    ) -> bool {
        let actual = digest_parts(
            &self.0.0,
            sender_fingerprint,
            confirmation_id,
            confirmation_nonce,
            device_id,
            action_kind,
            runtime_handle,
            workspace_handle,
            harness_profile_handle,
            task_preset_handle,
            run_handle,
            intent_id,
            expires_at,
            code,
        );
        bool::from(actual.as_bytes().ct_eq(expected.as_bytes()))
    }
}

#[allow(clippy::too_many_arguments)]
fn digest_parts(
    key: &[u8; KEY_BYTES],
    sender_fingerprint: &str,
    confirmation_id: &str,
    confirmation_nonce: &str,
    device_id: &str,
    action_kind: &str,
    runtime_handle: Option<&str>,
    workspace_handle: Option<&str>,
    harness_profile_handle: Option<&str>,
    task_preset_handle: Option<&str>,
    run_handle: Option<&str>,
    intent_id: &str,
    expires_at: i64,
    code: &str,
) -> String {
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(DIGEST_CONTEXT.as_bytes());
    for value in [
        Some(sender_fingerprint),
        Some(confirmation_id),
        Some(confirmation_nonce),
        Some(device_id),
        Some(action_kind),
        runtime_handle,
        workspace_handle,
        harness_profile_handle,
        task_preset_handle,
        run_handle,
        Some(intent_id),
    ] {
        match value {
            Some(value) => {
                hasher.update(&[1]);
                hasher.update(&(value.len() as u64).to_be_bytes());
                hasher.update(value.as_bytes());
            }
            None => {
                hasher.update(&[0]);
            }
        }
    }
    hasher.update(&expires_at.to_be_bytes());
    hasher.update(&(code.len() as u64).to_be_bytes());
    hasher.update(code.as_bytes());
    hasher.finalize().to_hex().to_string()
}

impl Drop for ConfirmationKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
