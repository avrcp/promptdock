//! Process-local authorization for Desktop-approved Admin WeChat login.
//!
//! Grant secrets and Admin login ownership are deliberately never persisted.
//! The shared lifecycle fence serializes authorization checks with Admin device
//! lifecycle mutations, while the registry mutexes are held only for in-memory
//! state transitions (never while starting or polling a login).

use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::TryRngCore as _;
use serde::{Serialize, ser::SerializeStruct as _};
use subtle::ConstantTimeEq as _;
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const GRANT_TOKEN_BYTES: usize = 32;
const GRANT_HASH_CONTEXT: &str = "promptdock-relay/wechat-admin-login-grant/v1";

#[derive(Clone)]
pub struct WechatAdminLoginGrantRegistry {
    core: Arc<AuthorizationCore>,
    state: Arc<Mutex<GrantState>>,
}

#[derive(Clone)]
pub struct AdminLoginAccessRegistry {
    state: Arc<Mutex<HashMap<Uuid, AdminLoginAccess>>>,
}

struct AuthorizationCore {
    lifecycle_fence: Arc<AsyncMutex<()>>,
    generations: Mutex<HashMap<Uuid, u64>>,
    process_epoch: AtomicU64,
    closed: AtomicBool,
    origin: Instant,
}

#[derive(Default)]
struct GrantState {
    grants: HashMap<Uuid, GrantEntry>,
    active_by_device: HashMap<Uuid, Uuid>,
}

