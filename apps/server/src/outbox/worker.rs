use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::{
    ChannelMessage, ChannelOutcome, OutboxError, OutboxService, repository::unix_timestamp_ms,
};

const IDLE_POLL: Duration = Duration::from_millis(250);

#[async_trait]
pub trait NotificationChannel: Send + Sync + 'static {
    async fn send(
        &self,
        message: &ChannelMessage,
        cancellation: CancellationToken,
    ) -> ChannelOutcome;
}

#[derive(Default)]
pub struct FakeChannel {
    sent: AtomicUsize,
}

impl FakeChannel {
    pub fn sent_count(&self) -> usize {
        self.sent.load(Ordering::Acquire)
    }
}

#[async_trait]
impl NotificationChannel for FakeChannel {
    async fn send(
        &self,
        message: &ChannelMessage,
        _cancellation: CancellationToken,
    ) -> ChannelOutcome {
        self.sent.fetch_add(1, Ordering::AcqRel);
        ChannelOutcome::Accepted {
            provider_message_id: Some(format!("fake:{}", message.client_id)),
        }
    }
}

pub struct OutboxWorker {
    service: OutboxService,
    channel: Arc<dyn NotificationChannel>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerPass {
    Idle,
    Applied,
    Interrupted,
}

impl OutboxWorker {
    pub fn new(service: OutboxService, channel: Arc<dyn NotificationChannel>) -> Self {
        Self { service, channel }
    }

    pub async fn run(self, cancellation: CancellationToken) -> Result<(), OutboxError> {
        self.run_with_progress(cancellation, || {}).await
    }

    pub(crate) async fn run_with_progress<F>(
        self,
        cancellation: CancellationToken,
        completed_pass: F,
    ) -> Result<(), OutboxError>
    where
        F: Fn() + Send + 'static,
    {
        // Startup can race with other database-backed workers. `OutboxError`
        // intentionally closes SQLite causes into `Database`, so use only a
        // bounded retry here; exhausting it and every non-database error still
        // remains a critical worker failure.
        recover_startup(|| self.service.recover_stale_claims()).await?;
        loop {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            match self.run_pass(cancellation.clone()).await? {
                WorkerPass::Applied => {
                    completed_pass();
                    continue;
                }
                WorkerPass::Idle => completed_pass(),
                WorkerPass::Interrupted if cancellation.is_cancelled() => return Ok(()),
                WorkerPass::Interrupted => {}
            }
            tokio::select! {
                () = cancellation.cancelled() => return Ok(()),
                () = self.service.wake.notified() => {},
                () = tokio::time::sleep(IDLE_POLL) => {},
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn run_once(
        &self,
        cancellation: CancellationToken,
    ) -> Result<bool, OutboxError> {
        self.run_pass(cancellation)
            .await
            .map(|pass| pass == WorkerPass::Applied)
    }

    async fn run_pass(&self, cancellation: CancellationToken) -> Result<WorkerPass, OutboxError> {
        let now = unix_timestamp_ms()?;
        if let Some(claim) = self.service.claim_next_at(now).await? {
            let body = if claim.kind == "result_link" {
                let Some(body) = self
                    .service
                    .protected_result_link_body(&claim.row_id, now)
                    .await?
                else {
                    let _ = self
                        .service
                        .finish_claim_at(&claim, ChannelOutcome::PermanentFailure, now)
                        .await?;
                    return Ok(WorkerPass::Applied);
                };
                body
            } else {
                claim.body.clone()
            };
            let message = ChannelMessage {
                client_id: stable_client_id(&claim.origin_key, &claim.dedupe_key),
                body,
                correlation_key: claim.correlation_key.clone(),
                target_account_fingerprint: claim.target_account_fingerprint.clone(),
            };
            let outcome = tokio::select! {
                biased;
                outcome = self.channel.send(&message, cancellation.clone()) => outcome,
                () = cancellation.cancelled() => {
                    let _ = self.service.release_claim_at(&claim, unix_timestamp_ms()?).await?;
                    return Ok(WorkerPass::Interrupted);
                }
            };
            if matches!(outcome, ChannelOutcome::Cancelled) {
                let _ = self
                    .service
                    .release_claim_at(&claim, unix_timestamp_ms()?)
                    .await?;
                return Ok(WorkerPass::Interrupted);
            }
            let applied = self
                .service
                .finish_claim_at(&claim, outcome, unix_timestamp_ms()?)
                .await?;
            return Ok(if applied {
                WorkerPass::Applied
            } else {
                WorkerPass::Interrupted
            });
        }

        // Results are the sole network-delivered publication format. Legacy
        // bundle rows are retained only as historical database data and never
        // enter the worker scheduler.
        Ok(if cancellation.is_cancelled() {
            WorkerPass::Interrupted
        } else {
            WorkerPass::Idle
        })
    }
}

async fn recover_startup<F, Fut>(mut recover: F) -> Result<(), OutboxError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<u64, OutboxError>>,
{
    for attempt in 0..4 {
        match recover().await {
            Ok(_) => return Ok(()),
            Err(OutboxError::Database) if attempt < 3 => {
                tokio::time::sleep(Duration::from_millis(20 * (attempt + 1))).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded startup recovery always returns")
}

pub(super) fn stable_client_id(origin_key: &str, dedupe_key: &str) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("promptdock-relay/provider-client-id/v1");
    frame(&mut hasher, origin_key.as_bytes());
    frame(&mut hasher, dedupe_key.as_bytes());
    let hash = hasher.finalize().to_hex();
    format!("promptdock-relay-{}", &hash.as_str()[..32])
}

fn frame(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[tokio::test]
    async fn startup_recovery_retries_database_only_and_exhaustion_is_an_error() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let retry_attempts = attempts.clone();
        recover_startup(move || {
            let attempt = retry_attempts.fetch_add(1, Ordering::AcqRel);
            async move {
                if attempt < 2 {
                    Err(OutboxError::Database)
                } else {
                    Ok(0)
                }
            }
        })
        .await
        .expect("third recovery succeeds");
        assert_eq!(attempts.load(Ordering::Acquire), 3);

        let exhausted = Arc::new(AtomicUsize::new(0));
        let exhausted_attempts = exhausted.clone();
        let result = recover_startup(move || {
            exhausted_attempts.fetch_add(1, Ordering::AcqRel);
            async { Err(OutboxError::Database) }
        })
        .await;
        assert!(matches!(result, Err(OutboxError::Database)));
        assert_eq!(exhausted.load(Ordering::Acquire), 4);
    }
}
