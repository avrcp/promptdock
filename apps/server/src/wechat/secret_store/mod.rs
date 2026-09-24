use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead as _, KeyInit as _, Payload},
};
use rand::TryRngCore as _;
use relay_provider_wechat::credentials::{ConnectionBundle, CredentialValidationError};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex, OwnedMutexGuard};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

pub const DEFAULT_CONNECTION_FILE: &str = "/var/lib/promptdock-relay/wechat-connection.enc";
pub const SYSTEMD_CREDENTIAL_NAME: &str = "relay-master-key";

const CREDENTIALS_DIRECTORY_ENV: &str = "CREDENTIALS_DIRECTORY";
const FORMAT_VERSION: u16 = 1;
const ALGORITHM: &str = "xchacha20poly1305";
const AAD_V1: &[u8] = b"promptdock-relay:wechat-connection:v1";
const MASTER_KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;
const MAX_MASTER_CREDENTIAL_BYTES: u64 = 1024;
const MAX_ENCRYPTED_FILE_BYTES: u64 = 4 * 1024 * 1024;
const TEMP_CREATE_ATTEMPTS: usize = 16;

/// Durable encrypted storage for the complete WeChat connection document.
///
/// The implementation is intentionally file-backed: the encrypted bundle is
/// the source of truth and must not be split across SQLite rows.
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn load(&self) -> Result<Option<ConnectionBundle>, SecretStoreError>;

    async fn save(&self, bundle: &ConnectionBundle) -> Result<(), SecretStoreError>;

    async fn clear(&self) -> Result<(), SecretStoreError>;
}

#[derive(Clone)]
pub struct EncryptedFileSecretStore {
    path: PathBuf,
    master_key: Arc<MasterKey>,
    write_lock: Arc<Mutex<()>>,
    identity: Arc<()>,
}

/// A same-directory candidate whose encrypted bytes have reached `sync_all`.
///
/// The token retains the store's writer lock until it is committed, discarded,
/// or dropped. Its custom Debug intentionally reveals no paths or contents.
#[must_use = "commit or discard the staged secret"]
pub struct StagedSecret {
    temporary_path: PathBuf,
    store_identity: Arc<()>,
    _guard: OwnedMutexGuard<()>,
}

impl std::fmt::Debug for StagedSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StagedSecret")
            .field("state", &"encrypted-and-synced")
            .finish()
    }
}

impl EncryptedFileSecretStore {
    #[cfg(test)]
    /// Deterministic key injection for black-box Relay HTTP tests. Production
    /// callers must load the systemd credential.
    pub fn new_for_test(path: PathBuf, master_key: [u8; MASTER_KEY_BYTES]) -> Self {
        Self {
            path,
            master_key: Arc::new(MasterKey(master_key)),
            write_lock: Arc::new(Mutex::new(())),
            identity: Arc::new(()),
        }
    }

