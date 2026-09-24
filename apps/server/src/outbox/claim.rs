use sqlx::Row as _;
use uuid::Uuid;

use super::{ClaimedNotification, OutboxError, OutboxService, repository::database_error};

pub(super) const STALE_CLAIM_MS: i64 = 5 * 60 * 1_000;

impl OutboxService {
    pub(super) async fn claim_next_at(
        &self,
        now: i64,
    ) -> Result<Option<ClaimedNotification>, OutboxError> {
        self.expire_due_at(now).await?;
        let claim_token = Uuid::new_v4().to_string();
        let row = sqlx::query(
            "UPDATE notification_outbox
             SET status = 'sending_channel', claim_token = ?1, claimed_at = ?2,
                 attempt_count = attempt_count + 1, updated_at = ?2
             WHERE id = (
                 SELECT id FROM notification_outbox
                 WHERE (
                       (status = 'pending_channel' AND not_before <= ?2)
                       OR (status = 'retry_wait' AND next_attempt_at IS NOT NULL
                           AND next_attempt_at <= ?2)
                   )
                   AND expires_at > ?2
                 ORDER BY priority DESC,
                          COALESCE(next_attempt_at, not_before) ASC,
                          created_at ASC, id ASC
                 LIMIT 1
             )
             AND status IN ('pending_channel', 'retry_wait')
             RETURNING id, origin_key, target_account_fingerprint,
                       notification_id, dedupe_key, kind, title,
                       body, correlation_key, priority, expires_at, attempt_count,
                       claim_token",
        )
        .bind(&claim_token)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        row.map(|row| claimed_from_row(&row)).transpose()
    }

    async fn expire_due_at(&self, now: i64) -> Result<u64, OutboxError> {
        let expired = sqlx::query(
            "UPDATE notification_outbox
             SET status = 'expired', claim_token = NULL, claimed_at = NULL,
                 next_attempt_at = NULL,
                 body = CASE WHEN sensitive_body = 1 THEN '[REDACTED]' ELSE body END,
                 updated_at = ?1
             WHERE expires_at <= ?1
               AND status IN (
                   'pending_channel', 'retry_wait', 'blocked_activation',
                   'blocked_reconnect'
               )",
        )
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(database_error)?
        .rows_affected();
        if expired > 0 {
            self.checkpoint_after_terminal_redaction().await;
        }
        Ok(expired)
    }

    pub async fn recover_stale_claims(&self) -> Result<u64, OutboxError> {
        let now = super::repository::unix_timestamp_ms()?;
        self.recover_stale_claims_at(now).await
    }

    pub(super) async fn recover_stale_claims_at(&self, now: i64) -> Result<u64, OutboxError> {
        let expired = sqlx::query(
            "UPDATE notification_outbox
             SET status = 'expired', claim_token = NULL, claimed_at = NULL,
                 next_attempt_at = NULL,
                 body = CASE WHEN sensitive_body = 1 THEN '[REDACTED]' ELSE body END,
                 updated_at = ?1
             WHERE status = 'sending_channel' AND expires_at <= ?1",
        )
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(database_error)?
        .rows_affected();
        if expired > 0 {
            self.checkpoint_after_terminal_redaction().await;
        }
        let retryable = sqlx::query(
            "UPDATE notification_outbox
             SET status = 'retry_wait', claim_token = NULL, claimed_at = NULL,
                 next_attempt_at = ?1, last_error_code = 'STALE_CLAIM_RECOVERED',
                 last_error_message = NULL, updated_at = ?1
             WHERE status = 'sending_channel' AND expires_at > ?1
               AND (claimed_at IS NULL OR claimed_at < ?2)",
        )
        .bind(now)
        .bind(now - STALE_CLAIM_MS)
        .execute(&self.pool)
        .await
        .map_err(database_error)?
        .rows_affected();
        if retryable > 0 {
            self.wake.notify_one();
        }
        // Bundle creation and segment delivery are retired.  Startup recovery
        // must only touch the active notification outbox; otherwise an
        // historical bundle row can make the critical worker fail before the
        // server becomes ready.
        Ok(expired + retryable)
    }

    pub(super) async fn release_claim_at(
        &self,
        claim: &ClaimedNotification,
        now: i64,
    ) -> Result<bool, OutboxError> {
        let changed = sqlx::query(
            "UPDATE notification_outbox
             SET status = CASE WHEN expires_at <= ?1 THEN 'expired' ELSE 'retry_wait' END,
                 next_attempt_at = CASE WHEN expires_at <= ?1 THEN NULL ELSE ?1 END,
                 attempt_count = CASE WHEN attempt_count > 0 THEN attempt_count - 1 ELSE 0 END,
                 claim_token = NULL, claimed_at = NULL,
                  last_error_code = 'SEND_CANCELLED', last_error_message = NULL,
                  body = CASE WHEN sensitive_body = 1 AND expires_at <= ?1 THEN '[REDACTED]' ELSE body END,
                  updated_at = ?1
             WHERE id = ?2 AND status = 'sending_channel' AND claim_token = ?3",
        )
        .bind(now)
        .bind(&claim.row_id)
        .bind(&claim.claim_token)
        .execute(&self.pool)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed == 1 && claim.expires_at <= now {
            self.checkpoint_after_terminal_redaction().await;
        }
        Ok(changed == 1)
    }
}

fn claimed_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ClaimedNotification, OutboxError> {
    Ok(ClaimedNotification {
        row_id: row.try_get("id").map_err(database_error)?,
        claim_token: row.try_get("claim_token").map_err(database_error)?,
        origin_key: row.try_get("origin_key").map_err(database_error)?,
        target_account_fingerprint: row
            .try_get("target_account_fingerprint")
            .map_err(database_error)?,
        notification_id: row.try_get("notification_id").map_err(database_error)?,
        dedupe_key: row.try_get("dedupe_key").map_err(database_error)?,
        kind: row.try_get("kind").map_err(database_error)?,
        title: row.try_get("title").map_err(database_error)?,
        body: row.try_get("body").map_err(database_error)?,
        correlation_key: row.try_get("correlation_key").map_err(database_error)?,
        priority: row.try_get("priority").map_err(database_error)?,
        expires_at: row.try_get("expires_at").map_err(database_error)?,
        attempt_count: row.try_get("attempt_count").map_err(database_error)?,
    })
}
