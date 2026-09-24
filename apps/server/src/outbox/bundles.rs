use sha2::{Digest as _, Sha256};
use sqlx::{Row as _, Sqlite, Transaction};
use uuid::Uuid;

use super::{
    BundleContentCipher, ChannelOutcome, ClaimedBundleSegment, NotificationBundleReceipt,
    NotificationBundleStatus, NotificationBundleV1, OutboxError, OutboxService, RetryClass,
    repository::database_error,
    retry::{NETWORK_RETRY_MS, RATE_LIMIT_RETRY_MS},
    validation::{
        MAX_BUNDLE_SEGMENTS, MAX_RENDERED_SEGMENT_BYTES, validate_bundle,
        validate_target_account_fingerprint,
    },
};

const MAX_QUEUED_BUNDLES: i64 = 128;
const MAX_QUEUED_BUNDLE_BYTES: i64 = 16 * 1024 * 1024;
const SEGMENT_INTERVAL_MS: i64 = 300;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SegmentRange {
    start: usize,
    end: usize,
}

impl OutboxService {
    pub(crate) async fn purge_bundle_content_at(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<u64, OutboxError> {
        sqlx::query(
            "UPDATE notification_bundles
             SET body_ciphertext = NULL, body_nonce = NULL, body_purged_at = ?1,
                 updated_at = MAX(updated_at, ?1)
             WHERE id IN (
                 SELECT id FROM notification_bundles
                 WHERE body_purged_at IS NULL AND body_retain_until <= ?1
                   AND status IN (
                       'provider_accepted', 'partial_failed', 'expired',
                       'blocked_target_changed', 'delivery_unknown'
                   )
                 ORDER BY body_retain_until, id LIMIT ?2
             )",
        )
        .bind(now)
        .bind(limit)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
        .map_err(database_error)
    }

    pub async fn enqueue_bundle(
        &self,
        device_id: Uuid,
        target_account_fingerprint: String,
        bundle: NotificationBundleV1,
    ) -> Result<NotificationBundleReceipt, OutboxError> {
        self.enqueue_bundle_at(
            device_id,
            target_account_fingerprint,
            bundle,
            super::repository::unix_timestamp_ms()?,
        )
        .await
    }

    pub(super) async fn enqueue_bundle_at(
        &self,
        device_id: Uuid,
        target_account_fingerprint: String,
        bundle: NotificationBundleV1,
        accepted_at: i64,
    ) -> Result<NotificationBundleReceipt, OutboxError> {
        validate_bundle(&bundle, accepted_at)?;
        validate_target_account_fingerprint(&target_account_fingerprint)?;
        let ranges = segment_ranges(&bundle.title, &bundle.body)?;
        let cipher = self
            .bundle_cipher
            .as_ref()
            .ok_or(OutboxError::CryptoUnavailable)?;
        let request_digest =
            cipher.request_digest(&framed_request(&bundle, &target_account_fingerprint));
        let aad = content_aad(device_id, &bundle.bundle_id, &bundle.source_hash);
        let (nonce, ciphertext) = cipher.encrypt(bundle.body.as_bytes(), &aad)?;
        let bundle_row_id = Uuid::new_v4().to_string();
        let origin_key = format!("device:{device_id}");
        let segment_count = i64::try_from(ranges.len()).map_err(|_| OutboxError::Validation)?;
        let body_bytes = i64::try_from(bundle.body.len()).map_err(|_| OutboxError::Validation)?;
        let mut transaction = self.pool.begin().await.map_err(database_error)?;

        let inserted = sqlx::query(
            "INSERT INTO notification_bundles (
                id, origin_key, origin_device_id, bundle_id, dedupe_key,
                request_digest, kind, content_mode, source, correlation_key,
                result_revision, title, body_ciphertext, body_nonce, body_bytes,
                source_hash, target_account_fingerprint, status, segment_count,
                accepted_segments, accepted_at, created_at, expires_at, updated_at,
                body_retain_until
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, 'queued', ?18, 0, ?19, ?20, ?21, ?19,
                ?19 + ?22
             )
             ON CONFLICT(origin_key, bundle_id) DO NOTHING
             ON CONFLICT(origin_key, dedupe_key) DO NOTHING",
        )
        .bind(&bundle_row_id)
        .bind(&origin_key)
        .bind(device_id.to_string())
        .bind(&bundle.bundle_id)
        .bind(&bundle.dedupe_key)
        .bind(request_digest.as_slice())
        .bind(&bundle.kind)
        .bind(&bundle.content_mode)
        .bind(&bundle.source)
        .bind(&bundle.correlation_key)
        .bind(i64::try_from(bundle.result_revision).map_err(|_| OutboxError::Validation)?)
        .bind(&bundle.title)
        .bind(ciphertext)
        .bind(nonce.as_slice())
        .bind(body_bytes)
        .bind(&bundle.source_hash)
        .bind(&target_account_fingerprint)
        .bind(segment_count)
        .bind(accepted_at)
        .bind(bundle.created_at)
        .bind(bundle.expires_at)
        .bind(self.bundle_content_retention_ms)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();

        if inserted == 0 {
            let receipt = replay_bundle(
                &mut transaction,
                &origin_key,
                &bundle.bundle_id,
                &bundle.dedupe_key,
                &request_digest,
            )
            .await?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(receipt);
        }

        let queued = sqlx::query(
            "SELECT COUNT(*) AS bundle_count, COALESCE(SUM(body_bytes), 0) AS body_bytes
             FROM notification_bundles
             WHERE status IN ('queued', 'delivering')",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        if queued
            .try_get::<i64, _>("bundle_count")
            .map_err(database_error)?
            > MAX_QUEUED_BUNDLES
            || queued
                .try_get::<i64, _>("body_bytes")
                .map_err(database_error)?
                > MAX_QUEUED_BUNDLE_BYTES
        {
            transaction.rollback().await.map_err(database_error)?;
            return Err(OutboxError::QueueFull);
        }

        for (index, range) in ranges.iter().enumerate() {
            let one_based = index + 1;
            let header = rendered_header(one_based, ranges.len(), &bundle.title);
            let content = &bundle.body.as_bytes()[range.start..range.end];
            let content_hash = format!("{:x}", Sha256::digest(content));
            let client_id = stable_segment_client_id(&origin_key, &bundle.dedupe_key, index);
            sqlx::query(
                "INSERT INTO notification_bundle_segments (
                    id, bundle_row_id, segment_index, byte_start, byte_end,
                    content_hash, rendered_header, client_id, status, not_before,
                    expires_at, attempt_count, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending_channel',
                           ?9, ?10, 0, ?9, ?9)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(&bundle_row_id)
            .bind(i64::try_from(index).map_err(|_| OutboxError::Validation)?)
            .bind(i64::try_from(range.start).map_err(|_| OutboxError::Validation)?)
            .bind(i64::try_from(range.end).map_err(|_| OutboxError::Validation)?)
            .bind(content_hash)
            .bind(header)
            .bind(client_id)
            .bind(accepted_at)
            .bind(bundle.expires_at)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        self.notify_worker();
        Ok(NotificationBundleReceipt {
            bundle_id: bundle.bundle_id,
            accepted_at,
            status: NotificationBundleStatus::Queued,
            segment_count: u32::try_from(segment_count).map_err(|_| OutboxError::Database)?,
            accepted_segments: 0,
            source_hash: bundle.source_hash,
            updated_at: accepted_at,
        })
    }

    pub async fn bundle_status(
        &self,
        device_id: Uuid,
        bundle_id: &str,
    ) -> Result<NotificationBundleReceipt, OutboxError> {
        if bundle_id.is_empty() || bundle_id.len() > 128 {
            return Err(OutboxError::NotFound);
        }
        let row = sqlx::query(
            "SELECT bundle_id, accepted_at, status, segment_count, accepted_segments,
                    source_hash, updated_at
             FROM notification_bundles
             WHERE origin_device_id = ?1 AND origin_key = 'device:' || ?1
               AND bundle_id = ?2",
        )
        .bind(device_id.to_string())
        .bind(bundle_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?
        .ok_or(OutboxError::NotFound)?;
        receipt_from_row(&row)
    }

    pub(super) async fn claim_next_bundle_segment_at(
        &self,
        now: i64,
    ) -> Result<Option<ClaimedBundleSegment>, OutboxError> {
        self.expire_bundle_segments_at(now).await?;
        let claim_token = Uuid::new_v4().to_string();
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let row = sqlx::query(
            "UPDATE notification_bundle_segments
             SET status = 'sending_channel', claim_token = ?1, claimed_at = ?2,
                 attempt_count = attempt_count + 1, updated_at = ?2
             WHERE id = (
                 SELECT segment.id
                 FROM notification_bundle_segments AS segment
                 JOIN notification_bundles AS bundle ON bundle.id = segment.bundle_row_id
                 WHERE ((segment.status = 'pending_channel' AND segment.not_before <= ?2)
                     OR (segment.status = 'retry_wait' AND segment.next_attempt_at <= ?2))
                   AND segment.expires_at > ?2
                   AND bundle.body_ciphertext IS NOT NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM notification_bundle_segments AS predecessor
                       WHERE predecessor.bundle_row_id = segment.bundle_row_id
                         AND predecessor.segment_index = segment.segment_index - 1
                         AND predecessor.status <> 'provider_accepted'
                   )
                 ORDER BY bundle.accepted_at ASC, segment.segment_index ASC, segment.id ASC
                 LIMIT 1
             )
             AND status IN ('pending_channel', 'retry_wait')
             RETURNING id, bundle_row_id, segment_index, byte_start, byte_end,
                       content_hash, rendered_header, client_id, expires_at,
                       attempt_count, claim_token",
        )
        .bind(&claim_token)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let Some(row) = row else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let bundle_row_id: String = row.try_get("bundle_row_id").map_err(database_error)?;
        sqlx::query(
            "UPDATE notification_bundles
             SET status = 'delivering', updated_at = MAX(updated_at + 1, ?1)
             WHERE id = ?2 AND status = 'queued'",
        )
        .bind(now)
        .bind(&bundle_row_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let parent = sqlx::query(
            "SELECT origin_device_id, bundle_id, source_hash, body_nonce, body_ciphertext,
                    correlation_key, target_account_fingerprint
             FROM notification_bundles WHERE id = ?1",
        )
        .bind(&bundle_row_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let row_id: String = row.try_get("id").map_err(database_error)?;
        let claim_token: String = row.try_get("claim_token").map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;

        let cipher = self
            .bundle_cipher
            .as_ref()
            .ok_or(OutboxError::CryptoUnavailable)?;
        match render_claimed_segment(
            cipher,
            &row,
            &parent,
            row_id.clone(),
            bundle_row_id.clone(),
            claim_token.clone(),
        ) {
            Ok(claim) => Ok(Some(claim)),
            Err(OutboxError::CorruptContent) => {
                self.dead_letter_corrupt_bundle_claim_at(
                    &row_id,
                    &bundle_row_id,
                    &claim_token,
                    now,
                )
                .await?;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    async fn dead_letter_corrupt_bundle_claim_at(
        &self,
        row_id: &str,
        bundle_row_id: &str,
        claim_token: &str,
        now: i64,
    ) -> Result<(), OutboxError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let changed = sqlx::query(
            "UPDATE notification_bundle_segments
             SET status = 'dead_letter', claim_token = NULL, claimed_at = NULL,
                 next_attempt_at = NULL, last_error_code = 'CONTENT_AUTH_FAILED',
                 updated_at = ?1
             WHERE id = ?2 AND bundle_row_id = ?3
               AND status = 'sending_channel' AND claim_token = ?4",
        )
        .bind(now)
        .bind(row_id)
        .bind(bundle_row_id)
        .bind(claim_token)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed == 1 {
            refresh_parent(
                &mut transaction,
                bundle_row_id,
                now,
                self.bundle_content_retention_ms,
            )
            .await?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }

    pub(super) async fn finish_bundle_claim_at(
        &self,
        claim: &ClaimedBundleSegment,
        outcome: ChannelOutcome,
        now: i64,
    ) -> Result<bool, OutboxError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let changed = match outcome {
            ChannelOutcome::Accepted {
                provider_message_id,
            } => {
                let changed = sqlx::query(
                    "UPDATE notification_bundle_segments
                     SET status = 'provider_accepted', provider_message_id = ?1,
                         provider_accepted_at = ?2, claim_token = NULL, claimed_at = NULL,
                         next_attempt_at = NULL, last_error_code = NULL, updated_at = ?2
                     WHERE id = ?3 AND status = 'sending_channel' AND claim_token = ?4",
                )
                .bind(provider_message_id)
                .bind(now)
                .bind(&claim.row_id)
                .bind(&claim.claim_token)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?
                .rows_affected();
                if changed == 1 {
                    sqlx::query(
                        "UPDATE notification_bundle_segments
                         SET not_before = MAX(not_before, ?1), updated_at = MAX(updated_at, ?2)
                         WHERE bundle_row_id = ?3 AND segment_index = (
                             SELECT segment_index + 1 FROM notification_bundle_segments WHERE id = ?4
                         )",
                    )
                    .bind(now.saturating_add(SEGMENT_INTERVAL_MS))
                    .bind(now)
                    .bind(&claim.bundle_row_id)
                    .bind(&claim.row_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(database_error)?;
                }
                changed
            }
            outcome => {
                let (status, next_attempt_at, error_code) =
                    bundle_failure_transition(&outcome, claim.attempt_count, now, claim.expires_at);
                sqlx::query(
                    "UPDATE notification_bundle_segments
                     SET status = ?1, next_attempt_at = ?2, last_error_code = ?3,
                         claim_token = NULL, claimed_at = NULL, updated_at = ?4
                     WHERE id = ?5 AND status = 'sending_channel' AND claim_token = ?6",
                )
                .bind(status)
                .bind(next_attempt_at)
                .bind(error_code)
                .bind(now)
                .bind(&claim.row_id)
                .bind(&claim.claim_token)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?
                .rows_affected()
            }
        };
        if changed == 1 {
            refresh_parent(
                &mut transaction,
                &claim.bundle_row_id,
                now,
                self.bundle_content_retention_ms,
            )
            .await?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(changed == 1)
    }

    pub(super) async fn release_bundle_claim_at(
        &self,
        claim: &ClaimedBundleSegment,
        now: i64,
    ) -> Result<bool, OutboxError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let changed = sqlx::query(
            "UPDATE notification_bundle_segments
             SET status = CASE WHEN expires_at <= ?1 THEN 'expired' ELSE 'retry_wait' END,
                 next_attempt_at = CASE WHEN expires_at <= ?1 THEN NULL ELSE ?1 END,
                 attempt_count = MAX(attempt_count - 1, 0), claim_token = NULL,
                 claimed_at = NULL, last_error_code = 'SEND_CANCELLED', updated_at = ?1
             WHERE id = ?2 AND status = 'sending_channel' AND claim_token = ?3",
        )
        .bind(now)
        .bind(&claim.row_id)
        .bind(&claim.claim_token)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed == 1 {
            refresh_parent(
                &mut transaction,
                &claim.bundle_row_id,
                now,
                self.bundle_content_retention_ms,
            )
            .await?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(changed == 1)
    }

    pub(super) async fn recover_stale_bundle_claims_at(
        &self,
        now: i64,
        stale_before: i64,
    ) -> Result<u64, OutboxError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let rows = sqlx::query(
            "SELECT DISTINCT bundle_row_id FROM notification_bundle_segments
             WHERE status = 'sending_channel' AND (expires_at <= ?1 OR claimed_at < ?2)",
        )
        .bind(now)
        .bind(stale_before)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let changed = sqlx::query(
            "UPDATE notification_bundle_segments
             SET status = CASE WHEN expires_at <= ?1 THEN 'expired' ELSE 'retry_wait' END,
                 next_attempt_at = CASE WHEN expires_at <= ?1 THEN NULL ELSE ?1 END,
                 claim_token = NULL, claimed_at = NULL,
                 last_error_code = CASE WHEN expires_at <= ?1 THEN NULL ELSE 'STALE_CLAIM_RECOVERED' END,
                 updated_at = ?1
             WHERE status = 'sending_channel' AND (expires_at <= ?1 OR claimed_at < ?2)",
        )
        .bind(now)
        .bind(stale_before)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        for row in rows {
            let bundle_row_id: String = row.try_get("bundle_row_id").map_err(database_error)?;
            refresh_parent(
                &mut transaction,
                &bundle_row_id,
                now,
                self.bundle_content_retention_ms,
            )
            .await?;
        }
        transaction.commit().await.map_err(database_error)?;
        if changed > 0 {
            self.notify_worker();
        }
        Ok(changed)
    }

    pub(super) async fn unblock_bundle_segments_at(
        &self,
        status: &str,
        now: i64,
    ) -> Result<u64, OutboxError> {
        let result = sqlx::query(
            "UPDATE notification_bundle_segments
             SET status = 'retry_wait', next_attempt_at = ?1,
                 last_error_code = NULL, updated_at = ?1
             WHERE status = ?2 AND expires_at > ?1",
        )
        .bind(now)
        .bind(status)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        if result.rows_affected() > 0 {
            self.notify_worker();
        }
        Ok(result.rows_affected())
    }

    async fn expire_bundle_segments_at(&self, now: i64) -> Result<(), OutboxError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let rows = sqlx::query(
            "SELECT DISTINCT bundle_row_id FROM notification_bundle_segments
             WHERE expires_at <= ?1 AND status IN (
                 'pending_channel', 'retry_wait', 'blocked_activation', 'blocked_reconnect'
             )",
        )
        .bind(now)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "UPDATE notification_bundle_segments
             SET status = 'expired', next_attempt_at = NULL, claim_token = NULL,
                 claimed_at = NULL, updated_at = ?1
             WHERE expires_at <= ?1 AND status IN (
                 'pending_channel', 'retry_wait', 'blocked_activation', 'blocked_reconnect'
             )",
        )
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        for row in rows {
            let bundle_row_id: String = row.try_get("bundle_row_id").map_err(database_error)?;
            refresh_parent(
                &mut transaction,
                &bundle_row_id,
                now,
                self.bundle_content_retention_ms,
            )
            .await?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }
}

fn render_claimed_segment(
    cipher: &BundleContentCipher,
    row: &sqlx::sqlite::SqliteRow,
    parent: &sqlx::sqlite::SqliteRow,
    row_id: String,
    bundle_row_id: String,
    claim_token: String,
) -> Result<ClaimedBundleSegment, OutboxError> {
    let device_id: String = parent.try_get("origin_device_id").map_err(database_error)?;
    let bundle_id: String = parent.try_get("bundle_id").map_err(database_error)?;
    let source_hash: String = parent.try_get("source_hash").map_err(database_error)?;
    let nonce: Vec<u8> = parent.try_get("body_nonce").map_err(database_error)?;
    let ciphertext: Vec<u8> = parent.try_get("body_ciphertext").map_err(database_error)?;
    let device_id = Uuid::parse_str(&device_id).map_err(|_| OutboxError::Database)?;
    let plaintext = cipher.decrypt(
        &nonce,
        &ciphertext,
        &content_aad(device_id, &bundle_id, &source_hash),
    )?;
    let start = usize::try_from(
        row.try_get::<i64, _>("byte_start")
            .map_err(database_error)?,
    )
    .map_err(|_| OutboxError::CorruptContent)?;
    let end = usize::try_from(row.try_get::<i64, _>("byte_end").map_err(database_error)?)
        .map_err(|_| OutboxError::CorruptContent)?;
    let content = plaintext
        .get(start..end)
        .ok_or(OutboxError::CorruptContent)?;
    let content_hash: String = row.try_get("content_hash").map_err(database_error)?;
    if format!("{:x}", Sha256::digest(content)) != content_hash {
        return Err(OutboxError::CorruptContent);
    }
    let content = std::str::from_utf8(content).map_err(|_| OutboxError::CorruptContent)?;
    let header: String = row.try_get("rendered_header").map_err(database_error)?;
    let body = format!("{header}{content}");
    if body.len() > MAX_RENDERED_SEGMENT_BYTES {
        return Err(OutboxError::CorruptContent);
    }
    Ok(ClaimedBundleSegment {
        row_id,
        bundle_row_id,
        claim_token,
        client_id: row.try_get("client_id").map_err(database_error)?,
        body,
        correlation_key: parent.try_get("correlation_key").map_err(database_error)?,
        target_account_fingerprint: parent
            .try_get("target_account_fingerprint")
            .map_err(database_error)?,
        expires_at: row.try_get("expires_at").map_err(database_error)?,
        attempt_count: row.try_get("attempt_count").map_err(database_error)?,
    })
}

async fn replay_bundle(
    transaction: &mut Transaction<'_, Sqlite>,
    origin_key: &str,
    bundle_id: &str,
    dedupe_key: &str,
    request_digest: &[u8; 32],
) -> Result<NotificationBundleReceipt, OutboxError> {
    let rows = sqlx::query(
        "SELECT bundle_id, dedupe_key, request_digest, accepted_at, status,
                segment_count, accepted_segments, source_hash, updated_at
         FROM notification_bundles
         WHERE origin_key = ?1 AND (bundle_id = ?2 OR dedupe_key = ?3)",
    )
    .bind(origin_key)
    .bind(bundle_id)
    .bind(dedupe_key)
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    if rows.len() == 1 {
        let row = &rows[0];
        if row
            .try_get::<String, _>("bundle_id")
            .map_err(database_error)?
            == bundle_id
            && row
                .try_get::<String, _>("dedupe_key")
                .map_err(database_error)?
                == dedupe_key
            && row
                .try_get::<Vec<u8>, _>("request_digest")
                .map_err(database_error)?
                .as_slice()
                == request_digest
        {
            return receipt_from_row(row);
        }
    }
    Err(OutboxError::IdempotencyConflict)
}

async fn refresh_parent(
    transaction: &mut Transaction<'_, Sqlite>,
    bundle_row_id: &str,
    now: i64,
    content_retention_ms: i64,
) -> Result<(), OutboxError> {
    sqlx::query(
        "UPDATE notification_bundles
         SET accepted_segments = (
                 SELECT COUNT(*) FROM notification_bundle_segments
                 WHERE bundle_row_id = ?1 AND status = 'provider_accepted'
             ),
             status = CASE
                WHEN EXISTS (SELECT 1 FROM notification_bundle_segments WHERE bundle_row_id = ?1 AND status = 'blocked_target_changed') THEN 'blocked_target_changed'
                WHEN EXISTS (SELECT 1 FROM notification_bundle_segments WHERE bundle_row_id = ?1 AND status = 'delivery_unknown') THEN 'delivery_unknown'
                WHEN NOT EXISTS (SELECT 1 FROM notification_bundle_segments WHERE bundle_row_id = ?1 AND status <> 'provider_accepted') THEN 'provider_accepted'
                 WHEN EXISTS (SELECT 1 FROM notification_bundle_segments WHERE bundle_row_id = ?1 AND status = 'dead_letter') THEN 'partial_failed'
                 WHEN EXISTS (SELECT 1 FROM notification_bundle_segments WHERE bundle_row_id = ?1 AND status = 'expired') THEN
                     CASE WHEN EXISTS (SELECT 1 FROM notification_bundle_segments WHERE bundle_row_id = ?1 AND status = 'provider_accepted') THEN 'partial_failed' ELSE 'expired' END
                WHEN EXISTS (SELECT 1 FROM notification_bundle_segments WHERE bundle_row_id = ?1 AND status IN ('sending_channel', 'provider_accepted', 'retry_wait', 'blocked_activation', 'blocked_reconnect')) THEN 'delivering'
                 ELSE 'queued'
             END,
             updated_at = MAX(updated_at + 1, ?2),
             body_retain_until = MAX(body_retain_until, ?2 + ?3)
         WHERE id = ?1",
    )
    .bind(bundle_row_id)
    .bind(now)
    .bind(content_retention_ms)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

fn receipt_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<NotificationBundleReceipt, OutboxError> {
    let segment_count = row
        .try_get::<i64, _>("segment_count")
        .map_err(database_error)?;
    let accepted_segments = row
        .try_get::<i64, _>("accepted_segments")
        .map_err(database_error)?;
    Ok(NotificationBundleReceipt {
        bundle_id: row.try_get("bundle_id").map_err(database_error)?,
        accepted_at: row.try_get("accepted_at").map_err(database_error)?,
        status: NotificationBundleStatus::parse(
            &row.try_get::<String, _>("status").map_err(database_error)?,
        )?,
        segment_count: u32::try_from(segment_count).map_err(|_| OutboxError::Database)?,
        accepted_segments: u32::try_from(accepted_segments).map_err(|_| OutboxError::Database)?,
        source_hash: row.try_get("source_hash").map_err(database_error)?,
        updated_at: row.try_get("updated_at").map_err(database_error)?,
    })
}

fn segment_ranges(title: &str, body: &str) -> Result<Vec<SegmentRange>, OutboxError> {
    let worst_header = rendered_header(MAX_BUNDLE_SEGMENTS, MAX_BUNDLE_SEGMENTS, title);
    let content_budget = MAX_RENDERED_SEGMENT_BYTES
        .checked_sub(worst_header.len())
        .filter(|budget| *budget > 0)
        .ok_or(OutboxError::Validation)?;
    let bytes = body.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        if ranges.len() == MAX_BUNDLE_SEGMENTS {
            return Err(OutboxError::Validation);
        }
        let mut end = (start + content_budget).min(bytes.len());
        while end > start && !body.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            return Err(OutboxError::Validation);
        }
        if end < bytes.len() {
            let window = &body[start..end];
            let minimum = content_budget / 2;
            if let Some(offset) = window
                .rfind('\n')
                .map(|offset| offset + 1)
                .filter(|offset| *offset >= minimum)
            {
                end = start + offset;
            } else if let Some(offset) = window
                .char_indices()
                .filter_map(|(offset, character)| {
                    character
                        .is_whitespace()
                        .then_some(offset + character.len_utf8())
                })
                .rfind(|offset| *offset >= minimum)
            {
                end = start + offset;
            }
            while end > start && unsafe_grapheme_boundary(body, end) {
                end = body[..end]
                    .char_indices()
                    .next_back()
                    .map(|(offset, _)| offset)
                    .unwrap_or(start);
            }
        }
        if end == start {
            return Err(OutboxError::Validation);
        }
        ranges.push(SegmentRange { start, end });
        start = end;
    }
    for (index, range) in ranges.iter().enumerate() {
        if rendered_header(index + 1, ranges.len(), title).len() + range.end - range.start
            > MAX_RENDERED_SEGMENT_BYTES
        {
            return Err(OutboxError::Validation);
        }
    }
    Ok(ranges)
}

fn unsafe_grapheme_boundary(body: &str, boundary: usize) -> bool {
    let before = body[..boundary].chars().next_back();
    let after = body[boundary..].chars().next();
    matches!(before, Some('\u{200d}'))
        || matches!(after, Some('\u{200d}'))
        || after.is_some_and(is_combining_or_variation)
}

fn is_combining_or_variation(character: char) -> bool {
    matches!(character as u32,
        0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff |
        0x20d0..=0x20ff | 0xfe00..=0xfe0f | 0xfe20..=0xfe2f |
        0xe0100..=0xe01ef
    )
}

fn rendered_header(index: usize, total: usize, title: &str) -> String {
    format!("[{index}/{total}] {title}\n\n")
}

fn stable_segment_client_id(origin_key: &str, dedupe_key: &str, index: usize) -> String {
    let mut hasher =
        blake3::Hasher::new_derive_key("promptdock-relay/provider-bundle-client-id/v1");
    frame(&mut hasher, origin_key.as_bytes());
    frame(&mut hasher, dedupe_key.as_bytes());
    frame(&mut hasher, &(index as u64).to_be_bytes());
    let hash = hasher.finalize().to_hex();
    format!("promptdock-bundle-{}", &hash.as_str()[..32])
}

fn framed_request(bundle: &NotificationBundleV1, target: &str) -> Vec<u8> {
    let mut output = Vec::new();
    for value in [
        bundle.bundle_id.as_bytes(),
        bundle.dedupe_key.as_bytes(),
        bundle.kind.as_bytes(),
        bundle.content_mode.as_bytes(),
        bundle.source.as_bytes(),
        bundle.correlation_key.as_bytes(),
        bundle.title.as_bytes(),
        bundle.body.as_bytes(),
        bundle.source_hash.as_bytes(),
        target.as_bytes(),
    ] {
        frame_vec(&mut output, value);
    }
    frame_vec(&mut output, &bundle.schema_version.to_be_bytes());
    frame_vec(&mut output, &bundle.result_revision.to_be_bytes());
    frame_vec(&mut output, &bundle.created_at.to_be_bytes());
    frame_vec(&mut output, &bundle.expires_at.to_be_bytes());
    output
}

fn content_aad(device_id: Uuid, bundle_id: &str, source_hash: &str) -> Vec<u8> {
    let mut output = Vec::new();
    frame_vec(&mut output, device_id.as_bytes());
    frame_vec(&mut output, bundle_id.as_bytes());
    frame_vec(&mut output, source_hash.as_bytes());
    output
}

fn frame(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn frame_vec(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn bundle_failure_transition(
    outcome: &ChannelOutcome,
    attempt_count: i64,
    now: i64,
    expires_at: i64,
) -> (&'static str, Option<i64>, Option<&'static str>) {
    if now >= expires_at {
        return ("expired", None, None);
    }
    match outcome {
        ChannelOutcome::Retryable { class } => {
            if matches!(class, RetryClass::AmbiguousMinusTwo) {
                return ("delivery_unknown", None, Some(class.error_code()));
            }
            let schedule = match class {
                RetryClass::RateLimited => &RATE_LIMIT_RETRY_MS[..],
                RetryClass::Network | RetryClass::ContextChanged | RetryClass::ContextRejected => {
                    &NETWORK_RETRY_MS[..]
                }
                RetryClass::AmbiguousMinusTwo => unreachable!("handled above"),
            };
            let delay = usize::try_from(attempt_count.saturating_sub(1))
                .ok()
                .and_then(|index| schedule.get(index))
                .copied();
            match delay.filter(|delay| now.saturating_add(*delay) < expires_at) {
                Some(delay) => (
                    "retry_wait",
                    Some(now.saturating_add(delay)),
                    Some(class.error_code()),
                ),
                None => ("dead_letter", None, Some(class.error_code())),
            }
        }
        ChannelOutcome::BlockedActivation => {
            ("blocked_activation", None, Some("ACTIVATION_REQUIRED"))
        }
        ChannelOutcome::BlockedReconnect => ("blocked_reconnect", None, Some("RECONNECT_REQUIRED")),
        ChannelOutcome::BlockedTargetChanged => (
            "blocked_target_changed",
            None,
            Some("TARGET_ACCOUNT_CHANGED"),
        ),
        ChannelOutcome::PermanentFailure => ("dead_letter", None, Some("PERMANENT_FAILURE")),
        ChannelOutcome::Cancelled => ("retry_wait", Some(now), Some("SEND_CANCELLED")),
        ChannelOutcome::Accepted { .. } => unreachable!("accepted handled before failure"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_ranges_reconstruct_exact_bytes_and_fit_render_budget() {
        let body = format!(
            "目录:\\虚构\\路径\r\n```rust\n\tfn main() {{}}\n```\n{}\n{}",
            "中🙂e\u{301}".repeat(900),
            "family 👩\u{200d}👩\u{200d}👧\u{200d}👦 ".repeat(200)
        );
        let ranges = segment_ranges("Codex 本轮输出", &body).expect("segments");
        assert!(ranges.len() > 1);
        let reconstructed = ranges
            .iter()
            .flat_map(|range| body.as_bytes()[range.start..range.end].iter().copied())
            .collect::<Vec<_>>();
        assert_eq!(reconstructed, body.as_bytes());
        for (index, range) in ranges.iter().enumerate() {
            assert!(body.is_char_boundary(range.start));
            assert!(body.is_char_boundary(range.end));
            assert!(
                rendered_header(index + 1, ranges.len(), "Codex 本轮输出").len() + range.end
                    - range.start
                    <= MAX_RENDERED_SEGMENT_BYTES
            );
        }
    }
}
