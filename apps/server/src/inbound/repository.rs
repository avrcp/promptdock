use std::{sync::Arc, time::SystemTime};

use sqlx::{Row as _, Sqlite, SqlitePool, Transaction, sqlite::SqliteRow};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    InboundAcceptOutcome, InboundCommandV5, InboundMessageError, InboundWorkerError,
    model::{
        ClaimedInboundCommand, ConfirmationActionKind, ControlDispatchOutcome,
        ControlDispatchRecord, PendingConfirmation, PendingJobPageSelection, RunFilter,
        SelectedCatalogContext, SelectionEntry, SelectionItemKind, SelectionLookupError,
        SelectionPageContext,
    },
};
use crate::outbox::{InteractiveReplyV1, OutboxService};

pub(super) const INBOUND_COMMAND_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
pub(super) const STALE_COMMAND_CLAIM_MS: i64 = 5 * 60 * 1_000;
pub(super) const SELECTION_TTL_MS: i64 = 15 * 60 * 1_000;
pub(super) const MAX_SELECTION_CONTEXTS: i64 = 512;
pub(super) const MAX_SELECTION_ENTRIES: usize = 10;
pub(super) const CONTROL_CONFIRMATION_TTL_MS: i64 = 2 * 60 * 1_000;

pub(super) struct DurableInboundCommand {
    pub message_key: String,
    pub sender_fingerprint: String,
    pub command: InboundCommandV5,
    pub command_json: String,
    pub payload_hash: String,
    pub received_at: i64,
    pub last_error_code: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConfirmationRecord {
    pub sender_fingerprint: String,
    pub confirmation_id: String,
    pub device_id: Uuid,
    pub action_kind: ConfirmationActionKind,
    pub runtime_handle: Option<String>,
    pub workspace_handle: Option<String>,
    pub profile_handle: Option<String>,
    pub preset_handle: Option<String>,
    pub run_handle: Option<String>,
    pub intent_id: String,
    pub expires_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConfirmationConsumeError {
    Missing,
    Expired,
    Replay,
    WrongScope,
}

#[derive(Clone)]
pub struct InboundCommandService {
    pool: SqlitePool,
    wake: Arc<Notify>,
}

impl InboundCommandService {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            wake: Arc::new(Notify::new()),
        }
    }

    pub(super) async fn accept(
        &self,
        durable: DurableInboundCommand,
        cancellation: &CancellationToken,
    ) -> Result<InboundAcceptOutcome, InboundMessageError> {
        self.accept_with_status(durable, "received", cancellation)
            .await
    }

    pub(super) async fn discard(
        &self,
        durable: DurableInboundCommand,
        cancellation: &CancellationToken,
    ) -> Result<InboundAcceptOutcome, InboundMessageError> {
        self.accept_with_status(durable, "expired", cancellation)
            .await
    }

