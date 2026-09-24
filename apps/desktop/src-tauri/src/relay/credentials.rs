use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::{fmt, fmt::Formatter};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::platform::dpapi::{read_encrypted_json, write_encrypted_json, DpapiError};

pub const RELAY_CONNECTION_PROFILE_SCHEMA_VERSION: u16 = 1;
const CONNECTION_FILE_NAME: &str = "relay-connection.dpapi";
const MAX_DEVICE_TOKEN_BYTES: usize = 85;
const MAX_DEVICE_ID_BYTES: usize = 36;
const MAX_BASE_URL_BYTES: usize = 2 * 1024;

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelayConnectionProfile {
    pub schema_version: u16,
    pub base_url: String,
    pub device_token: String,
    pub device_id: String,
    pub saved_at: i64,
}

impl fmt::Debug for RelayConnectionProfile {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayConnectionProfile")
            .field("schema_version", &self.schema_version)
            .field("base_url", &self.base_url)
            .field("device_token", &"[REDACTED]")
            .field("device_id", &"[REDACTED]")
            .field("saved_at", &self.saved_at)
            .finish()
    }
}

impl RelayConnectionProfile {
    pub fn new(
        base_url: String,
        device_token: String,
        saved_at: i64,
    ) -> Result<Self, RelayCredentialError> {
        let mut device_token = Zeroizing::new(device_token);
        validate_text("device_token", device_token.as_str())?;
        validate_max_bytes(
            "device_token",
            device_token.as_str(),
            MAX_DEVICE_TOKEN_BYTES,
        )?;
        let device_id = device_id_from_token(device_token.as_str())?.to_owned();
        let profile = Self {
            schema_version: RELAY_CONNECTION_PROFILE_SCHEMA_VERSION,
            base_url,
            device_token: std::mem::take(&mut *device_token),
            device_id,
            saved_at,
        };
        profile.validate()?;
        Ok(profile)
    }