    /// Initializes the store from systemd's credential directory.
    ///
    /// Only the directory locator comes from the environment. The key itself
    /// is read from the fixed `relay-master-key` credential file.
    pub async fn from_systemd_credential(
        path: impl Into<PathBuf>,
    ) -> Result<Self, SecretStoreError> {
        let directory = std::env::var_os(CREDENTIALS_DIRECTORY_ENV)
            .filter(|value| !value.is_empty())
            .ok_or(SecretStoreError::MasterCredentialUnavailable)?;
        let master_key = load_master_key(Path::new(&directory)).await?;
        Ok(Self {
            path: path.into(),
            master_key: Arc::new(master_key),
            write_lock: Arc::new(Mutex::new(())),
            identity: Arc::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Encrypts and fsyncs a candidate without replacing the active bundle.
    /// This is the first half of the Phase 6 blue/green cutover contract.
    pub async fn stage(&self, bundle: &ConnectionBundle) -> Result<StagedSecret, SecretStoreError> {
        self.stage_with_failpoint(bundle, StageFailpoint::None)
            .await
    }

    async fn stage_with_failpoint(
        &self,
        bundle: &ConnectionBundle,
        failpoint: StageFailpoint,
    ) -> Result<StagedSecret, SecretStoreError> {
        let guard = Arc::clone(&self.write_lock).lock_owned().await;
        bundle
            .validate()
            .map_err(SecretStoreError::InvalidConnectionBundle)?;

        let plaintext = Zeroizing::new(serde_json::to_vec(bundle).map_err(|_| {
            SecretStoreError::InvalidConnectionBundle(CredentialValidationError::InvalidData(
                "bundle serialization failed",
            ))
        })?);
        let envelope = encrypt_envelope(&self.master_key.0, &plaintext)?;
        let serialized =
            serde_json::to_vec(&envelope).map_err(|_| SecretStoreError::InvalidEncryptedFile)?;

        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let (temporary_path, mut temporary_file) = create_temporary(parent).await?;

        let write_result = async {
            temporary_file
                .write_all(&serialized)
                .await
                .map_err(SecretStoreError::Io)?;
            temporary_file
                .sync_all()
                .await
                .map_err(SecretStoreError::Io)?;
            drop(temporary_file);

            if failpoint == StageFailpoint::AfterTempSync {
                return Err(SecretStoreError::Io(io::Error::other(
                    "injected atomic save failure",
                )));
            }
            Ok(())
        }
        .await;

        if write_result.is_err() {
            let _ = tokio::fs::remove_file(&temporary_path).await;
        }
        write_result?;
        Ok(StagedSecret {
            temporary_path,
            store_identity: Arc::clone(&self.identity),
            _guard: guard,
        })
    }

    /// Atomically makes a previously fsynced candidate active.
    ///
    /// `CommitDurabilityUncertain` means rename already succeeded: callers must
    /// treat the new bundle as source of truth and must not restore an old token.
    pub async fn commit(&self, staged: StagedSecret) -> Result<(), SecretStoreError> {
        self.commit_with_failpoint(staged, CommitFailpoint::None)
            .await
    }

    async fn commit_with_failpoint(
        &self,
        staged: StagedSecret,
        failpoint: CommitFailpoint,
    ) -> Result<(), SecretStoreError> {
        self.verify_staged(&staged)?;
        let parent = self.parent();
        if let Err(error) = tokio::fs::rename(&staged.temporary_path, &self.path).await {
            let _ = tokio::fs::remove_file(&staged.temporary_path).await;
            return Err(SecretStoreError::Io(error));
        }
        if failpoint == CommitFailpoint::AfterRename {
            return Err(SecretStoreError::CommitDurabilityUncertain(
                io::Error::other("injected parent directory sync failure"),
            ));
        }
        sync_parent_directory(parent)
            .await
            .map_err(|error| match error {
                SecretStoreError::Io(source) => SecretStoreError::CommitDurabilityUncertain(source),
                other => other,
            })
    }

    /// Removes a staged candidate without changing the active bundle.
    pub async fn discard(&self, staged: StagedSecret) -> Result<(), SecretStoreError> {
        self.verify_staged(&staged)?;
        match tokio::fs::remove_file(&staged.temporary_path).await {
            Ok(()) => sync_parent_directory(self.parent()).await,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SecretStoreError::Io(error)),
        }
    }

    fn verify_staged(&self, staged: &StagedSecret) -> Result<(), SecretStoreError> {
        if Arc::ptr_eq(&self.identity, &staged.store_identity) {
            Ok(())
        } else {
            Err(SecretStoreError::ForeignStagedSecret)
        }
    }

    fn parent(&self) -> &Path {
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }
}

#[async_trait]
impl SecretStore for EncryptedFileSecretStore {
    async fn load(&self) -> Result<Option<ConnectionBundle>, SecretStoreError> {
        let encrypted = match read_encrypted_file(&self.path, MAX_ENCRYPTED_FILE_BYTES).await {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SecretStoreError::Io(error)),
        };
        let envelope: Envelope = serde_json::from_slice(&encrypted)
            .map_err(|_| SecretStoreError::InvalidEncryptedFile)?;
        let plaintext = decrypt_envelope(&self.master_key.0, &envelope)?;
        let bundle: ConnectionBundle = serde_json::from_slice(&plaintext)
            .map_err(|_| SecretStoreError::InvalidEncryptedFile)?;
        bundle
            .validate()
            .map_err(SecretStoreError::InvalidConnectionBundle)?;
        Ok(Some(bundle))
    }

    async fn save(&self, bundle: &ConnectionBundle) -> Result<(), SecretStoreError> {
        let staged = self.stage(bundle).await?;
        self.commit(staged).await
    }

    async fn clear(&self) -> Result<(), SecretStoreError> {
        let _guard = self.write_lock.lock().await;
        let parent = self.parent();
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => sync_parent_directory(parent).await,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SecretStoreError::Io(error)),
        }
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct MasterKey([u8; MASTER_KEY_BYTES]);

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    format_version: u16,
    algorithm: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StageFailpoint {
    None,
    AfterTempSync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitFailpoint {
    None,
    AfterRename,
}

#[derive(Debug, Error)]
pub enum SecretStoreError {
    #[error("systemd master credential is unavailable")]
    MasterCredentialUnavailable,
    #[error("systemd master credential is invalid")]
    InvalidMasterCredential,
    #[error("encrypted connection file is invalid or cannot be authenticated")]
    InvalidEncryptedFile,
    #[error("WeChat connection bundle is invalid")]
    InvalidConnectionBundle(#[source] CredentialValidationError),
    #[error("secret store I/O operation failed")]
    Io(#[source] io::Error),
    #[error("secret commit completed but crash durability is uncertain")]
    CommitDurabilityUncertain(#[source] io::Error),
    #[error("staged secret belongs to a different store")]
    ForeignStagedSecret,
}

async fn load_master_key(directory: &Path) -> Result<MasterKey, SecretStoreError> {
    let source = read_bounded(
        &directory.join(SYSTEMD_CREDENTIAL_NAME),
        MAX_MASTER_CREDENTIAL_BYTES,
    )
    .await
    .map_err(|error| match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => {
            SecretStoreError::MasterCredentialUnavailable
        }
        _ => SecretStoreError::Io(error),
    })?;
    parse_master_key(source)
}

fn parse_master_key(source: Vec<u8>) -> Result<MasterKey, SecretStoreError> {
    let source = Zeroizing::new(source);
    let first = source
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .ok_or(SecretStoreError::InvalidMasterCredential)?;
    let last = source
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .ok_or(SecretStoreError::InvalidMasterCredential)?;
    if source[first..=last].iter().any(u8::is_ascii_whitespace) {
        return Err(SecretStoreError::InvalidMasterCredential);
    }

    let encoded = std::str::from_utf8(&source[first..=last])
        .map_err(|_| SecretStoreError::InvalidMasterCredential)?;
    let decoded = Zeroizing::new(
        STANDARD
            .decode(encoded)
            .map_err(|_| SecretStoreError::InvalidMasterCredential)?,
    );
    if decoded.len() != MASTER_KEY_BYTES || STANDARD.encode(&decoded) != encoded {
        return Err(SecretStoreError::InvalidMasterCredential);
    }
    let key: [u8; MASTER_KEY_BYTES] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| SecretStoreError::InvalidMasterCredential)?;
    Ok(MasterKey(key))
}

fn encrypt_envelope(
    key: &[u8; MASTER_KEY_BYTES],
    plaintext: &[u8],
) -> Result<Envelope, SecretStoreError> {
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| SecretStoreError::InvalidMasterCredential)?;
    let mut nonce = [0_u8; NONCE_BYTES];
    rand::rngs::OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|error| SecretStoreError::Io(io::Error::other(error)))?;
    let nonce = XNonce::from(nonce);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: AAD_V1,
            },
        )
        .map_err(|_| SecretStoreError::InvalidEncryptedFile)?;

    Ok(Envelope {
        format_version: FORMAT_VERSION,
        algorithm: ALGORITHM.to_owned(),
        nonce: STANDARD.encode(nonce),
        ciphertext: STANDARD.encode(ciphertext),
    })
}

