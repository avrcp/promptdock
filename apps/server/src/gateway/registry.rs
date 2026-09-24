use std::{collections::BTreeSet, sync::Arc, time::Duration};

use tokio::{
    sync::{Mutex, mpsc, oneshot},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::protocol::{
    GatewayCapabilityV5, GetDeviceInfoResultV5, MAX_GATEWAY_OUTBOUND_QUEUE,
    MAX_GATEWAY_REQUEST_TTL_MS, PolicyRevocationCodeV5, RemoteRequestActionV5,
    RemoteResponseResultV5,
};

pub(super) const OUTBOUND_QUEUE_CAPACITY: usize = MAX_GATEWAY_OUTBOUND_QUEUE;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayConnectionSnapshot {
    pub device_id: Uuid,
    pub connection_id: Uuid,
    pub generation: u64,
    pub connected_at: i64,
    pub last_pong_at: i64,
    pub client_version: String,
    pub capabilities: BTreeSet<GatewayCapabilityV5>,
}

pub(super) enum GatewayCommand {
    Request {
        generation: u64,
        issued_at: i64,
        expires_at: i64,
        deadline: Instant,
        action: RemoteRequestActionV5,
        response: oneshot::Sender<Result<RemoteResponseResultV5, GatewayRequestError>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum GatewayControl {
    Superseded {
        generation: u64,
    },
    PolicyRevoked {
        generation: u64,
        code: PolicyRevocationCodeV5,
    },
    SlowConsumer,
}

pub(super) struct GatewayRegistration {
    pub connection_id: Uuid,
    pub generation: u64,
    pub cancellation: CancellationToken,
    pub commands: mpsc::Receiver<GatewayCommand>,
    pub control: mpsc::Receiver<GatewayControl>,
}

struct GatewayConnectionEntry {
    snapshot: GatewayConnectionSnapshot,
    authorization_revision: String,
    cancellation: CancellationToken,
    commands: mpsc::Sender<GatewayCommand>,
    control: mpsc::Sender<GatewayControl>,
}

pub(super) struct GatewayRegistrationAuthorization {
    revision: String,
    invalidation_generation: u64,
}

impl GatewayRegistrationAuthorization {
    pub(super) fn new(revision: String, invalidation_generation: u64) -> Self {
        Self {
            revision,
            invalidation_generation,
        }
    }
}

#[derive(Default)]
struct RegistryState {
    active: std::collections::HashMap<Uuid, GatewayConnectionEntry>,
    generations: std::collections::HashMap<Uuid, u64>,
    invalidation_generations: std::collections::HashMap<Uuid, u64>,
}

#[derive(Clone, Default)]
pub struct GatewayRegistry {
    inner: Arc<Mutex<RegistryState>>,
}

impl GatewayRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub(super) async fn register(
        &self,
        device_id: Uuid,
        connected_at: i64,
        client_version: String,
        capabilities: BTreeSet<GatewayCapabilityV5>,
        authorization: GatewayRegistrationAuthorization,
        shutdown: &CancellationToken,
    ) -> Result<GatewayRegistration, GatewayRegistryError> {
        let connection_id = Uuid::new_v4();
        let cancellation = shutdown.child_token();
        let (commands_tx, commands_rx) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
        let (control_tx, control_rx) = mpsc::channel(4);
        let old = {
            let mut state = self.inner.lock().await;
            let current_invalidation_generation = state
                .invalidation_generations
                .get(&device_id)
                .copied()
                .unwrap_or_default();
            if current_invalidation_generation != authorization.invalidation_generation {
                return Err(GatewayRegistryError::PolicyInvalidated);
            }
            let current = state.generations.entry(device_id).or_insert(0);
            *current = current
                .checked_add(1)
                .ok_or(GatewayRegistryError::GenerationExhausted)?;
            let generation = *current;
            let snapshot = GatewayConnectionSnapshot {
                device_id,
                connection_id,
                generation,
                connected_at,
                last_pong_at: connected_at,
                client_version,
                capabilities,
            };
            let old = state.active.insert(
                device_id,
                GatewayConnectionEntry {
                    snapshot,
                    authorization_revision: authorization.revision,
                    cancellation: cancellation.clone(),
                    commands: commands_tx,
                    control: control_tx,
                },
            );
            Ok::<_, GatewayRegistryError>((generation, old))
        };
        let (generation, old_entry) = old?;
        if let Some(old_entry) = old_entry {
            let _ = old_entry
                .control
                .try_send(GatewayControl::Superseded { generation });
            old_entry.cancellation.cancel();
        }
        Ok(GatewayRegistration {
            connection_id,
            generation,
            cancellation,
            commands: commands_rx,
            control: control_rx,
        })
    }

    pub(super) async fn cleanup(
        &self,
        device_id: Uuid,
        connection_id: Uuid,
        generation: u64,
    ) -> bool {
        let mut state = self.inner.lock().await;
        let is_current = state.active.get(&device_id).is_some_and(|entry| {
            entry.snapshot.connection_id == connection_id && entry.snapshot.generation == generation
        });
        if is_current {
            state.active.remove(&device_id);
        }
        is_current
    }

    pub(super) async fn mark_pong(
        &self,
        device_id: Uuid,
        connection_id: Uuid,
        generation: u64,
        at: i64,
    ) -> bool {
        let mut state = self.inner.lock().await;
        let Some(entry) = state.active.get_mut(&device_id) else {
            return false;
        };
        if entry.snapshot.connection_id != connection_id || entry.snapshot.generation != generation
        {
            return false;
        }
        entry.snapshot.last_pong_at = entry.snapshot.last_pong_at.max(at);
        true
    }

    pub async fn connection(&self, device_id: Uuid) -> Option<GatewayConnectionSnapshot> {
        self.inner
            .lock()
            .await
            .active
            .get(&device_id)
            .map(|entry| entry.snapshot.clone())
    }

    pub(super) async fn invalidation_generation(&self, device_id: Uuid) -> u64 {
        self.inner
            .lock()
            .await
            .invalidation_generations
            .get(&device_id)
            .copied()
            .unwrap_or_default()
    }

    /// Immediately removes and cancels the current connection for a device.
    ///
    /// The invalidation generation also fences a WebSocket handshake that read
    /// authorization before the corresponding lifecycle mutation committed but
    /// had not yet registered its session.
    pub async fn invalidate_device(
        &self,
        device_id: Uuid,
        reason: GatewayInvalidationReason,
    ) -> bool {
        let entry = {
            let mut state = self.inner.lock().await;
            let generation = state.invalidation_generations.entry(device_id).or_insert(0);
            *generation = generation.saturating_add(1);
            state.active.remove(&device_id)
        };
        let Some(entry) = entry else {
            return false;
        };
        let _ = entry.control.try_send(GatewayControl::PolicyRevoked {
            generation: entry.snapshot.generation,
            code: reason.policy_code(),
        });
        entry.cancellation.cancel();
        true
    }

    #[cfg(test)]
    pub(crate) async fn register_test_connection(&self, device_id: Uuid) -> CancellationToken {
        let invalidation_generation = self.invalidation_generation(device_id).await;
        self.register(
            device_id,
            unix_timestamp_ms(),
            "test-client".to_owned(),
            BTreeSet::new(),
            GatewayRegistrationAuthorization::new(
                "test-authorization-revision".to_owned(),
                invalidation_generation,
            ),
            &CancellationToken::new(),
        )
        .await
        .expect("test gateway registration")
        .cancellation
    }

    /// Returns a point-in-time view of active gateway connections.
    ///
    /// The registry lock only protects cloning the snapshots. Sorting happens
    /// after the lock is released so a slow consumer of this read path cannot
    /// delay gateway registration, cleanup, or heartbeat processing.
    pub async fn connections(&self) -> Vec<GatewayConnectionSnapshot> {
        let mut connections = {
            let state = self.inner.lock().await;
            state
                .active
                .values()
                .map(|entry| entry.snapshot.clone())
                .collect::<Vec<_>>()
        };
        connections.sort_by_key(|connection| connection.device_id);
        connections
    }

    pub async fn request_device_info(
        &self,
        device_id: Uuid,
    ) -> Result<GetDeviceInfoResultV5, GatewayRequestError> {
        match self
            .dispatch(device_id, RemoteRequestActionV5::GetDeviceInfo, None)
            .await?
        {
            RemoteResponseResultV5::GetDeviceInfo(result) => Ok(result),
            _ => Err(GatewayRequestError::ProtocolMismatch),
        }
    }

    pub(super) async fn request_job_authorized(
        &self,
        device_id: Uuid,
        action: RemoteRequestActionV5,
        authorization_revision: &str,
    ) -> Result<RemoteResponseResultV5, GatewayRequestError> {
        self.dispatch(device_id, action, Some(authorization_revision))
            .await
    }

    pub(super) async fn request_catalog_authorized(
        &self,
        device_id: Uuid,
        action: RemoteRequestActionV5,
        authorization_revision: &str,
    ) -> Result<RemoteResponseResultV5, GatewayRequestError> {
        self.dispatch(device_id, action, Some(authorization_revision))
            .await
    }

    pub(super) async fn request_control_authorized(
        &self,
        device_id: Uuid,
        action: RemoteRequestActionV5,
        authorization_revision: &str,
    ) -> Result<RemoteResponseResultV5, GatewayRequestError> {
        self.dispatch(device_id, action, Some(authorization_revision))
            .await
    }

    async fn dispatch(
        &self,
        device_id: Uuid,
        action: RemoteRequestActionV5,
        authorization_revision: Option<&str>,
    ) -> Result<RemoteResponseResultV5, GatewayRequestError> {
        let issued_at = unix_timestamp_ms();
        let expires_at = issued_at.saturating_add(MAX_GATEWAY_REQUEST_TTL_MS);
        let deadline = Instant::now() + Duration::from_millis(MAX_GATEWAY_REQUEST_TTL_MS as u64);
        let (command, response) = {
            let state = self.inner.lock().await;
            let entry = state
                .active
                .get(&device_id)
                .ok_or(GatewayRequestError::Offline)?;
            if authorization_revision
                .is_some_and(|revision| revision != entry.authorization_revision)
            {
                return Err(GatewayRequestError::Unauthorized);
            }
            if !GatewayCapabilityV5::supports_action(&entry.snapshot.capabilities, &action) {
                return Err(GatewayRequestError::UnsupportedCapability);
            }
            let (response_tx, response_rx) = oneshot::channel();
            (
                (
                    entry.commands.clone(),
                    entry.control.clone(),
                    entry.cancellation.clone(),
                    GatewayCommand::Request {
                        generation: entry.snapshot.generation,
                        issued_at,
                        expires_at,
                        deadline,
                        action,
                        response: response_tx,
                    },
                ),
                response_rx,
            )
        };
        let (sender, control, cancellation, command) = command;
        match sender.try_send(command) {
            Ok(()) => tokio::time::timeout_at(deadline, response)
                .await
                .map_err(|_| GatewayRequestError::Expired)?
                .unwrap_or(Err(GatewayRequestError::Disconnected)),
            Err(mpsc::error::TrySendError::Full(_)) => {
                let _ = control.try_send(GatewayControl::SlowConsumer);
                cancellation.cancel();
                Err(GatewayRequestError::SlowConsumer)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(GatewayRequestError::Disconnected),
        }
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub(super) enum GatewayRegistryError {
    #[error("gateway generation space was exhausted")]
    GenerationExhausted,
    #[error("gateway authorization was invalidated during registration")]
    PolicyInvalidated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayInvalidationReason {
    TokenRotated,
    DeviceDisabled,
    DeviceRevoked,
}

impl GatewayInvalidationReason {
    fn policy_code(self) -> PolicyRevocationCodeV5 {
        match self {
            Self::TokenRotated => PolicyRevocationCodeV5::TokenRotated,
            Self::DeviceDisabled => PolicyRevocationCodeV5::DeviceDisabled,
            Self::DeviceRevoked => PolicyRevocationCodeV5::DeviceRevoked,
        }
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum GatewayRequestError {
    #[error("gateway request is outside the closed read contract")]
    InvalidRequest,
    #[error("gateway device is offline")]
    Offline,
    #[error("gateway connection was superseded or disconnected")]
    Disconnected,
    #[error("gateway pending request limit was reached")]
    PendingLimit,
    #[error("gateway request expired")]
    Expired,
    #[error("gateway response was rejected")]
    Rejected,
    #[error("gateway authorization is no longer current")]
    Unauthorized,
    #[error("gateway authorization could not be revalidated")]
    AuthorizationUnavailable,
    #[error("gateway client did not negotiate the required capability")]
    UnsupportedCapability,
    #[error("gateway connection is a slow consumer")]
    SlowConsumer,
    #[error("gateway response did not match the requested action")]
    ProtocolMismatch,
}

fn unix_timestamp_ms() -> i64 {
    let millis = std::time::SystemTime::UNIX_EPOCH
        .elapsed()
        .map_or(0, |elapsed| elapsed.as_millis());
    i64::try_from(millis).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn register(registry: &GatewayRegistry, device_id: Uuid) -> GatewayRegistration {
        let invalidation_generation = registry.invalidation_generation(device_id).await;
        registry
            .register(
                device_id,
                1_000,
                "test-client".to_owned(),
                BTreeSet::from([GatewayCapabilityV5::DeviceInfoV5]),
                GatewayRegistrationAuthorization::new(
                    "revision-1".to_owned(),
                    invalidation_generation,
                ),
                &CancellationToken::new(),
            )
            .await
            .expect("gateway registration")
    }

    #[tokio::test]
    async fn generations_survive_removal_and_late_cleanup_cannot_remove_new_connection() {
        let registry = GatewayRegistry::new();
        let device_id = Uuid::new_v4();
        let first = register(&registry, device_id).await;
        assert_eq!(first.generation, 1);
        assert!(
            registry
                .cleanup(device_id, first.connection_id, first.generation)
                .await
        );

        let second = register(&registry, device_id).await;
        assert_eq!(second.generation, 2);
        assert!(
            !registry
                .cleanup(device_id, first.connection_id, first.generation)
                .await
        );
        let current = registry
            .connection(device_id)
            .await
            .expect("current connection");
        assert_eq!(current.connection_id, second.connection_id);
        assert_eq!(current.generation, second.generation);

        assert!(
            registry
                .cleanup(device_id, second.connection_id, second.generation)
                .await
        );
        let third = register(&registry, device_id).await;
        assert_eq!(third.generation, 3);
    }

    #[tokio::test]
    async fn replacement_is_installed_before_old_connection_is_cancelled() {
        let registry = GatewayRegistry::new();
        let device_id = Uuid::new_v4();
        let mut first = register(&registry, device_id).await;
        let second = register(&registry, device_id).await;

        assert!(first.cancellation.is_cancelled());
        assert_eq!(
            first.control.recv().await,
            Some(GatewayControl::Superseded {
                generation: second.generation,
            })
        );
        let current = registry
            .connection(device_id)
            .await
            .expect("new connection");
        assert_eq!(current.connection_id, second.connection_id);
    }

    #[tokio::test]
    async fn lifecycle_invalidation_removes_cancels_and_fences_stale_registration() {
        let registry = GatewayRegistry::new();
        let device_id = Uuid::new_v4();
        let stale_invalidation_generation = registry.invalidation_generation(device_id).await;
        let mut registration = register(&registry, device_id).await;

        assert!(
            registry
                .invalidate_device(device_id, GatewayInvalidationReason::DeviceDisabled)
                .await
        );
        assert_eq!(registry.connection(device_id).await, None);
        assert!(registration.cancellation.is_cancelled());
        assert_eq!(
            registration.control.recv().await,
            Some(GatewayControl::PolicyRevoked {
                generation: registration.generation,
                code: PolicyRevocationCodeV5::DeviceDisabled,
            })
        );

        let mut rotated = register(&registry, device_id).await;
        assert!(
            registry
                .invalidate_device(device_id, GatewayInvalidationReason::TokenRotated)
                .await
        );
        assert_eq!(
            rotated.control.recv().await,
            Some(GatewayControl::PolicyRevoked {
                generation: rotated.generation,
                code: PolicyRevocationCodeV5::TokenRotated,
            })
        );

        let stale = registry
            .register(
                device_id,
                2_000,
                "stale-client".to_owned(),
                BTreeSet::new(),
                GatewayRegistrationAuthorization::new(
                    "stale-revision".to_owned(),
                    stale_invalidation_generation,
                ),
                &CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            stale,
            Err(GatewayRegistryError::PolicyInvalidated)
        ));
        assert_eq!(registry.connection(device_id).await, None);
    }

    #[tokio::test]
    async fn connections_are_snapshots_sorted_by_device_id() {
        let registry = GatewayRegistry::new();
        let later = Uuid::from_u128(2);
        let earlier = Uuid::from_u128(1);
        let later_registration = register(&registry, later).await;
        let earlier_registration = register(&registry, earlier).await;

        let connections = registry.connections().await;
        assert_eq!(
            connections
                .iter()
                .map(|connection| connection.device_id)
                .collect::<Vec<_>>(),
            vec![earlier, later]
        );
        assert_eq!(
            connections[0].connection_id,
            earlier_registration.connection_id
        );
        assert_eq!(
            connections[1].connection_id,
            later_registration.connection_id
        );
    }

    #[tokio::test]
    async fn full_outbound_queue_cancels_the_slow_consumer_without_blocking() {
        let registry = GatewayRegistry::new();
        let device_id = Uuid::new_v4();
        let mut registration = register(&registry, device_id).await;
        let mut waiting = Vec::new();
        for _ in 0..OUTBOUND_QUEUE_CAPACITY {
            let registry = registry.clone();
            waiting.push(tokio::spawn(async move {
                registry.request_device_info(device_id).await
            }));
        }
        for _ in 0..16 {
            if registration.commands.len() == OUTBOUND_QUEUE_CAPACITY {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(registration.commands.len(), OUTBOUND_QUEUE_CAPACITY);

        assert_eq!(
            registry.request_device_info(device_id).await,
            Err(GatewayRequestError::SlowConsumer)
        );
        assert!(registration.cancellation.is_cancelled());
        assert_eq!(
            registration.control.recv().await,
            Some(GatewayControl::SlowConsumer)
        );
        for task in waiting {
            task.abort();
        }
    }

    #[tokio::test]
    async fn requests_are_not_dispatched_without_the_negotiated_capability() {
        let registry = GatewayRegistry::new();
        let device_id = Uuid::new_v4();
        let registration = registry
            .register(
                device_id,
                1_000,
                "presence-only".to_owned(),
                BTreeSet::new(),
                GatewayRegistrationAuthorization::new(
                    "revision-1".to_owned(),
                    registry.invalidation_generation(device_id).await,
                ),
                &CancellationToken::new(),
            )
            .await
            .expect("gateway registration");
        assert_eq!(
            registry.request_device_info(device_id).await,
            Err(GatewayRequestError::UnsupportedCapability)
        );
        assert!(registration.commands.is_empty());
    }

    #[tokio::test]
    async fn job_dispatch_requires_current_revision_and_captures_generation() {
        let registry = GatewayRegistry::new();
        let device_id = Uuid::new_v4();
        let mut registration = registry
            .register(
                device_id,
                1_000,
                "job-client".to_owned(),
                BTreeSet::from([GatewayCapabilityV5::RemoteRunsReadV2]),
                GatewayRegistrationAuthorization::new(
                    "revision-1".to_owned(),
                    registry.invalidation_generation(device_id).await,
                ),
                &CancellationToken::new(),
            )
            .await
            .expect("gateway registration");
        assert_eq!(
            registry
                .request_job_authorized(
                    device_id,
                    RemoteRequestActionV5::ListRuns {
                        filter: crate::gateway::RemoteRunFilterV5::All,
                        page_size: 10,
                        cursor: None,
                    },
                    "revision-2",
                )
                .await,
            Err(GatewayRequestError::Unauthorized)
        );
        assert!(registration.commands.is_empty());

        let request = tokio::spawn({
            let registry = registry.clone();
            async move {
                registry
                    .request_job_authorized(
                        device_id,
                        RemoteRequestActionV5::ListRuns {
                            filter: crate::gateway::RemoteRunFilterV5::All,
                            page_size: 10,
                            cursor: None,
                        },
                        "revision-1",
                    )
                    .await
            }
        });
        let command = registration.commands.recv().await.expect("job command");
        let GatewayCommand::Request {
            generation,
            action,
            response,
            ..
        } = command;
        assert_eq!(generation, registration.generation);
        assert_eq!(
            action,
            RemoteRequestActionV5::ListRuns {
                filter: crate::gateway::RemoteRunFilterV5::All,
                page_size: 10,
                cursor: None,
            }
        );
        response
            .send(Ok(RemoteResponseResultV5::ListRuns(
                crate::gateway::RemoteRunPageV5 {
                    items: Vec::new(),
                    next_cursor: None,
                },
            )))
            .expect("job response receiver");
        assert!(matches!(
            request.await.expect("job request task"),
            Ok(RemoteResponseResultV5::ListRuns(_))
        ));
    }
}