struct GrantEntry {
    token_hash: [u8; 32],
    owner_device_id: Uuid,
    authorization_revision: String,
    generation: u64,
    process_epoch: u64,
    expires_at_monotonic: Duration,
    state: GrantStatus,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GrantStatus {
    Active,
    Consuming,
    Consumed,
    Failed,
}

#[derive(Clone)]
struct AdminLoginAccess {
    owner_device_id: Uuid,
    authorization_revision: String,
    generation: u64,
    process_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantConsumption {
    pub grant_id: Uuid,
    pub owner_device_id: Uuid,
    pub authorization_revision: String,
    pub generation: u64,
    pub process_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminLoginAccessLease {
    pub owner_device_id: Uuid,
    pub authorization_revision: String,
    pub generation: u64,
    pub process_epoch: u64,
}

/// Authorization carried by an Admin-started login all the way through QR
/// confirmation and blue/green promotion. Debug output intentionally exposes
/// neither the opaque revision nor registry internals.
#[derive(Clone)]
pub struct AdminLoginAuthorization {
    grants: WechatAdminLoginGrantRegistry,
    access: AdminLoginAccessRegistry,
    owner_device_id: Uuid,
    authorization_revision: String,
    generation: u64,
    process_epoch: u64,
}

impl AdminLoginAuthorization {
    pub fn new(
        grants: WechatAdminLoginGrantRegistry,
        access: AdminLoginAccessRegistry,
        consumption: &GrantConsumption,
    ) -> Self {
        Self {
            grants,
            access,
            owner_device_id: consumption.owner_device_id,
            authorization_revision: consumption.authorization_revision.clone(),
            generation: consumption.generation,
            process_epoch: consumption.process_epoch,
        }
    }

    pub fn owner_device_id(&self) -> Uuid {
        self.owner_device_id
    }

    pub fn authorization_revision(&self) -> &str {
        &self.authorization_revision
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn generation_is_current(&self) -> bool {
        self.grants
            .authorization_matches(self.owner_device_id, self.generation, self.process_epoch)
    }

    pub async fn lifecycle_guard(&self) -> OwnedMutexGuard<()> {
        self.grants.lifecycle_guard().await
    }

    pub fn remove_access(&self, login_id: Uuid) {
        self.access.remove(login_id);
    }
}

impl fmt::Debug for AdminLoginAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdminLoginAuthorization")
            .field("owner_device_id", &self.owner_device_id)
            .field("authorization_revision", &"<redacted>")
            .field("generation", &self.generation)
            .field("process_epoch", &self.process_epoch)
            .finish()
    }
}

/// A one-time wire receipt. The secret is zeroized on drop and redacted from
/// Debug; serialization is the sole intentional plaintext exposure.
pub struct WechatAdminLoginGrantReceipt {
    pub grant_id: Uuid,
    grant_token: Zeroizing<String>,
    pub expires_at: i64,
}

impl WechatAdminLoginGrantReceipt {
    pub fn grant_token(&self) -> &str {
        self.grant_token.as_str()
    }
}

impl fmt::Debug for WechatAdminLoginGrantReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WechatAdminLoginGrantReceipt")
            .field("grant_id", &self.grant_id)
            .field("grant_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl Serialize for WechatAdminLoginGrantReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut receipt = serializer.serialize_struct("WechatAdminLoginGrantReceipt", 3)?;
        receipt.serialize_field("grantId", &self.grant_id.to_string())?;
        receipt.serialize_field("grantToken", self.grant_token.as_str())?;
        receipt.serialize_field("expiresAt", &self.expires_at)?;
        receipt.end()
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WechatAdminGrantError {
    #[error("grant request is invalid")]
    InvalidRequest,
    #[error("grant is unavailable")]
    Rejected,
    #[error("secure randomness is unavailable")]
    Random,
    #[error("system clock is unavailable")]
    Clock,
}

impl WechatAdminLoginGrantRegistry {
    pub fn new_pair() -> (Self, AdminLoginAccessRegistry) {
        let core = Arc::new(AuthorizationCore {
            lifecycle_fence: Arc::new(AsyncMutex::new(())),
            generations: Mutex::new(HashMap::new()),
            process_epoch: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            origin: Instant::now(),
        });
        (
            Self {
                core: Arc::clone(&core),
                state: Arc::new(Mutex::new(GrantState::default())),
            },
            AdminLoginAccessRegistry {
                state: Arc::new(Mutex::new(HashMap::new())),
            },
        )
    }

    pub async fn lifecycle_guard(&self) -> OwnedMutexGuard<()> {
        Arc::clone(&self.core.lifecycle_fence).lock_owned().await
    }

    pub fn issue(
        &self,
        owner_device_id: Uuid,
        authorization_revision: String,
        expires_in: Duration,
    ) -> Result<WechatAdminLoginGrantReceipt, WechatAdminGrantError> {
        if expires_in.is_zero() || expires_in > Duration::from_secs(600) {
            return Err(WechatAdminGrantError::InvalidRequest);
        }
        let now = self.now();
        let expires_at_monotonic = now
            .checked_add(expires_in)
            .ok_or(WechatAdminGrantError::Clock)?;
        let expires_at = wall_time_ms()?
            .checked_add(
                i64::try_from(expires_in.as_millis()).map_err(|_| WechatAdminGrantError::Clock)?,
            )
            .ok_or(WechatAdminGrantError::Clock)?;
        let mut secret = Zeroizing::new([0_u8; GRANT_TOKEN_BYTES]);
        rand::rngs::OsRng
            .try_fill_bytes(&mut *secret)
            .map_err(|_| WechatAdminGrantError::Random)?;
        let token = Zeroizing::new(URL_SAFE_NO_PAD.encode(*secret));
        let token_hash = hash_token(token.as_bytes());
        let grant_id = Uuid::new_v4();
        let generation = self.current_generation(owner_device_id);
        let process_epoch = self.current_process_epoch();
        let mut state = self.lock_state();
        if self.core.closed.load(Ordering::Acquire) {
            return Err(WechatAdminGrantError::Rejected);
        }
        cleanup_expired(&mut state, now);
        if let Some(previous) = state.active_by_device.insert(owner_device_id, grant_id) {
            state.grants.remove(&previous);
        }
        state.grants.insert(
            grant_id,
            GrantEntry {
                token_hash,
                owner_device_id,
                authorization_revision,
                generation,
                process_epoch,
                expires_at_monotonic,
                state: GrantStatus::Active,
            },
        );
        Ok(WechatAdminLoginGrantReceipt {
            grant_id,
            grant_token: token,
            expires_at,
        })
    }

    pub fn begin_consume(
        &self,
        grant_id: Uuid,
        token: &str,
    ) -> Result<GrantConsumption, WechatAdminGrantError> {
        if token.len() != 43 {
            return Err(WechatAdminGrantError::Rejected);
        }
        let decoded = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(token)
                .map_err(|_| WechatAdminGrantError::Rejected)?,
        );
        if decoded.len() != GRANT_TOKEN_BYTES {
            return Err(WechatAdminGrantError::Rejected);
        }
        let canonical = Zeroizing::new(URL_SAFE_NO_PAD.encode(decoded.as_slice()));
        let canonical_matches: bool = canonical.as_bytes().ct_eq(token.as_bytes()).into();
        if !canonical_matches {
            return Err(WechatAdminGrantError::Rejected);
        }
        let candidate_hash = hash_token(token.as_bytes());
        let now = self.now();
        let mut state = self.lock_state();
        if self.core.closed.load(Ordering::Acquire) {
            return Err(WechatAdminGrantError::Rejected);
        }
        cleanup_expired(&mut state, now);
        let entry = state
            .grants
            .get_mut(&grant_id)
            .ok_or(WechatAdminGrantError::Rejected)?;
        let token_matches: bool = entry.token_hash.ct_eq(&candidate_hash).into();
        let generation = self.current_generation(entry.owner_device_id);
        let process_epoch = self.current_process_epoch();
        if !token_matches
            || entry.state != GrantStatus::Active
            || entry.expires_at_monotonic <= now
            || generation != entry.generation
            || process_epoch != entry.process_epoch
        {
            return Err(WechatAdminGrantError::Rejected);
        }
        entry.state = GrantStatus::Consuming;
        let consumption = GrantConsumption {
            grant_id,
            owner_device_id: entry.owner_device_id,
            authorization_revision: entry.authorization_revision.clone(),
            generation: entry.generation,
            process_epoch: entry.process_epoch,
        };
        state.active_by_device.remove(&consumption.owner_device_id);
        Ok(consumption)
    }

    pub fn finish_consumption(&self, consumption: &GrantConsumption, succeeded: bool) -> bool {
        let mut state = self.lock_state();
        let Some(entry) = state.grants.get_mut(&consumption.grant_id) else {
            return false;
        };
        let current_generation = self.current_generation(consumption.owner_device_id);
        let current_process_epoch = self.current_process_epoch();
        let valid = entry.state == GrantStatus::Consuming
            && entry.owner_device_id == consumption.owner_device_id
            && entry.authorization_revision == consumption.authorization_revision
            && entry.generation == consumption.generation
            && current_generation == consumption.generation
            && entry.process_epoch == consumption.process_epoch
            && current_process_epoch == consumption.process_epoch
            && !self.core.closed.load(Ordering::Acquire)
            && entry.expires_at_monotonic > self.now();
        entry.state = if succeeded && valid {
            GrantStatus::Consumed
        } else {
            GrantStatus::Failed
        };
        valid && succeeded
    }

    /// Idempotently removes a still-active grant owned by the authenticated
    /// device. Unknown, expired, consumed, or other-owner ids are indistinguishable.
    pub fn cancel_owned(&self, grant_id: Uuid, owner_device_id: Uuid) {
        let mut state = self.lock_state();
        cleanup_expired(&mut state, self.now());
        let owned = state.grants.get(&grant_id).is_some_and(|entry| {
            entry.owner_device_id == owner_device_id && entry.state == GrantStatus::Active
        });
        if owned {
            state.grants.remove(&grant_id);
            state.active_by_device.remove(&owner_device_id);
        }
    }

    pub fn invalidate_device(&self, device_id: Uuid) {
        let mut state = self.lock_state();
        state
            .grants
            .retain(|_, entry| entry.owner_device_id != device_id);
        state.active_by_device.remove(&device_id);
        drop(state);
        let mut generations = self.lock_generations();
        let generation = generations.entry(device_id).or_default();
        *generation = generation.wrapping_add(1);
    }

    pub fn authorization_matches(
        &self,
        device_id: Uuid,
        generation: u64,
        process_epoch: u64,
    ) -> bool {
        !self.core.closed.load(Ordering::Acquire)
            && self.current_generation(device_id) == generation
            && self.current_process_epoch() == process_epoch
    }

    /// Permanently closes this process-local registry. The epoch is bumped
    /// rather than reset so a candidate prepared before shutdown can never
    /// become current again while the process drains.
    pub fn clear(&self) {
        self.core.closed.store(true, Ordering::Release);
        self.core.process_epoch.fetch_add(1, Ordering::AcqRel);
        let mut state = self.lock_state();
        state.grants.clear();
        state.active_by_device.clear();
    }

    fn now(&self) -> Duration {
        self.core.origin.elapsed()
    }

    fn current_generation(&self, device_id: Uuid) -> u64 {
        self.lock_generations()
            .get(&device_id)
            .copied()
            .unwrap_or(0)
    }

    fn current_process_epoch(&self) -> u64 {
        self.core.process_epoch.load(Ordering::Acquire)
    }

    fn lock_state(&self) -> MutexGuard<'_, GrantState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_generations(&self) -> MutexGuard<'_, HashMap<Uuid, u64>> {
        self.core
            .generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    fn expire_for_test(&self, grant_id: Uuid) {
        if let Some(entry) = self.lock_state().grants.get_mut(&grant_id) {
            entry.expires_at_monotonic = Duration::ZERO;
        }
    }
}

impl fmt::Debug for WechatAdminLoginGrantRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WechatAdminLoginGrantRegistry")
            .finish_non_exhaustive()
    }
}

impl AdminLoginAccessRegistry {
    pub fn insert(&self, login_id: Uuid, consumption: &GrantConsumption) {
        self.lock_state().insert(
            login_id,
            AdminLoginAccess {
                owner_device_id: consumption.owner_device_id,
                authorization_revision: consumption.authorization_revision.clone(),
                generation: consumption.generation,
                process_epoch: consumption.process_epoch,
            },
        );
    }

