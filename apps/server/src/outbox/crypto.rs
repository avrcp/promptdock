use std::{path::Path, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead as _, KeyInit as _, Payload},
};
use rand::TryRngCore as _;
use zeroize::{ZeroizeOnDrop, Zeroizing};

use super::OutboxError;

const CREDENTIALS_DIRECTORY_ENV: &str = "CREDENTIALS_DIRECTORY";
const SYSTEMD_CREDENTIAL_NAME: &str = "relay-master-key";
const AAD_CONTEXT: &[u8] = b"promptdock-relay:notification-bundle:v1";
const MASTER_KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;
const MAX_CREDENTIAL_BYTES: u64 = 1024;

#[derive(Clone)]
pub struct BundleContentCipher {
    key: Arc<MasterKey>,
}

#[derive(ZeroizeOnDrop)]
struct MasterKey([u8; MASTER_KEY_BYTES]);

impl BundleContentCipher {
    pub async fn from_systemd_credential() -> Result<Self, OutboxError> {
        let directory = std::env::var_os(CREDENTIALS_DIRECTORY_ENV)
            .filter(|value| !value.is_empty())
            .ok_or(OutboxError::CryptoUnavailable)?;
        let path = Path::new(&directory).join(SYSTEMD_CREDENTIAL_NAME);
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|_| OutboxError::CryptoUnavailable)?;
        if !metadata.is_file() || metadata.len() > MAX_CREDENTIAL_BYTES {
            return Err(OutboxError::CryptoUnavailable);
        }
        let source = Zeroizing::new(
            tokio::fs::read(path)
                .await
                .map_err(|_| OutboxError::CryptoUnavailable)?,
        );
        let first = source
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
            .ok_or(OutboxError::CryptoUnavailable)?;
        let last = source
            .iter()
            .rposition(|byte| !byte.is_ascii_whitespace())
            .ok_or(OutboxError::CryptoUnavailable)?;
        if source[first..=last].iter().any(u8::is_ascii_whitespace) {
            return Err(OutboxError::CryptoUnavailable);
        }
        let encoded = std::str::from_utf8(&source[first..=last])
            .map_err(|_| OutboxError::CryptoUnavailable)?;
        let decoded = Zeroizing::new(
            STANDARD
                .decode(encoded)
                .map_err(|_| OutboxError::CryptoUnavailable)?,
        );
        if decoded.len() != MASTER_KEY_BYTES || STANDARD.encode(&decoded) != encoded {
            return Err(OutboxError::CryptoUnavailable);
        }
        let key = decoded
            .as_slice()
            .try_into()
            .map_err(|_| OutboxError::CryptoUnavailable)?;
        Ok(Self::new(key))
    }

    pub(crate) fn new(key: [u8; MASTER_KEY_BYTES]) -> Self {
        Self {
            key: Arc::new(MasterKey(key)),
        }
    }

    /// Deterministic key injection for black-box Relay HTTP tests. Production
    /// callers must load the systemd credential instead.
    pub fn new_for_test(key: [u8; MASTER_KEY_BYTES]) -> Self {
        Self::new(key)
    }

    pub(crate) fn encrypt(
        &self,
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<([u8; NONCE_BYTES], Vec<u8>), OutboxError> {
        let cipher = XChaCha20Poly1305::new_from_slice(&self.key.0)
            .map_err(|_| OutboxError::CryptoUnavailable)?;
        let mut nonce = [0_u8; NONCE_BYTES];
        rand::rngs::OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| OutboxError::CryptoUnavailable)?;
        let xnonce = XNonce::from(nonce);
        let ciphertext = cipher
            .encrypt(
                &xnonce,
                Payload {
                    msg: plaintext,
                    aad: &framed_aad(aad),
                },
            )
            .map_err(|_| OutboxError::CryptoUnavailable)?;
        Ok((nonce, ciphertext))
    }

    pub(crate) fn decrypt(
        &self,
        nonce: &[u8],
        ciphertext: &[u8],
        aad: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, OutboxError> {
        let nonce: [u8; NONCE_BYTES] = nonce.try_into().map_err(|_| OutboxError::CorruptContent)?;
        let nonce = XNonce::from(nonce);
        let cipher = XChaCha20Poly1305::new_from_slice(&self.key.0)
            .map_err(|_| OutboxError::CryptoUnavailable)?;
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: &framed_aad(aad),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| OutboxError::CorruptContent)
    }

    pub(crate) fn request_digest(&self, framed_request: &[u8]) -> [u8; 32] {
        *blake3::keyed_hash(&self.key.0, framed_request).as_bytes()
    }
}

fn framed_aad(aad: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(AAD_CONTEXT.len() + 8 + aad.len());
    framed.extend_from_slice(AAD_CONTEXT);
    framed.extend_from_slice(&(aad.len() as u64).to_be_bytes());
    framed.extend_from_slice(aad);
    framed
}