fn decrypt_envelope(
    key: &[u8; MASTER_KEY_BYTES],
    envelope: &Envelope,
) -> Result<Zeroizing<Vec<u8>>, SecretStoreError> {
    if envelope.format_version != FORMAT_VERSION || envelope.algorithm != ALGORITHM {
        return Err(SecretStoreError::InvalidEncryptedFile);
    }
    let nonce = decode_canonical(&envelope.nonce)?;
    let nonce: [u8; NONCE_BYTES] = nonce
        .try_into()
        .map_err(|_| SecretStoreError::InvalidEncryptedFile)?;
    let nonce = XNonce::from(nonce);
    let ciphertext = decode_canonical(&envelope.ciphertext)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| SecretStoreError::InvalidMasterCredential)?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &ciphertext,
                aad: AAD_V1,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| SecretStoreError::InvalidEncryptedFile)
}

fn decode_canonical(source: &str) -> Result<Vec<u8>, SecretStoreError> {
    let decoded = STANDARD
        .decode(source)
        .map_err(|_| SecretStoreError::InvalidEncryptedFile)?;
    if STANDARD.encode(&decoded) == source {
        Ok(decoded)
    } else {
        Err(SecretStoreError::InvalidEncryptedFile)
    }
}

async fn read_bounded(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let file = tokio::fs::File::open(path).await?;
    read_open_file(file, maximum).await
}

