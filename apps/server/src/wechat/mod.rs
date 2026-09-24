//! WeChat provider integration facade.
//!
//! Runtime ownership lives below this module; the crate-root compatibility
//! modules re-export the same items while existing callers migrate.

pub mod channel;
pub mod login;
pub mod monitor;
pub mod secret_store;

pub(crate) fn account_fingerprint(account_id: &str) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("promptdock-relay/wechat-sender/v1");
    hasher.update(account_id.as_bytes());
    format!("wx:{}", hasher.finalize().to_hex())
}