    async fn accept_with_status(
        &self,
        durable: DurableInboundCommand,
        initial_status: &'static str,
        cancellation: &CancellationToken,
    ) -> Result<InboundAcceptOutcome, InboundMessageError> {
        if cancellation.is_cancelled() {
            return Err(InboundMessageError::Cancelled);
        }
        let expires_at = durable.received_at.saturating_add(INBOUND_COMMAND_TTL_MS);
        let mut transaction = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(InboundMessageError::Cancelled),
            result = self.pool.begin() => result.map_err(message_database_error)?,
        };
        let inserted = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(InboundMessageError::Cancelled),
            result = sqlx::query(
                "INSERT INTO inbound_commands (
                    message_key, sender_fingerprint, command_kind, command_json, payload_hash,
                    status, expires_at, attempt_count, last_error_code, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?9)
                 ON CONFLICT(message_key) DO NOTHING",
            )
            .bind(&durable.message_key)
            .bind(&durable.sender_fingerprint)
            .bind(durable.command.kind())
            .bind(&durable.command_json)
            .bind(&durable.payload_hash)
            .bind(initial_status)
            .bind(expires_at)
            .bind(durable.last_error_code)
            .bind(durable.received_at)
            .execute(&mut *transaction) => result.map_err(message_database_error)?.rows_affected(),
        };
        let outcome = if inserted == 1 {
            InboundAcceptOutcome::Fresh
        } else {
            let existing = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(InboundMessageError::Cancelled),
                result = sqlx::query(
                    "SELECT sender_fingerprint, payload_hash
                     FROM inbound_commands WHERE message_key = ?1",
                )
                .bind(&durable.message_key)
                .fetch_one(&mut *transaction) => result.map_err(message_database_error)?,
            };
            let same_sender = existing
                .try_get::<String, _>("sender_fingerprint")
                .map_err(message_database_error)?
                == durable.sender_fingerprint;
            let same_payload = existing
                .try_get::<String, _>("payload_hash")
                .map_err(message_database_error)?
                == durable.payload_hash;
            if !same_sender || !same_payload {
                transaction
                    .rollback()
                    .await
                    .map_err(message_database_error)?;
                return Err(InboundMessageError::IdempotencyConflict);
            }
            InboundAcceptOutcome::ExactReplay
        };
        if cancellation.is_cancelled() {
            transaction
                .rollback()
                .await
                .map_err(message_database_error)?;
            return Err(InboundMessageError::Cancelled);
        }
        transaction.commit().await.map_err(message_database_error)?;
        if outcome == InboundAcceptOutcome::Fresh && initial_status == "received" {
            self.wake.notify_one();
        }
        Ok(outcome)
    }

    /// Verify an ephemeral six-digit code against the one pending confirmation
    /// for this sender.  The input code is never serialized or stored.  A
    /// mismatch increments the per-sender credential attempt counter and locks
    /// that credential at its durable per-row attempt limit in the same
    /// transaction.
    pub(super) async fn resolve_confirmation_code(
        &self,
        sender_fingerprint: &str,
        code: &str,
        verifier: &super::ConfirmationVerifier,
        now: i64,
    ) -> Result<String, &'static str> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| "CONFIRMATION_UNAVAILABLE")?;
        let row = sqlx::query(
            "SELECT confirmation_id, confirmation_digest, confirmation_nonce, device_id,
                    action_kind, runtime_handle, workspace_handle,
                    harness_profile_handle, task_preset_handle, run_handle,
                    intent_id, expires_at, attempt_count, locked_at
             FROM control_confirmations
             WHERE sender_fingerprint = ?1 AND dispatch_state = 'awaiting_confirmation'",
        )
        .bind(sender_fingerprint)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| "CONFIRMATION_UNAVAILABLE")?;
        let Some(row) = row else {
            return Err("CONFIRMATION_MISSING");
        };
        let confirmation_id: String = row
            .try_get("confirmation_id")
            .map_err(|_| "CONFIRMATION_UNAVAILABLE")?;
        let expires_at: i64 = row
            .try_get("expires_at")
            .map_err(|_| "CONFIRMATION_UNAVAILABLE")?;
        if expires_at <= now {
            sqlx::query(
                "UPDATE control_confirmations SET dispatch_state = 'cancelled'
                 WHERE confirmation_id = ?1 AND dispatch_state = 'awaiting_confirmation'",
            )
            .bind(&confirmation_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| "CONFIRMATION_UNAVAILABLE")?;
            transaction
                .commit()
                .await
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?;
            return Err("CONFIRMATION_EXPIRED");
        }
        if row
            .try_get::<Option<i64>, _>("locked_at")
            .map_err(|_| "CONFIRMATION_UNAVAILABLE")?
            .is_some()
        {
            return Err("CONFIRMATION_LOCKED");
        }
        let matches = verifier.matches(
            sender_fingerprint,
            &confirmation_id,
            &row.try_get::<String, _>("confirmation_nonce")
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?,
            &row.try_get::<String, _>("device_id")
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?,
            &row.try_get::<String, _>("action_kind")
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?,
            row.try_get::<Option<String>, _>("runtime_handle")
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?
                .as_deref(),
            row.try_get::<Option<String>, _>("workspace_handle")
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?
                .as_deref(),
            row.try_get::<Option<String>, _>("harness_profile_handle")
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?
                .as_deref(),
            row.try_get::<Option<String>, _>("task_preset_handle")
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?
                .as_deref(),
            row.try_get::<Option<String>, _>("run_handle")
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?
                .as_deref(),
            &row.try_get::<String, _>("intent_id")
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?,
            expires_at,
            code,
            &row.try_get::<String, _>("confirmation_digest")
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?,
        );
        if !matches {
            let changed = sqlx::query(
                "UPDATE control_confirmations
                 SET attempt_count = attempt_count + 1,
                     locked_at = CASE WHEN attempt_count + 1 >= max_attempts THEN ?1 ELSE NULL END,
                     dispatch_state = CASE WHEN attempt_count + 1 >= max_attempts THEN 'locked' ELSE dispatch_state END
                 WHERE confirmation_id = ?2 AND dispatch_state = 'awaiting_confirmation'
                   AND locked_at IS NULL AND attempt_count < max_attempts",
            )
            .bind(now)
            .bind(&confirmation_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| "CONFIRMATION_UNAVAILABLE")?
            .rows_affected();
            if changed != 1 {
                return Err("CONFIRMATION_LOCKED");
            }
            transaction
                .commit()
                .await
                .map_err(|_| "CONFIRMATION_UNAVAILABLE")?;
            return Err("CONFIRMATION_REJECTED");
        }
        transaction
            .commit()
            .await
            .map_err(|_| "CONFIRMATION_UNAVAILABLE")?;
        Ok(confirmation_id)
    }

    pub async fn recover_stale_claims(&self) -> Result<u64, InboundWorkerError> {
        self.recover_stale_claims_at(unix_timestamp_ms()?).await
    }

    pub(super) async fn recover_stale_claims_at(
        &self,
        now: i64,
    ) -> Result<u64, InboundWorkerError> {
        let mut transaction = self.pool.begin().await.map_err(worker_database_error)?;
        let expired = sqlx::query(
            "UPDATE inbound_commands
            SET status = 'expired', claim_token = NULL, claimed_at = NULL,
                next_attempt_at = NULL, updated_at = ?1
             WHERE status IN ('received', 'dispatching', 'waiting_gateway') AND expires_at <= ?1",
        )
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        let interrupted = sqlx::query(
            "UPDATE inbound_commands
            SET status = 'received', claim_token = NULL, claimed_at = NULL,
                next_attempt_at = NULL,
                last_error_code = 'GATEWAY_INTERRUPTED_RESTART', updated_at = ?1
             WHERE status = 'waiting_gateway' AND expires_at > ?1
               AND NOT EXISTS (
                   SELECT 1 FROM control_confirmations c
                   WHERE c.dispatch_message_key = inbound_commands.message_key
                     AND c.dispatch_state IN ('pending', 'outcome', 'failed')
               )",
        )
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        let control_resumed = sqlx::query(
            "UPDATE inbound_commands
             SET status = 'received', claim_token = NULL, claimed_at = NULL,
                 next_attempt_at = NULL,
                 last_error_code = CASE
                     WHEN last_error_code = 'GATEWAY_INTERRUPTED_RESTART' THEN NULL
                     ELSE last_error_code
                 END,
                 updated_at = ?1
             WHERE status = 'waiting_gateway' AND expires_at > ?1
               AND EXISTS (
                   SELECT 1 FROM control_confirmations c
                   WHERE c.dispatch_message_key = inbound_commands.message_key
                     AND c.dispatch_state IN ('pending', 'outcome', 'failed')
               )",
        )
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        let recovered = sqlx::query(
            "UPDATE inbound_commands
            SET status = 'received', claim_token = NULL, claimed_at = NULL,
                next_attempt_at = NULL,
                last_error_code = CASE
                    WHEN last_error_code = 'GATEWAY_INTERRUPTED_RESTART'
                         AND EXISTS (
                             SELECT 1 FROM control_confirmations c
                             WHERE c.dispatch_message_key = inbound_commands.message_key
                               AND c.dispatch_state IN ('pending', 'outcome', 'failed')
                         ) THEN NULL
                    ELSE COALESCE(last_error_code, 'STALE_CLAIM_RECOVERED')
                END,
                 updated_at = ?1
             WHERE status = 'dispatching' AND expires_at > ?1
               AND (claimed_at IS NULL OR claimed_at < ?2)",
        )
        .bind(now)
        .bind(now.saturating_sub(STALE_COMMAND_CLAIM_MS))
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        transaction.commit().await.map_err(worker_database_error)?;
        if recovered > 0 || interrupted > 0 || control_resumed > 0 {
            self.wake.notify_one();
        }
        Ok(expired + interrupted + control_resumed + recovered)
    }

    pub(super) async fn claim_next_at(
        &self,
        now: i64,
        cancellation: &CancellationToken,
    ) -> Result<Option<ClaimedInboundCommand>, InboundWorkerError> {
        if cancellation.is_cancelled() {
            return Ok(None);
        }
        let claim_token = Uuid::new_v4().to_string();
        // Once SQLite starts the mutating statement its outcome must be observed.
        // The worker re-checks cancellation after receiving the fenced claim.
        let row = sqlx::query(
            "UPDATE inbound_commands
                 SET status = 'dispatching', claim_token = ?1, claimed_at = ?2,
                     next_attempt_at = NULL,
                     attempt_count = attempt_count + 1, updated_at = ?2
                 WHERE message_key = (
                     SELECT message_key FROM inbound_commands
                     WHERE status = 'received' AND expires_at > ?2
                       AND (next_attempt_at IS NULL OR next_attempt_at <= ?2)
                     ORDER BY created_at ASC, message_key ASC LIMIT 1
                 ) AND status = 'received'
                 RETURNING message_key, claim_token, sender_fingerprint,
                           command_kind, command_json, last_error_code, expires_at",
        )
        .bind(&claim_token)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(worker_database_error)?;
        row.map(|row| {
            Ok(ClaimedInboundCommand {
                message_key: row.try_get("message_key").map_err(worker_database_error)?,
                claim_token: row.try_get("claim_token").map_err(worker_database_error)?,
                sender_fingerprint: row
                    .try_get("sender_fingerprint")
                    .map_err(worker_database_error)?,
                command_kind: row.try_get("command_kind").map_err(worker_database_error)?,
                command_json: row.try_get("command_json").map_err(worker_database_error)?,
                last_error_code: row
                    .try_get("last_error_code")
                    .map_err(worker_database_error)?,
                expires_at: row.try_get("expires_at").map_err(worker_database_error)?,
            })
        })
        .transpose()
    }

    pub(super) async fn release_claim_at(
        &self,
        claim: &ClaimedInboundCommand,
        now: i64,
    ) -> Result<bool, InboundWorkerError> {
        let changed = sqlx::query(
            "UPDATE inbound_commands
             SET status = CASE WHEN expires_at <= ?1 THEN 'expired' ELSE 'received' END,
                 claim_token = NULL, claimed_at = NULL,
                 next_attempt_at = NULL,
                 attempt_count = CASE WHEN attempt_count > 0 THEN attempt_count - 1 ELSE 0 END,
                 updated_at = ?1
             WHERE message_key = ?2 AND status IN ('dispatching', 'waiting_gateway')
               AND claim_token = ?3",
        )
        .bind(now)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .execute(&self.pool)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        if changed == 1 && now < claim.expires_at {
            self.wake.notify_one();
        }
        Ok(changed == 1)
    }

    pub(super) async fn dead_letter_at(
        &self,
        claim: &ClaimedInboundCommand,
        error_code: &'static str,
        now: i64,
    ) -> Result<bool, InboundWorkerError> {
        sqlx::query(
            "UPDATE inbound_commands
             SET status = 'dead_letter', claim_token = NULL, claimed_at = NULL,
                 next_attempt_at = NULL,
                 last_error_code = ?1, updated_at = ?2
             WHERE message_key = ?3 AND status IN ('dispatching', 'waiting_gateway')
               AND claim_token = ?4",
        )
        .bind(error_code)
        .bind(now)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .execute(&self.pool)
        .await
        .map_err(worker_database_error)
        .map(|result| result.rows_affected() == 1)
    }

    pub(super) async fn queue_reply_at(
        &self,
        outbox: &OutboxService,
        claim: &ClaimedInboundCommand,
        reply: InteractiveReplyV1,
        now: i64,
    ) -> Result<bool, InboundWorkerError> {
        if reply.target_account_fingerprint != claim.sender_fingerprint
            || reply.expires_at > claim.expires_at
        {
            return Err(InboundWorkerError::Outbox);
        }
        let mut transaction = self.pool.begin().await.map_err(worker_database_error)?;
        let accepted = outbox
            .stage_system_reply_in(&mut transaction, reply, now)
            .await
            .map_err(|_| InboundWorkerError::Outbox)?;
        let updated = sqlx::query(
            "UPDATE inbound_commands
             SET status = 'reply_queued', reply_notification_id = ?1,
                 claim_token = NULL, claimed_at = NULL, next_attempt_at = NULL,
                 updated_at = ?2
             WHERE message_key = ?3 AND status IN ('dispatching', 'waiting_gateway')
               AND claim_token = ?4
               AND expires_at > ?2",
        )
        .bind(&accepted.notification_id)
        .bind(now)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        if updated != 1 {
            transaction
                .rollback()
                .await
                .map_err(worker_database_error)?;
            return Ok(false);
        }
        transaction.commit().await.map_err(worker_database_error)?;
        if !accepted.existing {
            outbox.notify_worker();
        }
        Ok(true)
    }

    /// Atomically persist the hash-only confirmation, stage its one-time code
    /// reply, and advance the inbound claim. The plaintext code exists only in
    /// `reply.body` while this transaction is being assembled and is never
    /// inserted into SQLite.
    pub(super) async fn queue_reply_with_confirmation_at(
        &self,
        outbox: &OutboxService,
        claim: &ClaimedInboundCommand,
        reply: InteractiveReplyV1,
        confirmation: PendingConfirmation,
        now: i64,
    ) -> Result<bool, InboundWorkerError> {
        if reply.target_account_fingerprint != claim.sender_fingerprint
            || reply.expires_at > claim.expires_at
            || confirmation.sender_fingerprint != claim.sender_fingerprint
            || confirmation.expires_at > claim.expires_at
            || !(1..=128).contains(&confirmation.confirmation_id.len())
            || confirmation.confirmation_digest.len() != 64
            || confirmation.confirmation_nonce.len() != 32
            || !confirmation
                .confirmation_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(InboundWorkerError::Outbox);
        }
        let mut transaction = self.pool.begin().await.map_err(worker_database_error)?;
        self.delete_expired_confirmations_in(&mut transaction, now)
            .await?;
        // A sender can possess only one live credential.  Replacing the
        // selection invalidates the former credential rather than allowing a
        // code intended for an earlier action to remain actionable.
        sqlx::query(
            "UPDATE control_confirmations SET dispatch_state = 'cancelled'
             WHERE sender_fingerprint = ?1 AND dispatch_state = 'awaiting_confirmation'",
        )
        .bind(&confirmation.sender_fingerprint)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?;
        sqlx::query(
            "INSERT INTO control_confirmations(
                confirmation_id, sender_fingerprint, confirmation_digest, confirmation_nonce,
                device_id, action_kind, runtime_handle, workspace_handle,
                harness_profile_handle, task_preset_handle, run_handle,
                intent_id, expires_at, consumed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL)",
        )
        .bind(&confirmation.confirmation_id)
        .bind(&confirmation.sender_fingerprint)
        .bind(&confirmation.confirmation_digest)
        .bind(&confirmation.confirmation_nonce)
        .bind(confirmation.device_id.to_string())
        .bind(confirmation.action_kind.as_str())
        .bind(confirmation.runtime_handle.as_deref())
        .bind(confirmation.workspace_handle.as_deref())
        .bind(confirmation.harness_profile_handle.as_deref())
        .bind(confirmation.task_preset_handle.as_deref())
        .bind(confirmation.run_handle.as_deref())
        .bind(&confirmation.intent_id)
        .bind(confirmation.expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?;
        let accepted = outbox
            .stage_system_reply_in(&mut transaction, reply, now)
            .await
            .map_err(|_| InboundWorkerError::Outbox)?;
        let updated = sqlx::query(
            "UPDATE inbound_commands
             SET status = 'reply_queued', reply_notification_id = ?1,
                 claim_token = NULL, claimed_at = NULL, next_attempt_at = NULL,
                 updated_at = ?2
             WHERE message_key = ?3 AND status IN ('dispatching', 'waiting_gateway')
               AND claim_token = ?4 AND expires_at > ?2",
        )
        .bind(&accepted.notification_id)
        .bind(now)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        if updated != 1 {
            transaction
                .rollback()
                .await
                .map_err(worker_database_error)?;
            return Ok(false);
        }
        transaction.commit().await.map_err(worker_database_error)?;
        if !accepted.existing {
            outbox.notify_worker();
        }
        Ok(true)
    }

    pub(super) async fn cancel_confirmations_at(
        &self,
        sender_fingerprint: &str,
        now: i64,
    ) -> Result<u64, InboundWorkerError> {
        self.delete_expired_confirmations(sender_fingerprint, now)
            .await?;
        Ok(sqlx::query(
            "DELETE FROM control_confirmations
             WHERE sender_fingerprint = ?1 AND consumed_at IS NULL",
        )
        .bind(sender_fingerprint)
        .execute(&self.pool)
        .await
        .map_err(worker_database_error)?
        .rows_affected())
    }

    #[cfg(test)]
    pub(super) async fn consume_confirmation_at(
        &self,
        sender_fingerprint: &str,
        confirmation_id: &str,
        expected_device_id: Uuid,
        expected_action: ConfirmationActionKind,
        now: i64,
    ) -> Result<ConfirmationRecord, ConfirmationConsumeError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| ConfirmationConsumeError::Missing)?;
        let row = sqlx::query(
            "SELECT sender_fingerprint, confirmation_id, device_id, action_kind,
                    runtime_handle, workspace_handle, harness_profile_handle AS profile_handle, task_preset_handle AS preset_handle, run_handle,
                    intent_id, expires_at, consumed_at
             FROM control_confirmations
             WHERE sender_fingerprint = ?1 AND confirmation_id = ?2",
        )
        .bind(sender_fingerprint)
        .bind(confirmation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| ConfirmationConsumeError::Missing)?;
        let Some(row) = row else {
            return Err(ConfirmationConsumeError::Missing);
        };
        let expires_at = row
            .try_get::<i64, _>("expires_at")
            .map_err(|_| ConfirmationConsumeError::Missing)?;
        if expires_at <= now {
            sqlx::query(
                "DELETE FROM control_confirmations
                 WHERE sender_fingerprint = ?1 AND confirmation_id = ?2",
            )
            .bind(sender_fingerprint)
            .bind(confirmation_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| ConfirmationConsumeError::Missing)?;
            transaction
                .commit()
                .await
                .map_err(|_| ConfirmationConsumeError::Missing)?;
            return Err(ConfirmationConsumeError::Expired);
        }
        if row
            .try_get::<Option<i64>, _>("consumed_at")
            .map_err(|_| ConfirmationConsumeError::Missing)?
            .is_some()
        {
            return Err(ConfirmationConsumeError::Replay);
        }
        let device_id = row
            .try_get::<String, _>("device_id")
            .ok()
            .and_then(|value| Uuid::parse_str(&value).ok());
        let action_kind = row
            .try_get::<String, _>("action_kind")
            .ok()
            .and_then(|value| parse_confirmation_action(&value));
        if device_id != Some(expected_device_id) || action_kind != Some(expected_action) {
            return Err(ConfirmationConsumeError::WrongScope);
        }
        let changed = sqlx::query(
            "UPDATE control_confirmations SET consumed_at = ?1
             WHERE sender_fingerprint = ?2 AND confirmation_id = ?3
               AND consumed_at IS NULL AND expires_at > ?1",
        )
        .bind(now)
        .bind(sender_fingerprint)
        .bind(confirmation_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| ConfirmationConsumeError::Missing)?
        .rows_affected();
        if changed != 1 {
            return Err(ConfirmationConsumeError::Replay);
        }
        let record = ConfirmationRecord {
            sender_fingerprint: row
                .try_get("sender_fingerprint")
                .map_err(|_| ConfirmationConsumeError::Missing)?,
            confirmation_id: row
                .try_get("confirmation_id")
                .map_err(|_| ConfirmationConsumeError::Missing)?,
            device_id: device_id.expect("validated device id"),
            action_kind: action_kind.expect("validated action kind"),
            runtime_handle: row
                .try_get("runtime_handle")
                .map_err(|_| ConfirmationConsumeError::Missing)?,
            workspace_handle: row
                .try_get("workspace_handle")
                .map_err(|_| ConfirmationConsumeError::Missing)?,
            profile_handle: row
                .try_get("profile_handle")
                .map_err(|_| ConfirmationConsumeError::Missing)?,
            preset_handle: row
                .try_get("preset_handle")
                .map_err(|_| ConfirmationConsumeError::Missing)?,
            run_handle: row
                .try_get("run_handle")
                .map_err(|_| ConfirmationConsumeError::Missing)?,
            intent_id: row
                .try_get("intent_id")
                .map_err(|_| ConfirmationConsumeError::Missing)?,
            expires_at,
        };
        transaction
            .commit()
            .await
            .map_err(|_| ConfirmationConsumeError::Missing)?;
        Ok(record)
    }

    /// Atomically consume a confirmation and bind its durable control intent
    /// to the inbound claim. A later worker can find this binding after a
    /// restart and retry the exact same intent without accepting the code a
    /// second time.
    pub(super) async fn prepare_control_dispatch_at(
        &self,
        claim: &ClaimedInboundCommand,
        confirmation_id: &str,
        now: i64,
    ) -> Result<ControlDispatchRecord, ConfirmationConsumeError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| ConfirmationConsumeError::Missing)?;
        let row = sqlx::query(
            "SELECT sender_fingerprint, confirmation_id, device_id, action_kind,
                    runtime_handle, workspace_handle, harness_profile_handle AS profile_handle, task_preset_handle AS preset_handle, run_handle,
                    intent_id, expires_at, consumed_at, dispatch_state,
                    dispatch_message_key, outcome_accepted, outcome_run_handle,
                    outcome_status, outcome_error_code
             FROM control_confirmations
             WHERE sender_fingerprint = ?1 AND confirmation_id = ?2",
        )
        .bind(&claim.sender_fingerprint)
        .bind(confirmation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| ConfirmationConsumeError::Missing)?;
        let Some(row) = row else {
            return Err(ConfirmationConsumeError::Missing);
        };
        let expires_at = row
            .try_get::<i64, _>("expires_at")
            .map_err(|_| ConfirmationConsumeError::Missing)?;
        let dispatch_message_key = row
            .try_get::<Option<String>, _>("dispatch_message_key")
            .map_err(|_| ConfirmationConsumeError::Missing)?;
        if let Some(dispatch_message_key) = dispatch_message_key {
            if dispatch_message_key != claim.message_key {
                return Err(ConfirmationConsumeError::WrongScope);
            }
            if row
                .try_get::<String, _>("dispatch_state")
                .map_err(|_| ConfirmationConsumeError::Missing)?
                == "pending"
            {
                let changed = sqlx::query(
                    "UPDATE inbound_commands SET status = 'waiting_gateway',
                            next_attempt_at = NULL, updated_at = ?1
                     WHERE message_key = ?2 AND status = 'dispatching' AND claim_token = ?3
                       AND expires_at > ?1",
                )
                .bind(now)
                .bind(&claim.message_key)
                .bind(&claim.claim_token)
                .execute(&mut *transaction)
                .await
                .map_err(|_| ConfirmationConsumeError::Missing)?
                .rows_affected();
                if changed != 1 {
                    return Err(ConfirmationConsumeError::Missing);
                }
            }
            let record = control_dispatch_record_from_row(&row, expires_at)?;
            transaction
                .commit()
                .await
                .map_err(|_| ConfirmationConsumeError::Missing)?;
            return Ok(record);
        }
        if expires_at <= now {
            return Err(ConfirmationConsumeError::Expired);
        }
        if row
            .try_get::<Option<i64>, _>("consumed_at")
            .map_err(|_| ConfirmationConsumeError::Missing)?
            .is_some()
        {
            return Err(ConfirmationConsumeError::Replay);
        }
        let changed = sqlx::query(
            "UPDATE control_confirmations
             SET consumed_at = ?1, dispatch_state = 'pending', dispatch_message_key = ?2
             WHERE sender_fingerprint = ?3 AND confirmation_id = ?4
               AND consumed_at IS NULL AND dispatch_message_key IS NULL
               AND expires_at > ?1",
        )
        .bind(now)
        .bind(&claim.message_key)
        .bind(&claim.sender_fingerprint)
        .bind(confirmation_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| ConfirmationConsumeError::Missing)?
        .rows_affected();
        if changed != 1 {
            return Err(ConfirmationConsumeError::Replay);
        }
        let claim_changed = sqlx::query(
            "UPDATE inbound_commands SET status = 'waiting_gateway',
                    next_attempt_at = NULL, updated_at = ?1
             WHERE message_key = ?2 AND status = 'dispatching' AND claim_token = ?3
               AND expires_at > ?1",
        )
        .bind(now)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .execute(&mut *transaction)
        .await
        .map_err(|_| ConfirmationConsumeError::Missing)?
        .rows_affected();
        if claim_changed != 1 {
            return Err(ConfirmationConsumeError::Missing);
        }
        transaction
            .commit()
            .await
            .map_err(|_| ConfirmationConsumeError::Missing)?;
        let confirmation = confirmation_record_from_row(&row, expires_at)?;
        Ok(ControlDispatchRecord {
            confirmation,
            outcome: None,
            error_code: None,
        })
    }

    pub(super) async fn persist_control_outcome_at(
        &self,
        claim: &ClaimedInboundCommand,
        confirmation_id: &str,
        outcome: &ControlDispatchOutcome,
        now: i64,
    ) -> Result<bool, InboundWorkerError> {
        let mut transaction = self.pool.begin().await.map_err(worker_database_error)?;
        let changed = sqlx::query(
            "UPDATE control_confirmations
             SET dispatch_state = 'outcome', outcome_accepted = ?1,
                 outcome_run_handle = ?2, outcome_status = ?3,
                 outcome_error_code = NULL
             WHERE sender_fingerprint = ?4 AND confirmation_id = ?5
               AND dispatch_message_key = ?6 AND dispatch_state = 'pending'
               AND EXISTS (
                   SELECT 1 FROM inbound_commands
                   WHERE message_key = ?6 AND status = 'waiting_gateway'
                     AND claim_token = ?7 AND expires_at > ?8
               )",
        )
        .bind(i64::from(outcome.accepted))
        .bind(&outcome.run_handle)
        .bind(&outcome.status)
        .bind(&claim.sender_fingerprint)
        .bind(confirmation_id)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        if changed != 1 {
            transaction
                .rollback()
                .await
                .map_err(worker_database_error)?;
            return Ok(false);
        }
        transaction.commit().await.map_err(worker_database_error)?;
        Ok(true)
    }

    pub(super) async fn fail_control_dispatch_at(
        &self,
        claim: &ClaimedInboundCommand,
        confirmation_id: &str,
        error_code: &str,
        now: i64,
    ) -> Result<bool, InboundWorkerError> {
        let mut transaction = self.pool.begin().await.map_err(worker_database_error)?;
        let changed = sqlx::query(
            "UPDATE control_confirmations
             SET dispatch_state = 'failed', outcome_error_code = ?1,
                 outcome_accepted = NULL, outcome_run_handle = NULL,
                 outcome_status = NULL
             WHERE sender_fingerprint = ?2 AND confirmation_id = ?3
               AND dispatch_message_key = ?4 AND dispatch_state = 'pending'
               AND EXISTS (
                   SELECT 1 FROM inbound_commands
                   WHERE message_key = ?4 AND status = 'waiting_gateway'
                     AND claim_token = ?5 AND expires_at > ?6
               )",
        )
        .bind(error_code)
        .bind(&claim.sender_fingerprint)
        .bind(confirmation_id)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        if changed != 1 {
            transaction
                .rollback()
                .await
                .map_err(worker_database_error)?;
            return Ok(false);
        }
        transaction.commit().await.map_err(worker_database_error)?;
        Ok(true)
    }

    pub(super) async fn retry_control_dispatch_at(
        &self,
        claim: &ClaimedInboundCommand,
        now: i64,
    ) -> Result<bool, InboundWorkerError> {
        let changed = sqlx::query(
            "UPDATE inbound_commands
             SET status = 'received', claim_token = NULL, claimed_at = NULL,
                 next_attempt_at = ?1, last_error_code = 'CONTROL_RETRY_PENDING',
                 updated_at = ?2
             WHERE message_key = ?3 AND status = 'waiting_gateway' AND claim_token = ?4
               AND expires_at > ?2",
        )
        .bind(now.saturating_add(1_000))
        .bind(now)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .execute(&self.pool)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        if changed == 1 {
            self.wake.notify_one();
        }
        Ok(changed == 1)
    }

    pub(super) async fn queue_reply_with_job_page_at(
        &self,
        outbox: &OutboxService,
        claim: &ClaimedInboundCommand,
        reply: InteractiveReplyV1,
        selection: PendingJobPageSelection,
        now: i64,
    ) -> Result<bool, InboundWorkerError> {
        if reply.target_account_fingerprint != claim.sender_fingerprint
            || reply.expires_at > claim.expires_at
            || !valid_job_page_selection(&selection)
        {
            return Err(InboundWorkerError::Outbox);
        }
        let query_kind = match selection.filter {
            RunFilter::All => "runs_all",
            RunFilter::Recent => "runs_recent",
            RunFilter::Failed => "runs_failed",
        };
        let selection_expires_at = now.saturating_add(SELECTION_TTL_MS);
        let mut transaction = self.pool.begin().await.map_err(worker_database_error)?;
        let accepted = outbox
            .stage_system_reply_in(&mut transaction, reply, now)
            .await
            .map_err(|_| InboundWorkerError::Outbox)?;
        sqlx::query(
            "INSERT INTO selection_contexts(
                sender_fingerprint, selected_device, query_kind, next_cursor, expires_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(sender_fingerprint) DO UPDATE SET
                selected_device = excluded.selected_device,
                query_kind = excluded.query_kind,
                next_cursor = excluded.next_cursor,
                expires_at = excluded.expires_at",
        )
        .bind(&claim.sender_fingerprint)
        .bind(selection.selected_device.to_string())
        .bind(query_kind)
        .bind(selection.next_cursor.as_deref())
        .bind(selection_expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?;
        sqlx::query("DELETE FROM selection_entries WHERE sender_fingerprint = ?1")
            .bind(&claim.sender_fingerprint)
            .execute(&mut *transaction)
            .await
            .map_err(worker_database_error)?;
        for entry in &selection.entries {
            sqlx::query(
                "INSERT INTO selection_entries(
                    sender_fingerprint, slot, device_id, client_opaque_handle, item_kind
                 ) VALUES (?1, ?2, ?3, ?4, 'run')",
            )
            .bind(&claim.sender_fingerprint)
            .bind(i64::from(entry.slot))
            .bind(entry.device_id.to_string())
            .bind(entry.client_opaque_handle.as_deref())
            .execute(&mut *transaction)
            .await
            .map_err(worker_database_error)?;
        }
        let updated = sqlx::query(
            "UPDATE inbound_commands
             SET status = 'reply_queued', reply_notification_id = ?1,
                 claim_token = NULL, claimed_at = NULL, next_attempt_at = NULL,
                 updated_at = ?2
             WHERE message_key = ?3 AND status IN ('dispatching', 'waiting_gateway')
               AND claim_token = ?4 AND expires_at > ?2",
        )
        .bind(&accepted.notification_id)
        .bind(now)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        if updated != 1 {
            transaction
                .rollback()
                .await
                .map_err(worker_database_error)?;
            return Ok(false);
        }
        transaction.commit().await.map_err(worker_database_error)?;
        if !accepted.existing {
            outbox.notify_worker();
        }
        Ok(true)
    }

    pub(super) fn wake(&self) -> &Notify {
        &self.wake
    }

    pub(super) async fn mark_waiting_gateway_at(
        &self,
        claim: &ClaimedInboundCommand,
        now: i64,
    ) -> Result<bool, InboundWorkerError> {
        sqlx::query(
            "UPDATE inbound_commands SET status = 'waiting_gateway', next_attempt_at = NULL,
                    updated_at = ?1
             WHERE message_key = ?2 AND status = 'dispatching' AND claim_token = ?3
               AND expires_at > ?1",
        )
        .bind(now)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .execute(&self.pool)
        .await
        .map_err(worker_database_error)
        .map(|result| result.rows_affected() == 1)
    }

    pub(super) async fn interrupt_claim_at(
        &self,
        claim: &ClaimedInboundCommand,
        now: i64,
    ) -> Result<bool, InboundWorkerError> {
        sqlx::query(
            "UPDATE inbound_commands
             SET status = 'received', claim_token = NULL, claimed_at = NULL,
                 next_attempt_at = NULL,
                 last_error_code = CASE
                     WHEN status = 'waiting_gateway' AND NOT EXISTS (
                         SELECT 1 FROM control_confirmations c
                         WHERE c.dispatch_message_key = inbound_commands.message_key
                           AND c.dispatch_state IN ('pending', 'outcome', 'failed')
                     ) THEN 'GATEWAY_INTERRUPTED_RESTART'
                     ELSE last_error_code
                 END,
                 updated_at = ?1
             WHERE message_key = ?2 AND status IN ('dispatching', 'waiting_gateway')
               AND claim_token = ?3",
        )
        .bind(now)
        .bind(&claim.message_key)
        .bind(&claim.claim_token)
        .execute(&self.pool)
        .await
        .map_err(worker_database_error)
        .map(|result| {
            if result.rows_affected() == 1 {
                self.wake.notify_one();
                true
            } else {
                false
            }
        })
    }

    pub(super) async fn replace_selection_at(
        &self,
        sender_fingerprint: &str,
        selected_device: Option<Uuid>,
        entries: &[SelectionEntry],
        now: i64,
    ) -> Result<(), InboundWorkerError> {
        if entries.len() > MAX_SELECTION_ENTRIES
            || entries
                .iter()
                .enumerate()
                .any(|(index, entry)| usize::from(entry.slot) != index + 1)
        {
            return Err(InboundWorkerError::Database);
        }
        let expires_at = now.saturating_add(SELECTION_TTL_MS);
        let mut transaction = self.pool.begin().await.map_err(worker_database_error)?;
        sqlx::query("DELETE FROM selection_contexts WHERE expires_at <= ?1")
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(worker_database_error)?;
        let existing: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM selection_contexts WHERE sender_fingerprint = ?1)",
        )
        .bind(sender_fingerprint)
        .fetch_one(&mut *transaction)
        .await
        .map_err(worker_database_error)?;
        if !existing {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM selection_contexts")
                .fetch_one(&mut *transaction)
                .await
                .map_err(worker_database_error)?;
            if count >= MAX_SELECTION_CONTEXTS {
                let victim: Option<String> = sqlx::query_scalar(
                    "SELECT sender_fingerprint FROM selection_contexts
                     ORDER BY expires_at ASC, sender_fingerprint ASC LIMIT 1",
                )
                .fetch_optional(&mut *transaction)
                .await
                .map_err(worker_database_error)?;
                if let Some(victim) = victim {
                    sqlx::query("DELETE FROM selection_contexts WHERE sender_fingerprint = ?1")
                        .bind(victim)
                        .execute(&mut *transaction)
                        .await
                        .map_err(worker_database_error)?;
                }
            }
        }
        sqlx::query(
            "INSERT INTO selection_contexts(
                 sender_fingerprint, selected_device, selected_runtime_handle,
                 selected_workspace_handle, selected_harness_profile_handle,
                 query_kind, expires_at
             ) VALUES (?1, ?2, NULL, NULL, NULL, 'devices', ?3)
             ON CONFLICT(sender_fingerprint) DO UPDATE SET
                 selected_device = excluded.selected_device,
                 selected_runtime_handle = NULL,
                 selected_workspace_handle = NULL,
                 selected_harness_profile_handle = NULL,
                 query_kind = 'devices', next_cursor = NULL,
                 expires_at = excluded.expires_at",
        )
        .bind(sender_fingerprint)
        .bind(selected_device.map(|id| id.to_string()))
        .bind(expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?;
        sqlx::query("DELETE FROM selection_entries WHERE sender_fingerprint = ?1")
            .bind(sender_fingerprint)
            .execute(&mut *transaction)
            .await
            .map_err(worker_database_error)?;
        for entry in entries {
            sqlx::query(
                "INSERT INTO selection_entries(
                    sender_fingerprint, slot, device_id, client_opaque_handle,
                    item_kind
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(sender_fingerprint)
            .bind(i64::from(entry.slot))
            .bind(entry.device_id.to_string())
            .bind(entry.client_opaque_handle.as_deref())
            .bind(entry.item_kind.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(worker_database_error)?;
        }
        transaction.commit().await.map_err(worker_database_error)
    }

    /// Store one of the v5 safe catalog pages and its typed slot mapping in the
    /// same transaction. Handles are opaque and are never rendered to WeChat.
    pub(super) async fn replace_catalog_selection_at(
        &self,
        sender_fingerprint: &str,
        selected_device: Uuid,
        query_kind: &str,
        entries: &[SelectionEntry],
        now: i64,
    ) -> Result<(), InboundWorkerError> {
        if !matches!(
            query_kind,
            "runtimes" | "workspaces" | "harness_profiles" | "task_presets"
        ) || entries.len() > MAX_SELECTION_ENTRIES
            || entries.iter().enumerate().any(|(index, entry)| {
                usize::from(entry.slot) != index + 1
                    || entry.device_id != selected_device
                    || !matches!(
                        (query_kind, entry.item_kind),
                        ("runtimes", SelectionItemKind::Runtime)
                            | ("workspaces", SelectionItemKind::Workspace)
                            | ("harness_profiles", SelectionItemKind::HarnessProfile)
                            | ("task_presets", SelectionItemKind::TaskPreset)
                    )
                    || entry.client_opaque_handle.is_none()
            })
        {
            return Err(InboundWorkerError::Database);
        }
        let expires_at = now.saturating_add(SELECTION_TTL_MS);
        let mut transaction = self.pool.begin().await.map_err(worker_database_error)?;
        sqlx::query(
            "INSERT INTO selection_contexts(
                sender_fingerprint, selected_device, query_kind, next_cursor,
                selected_runtime_handle, selected_workspace_handle,
                selected_harness_profile_handle, expires_at
             ) VALUES (?1, ?2, ?3, NULL, NULL, NULL, NULL, ?4)
             ON CONFLICT(sender_fingerprint) DO UPDATE SET
                selected_device = excluded.selected_device,
                query_kind = excluded.query_kind,
                next_cursor = NULL,
                selected_runtime_handle = CASE
                    WHEN excluded.query_kind = 'runtimes'
                        THEN NULL ELSE selected_runtime_handle END,
                selected_workspace_handle = CASE
                    WHEN excluded.query_kind IN ('runtimes', 'workspaces')
                        THEN NULL ELSE selected_workspace_handle END,
                selected_harness_profile_handle = CASE
                    WHEN excluded.query_kind IN ('runtimes', 'workspaces', 'harness_profiles')
                        THEN NULL ELSE selected_harness_profile_handle END,
                expires_at = excluded.expires_at",
        )
        .bind(sender_fingerprint)
        .bind(selected_device.to_string())
        .bind(query_kind)
        .bind(expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?;
        sqlx::query("DELETE FROM selection_entries WHERE sender_fingerprint = ?1")
            .bind(sender_fingerprint)
            .execute(&mut *transaction)
            .await
            .map_err(worker_database_error)?;
        for entry in entries {
            sqlx::query(
                "INSERT INTO selection_entries(
                    sender_fingerprint, slot, device_id, client_opaque_handle, item_kind
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(sender_fingerprint)
            .bind(i64::from(entry.slot))
            .bind(entry.device_id.to_string())
            .bind(entry.client_opaque_handle.as_deref())
            .bind(entry.item_kind.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(worker_database_error)?;
        }
        transaction.commit().await.map_err(worker_database_error)
    }

    pub(super) async fn select_catalog_slot_at(
        &self,
        sender_fingerprint: &str,
        slot: u16,
        expected_kind: SelectionItemKind,
        now: i64,
    ) -> Result<Result<SelectionEntry, SelectionLookupError>, InboundWorkerError> {
        let entry = match self
            .selection_entry_at(sender_fingerprint, slot, now)
            .await?
        {
            Ok(entry) if entry.item_kind == expected_kind => entry,
            Ok(_) => return Ok(Err(SelectionLookupError::WrongKind)),
            Err(error) => return Ok(Err(error)),
        };
        let Some(handle) = entry.client_opaque_handle.as_deref() else {
            return Ok(Err(SelectionLookupError::WrongKind));
        };
        let (runtime, workspace, profile) = match expected_kind {
            SelectionItemKind::Runtime => (Some(handle), None, None),
            SelectionItemKind::Workspace => (None, Some(handle), None),
            SelectionItemKind::HarnessProfile => (None, None, Some(handle)),
            SelectionItemKind::TaskPreset | SelectionItemKind::Device | SelectionItemKind::Run => {
                (None, None, None)
            }
        };
        let changed = sqlx::query(
            "UPDATE selection_contexts SET
                 selected_runtime_handle = CASE
                     WHEN ?1 IS NOT NULL THEN ?1 ELSE selected_runtime_handle END,
                 selected_workspace_handle = CASE
                     WHEN ?2 IS NOT NULL THEN ?2 ELSE selected_workspace_handle END,
                 selected_harness_profile_handle = CASE
                     WHEN ?3 IS NOT NULL THEN ?3 ELSE selected_harness_profile_handle END,
                 expires_at = ?4
             WHERE sender_fingerprint = ?5 AND selected_device = ?6
               AND expires_at > ?7",
        )
        .bind(runtime)
        .bind(workspace)
        .bind(profile)
        .bind(now.saturating_add(SELECTION_TTL_MS))
        .bind(sender_fingerprint)
        .bind(entry.device_id.to_string())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        if changed == 1 {
            Ok(Ok(entry))
        } else {
            Ok(Err(SelectionLookupError::Expired))
        }
    }

    pub(super) async fn resolve_catalog_slot_at(
        &self,
        sender_fingerprint: &str,
        slot: u16,
        expected_kind: SelectionItemKind,
        now: i64,
    ) -> Result<Result<SelectionEntry, SelectionLookupError>, InboundWorkerError> {
        match self
            .selection_entry_at(sender_fingerprint, slot, now)
            .await?
        {
            Ok(entry) if entry.item_kind == expected_kind => Ok(Ok(entry)),
            Ok(_) => Ok(Err(SelectionLookupError::WrongKind)),
            Err(error) => Ok(Err(error)),
        }
    }

    pub(super) async fn selected_catalog_context_at(
        &self,
        sender_fingerprint: &str,
        now: i64,
    ) -> Result<Result<SelectedCatalogContext, SelectionLookupError>, InboundWorkerError> {
        let row = sqlx::query(
            "SELECT selected_device, selected_runtime_handle, selected_workspace_handle,
                    selected_harness_profile_handle, expires_at
             FROM selection_contexts WHERE sender_fingerprint = ?1",
        )
        .bind(sender_fingerprint)
        .fetch_optional(&self.pool)
        .await
        .map_err(worker_database_error)?;
        let Some(row) = row else {
            return Ok(Err(SelectionLookupError::Missing));
        };
        if row
            .try_get::<i64, _>("expires_at")
            .map_err(worker_database_error)?
            <= now
        {
            self.delete_selection(sender_fingerprint).await?;
            return Ok(Err(SelectionLookupError::Expired));
        }
        let Some(device) = row
            .try_get::<Option<String>, _>("selected_device")
            .map_err(worker_database_error)?
            .and_then(|value| Uuid::parse_str(&value).ok())
        else {
            return Ok(Err(SelectionLookupError::Missing));
        };
        Ok(Ok(SelectedCatalogContext {
            device_id: device,
            runtime_handle: row
                .try_get("selected_runtime_handle")
                .map_err(worker_database_error)?,
            workspace_handle: row
                .try_get("selected_workspace_handle")
                .map_err(worker_database_error)?,
            harness_profile_handle: row
                .try_get("selected_harness_profile_handle")
                .map_err(worker_database_error)?,
        }))
    }

    #[cfg(test)]
    pub(super) async fn replace_job_page_at(
        &self,
        sender_fingerprint: &str,
        selected_device: Uuid,
        filter: RunFilter,
        next_cursor: Option<&str>,
        entries: &[SelectionEntry],
        now: i64,
    ) -> Result<(), InboundWorkerError> {
        if entries.len() > MAX_SELECTION_ENTRIES
            || entries.iter().enumerate().any(|(index, entry)| {
                usize::from(entry.slot) != index + 1
                    || entry.device_id != selected_device
                    || entry.item_kind != SelectionItemKind::Run
            })
            || next_cursor.is_some_and(|cursor| {
                !(32..=128).contains(&cursor.len())
                    || !cursor
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
        {
            return Err(InboundWorkerError::Database);
        }
        let query_kind = match filter {
            RunFilter::All => "runs_all",
            RunFilter::Recent => "runs_recent",
            RunFilter::Failed => "runs_failed",
        };
        let expires_at = now.saturating_add(SELECTION_TTL_MS);
        let mut transaction = self.pool.begin().await.map_err(worker_database_error)?;
        sqlx::query(
            "INSERT INTO selection_contexts(
                sender_fingerprint, selected_device, query_kind, next_cursor, expires_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(sender_fingerprint) DO UPDATE SET
                selected_device = excluded.selected_device,
                query_kind = excluded.query_kind,
                next_cursor = excluded.next_cursor,
                expires_at = excluded.expires_at",
        )
        .bind(sender_fingerprint)
        .bind(selected_device.to_string())
        .bind(query_kind)
        .bind(next_cursor)
        .bind(expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(worker_database_error)?;
        sqlx::query("DELETE FROM selection_entries WHERE sender_fingerprint = ?1")
            .bind(sender_fingerprint)
            .execute(&mut *transaction)
            .await
            .map_err(worker_database_error)?;
        for entry in entries {
            sqlx::query(
                "INSERT INTO selection_entries(
                    sender_fingerprint, slot, device_id, client_opaque_handle, item_kind
                 ) VALUES (?1, ?2, ?3, ?4, 'run')",
            )
            .bind(sender_fingerprint)
            .bind(i64::from(entry.slot))
            .bind(entry.device_id.to_string())
            .bind(entry.client_opaque_handle.as_deref())
            .execute(&mut *transaction)
            .await
            .map_err(worker_database_error)?;
        }
        transaction.commit().await.map_err(worker_database_error)
    }

    pub(super) async fn next_page_context_at(
        &self,
        sender_fingerprint: &str,
        now: i64,
    ) -> Result<Result<SelectionPageContext, SelectionLookupError>, InboundWorkerError> {
        let row = sqlx::query(
            "SELECT selected_device, query_kind, next_cursor, expires_at
             FROM selection_contexts WHERE sender_fingerprint = ?1",
        )
        .bind(sender_fingerprint)
        .fetch_optional(&self.pool)
        .await
        .map_err(worker_database_error)?;
        let Some(row) = row else {
            return Ok(Err(SelectionLookupError::Missing));
        };
        if row
            .try_get::<i64, _>("expires_at")
            .map_err(worker_database_error)?
            <= now
        {
            self.delete_selection(sender_fingerprint).await?;
            return Ok(Err(SelectionLookupError::Expired));
        }
        let selected_device = row
            .try_get::<Option<String>, _>("selected_device")
            .map_err(worker_database_error)?
            .and_then(|value| Uuid::parse_str(&value).ok())
            .ok_or(InboundWorkerError::Database)?;
        let filter = match row
            .try_get::<Option<String>, _>("query_kind")
            .map_err(worker_database_error)?
            .as_deref()
        {
            Some("runs_all") => RunFilter::All,
            Some("runs_recent") => RunFilter::Recent,
            Some("runs_failed") => RunFilter::Failed,
            _ => return Ok(Err(SelectionLookupError::WrongKind)),
        };
        let Some(next_cursor) = row
            .try_get::<Option<String>, _>("next_cursor")
            .map_err(worker_database_error)?
        else {
            return Ok(Err(SelectionLookupError::Missing));
        };
        Ok(Ok(SelectionPageContext {
            selected_device,
            filter,
            next_cursor,
        }))
    }

    pub(super) async fn selected_device_at(
        &self,
        sender_fingerprint: &str,
        now: i64,
    ) -> Result<Result<Uuid, SelectionLookupError>, InboundWorkerError> {
        let row = sqlx::query(
            "SELECT selected_device, expires_at FROM selection_contexts
             WHERE sender_fingerprint = ?1",
        )
        .bind(sender_fingerprint)
        .fetch_optional(&self.pool)
        .await
        .map_err(worker_database_error)?;
        let Some(row) = row else {
            return Ok(Err(SelectionLookupError::Missing));
        };
        if row
            .try_get::<i64, _>("expires_at")
            .map_err(worker_database_error)?
            <= now
        {
            self.delete_selection(sender_fingerprint).await?;
            return Ok(Err(SelectionLookupError::Expired));
        }
        let device_id = row
            .try_get::<Option<String>, _>("selected_device")
            .map_err(worker_database_error)?
            .and_then(|value| Uuid::parse_str(&value).ok());
        Ok(device_id.ok_or(SelectionLookupError::Missing))
    }

    pub(super) async fn select_device_slot_at(
        &self,
        sender_fingerprint: &str,
        slot: u16,
        now: i64,
    ) -> Result<Result<Uuid, SelectionLookupError>, InboundWorkerError> {
        let entry = self
            .selection_entry_at(sender_fingerprint, slot, now)
            .await?;
        let entry = match entry {
            Ok(entry) if entry.item_kind == SelectionItemKind::Device => entry,
            Ok(_) => return Ok(Err(SelectionLookupError::WrongKind)),
            Err(error) => return Ok(Err(error)),
        };
        let changed = sqlx::query(
            "UPDATE selection_contexts SET selected_device = ?1
             WHERE sender_fingerprint = ?2 AND expires_at > ?3",
        )
        .bind(entry.device_id.to_string())
        .bind(sender_fingerprint)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(worker_database_error)?
        .rows_affected();
        if changed == 1 {
            Ok(Ok(entry.device_id))
        } else {
            Ok(Err(SelectionLookupError::Expired))
        }
    }

    pub(super) async fn resolve_job_slot_at(
        &self,
        sender_fingerprint: &str,
        slot: u16,
        now: i64,
    ) -> Result<Result<SelectionEntry, SelectionLookupError>, InboundWorkerError> {
        match self
            .selection_entry_at(sender_fingerprint, slot, now)
            .await?
        {
            Ok(entry) if entry.item_kind == SelectionItemKind::Run => Ok(Ok(entry)),
            Ok(_) => Ok(Err(SelectionLookupError::WrongKind)),
            Err(error) => Ok(Err(error)),
        }
    }

    async fn selection_entry_at(
        &self,
        sender_fingerprint: &str,
        slot: u16,
        now: i64,
    ) -> Result<Result<SelectionEntry, SelectionLookupError>, InboundWorkerError> {
        let row = sqlx::query(
            "SELECT e.device_id, e.client_opaque_handle, e.item_kind,
                    c.expires_at AS context_expires_at
             FROM selection_entries e
             JOIN selection_contexts c USING(sender_fingerprint)
             WHERE e.sender_fingerprint = ?1 AND e.slot = ?2",
        )
        .bind(sender_fingerprint)
        .bind(i64::from(slot))
        .fetch_optional(&self.pool)
        .await
        .map_err(worker_database_error)?;
        let Some(row) = row else {
            return Ok(Err(SelectionLookupError::Missing));
        };
        let context_expiry = row
            .try_get::<i64, _>("context_expires_at")
            .map_err(worker_database_error)?;
        if context_expiry <= now {
            self.delete_selection(sender_fingerprint).await?;
            return Ok(Err(SelectionLookupError::Expired));
        }
        let device_id = row
            .try_get::<String, _>("device_id")
            .map_err(worker_database_error)?;
        let device_id = Uuid::parse_str(&device_id).map_err(worker_database_error)?;
        let item_kind = match row
            .try_get::<String, _>("item_kind")
            .map_err(worker_database_error)?
            .as_str()
        {
            "device" => SelectionItemKind::Device,
            "run" => SelectionItemKind::Run,
            "runtime" => SelectionItemKind::Runtime,
            "workspace" => SelectionItemKind::Workspace,
            "harness_profile" => SelectionItemKind::HarnessProfile,
            "task_preset" => SelectionItemKind::TaskPreset,
            _ => return Err(InboundWorkerError::Database),
        };
        Ok(Ok(SelectionEntry {
            slot,
            device_id,
            client_opaque_handle: row
                .try_get("client_opaque_handle")
                .map_err(worker_database_error)?,
            item_kind,
        }))
    }

    async fn delete_selection(&self, sender_fingerprint: &str) -> Result<(), InboundWorkerError> {
        sqlx::query("DELETE FROM selection_contexts WHERE sender_fingerprint = ?1")
            .bind(sender_fingerprint)
            .execute(&self.pool)
            .await
            .map_err(worker_database_error)?;
        Ok(())
    }

    async fn delete_expired_confirmations(
        &self,
        sender_fingerprint: &str,
        now: i64,
    ) -> Result<u64, InboundWorkerError> {
        Ok(sqlx::query(
            "DELETE FROM control_confirmations
             WHERE sender_fingerprint = ?1 AND expires_at <= ?2
               AND dispatch_state <> 'pending'",
        )
        .bind(sender_fingerprint)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(worker_database_error)?
        .rows_affected())
    }

    async fn delete_expired_confirmations_in(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
        now: i64,
    ) -> Result<u64, InboundWorkerError> {
        Ok(sqlx::query(
            "DELETE FROM control_confirmations
                 WHERE expires_at <= ?1 AND dispatch_state <> 'pending'",
        )
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(worker_database_error)?
        .rows_affected())
    }
}

pub(super) fn unix_timestamp_ms() -> Result<i64, InboundWorkerError> {
    let millis = SystemTime::UNIX_EPOCH
        .elapsed()
        .map_err(|_| InboundWorkerError::Clock)?
        .as_millis();
    i64::try_from(millis).map_err(|_| InboundWorkerError::Clock)
}

fn message_database_error<T>(_error: T) -> InboundMessageError {
    InboundMessageError::Database
}

fn valid_job_page_selection(selection: &PendingJobPageSelection) -> bool {
    selection.entries.len() <= MAX_SELECTION_ENTRIES
        && selection.entries.iter().enumerate().all(|(index, entry)| {
            usize::from(entry.slot) == index + 1
                && entry.device_id == selection.selected_device
                && entry.item_kind == SelectionItemKind::Run
        })
        && selection.next_cursor.as_deref().is_none_or(|cursor| {
            (32..=128).contains(&cursor.len())
                && cursor
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
}

fn worker_database_error<T>(_error: T) -> InboundWorkerError {
    InboundWorkerError::Database
}

fn parse_confirmation_action(value: &str) -> Option<ConfirmationActionKind> {
    match value {
        "start_run" => Some(ConfirmationActionKind::StartRun),
        "cancel_run" => Some(ConfirmationActionKind::CancelRun),
        _ => None,
    }
}

fn confirmation_record_from_row(
    row: &SqliteRow,
    expires_at: i64,
) -> Result<ConfirmationRecord, ConfirmationConsumeError> {
    let device_id = row
        .try_get::<String, _>("device_id")
        .ok()
        .and_then(|value| Uuid::parse_str(&value).ok())
        .ok_or(ConfirmationConsumeError::Missing)?;
    let action_kind = row
        .try_get::<String, _>("action_kind")
        .ok()
        .and_then(|value| parse_confirmation_action(&value))
        .ok_or(ConfirmationConsumeError::Missing)?;
    Ok(ConfirmationRecord {
        sender_fingerprint: row
            .try_get("sender_fingerprint")
            .map_err(|_| ConfirmationConsumeError::Missing)?,
        confirmation_id: row
            .try_get("confirmation_id")
            .map_err(|_| ConfirmationConsumeError::Missing)?,
        device_id,
        action_kind,
        runtime_handle: row
            .try_get("runtime_handle")
            .map_err(|_| ConfirmationConsumeError::Missing)?,
        workspace_handle: row
            .try_get("workspace_handle")
            .map_err(|_| ConfirmationConsumeError::Missing)?,
        profile_handle: row
            .try_get("profile_handle")
            .map_err(|_| ConfirmationConsumeError::Missing)?,
        preset_handle: row
            .try_get("preset_handle")
            .map_err(|_| ConfirmationConsumeError::Missing)?,
        run_handle: row
            .try_get("run_handle")
            .map_err(|_| ConfirmationConsumeError::Missing)?,
        intent_id: row
            .try_get("intent_id")
            .map_err(|_| ConfirmationConsumeError::Missing)?,
        expires_at,
    })
}

fn control_dispatch_record_from_row(
    row: &SqliteRow,
    expires_at: i64,
) -> Result<ControlDispatchRecord, ConfirmationConsumeError> {
    let confirmation = confirmation_record_from_row(row, expires_at)?;
    let state = row
        .try_get::<String, _>("dispatch_state")
        .map_err(|_| ConfirmationConsumeError::Missing)?;
    match state.as_str() {
        "pending" => Ok(ControlDispatchRecord {
            confirmation,
            outcome: None,
            error_code: None,
        }),
        "outcome" => Ok(ControlDispatchRecord {
            confirmation,
            outcome: Some(ControlDispatchOutcome {
                accepted: row
                    .try_get::<i64, _>("outcome_accepted")
                    .map_err(|_| ConfirmationConsumeError::Missing)?
                    != 0,
                run_handle: row
                    .try_get::<String, _>("outcome_run_handle")
                    .map_err(|_| ConfirmationConsumeError::Missing)?,
                status: row
                    .try_get::<String, _>("outcome_status")
                    .map_err(|_| ConfirmationConsumeError::Missing)?,
            }),
            error_code: None,
        }),
        "failed" => Ok(ControlDispatchRecord {
            confirmation,
            outcome: None,
            error_code: Some(
                row.try_get::<String, _>("outcome_error_code")
                    .map_err(|_| ConfirmationConsumeError::Missing)?,
            ),
        }),
        _ => Err(ConfirmationConsumeError::Missing),
    }
}