#[cfg(unix)]
async fn read_encrypted_file(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    use rustix::fs::{Mode, OFlags};
    use std::os::unix::fs::MetadataExt as _;

    let owned_path = path.to_owned();
    let file = tokio::task::spawn_blocking(move || {
        let descriptor = rustix::fs::open(
            &owned_path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
        let file: std::fs::File = descriptor.into();
        let metadata = file.metadata()?;
        validate_unix_file_security(
            metadata.file_type().is_file(),
            metadata.mode(),
            metadata.uid(),
            rustix::process::geteuid().as_raw(),
        )?;
        Ok::<std::fs::File, io::Error>(file)
    })
    .await
    .map_err(io::Error::other)??;
    read_open_file(tokio::fs::File::from_std(file), maximum).await
}

#[cfg(unix)]
fn validate_unix_file_security(
    is_regular: bool,
    mode: u32,
    owner: u32,
    effective_user: u32,
) -> io::Result<()> {
    if is_regular && mode & 0o777 == 0o600 && owner == effective_user {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "encrypted connection file has unsafe ownership, mode, or type",
        ))
    }
}

#[cfg(not(unix))]
async fn read_encrypted_file(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let file = tokio::fs::File::open(path).await?;
    if !file.metadata().await?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "encrypted connection path is not a regular file",
        ));
    }
    read_open_file(file, maximum).await
}

async fn read_open_file(file: tokio::fs::File, maximum: u64) -> io::Result<Vec<u8>> {
    let metadata = file.metadata().await?;
    if metadata.len() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds secret store limit",
        ));
    }
    let capacity = usize::try_from(metadata.len()).unwrap_or(0);
    let mut output = Vec::with_capacity(capacity);
    let mut limited = file.take(maximum + 1);
    tokio::io::AsyncReadExt::read_to_end(&mut limited, &mut output).await?;
    if output.len() as u64 > maximum {
        output.zeroize();
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds secret store limit",
        ));
    }
    Ok(output)
}

async fn create_temporary(parent: &Path) -> Result<(PathBuf, tokio::fs::File), SecretStoreError> {
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let mut suffix = [0_u8; 16];
        rand::rngs::OsRng
            .try_fill_bytes(&mut suffix)
            .map_err(|error| SecretStoreError::Io(io::Error::other(error)))?;
        let name = format!(
            ".wechat-connection.enc.tmp-{}-{}",
            std::process::id(),
            hex(&suffix)
        );
        let path = parent.join(name);
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path).await {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(SecretStoreError::Io(error)),
        }
    }
    Err(SecretStoreError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate secret temporary file",
    )))
}

async fn sync_parent_directory(parent: &Path) -> Result<(), SecretStoreError> {
    #[cfg(unix)]
    {
        tokio::fs::File::open(parent)
            .await
            .map_err(SecretStoreError::Io)?
            .sync_all()
            .await
            .map_err(SecretStoreError::Io)?;
    }
    #[cfg(not(unix))]
    let _ = parent;
    Ok(())
}