    pub fn resolve(&self, login_id: Uuid) -> Option<AdminLoginAccessLease> {
        self.lock_state()
            .get(&login_id)
            .map(|access| AdminLoginAccessLease {
                owner_device_id: access.owner_device_id,
                authorization_revision: access.authorization_revision.clone(),
                generation: access.generation,
                process_epoch: access.process_epoch,
            })
    }

    pub fn remove(&self, login_id: Uuid) {
        self.lock_state().remove(&login_id);
    }

    pub fn invalidate_device(&self, device_id: Uuid) -> Vec<Uuid> {
        let mut state = self.lock_state();
        let login_ids = state
            .iter()
            .filter_map(|(login_id, access)| {
                (access.owner_device_id == device_id).then_some(*login_id)
            })
            .collect::<Vec<_>>();
        state.retain(|_, access| access.owner_device_id != device_id);
        login_ids
    }

    pub fn clear(&self) {
        self.lock_state().clear();
    }

    pub fn drain(&self) -> Vec<(Uuid, Uuid)> {
        let mut state = self.lock_state();
        let entries = state
            .iter()
            .map(|(login_id, access)| (*login_id, access.owner_device_id))
            .collect();
        state.clear();
        entries
    }

    fn lock_state(&self) -> MutexGuard<'_, HashMap<Uuid, AdminLoginAccess>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl fmt::Debug for AdminLoginAccessRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdminLoginAccessRegistry")
            .finish_non_exhaustive()
    }
}