    pub(crate) fn validate(&self) -> Result<(), RelayCredentialError> {
        if self.schema_version != RELAY_CONNECTION_PROFILE_SCHEMA_VERSION {
            return Err(RelayCredentialError::InvalidData(
                "unsupported relay connection schema version",
            ));
        }
        validate_text("base_url", &self.base_url)?;
        validate_text("device_token", &self.device_token)?;
        validate_text("device_id", &self.device_id)?;
        validate_max_bytes("base_url", &self.base_url, MAX_BASE_URL_BYTES)?;
        validate_max_bytes("device_token", &self.device_token, MAX_DEVICE_TOKEN_BYTES)?;
        validate_max_bytes("device_id", &self.device_id, MAX_DEVICE_ID_BYTES)?;
        if self.device_id != device_id_from_token(&self.device_token)? {
            return Err(RelayCredentialError::InvalidData(
                "device_id does not match device_token",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RelayCredentialError {
    #[error("relay credential store is temporarily unavailable")]
    Unavailable,
    #[error("relay credential data is invalid: {0}")]
    InvalidData(&'static str),
    #[error("relay credential serialization failed")]
    Serialization(#[source] serde_json::Error),
    #[error("relay credential file operation failed")]
    Io(#[source] std::io::Error),
    #[error("Windows data protection failed during {operation}")]
    DataProtection {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("relay credential store is unavailable on this platform")]
    UnsupportedPlatform,
}

impl From<DpapiError> for RelayCredentialError {
    fn from(error: DpapiError) -> Self {
        match error {
            DpapiError::UnsupportedPlatform => Self::UnsupportedPlatform,
            DpapiError::InvalidData(message) => Self::InvalidData(message),
            DpapiError::Serialization(source) => Self::Serialization(source),
            DpapiError::Io(source) => Self::Io(source),
            DpapiError::DataProtection { operation, source } => {
                Self::DataProtection { operation, source }
            }
        }
    }
}

pub struct RelayCredentialStore {
    connection_path: PathBuf,
    operation_lock: Mutex<()>,
}

impl RelayCredentialStore {
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            connection_path: app_data_dir.join(CONNECTION_FILE_NAME),
            operation_lock: Mutex::new(()),
        }
    }

    pub fn load_profile(&self) -> Result<Option<RelayConnectionProfile>, RelayCredentialError> {
        let _guard = self.lock()?;
        let profile: Option<RelayConnectionProfile> =
            read_encrypted_json(&self.connection_path).map_err(RelayCredentialError::from)?;
        if let Some(profile) = &profile {
            profile.validate()?;
        }
        Ok(profile)
    }

    pub fn is_configured(&self) -> bool {
        self.connection_path.exists()
    }

    pub fn save_profile(
        &self,
        profile: &RelayConnectionProfile,
    ) -> Result<(), RelayCredentialError> {
        profile.validate()?;
        let _guard = self.lock()?;
        write_encrypted_json(
            &self.connection_path,
            profile,
            "RELAY_SECRET_WRITE_FAILED",
            "中继凭据写入失败",
        )
        .map_err(RelayCredentialError::from)?;
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, ()>, RelayCredentialError> {
        self.operation_lock
            .lock()
            .map_err(|_| RelayCredentialError::Unavailable)
    }
}

fn validate_text(name: &'static str, value: &str) -> Result<(), RelayCredentialError> {
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        Err(RelayCredentialError::InvalidData(name))
    } else {
        Ok(())
    }
}

pub(super) fn device_id_from_token(token: &str) -> Result<&str, RelayCredentialError> {
    let Some((version, remainder)) = token.split_once('.') else {
        return Err(RelayCredentialError::InvalidData("device_token"));
    };
    let Some((device_id, secret)) = remainder.split_once('.') else {
        return Err(RelayCredentialError::InvalidData("device_token"));
    };
    if version != "pdv2"
        || secret.contains('.')
        || device_id.len() != MAX_DEVICE_ID_BYTES
        || Uuid::parse_str(device_id)
            .map(|uuid| uuid.to_string() != device_id)
            .unwrap_or(true)
        || secret.len() != 43
        || !secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RelayCredentialError::InvalidData("device_token"));
    }
    let decoded_secret = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(secret)
            .map_err(|_| RelayCredentialError::InvalidData("device_token"))?,
    );
    if decoded_secret.len() != 32 {
        return Err(RelayCredentialError::InvalidData("device_token"));
    }
    Ok(device_id)
}

pub(super) fn validate_device_token(token: &str) -> Result<(), RelayCredentialError> {
    validate_text("device_token", token)?;
    validate_max_bytes("device_token", token, MAX_DEVICE_TOKEN_BYTES)?;
    device_id_from_token(token).map(|_| ())
}

fn validate_max_bytes(
    name: &'static str,
    value: &str,
    limit: usize,
) -> Result<(), RelayCredentialError> {
    if value.len() > limit {
        Err(RelayCredentialError::InvalidData(name))
    } else {
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::platform::dpapi::write_encrypted_json;

    const DEVICE_TOKEN: &str =
        "pdv2.123e4567-e89b-12d3-a456-426614174000.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    fn profile() -> RelayConnectionProfile {
        RelayConnectionProfile::new(
            "https://relay.example.test".into(),
            DEVICE_TOKEN.into(),
            1_700_000_000_000,
        )
        .expect("valid relay profile")
    }

    #[test]
    fn dpapi_round_trips_profile_and_never_writes_plaintext() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = RelayCredentialStore::new(dir.path());
        let profile = profile();

        store.save_profile(&profile).expect("save profile");

        let bytes = std::fs::read(&store.connection_path).expect("read profile");
        assert!(!bytes
            .windows(profile.device_token.len())
            .any(|window| window == profile.device_token.as_bytes()));
        let loaded = store.load_profile().expect("load profile");
        assert!(loaded.as_ref().is_some_and(|loaded| loaded == &profile));
    }

    #[test]
    fn corrupt_profile_is_rejected_without_replacement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = RelayCredentialStore::new(dir.path());
        let corrupted = b"not-a-dpapi-document";
        std::fs::write(&store.connection_path, corrupted).expect("seed corrupt profile");

        assert!(store.is_configured());
        assert!(store.load_profile().is_err());
        assert_eq!(
            std::fs::read(&store.connection_path).expect("read corrupt profile"),
            corrupted
        );
    }

    #[test]
    fn future_profile_schema_is_rejected_without_replacement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = RelayCredentialStore::new(dir.path());
        let mut future = profile();
        future.schema_version += 1;
        write_encrypted_json(
            &store.connection_path,
            &future,
            "TEST_WRITE_FAILED",
            "test write failed",
        )
        .expect("seed future profile");
        let stored = std::fs::read(&store.connection_path).expect("read future profile");

        assert!(matches!(
            store.load_profile(),
            Err(RelayCredentialError::InvalidData(
                "unsupported relay connection schema version"
            ))
        ));
        assert_eq!(std::fs::read(&store.connection_path).unwrap(), stored);
    }