fn hex(source: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(source.len() * 2);
    for byte in source {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
fn is_temporary_file(path: &Path) -> bool {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.starts_with(".wechat-connection.enc.tmp-"))
}

#[cfg(test)]
mod tests {
    use relay_provider_wechat::credentials::{
        WECHAT_SECRET_SCHEMA_VERSION, WechatCredentials, WechatSessionSecrets,
    };

    use super::*;

    fn bundle(marker: &str) -> ConnectionBundle {
        let user_id = format!("user-{marker}@im.wechat");
        let credentials = WechatCredentials {
            schema_version: WECHAT_SECRET_SCHEMA_VERSION,
            bot_token: format!("bot-secret-{marker}"),
            account_id: format!("account-{marker}@im.bot"),
            user_id: user_id.clone(),
            base_url: "https://ilinkai.weixin.qq.com".into(),
            saved_at: 1_700_000_000_000,
        };
        let mut session = WechatSessionSecrets::default();
        session.get_updates_buf = format!("cursor-secret-{marker}");
        session.context_token = Some(format!("context-secret-{marker}"));
        session.context_token_user_id = Some(user_id);
        session.context_token_updated_at = Some(1_700_000_000_100);
        assert!(session.remember_message_key(format!("message-{marker}")));
        ConnectionBundle::new(credentials, session).expect("valid bundle")
    }

    fn test_store(path: PathBuf, key: u8) -> EncryptedFileSecretStore {
        EncryptedFileSecretStore {
            path,
            master_key: Arc::new(MasterKey([key; MASTER_KEY_BYTES])),
            write_lock: Arc::new(Mutex::new(())),
            identity: Arc::new(()),
        }
    }

    #[test]
    fn master_credential_requires_canonical_base64_of_exactly_32_bytes() {
        let encoded = STANDARD.encode([7_u8; MASTER_KEY_BYTES]);
        assert_eq!(
            parse_master_key(format!("\n{encoded}\r\n").into_bytes())
                .unwrap()
                .0,
            [7; 32]
        );
        for invalid in [
            String::new(),
            STANDARD.encode([7_u8; MASTER_KEY_BYTES - 1]),
            format!("{encoded}="),
            format!("{} {}", &encoded[..4], &encoded[4..]),
            "not-base64".to_owned(),
        ] {
            assert!(matches!(
                parse_master_key(invalid.into_bytes()),
                Err(SecretStoreError::InvalidMasterCredential)
            ));
        }
    }

    #[tokio::test]
    async fn loads_systemd_credential_from_fixed_filename() {
        let directory = tempfile::tempdir().expect("credential directory");
        tokio::fs::write(
            directory.path().join(SYSTEMD_CREDENTIAL_NAME),
            format!("{}\n", STANDARD.encode([9_u8; MASTER_KEY_BYTES])),
        )
        .await
        .expect("credential");
        let loaded = load_master_key(directory.path()).await.expect("master key");
        assert_eq!(loaded.0, [9; MASTER_KEY_BYTES]);
        assert!(matches!(
            load_master_key(&directory.path().join("missing")).await,
            Err(SecretStoreError::MasterCredentialUnavailable)
        ));
    }

    #[tokio::test]
    async fn encrypted_bundle_roundtrips_without_plaintext_on_disk() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("wechat-connection.enc");
        let store = test_store(path.clone(), 7);
        let expected = bundle("roundtrip-sentinel");
        store.save(&expected).await.expect("save");
        let bytes = tokio::fs::read(&path).await.expect("encrypted file");
        for secret in [
            "bot-secret-roundtrip-sentinel",
            "cursor-secret-roundtrip-sentinel",
            "context-secret-roundtrip-sentinel",
        ] {
            assert!(!contains(&bytes, secret.as_bytes()), "plaintext leaked");
        }
        assert!(String::from_utf8_lossy(&bytes).contains("\"formatVersion\":1"));
        assert!(store.load().await.expect("load") == Some(expected));
    }

    #[tokio::test]
    async fn wrong_key_and_corruption_are_rejected() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("wechat-connection.enc");
        let store = test_store(path.clone(), 7);
        store.save(&bundle("auth")).await.expect("save");
        assert!(matches!(
            test_store(path.clone(), 8).load().await,
            Err(SecretStoreError::InvalidEncryptedFile)
        ));

        let original = tokio::fs::read(&path).await.expect("envelope");
        let mut envelope: serde_json::Value = serde_json::from_slice(&original).expect("json");
        envelope["nonce"] = serde_json::Value::String(STANDARD.encode([1_u8; NONCE_BYTES - 1]));
        tokio::fs::write(&path, serde_json::to_vec(&envelope).expect("json"))
            .await
            .expect("corrupt nonce");
        assert!(matches!(
            store.load().await,
            Err(SecretStoreError::InvalidEncryptedFile)
        ));

        let mut envelope: serde_json::Value = serde_json::from_slice(&original).expect("json");
        envelope["algorithm"] = serde_json::json!("XChaCha20-Poly1305");
        tokio::fs::write(&path, serde_json::to_vec(&envelope).expect("json"))
            .await
            .expect("wrong algorithm");
        assert!(matches!(
            store.load().await,
            Err(SecretStoreError::InvalidEncryptedFile)
        ));

        let mut envelope: serde_json::Value = serde_json::from_slice(&original).expect("json");
        envelope["ciphertext"] = serde_json::Value::String(STANDARD.encode([1_u8; 15]));
        tokio::fs::write(&path, serde_json::to_vec(&envelope).expect("json"))
            .await
            .expect("short authentication tag");
        assert!(matches!(
            store.load().await,
            Err(SecretStoreError::InvalidEncryptedFile)
        ));

        let mut envelope: serde_json::Value = serde_json::from_slice(&original).expect("json");
        let ciphertext = envelope["ciphertext"].as_str().expect("ciphertext");
        let mut corrupted = STANDARD.decode(ciphertext).expect("base64");
        corrupted[0] ^= 1;
        envelope["ciphertext"] = serde_json::Value::String(STANDARD.encode(corrupted));
        tokio::fs::write(&path, serde_json::to_vec(&envelope).expect("json"))
            .await
            .expect("corrupt ciphertext");
        assert!(matches!(
            store.load().await,
            Err(SecretStoreError::InvalidEncryptedFile)
        ));
    }

    #[tokio::test]
    async fn rejects_future_envelope_version_and_context_mismatch() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("wechat-connection.enc");
        let store = test_store(path.clone(), 7);
        store.save(&bundle("version")).await.expect("save");
        let mut envelope: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.expect("envelope")).expect("json");
        envelope["formatVersion"] = serde_json::json!(2);
        tokio::fs::write(&path, serde_json::to_vec(&envelope).expect("json"))
            .await
            .expect("future version");
        assert!(matches!(
            store.load().await,
            Err(SecretStoreError::InvalidEncryptedFile)
        ));

        let mut invalid = serde_json::to_value(bundle("mismatch")).expect("bundle json");
        invalid["session"]["context_token_user_id"] = serde_json::json!("another-user");
        let plaintext = Zeroizing::new(serde_json::to_vec(&invalid).expect("plaintext"));
        let invalid_envelope = encrypt_envelope(&store.master_key.0, &plaintext).expect("encrypt");
        tokio::fs::write(
            &path,
            serde_json::to_vec(&invalid_envelope).expect("envelope"),
        )
        .await
        .expect("write");
        assert!(matches!(
            store.load().await,
            Err(SecretStoreError::InvalidConnectionBundle(_))
        ));
    }

    #[tokio::test]
    async fn failed_save_keeps_active_file_and_removes_its_temporary_file() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("wechat-connection.enc");
        let store = test_store(path.clone(), 7);
        let active = bundle("active");
        store.save(&active).await.expect("active save");
        let before = tokio::fs::read(&path).await.expect("active bytes");

        assert!(matches!(
            store
                .stage_with_failpoint(&bundle("candidate"), StageFailpoint::AfterTempSync)
                .await,
            Err(SecretStoreError::Io(_))
        ));
        assert_eq!(tokio::fs::read(&path).await.expect("active bytes"), before);
        assert!(store.load().await.expect("load") == Some(active));
        let mut entries = tokio::fs::read_dir(directory.path())
            .await
            .expect("directory");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            assert!(!is_temporary_file(&entry.path()), "temporary file leaked");
        }
    }

    #[tokio::test]
    async fn staged_candidate_does_not_replace_active_until_commit() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("wechat-connection.enc");
        let store = test_store(path.clone(), 7);
        let active = bundle("blue");
        store.save(&active).await.expect("active save");

        let staged = store.stage(&bundle("green")).await.expect("stage");
        assert!(store.load().await.expect("load") == Some(active));
        let debug = format!("{staged:?}");
        assert!(!debug.contains(path.to_string_lossy().as_ref()));
        assert!(!debug.contains("green"));
        store.discard(staged).await.expect("discard");
        assert!(store.load().await.expect("load") == Some(bundle("blue")));
    }

    #[tokio::test]
    async fn writer_operations_wait_for_the_staged_token() {
        let directory = tempfile::tempdir().expect("directory");
        let store = test_store(directory.path().join("wechat-connection.enc"), 7);
        let staged = store.stage(&bundle("first-writer")).await.expect("stage");
        let concurrent_store = store.clone();
        let save_task =
            tokio::spawn(async move { concurrent_store.save(&bundle("second-writer")).await });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(!save_task.is_finished(), "writer lock was bypassed");

        store.discard(staged).await.expect("discard");
        tokio::time::timeout(std::time::Duration::from_secs(2), save_task)
            .await
            .expect("writer unblocked")
            .expect("writer task")
            .expect("save");
        assert!(store.load().await.expect("load") == Some(bundle("second-writer")));
    }

    #[tokio::test]
    async fn post_rename_failure_reports_uncertain_durability_and_keeps_new_truth() {
        let directory = tempfile::tempdir().expect("directory");
        let store = test_store(directory.path().join("wechat-connection.enc"), 7);
        let staged = store.stage(&bundle("post-commit")).await.expect("stage");
        assert!(matches!(
            store
                .commit_with_failpoint(staged, CommitFailpoint::AfterRename)
                .await,
            Err(SecretStoreError::CommitDurabilityUncertain(_))
        ));
        assert!(store.load().await.expect("load") == Some(bundle("post-commit")));
    }

    #[tokio::test]
    async fn orphan_temporary_file_is_ignored_and_clear_is_idempotent() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("wechat-connection.enc");
        let store = test_store(path, 7);
        let active = bundle("orphan");
        store.save(&active).await.expect("save");
        tokio::fs::write(
            directory
                .path()
                .join(".wechat-connection.enc.tmp-999-orphan"),
            b"not an envelope",
        )
        .await
        .expect("orphan");
        assert!(store.load().await.expect("load") == Some(active));
        store.clear().await.expect("clear");
        store.clear().await.expect("idempotent clear");
        assert!(store.load().await.expect("load missing").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn atomic_replace_uses_mode_0600() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("wechat-connection.enc");
        let store = test_store(path.clone(), 7);
        store.save(&bundle("old")).await.expect("old save");
        let old_inode = tokio::fs::metadata(&path)
            .await
            .expect("old metadata")
            .ino();
        store.save(&bundle("new")).await.expect("replace");
        let metadata = tokio::fs::metadata(&path).await.expect("new metadata");
        assert_ne!(metadata.ino(), old_inode);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert!(store.load().await.expect("load") == Some(bundle("new")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn load_rejects_symlink_permissive_mode_and_non_regular_file() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("wechat-connection.enc");
        let store = test_store(path.clone(), 7);
        store.save(&bundle("security")).await.expect("save");

        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .await
            .expect("permissions");
        assert!(matches!(store.load().await, Err(SecretStoreError::Io(_))));

        tokio::fs::remove_file(&path).await.expect("remove");
        tokio::fs::create_dir(&path)
            .await
            .expect("directory at path");
        assert!(matches!(store.load().await, Err(SecretStoreError::Io(_))));
        tokio::fs::remove_dir(&path)
            .await
            .expect("remove directory");

        let target = directory.path().join("target.enc");
        let target_store = test_store(target.clone(), 7);
        target_store
            .save(&bundle("symlink-target"))
            .await
            .expect("target save");
        symlink(&target, &path).expect("symlink");
        assert!(matches!(store.load().await, Err(SecretStoreError::Io(_))));
    }

    #[cfg(unix)]
    #[test]
    fn unix_security_check_rejects_wrong_owner() {
        assert!(validate_unix_file_security(true, 0o100600, 1000, 1000).is_ok());
        assert!(validate_unix_file_security(true, 0o100600, 1000, 1001).is_err());
        assert!(validate_unix_file_security(true, 0o100640, 1000, 1000).is_err());
        assert!(validate_unix_file_security(false, 0o040600, 1000, 1000).is_err());
    }

    #[test]
    fn public_errors_and_debug_output_do_not_expose_secret_material() {
        let secret = "do-not-expose-master-or-token";
        let errors = [
            SecretStoreError::MasterCredentialUnavailable,
            SecretStoreError::InvalidMasterCredential,
            SecretStoreError::InvalidEncryptedFile,
            SecretStoreError::ForeignStagedSecret,
        ];
        for error in errors {
            assert!(!error.to_string().contains(secret));
            assert!(!format!("{error:?}").contains(secret));
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}
