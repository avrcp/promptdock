#![forbid(unsafe_code)]

use async_trait::async_trait;
use relay_domain::{CapabilityFacet, SafeRunProjection};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimedDelivery {
    pub delivery_id: String,
    pub safe_title: String,
    pub safe_body: String,
    pub claim_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderReceipt {
    pub provider_message_id: String,
    pub accepted_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationError {
    pub code: &'static str,
}

#[async_trait]
pub trait OutboxRepository: Send + Sync {
    /// Claims and commits one row before returning. Implementations must not
    /// retain a database transaction in the returned value.
    async fn claim_next(&self, now: i64) -> Result<Option<ClaimedDelivery>, ApplicationError>;
    async fn record_provider_acceptance(
        &self,
        delivery: &ClaimedDelivery,
        receipt: &ProviderReceipt,
    ) -> Result<(), ApplicationError>;
    async fn record_provider_failure(
        &self,
        delivery: &ClaimedDelivery,
        error_code: &'static str,
        retry_at: Option<i64>,
    ) -> Result<(), ApplicationError>;
}

#[async_trait]
pub trait WechatProvider: Send + Sync {
    async fn send(&self, delivery: &ClaimedDelivery) -> Result<ProviderReceipt, ApplicationError>;
}

#[async_trait]
pub trait GatewayPort: Send + Sync {
    async fn run_projection(
        &self,
        opaque_handle: &str,
    ) -> Result<SafeRunProjection, ApplicationError>;
    async fn capability_facets(&self) -> Result<Vec<CapabilityFacet>, ApplicationError>;
    async fn stop_run(&self, opaque_handle: &str, command_id: &str)
    -> Result<(), ApplicationError>;
}

pub trait Clock: Send + Sync {
    fn unix_timestamp_ms(&self) -> i64;
}

pub struct DispatchNotification<R, P, C> {
    repository: R,
    provider: P,
    clock: C,
}

impl<R, P, C> DispatchNotification<R, P, C>
where
    R: OutboxRepository,
    P: WechatProvider,
    C: Clock,
{
    pub const fn new(repository: R, provider: P, clock: C) -> Self {
        Self {
            repository,
            provider,
            clock,
        }
    }

    /// Repository claim is committed before provider IO starts, and provider
    /// IO finishes before the result is recorded in a new transaction.
    pub async fn dispatch_one(&self) -> Result<bool, ApplicationError> {
        let Some(delivery) = self
            .repository
            .claim_next(self.clock.unix_timestamp_ms())
            .await?
        else {
            return Ok(false);
        };
        match self.provider.send(&delivery).await {
            Ok(receipt) => {
                self.repository
                    .record_provider_acceptance(&delivery, &receipt)
                    .await?
            }
            Err(error) => {
                self.repository
                    .record_provider_failure(&delivery, error.code, None)
                    .await?;
            }
        }
        Ok(true)
    }
}
