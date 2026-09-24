use std::{sync::Arc, time::SystemTime};

use sqlx::Row as _;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

use super::{BundleContentCipher, OutboxError};

#[derive(Clone)]
pub struct OutboxService {
    pub(super) pool: SqlitePool,
    pub(super) wake: Arc<Notify>,
    checkpoint_gate: Arc<Mutex<()>>,
    pub(super) bundle_cipher: Option<BundleContentCipher>,
    pub(super) bundle_content_retention_ms: i64,
}

impl OutboxService {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            wake: Arc::new(Notify::new()),
            checkpoint_gate: Arc::new(Mutex::new(())),
            bundle_cipher: None,
            bundle_content_retention_ms: 7 * 24 * 60 * 60 * 1_000,
        }
    }

    pub fn with_bundle_cipher(mut self, cipher: BundleContentCipher) -> Self {
        self.bundle_cipher = Some(cipher);
        self
    }

    pub fn with_bundle_content_retention_days(mut self, days: u32) -> Self {
        self.bundle_content_retention_ms = i64::from(days) * 24 * 60 * 60 * 1_000;
        self
    }

    pub fn wake_worker(&self) {
        self.wake.notify_one();
    }

    /// Resolves result-link text only at send time. The outbox row itself stores
    /// a fixed placeholder, so a bearer URL never rests in plaintext there.
    pub(super) async fn protected_result_link_body(
        &self,
        outbox_row_id: &str,
        now: i64,
    ) -> Result<Option<String>, OutboxError> {
        let row = sqlx::query(
            "SELECT r.origin_device_id,r.result_id,r.source_hash,r.page_expires_at,r.revoked_at,
                    o.protected_body_ciphertext,o.protected_body_nonce
             FROM notification_outbox o JOIN results r ON r.id=o.result_row_id
             WHERE o.id=?1 AND o.body_kind='protected_result_link'",
        )
        .bind(outbox_row_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        let Some(row) = row else { return Ok(None) };
        if row
            .try_get::<Option<i64>, _>("revoked_at")
            .map_err(database_error)?
            .is_some()
            || row
                .try_get::<i64, _>("page_expires_at")
                .map_err(database_error)?
                <= now
        {
            return Ok(None);
        }
        let cipher = self
            .bundle_cipher
            .as_ref()
            .ok_or(OutboxError::CryptoUnavailable)?;
        let device = Uuid::parse_str(
            &row.try_get::<String, _>("origin_device_id")
                .map_err(database_error)?,
        )
        .map_err(|_| OutboxError::Database)?;
        let id: String = row.try_get("result_id").map_err(database_error)?;
        let hash: String = row.try_get("source_hash").map_err(database_error)?;
        let notification_id: String =
            sqlx::query_scalar("SELECT notification_id FROM notification_outbox WHERE id=?1")
                .bind(outbox_row_id)
                .fetch_one(&self.pool)
                .await
                .map_err(database_error)?;
        let mut aad = Vec::new();
        aad.extend_from_slice(b"ResultLinkNotice");
        aad.extend_from_slice(device.as_bytes());
        aad.extend_from_slice(id.as_bytes());
        aad.extend_from_slice(hash.as_bytes());
        aad.extend_from_slice(notification_id.as_bytes());
        let token = cipher.decrypt(
            &row.try_get::<Vec<u8>, _>("protected_body_nonce")
                .map_err(database_error)?,
            &row.try_get::<Vec<u8>, _>("protected_body_ciphertext")
                .map_err(database_error)?,
            &aad,
        )?;
        let token = String::from_utf8(token.to_vec()).map_err(|_| OutboxError::CorruptContent)?;
        Ok(Some(token))
    }

    /// Coalesce best-effort checkpoints after a sensitive terminal rewrite.
    ///
    /// The logical redaction is already committed before this runs.  A busy
    /// reader therefore cannot roll that transition back or delay the worker;
    /// later terminal rewrites will try again.  This reduces WAL residency but
    /// intentionally does not claim physical-media secure deletion.
    pub(super) async fn checkpoint_after_terminal_redaction(&self) {
        let Ok(_gate) = self.checkpoint_gate.try_lock() else {
            return;
        };
        let _ = crate::db::checkpoint_wal(&self.pool).await;
    }
}

pub(super) fn unix_timestamp_ms() -> Result<i64, OutboxError> {
    let millis = SystemTime::UNIX_EPOCH
        .elapsed()
        .map_err(|_| OutboxError::Clock)?
        .as_millis();
    i64::try_from(millis).map_err(|_| OutboxError::Clock)
}

pub(super) fn database_error<T>(_error: T) -> OutboxError {
    OutboxError::Database
}