    #[test]
    fn legacy_pdv1_profile_is_rejected_and_requires_reconfiguration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = RelayCredentialStore::new(dir.path());
        let mut legacy = profile();
        legacy.device_token = legacy.device_token.replacen("pdv2.", "pdv1.", 1);
        write_encrypted_json(
            &store.connection_path,
            &legacy,
            "TEST_WRITE_FAILED",
            "test write failed",
        )
        .expect("seed legacy profile");

        assert!(matches!(
            store.load_profile(),
            Err(RelayCredentialError::InvalidData("device_token"))
        ));
        assert!(store.is_configured());
    }

    #[test]
    fn oversized_profile_is_rejected_without_replacing_existing_profile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = RelayCredentialStore::new(dir.path());
        store.save_profile(&profile()).expect("save first");
        let original = std::fs::read(&store.connection_path).expect("read first profile");

        let mut oversized = profile();
        oversized.device_token = "x".repeat(MAX_DEVICE_TOKEN_BYTES + 1);
        assert!(matches!(
            store.save_profile(&oversized),
            Err(RelayCredentialError::InvalidData("device_token"))
        ));
        assert_eq!(std::fs::read(&store.connection_path).unwrap(), original);
        assert_eq!(
            store
                .load_profile()
                .expect("load retained profile")
                .expect("retained profile")
                .device_token,
            DEVICE_TOKEN
        );
    }

    #[test]
    fn repeated_save_atomically_replaces_only_the_relay_profile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = RelayCredentialStore::new(dir.path());
        let unrelated = dir.path().join("keep-me.txt");
        std::fs::write(&unrelated, b"keep").expect("seed unrelated file");

        store.save_profile(&profile()).expect("save first");
        store
            .save_profile(
                &RelayConnectionProfile::new(
                    "https://relay.example.test".into(),
                    "pdv2.123e4567-e89b-12d3-a456-426614174001.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                    1_700_000_000_001,
                )
                .expect("valid replacement profile"),
            )
            .expect("replace profile");

        assert_eq!(
            store
                .load_profile()
                .expect("load profile")
                .expect("profile")
                .device_token,
            "pdv2.123e4567-e89b-12d3-a456-426614174001.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        );
        assert_eq!(std::fs::read(unrelated).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn profile_validation_rejects_noncanonical_or_whitespace_values_without_leaking_tokens() {
        let invalid_token = RelayConnectionProfile::new(
            "https://relay.example.test".into(),
            "pdv2.123E4567-e89b-12d3-a456-426614174000.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                .into(),
            1,
        )
        .unwrap_err();
        assert!(matches!(
            invalid_token,
            RelayCredentialError::InvalidData("device_token")
        ));

        let noncanonical_secret = RelayConnectionProfile::new(
            "https://relay.example.test".into(),
            "pdv2.123e4567-e89b-12d3-a456-426614174000.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB"
                .into(),
            1,
        )
        .unwrap_err();
        assert!(matches!(
            noncanonical_secret,
            RelayCredentialError::InvalidData("device_token")
        ));

        let whitespace_url = RelayConnectionProfile::new(
            " https://relay.example.test".into(),
            DEVICE_TOKEN.into(),
            1,
        )
        .unwrap_err();
        assert!(matches!(
            whitespace_url,
            RelayCredentialError::InvalidData("base_url")
        ));

        let profile = profile();
        let debug = format!("{profile:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(DEVICE_TOKEN));
        assert!(!debug.contains(&profile.device_id));
    }
}
