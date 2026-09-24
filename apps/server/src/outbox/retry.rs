use super::{
    ChannelOutcome, ClaimedNotification, OutboxError, OutboxService, RetryClass,
    repository::{database_error, unix_timestamp_ms},
};

pub(super) const NETWORK_RETRY_MS: [i64; 5] = [5_000, 30_000, 120_000, 600_000, 1_800_000];
pub(super) const RATE_LIMIT_RETRY_MS: [i64; 3] = [30_000, 120_000, 600_000];

impl OutboxService {
    pub async fn unblock_activation(&self) -> Result<u64, OutboxError> {
        self.unblock_at("blocked_activation", unix_timestamp_ms()?)
            .await
    }

    pub async fn unblock_reconnect(&self) -> Result<u64, OutboxError> {
        self.unblock_at("blocked_reconnect", unix_timestamp_ms()?)
            .await
    }

    pub(super) async fn unblock_at(&self, status: &str, now: i64) -> Result<u64, OutboxError> {
        let result = sqlx::query(
            "UPDATE notification_outbox
             SET status = 'retry_wait', next_attempt_at = ?1,
                 last_error_code = NULL, last_error_message = NULL, updated_at = ?1
             WHERE status = ?2 AND expires_at > ?1",
        )
        .bind(now)
        .bind(status)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        let bundle_rows = self.unblock_bundle_segments_at(status, now).await?;
        if result.rows_affected() > 0 || bundle_rows > 0 {
            self.wake.notify_one();
        }
        Ok(result.rows_affected() + bundle_rows)
    }

    pub(super) async fn finish_claim_at(
        &self,
        claim: &ClaimedNotification,
        outcome: ChannelOutcome,
        now: i64,
    ) -> Result<bool, OutboxError> {
        if let ChannelOutcome::Accepted {
            provider_message_id,
        } = outcome
        {
            let result = sqlx::query(
                "UPDATE notification_outbox
                 SET status = 'provider_accepted', provider_message_id = ?1,
                     provider_accepted_at = ?2, claim_token = NULL, claimed_at = NULL,
                     next_attempt_at = NULL, last_error_code = NULL,
                     last_error_message = NULL,
                     body = CASE WHEN sensitive_body = 1 THEN '[REDACTED]' ELSE body END,
                     updated_at = ?2
                 WHERE id = ?3 AND status = 'sending_channel' AND claim_token = ?4",
            )
            .bind(provider_message_id)
            .bind(now)
            .bind(&claim.row_id)
            .bind(&claim.claim_token)
            .execute(&self.pool)
            .await
            .map_err(database_error)?;
            let applied = result.rows_affected() == 1;
            if applied {
                self.checkpoint_after_terminal_redaction().await;
            }
            return Ok(applied);
        }

        let (mut status, mut next_attempt_at, mut error_code) = if now >= claim.expires_at {
            ("expired", None, None)
        } else {
            failure_transition(&outcome, claim.attempt_count, now)
        };
        if next_attempt_at.is_some_and(|next| next >= claim.expires_at) {
            status = "expired";
            next_attempt_at = None;
            error_code = None;
        }
        let result = sqlx::query(
            "UPDATE notification_outbox
             SET status = ?1, next_attempt_at = ?2, last_error_code = ?3,
                 last_error_message = NULL, claim_token = NULL, claimed_at = NULL,
                 body = CASE WHEN sensitive_body = 1 AND ?1 IN ('expired', 'dead_letter', 'cancelled')
                     THEN '[REDACTED]' ELSE body END,
                 updated_at = ?4
             WHERE id = ?5 AND status = 'sending_channel' AND claim_token = ?6",
        )
        .bind(status)
        .bind(next_attempt_at)
        .bind(error_code)
        .bind(now)
        .bind(&claim.row_id)
        .bind(&claim.claim_token)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        let applied = result.rows_affected() == 1;
        if applied && matches!(status, "expired" | "dead_letter" | "cancelled") {
            self.checkpoint_after_terminal_redaction().await;
        }
        Ok(applied)
    }
}

pub(super) fn failure_transition(
    outcome: &ChannelOutcome,
    attempt_count: i64,
    now: i64,
) -> (&'static str, Option<i64>, Option<&str>) {
    match outcome {
        ChannelOutcome::Retryable { class } => {
            let retry = match class {
                RetryClass::Network => retry_delay(&NETWORK_RETRY_MS, attempt_count),
                RetryClass::RateLimited => retry_delay(&RATE_LIMIT_RETRY_MS, attempt_count),
                RetryClass::ContextChanged => (attempt_count <= 1).then_some(0),
                RetryClass::ContextRejected | RetryClass::AmbiguousMinusTwo => {
                    (attempt_count <= 1).then_some(30_000)
                }
            };
            match retry {
                Some(delay) => ("retry_wait", Some(now + delay), Some(class.error_code())),
                None if matches!(
                    class,
                    RetryClass::ContextChanged
                        | RetryClass::ContextRejected
                        | RetryClass::AmbiguousMinusTwo
                ) =>
                {
                    ("blocked_activation", None, Some(class.error_code()))
                }
                None => ("dead_letter", None, Some(class.error_code())),
            }
        }
        ChannelOutcome::BlockedActivation => {
            ("blocked_activation", None, Some("ACTIVATION_REQUIRED"))
        }
        ChannelOutcome::BlockedReconnect => ("blocked_reconnect", None, Some("RECONNECT_REQUIRED")),
        ChannelOutcome::BlockedTargetChanged => {
            ("dead_letter", None, Some("TARGET_ACCOUNT_CHANGED"))
        }
        ChannelOutcome::PermanentFailure => ("dead_letter", None, Some("PERMANENT_FAILURE")),
        ChannelOutcome::Cancelled => ("cancelled", None, None),
        ChannelOutcome::Accepted { .. } => {
            unreachable!("non-failure outcome handled before transition")
        }
    }
}

fn retry_delay(schedule: &[i64], attempt_count: i64) -> Option<i64> {
    let index = usize::try_from(attempt_count.saturating_sub(1)).ok()?;
    schedule.get(index).copied()
}