fn cleanup_expired(state: &mut GrantState, now: Duration) {
    state
        .grants
        .retain(|_, entry| entry.expires_at_monotonic > now);
    state.active_by_device.retain(|device_id, grant_id| {
        state.grants.get(grant_id).is_some_and(|entry| {
            entry.owner_device_id == *device_id && entry.state == GrantStatus::Active
        })
    });
}

fn hash_token(token: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(GRANT_HASH_CONTEXT);
    hasher.update(token);
    *hasher.finalize().as_bytes()
}

fn wall_time_ms() -> Result<i64, WechatAdminGrantError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .ok_or(WechatAdminGrantError::Clock)
}

impl Drop for GrantEntry {
    fn drop(&mut self) {
        self.token_hash.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::*;

    fn issue(
        registry: &WechatAdminLoginGrantRegistry,
        owner: Uuid,
    ) -> WechatAdminLoginGrantReceipt {
        registry
            .issue(
                owner,
                "authorization-revision".to_owned(),
                Duration::from_secs(60),
            )
            .expect("grant")
    }

    #[test]
    fn receipt_is_strict_one_time_and_secret_safe() {
        let (registry, _access) = WechatAdminLoginGrantRegistry::new_pair();
        let receipt = issue(&registry, Uuid::new_v4());
        let token = receipt.grant_token().to_owned();
        let decoded = URL_SAFE_NO_PAD.decode(&token).expect("base64url token");
        assert_eq!(decoded.len(), GRANT_TOKEN_BYTES);
        assert!(!format!("{receipt:?}").contains(&token));
        let json = serde_json::to_string(&receipt).expect("receipt JSON");
        assert_eq!(json.matches(&token).count(), 1);
        let value: serde_json::Value = serde_json::from_str(&json).expect("receipt value");
        assert_eq!(value.as_object().expect("object").len(), 3);
    }

    #[test]
    fn one_active_grant_replay_wrong_token_and_restart_fail_closed() {
        let owner = Uuid::new_v4();
        let (registry, _access) = WechatAdminLoginGrantRegistry::new_pair();
        let replaced = issue(&registry, owner);
        let active = issue(&registry, owner);
        assert_eq!(
            registry.begin_consume(replaced.grant_id, replaced.grant_token()),
            Err(WechatAdminGrantError::Rejected)
        );
        assert_eq!(
            registry.begin_consume(active.grant_id, "wrong-token"),
            Err(WechatAdminGrantError::Rejected)
        );
        assert_eq!(
            registry.begin_consume(active.grant_id, &format!("{}=", active.grant_token())),
            Err(WechatAdminGrantError::Rejected)
        );
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut noncanonical = active.grant_token().as_bytes().to_vec();
        let last = noncanonical.last_mut().expect("token has a final symbol");
        let index = alphabet
            .iter()
            .position(|candidate| candidate == last)
            .expect("base64url symbol");
        *last = alphabet[index + 1];
        let noncanonical = String::from_utf8(noncanonical).expect("base64url is UTF-8");
        assert_eq!(noncanonical.len(), 43);
        assert_eq!(
            registry.begin_consume(active.grant_id, &noncanonical),
            Err(WechatAdminGrantError::Rejected)
        );
        let consumption = registry
            .begin_consume(active.grant_id, active.grant_token())
            .expect("one-time consume");
        assert!(registry.finish_consumption(&consumption, true));
        assert_eq!(
            registry.begin_consume(active.grant_id, active.grant_token()),
            Err(WechatAdminGrantError::Rejected)
        );

        let (restarted, _access) = WechatAdminLoginGrantRegistry::new_pair();
        assert_eq!(
            restarted.begin_consume(active.grant_id, active.grant_token()),
            Err(WechatAdminGrantError::Rejected)
        );
    }

    #[test]
    fn concurrent_consumption_has_exactly_one_winner() {
        let (registry, _access) = WechatAdminLoginGrantRegistry::new_pair();
        let receipt = issue(&registry, Uuid::new_v4());
        let grant_id = receipt.grant_id;
        let token = receipt.grant_token().to_owned();
        let mut workers = Vec::new();
        for _ in 0..32 {
            let registry = registry.clone();
            let token = token.clone();
            workers.push(thread::spawn(move || {
                registry.begin_consume(grant_id, &token).is_ok()
            }));
        }
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().expect("worker"))
                .filter(|won| *won)
                .count(),
            1
        );
    }

    #[test]
    fn invalidation_and_clear_remove_grants_and_owner_access() {
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let (registry, access) = WechatAdminLoginGrantRegistry::new_pair();
        let receipt = issue(&registry, owner);
        let consumption = registry
            .begin_consume(receipt.grant_id, receipt.grant_token())
            .expect("consume");
        let login_id = Uuid::new_v4();
        access.insert(login_id, &consumption);
        assert_eq!(
            access.resolve(login_id).expect("access").owner_device_id,
            owner
        );
        registry.invalidate_device(other);
        assert!(access.invalidate_device(other).is_empty());
        assert!(access.resolve(login_id).is_some());
        registry.invalidate_device(owner);
        assert_eq!(access.invalidate_device(owner), vec![login_id]);
        assert!(access.resolve(login_id).is_none());
        assert!(!registry.authorization_matches(
            owner,
            consumption.generation,
            consumption.process_epoch
        ));

        let receipt = issue(&registry, owner);
        registry.clear();
        access.clear();
        assert_eq!(
            registry.begin_consume(receipt.grant_id, receipt.grant_token()),
            Err(WechatAdminGrantError::Rejected)
        );
        assert!(matches!(
            registry.issue(owner, "post-shutdown".into(), Duration::from_secs(60)),
            Err(WechatAdminGrantError::Rejected)
        ));
    }

    #[test]
    fn expiration_bounds_are_strict() {
        let owner = Uuid::new_v4();
        let (registry, _access) = WechatAdminLoginGrantRegistry::new_pair();
        assert!(matches!(
            registry.issue(owner, "revision".into(), Duration::ZERO),
            Err(WechatAdminGrantError::InvalidRequest)
        ));
        assert!(matches!(
            registry.issue(owner, "revision".into(), Duration::from_secs(601)),
            Err(WechatAdminGrantError::InvalidRequest)
        ));
        let receipt = issue(&registry, owner);
        registry.expire_for_test(receipt.grant_id);
        assert_eq!(
            registry.begin_consume(receipt.grant_id, receipt.grant_token()),
            Err(WechatAdminGrantError::Rejected)
        );
    }
}
