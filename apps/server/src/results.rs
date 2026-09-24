use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::TryRngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use sqlx::{Row as _, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::{config::ResultsConfig, outbox::BundleContentCipher};

const DAY_MS: i64 = 86_400_000;

#[derive(Clone)]
pub struct ResultService {
    pool: SqlitePool,
    config: ResultsConfig,
    cipher: Option<BundleContentCipher>,
}
impl ResultService {
    pub fn disabled(pool: SqlitePool) -> Self {
        Self {
            pool,
            config: ResultsConfig::default(),
            cipher: None,
        }
    }
    pub fn new(
        pool: SqlitePool,
        config: ResultsConfig,
        cipher: Option<BundleContentCipher>,
    ) -> Self {
        Self {
            pool,
            config,
            cipher,
        }
    }
    pub fn available(&self) -> bool {
        self.config.enabled
            && self.cipher.is_some()
            && self.config.normalized_public_origin().is_ok()
    }
    pub async fn publish(
        &self,
        device: Uuid,
        input: ResultPublicationV1,
        target: Option<String>,
    ) -> Result<ResultReceipt, ResultError> {
        // SQLite read-to-write upgrades can race even with a configured busy
        // timeout. Retry only the transient database outcome; every attempt
        // begins with the owner/dedupe replay lookup and never maps it to 409.
        for attempt in 0..4 {
            match self
                .publish_once(device, input.clone(), target.clone())
                .await
            {
                Err(ResultError::Database) if attempt < 3 => {
                    tokio::time::sleep(std::time::Duration::from_millis(5 * (attempt + 1))).await;
                }
                result => return result,
            }
        }
        unreachable!("bounded publish retry always returns")
    }
    async fn publish_once(
        &self,
        device: Uuid,
        input: ResultPublicationV1,
        target: Option<String>,
    ) -> Result<ResultReceipt, ResultError> {
        input.validate(262_144)?;
        let origin = format!("device:{device}");
        let digest = self.digest(&input)?;
        if let Some(existing) = self
            .existing(&origin, &input.result_id, &input.dedupe_key, &digest)
            .await?
        {
            return Ok(existing);
        }
        if !self.available() {
            return Err(ResultError::Unavailable);
        }
        input.validate(self.config.max_body_bytes)?;
        let target = target.ok_or(ResultError::TargetUnavailable)?;
        let now = now()?;
        let expires = now
            .checked_add(i64::from(self.config.share_ttl_days) * DAY_MS)
            .ok_or(ResultError::Clock)?;
        if input.notification_expires_at <= now || input.notification_expires_at > expires {
            return Err(ResultError::Validation);
        }
        let cipher = self.cipher.as_ref().ok_or(ResultError::Unavailable)?;
        let row_id = Uuid::new_v4().to_string();
        let notification_id = Uuid::new_v4().to_string();
        let mut token = [0u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut token)
            .map_err(|_| ResultError::Unavailable)?;
        let token_text = URL_SAFE_NO_PAD.encode(token);
        let token_digest = Sha256::digest(token);
        let body_aad = aad(device, &input.result_id, &input.source_hash, b"ResultBody");
        let (body_nonce, body_ciphertext) = cipher
            .encrypt(input.body.as_bytes(), &body_aad)
            .map_err(|_| ResultError::Unavailable)?;
        let token_aad = aad(
            device,
            &input.result_id,
            &input.source_hash,
            b"ResultShareToken",
        );
        let (token_nonce, token_ciphertext) = cipher
            .encrypt(token_text.as_bytes(), &token_aad)
            .map_err(|_| ResultError::Unavailable)?;
        let origin_url = self
            .config
            .normalized_public_origin()
            .map_err(|_| ResultError::Unavailable)?;
        let mut tx = self.pool.begin().await.map_err(|_| ResultError::Database)?;
        let retained: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(body_bytes),0) FROM results WHERE body_purged_at IS NULL",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| ResultError::Database)?;
        if retained
            .saturating_add(i64::try_from(input.body.len()).map_err(|_| ResultError::Validation)?)
            > i64::try_from(self.config.max_retained_body_bytes)
                .map_err(|_| ResultError::Validation)?
        {
            return Err(ResultError::Capacity);
        }
        let insert = sqlx::query("INSERT INTO results(id,origin_device_id,origin_key,result_id,dedupe_key,request_digest,kind,content_mode,source,correlation_key,result_revision,safe_title,body_ciphertext,body_nonce,body_bytes,source_hash,started_at,completed_at,duration_ms,accepted_at,created_at,page_expires_at,body_retain_until,initial_notification_id,current_notification_id,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?22,?23,?23,?20)")
            .bind(&row_id).bind(device.to_string()).bind(&origin).bind(&input.result_id).bind(&input.dedupe_key).bind(digest.to_vec()).bind(&input.kind).bind(&input.content_mode).bind(&input.source).bind(&input.correlation_key).bind(&input.result_revision).bind(&input.title).bind(body_ciphertext).bind(body_nonce.to_vec()).bind(input.body.len() as i64).bind(&input.source_hash).bind(input.started_at).bind(input.completed_at).bind(input.duration_ms).bind(now).bind(input.created_at).bind(expires).bind(&notification_id).execute(&mut *tx).await;
        if let Err(error) = insert {
            drop(tx);
            if !error
                .as_database_error()
                .is_some_and(|database| database.is_unique_violation())
            {
                return Err(ResultError::Database);
            }
            return self
                .existing(&origin, &input.result_id, &input.dedupe_key, &digest)
                .await?
                .ok_or(ResultError::Conflict);
        }
        sqlx::query("INSERT INTO result_shares(result_row_id,token_digest,token_ciphertext,token_nonce,public_origin,created_at) VALUES(?1,?2,?3,?4,?5,?6)").bind(&row_id).bind(token_digest.to_vec()).bind(token_ciphertext).bind(token_nonce.to_vec()).bind(&origin_url).bind(now).execute(&mut *tx).await.map_err(|_| ResultError::Database)?;
        let notice = result_link_notice(&input.title, &origin_url, &token_text);
        let (notice_nonce, notice_ciphertext) = cipher
            .encrypt(
                notice.as_bytes(),
                &notice_aad(
                    device,
                    &input.result_id,
                    &input.source_hash,
                    &notification_id,
                ),
            )
            .map_err(|_| ResultError::Unavailable)?;
        sqlx::query("INSERT INTO notification_outbox(id,origin_kind,origin_key,origin_device_id,notification_id,dedupe_key,payload_hash,kind,target_account_fingerprint,title,body,body_kind,protected_body_ciphertext,protected_body_nonce,result_row_id,correlation_key,priority,status,not_before,expires_at,created_at,updated_at,sensitive_body) VALUES(?1,'device',?2,?3,?4,?5,?6,'result_link',?7,?8,'[PROTECTED_RESULT_LINK]','protected_result_link',?9,?10,?11,?12,80,'pending_channel',?13,?14,?13,?13,0)")
            .bind(Uuid::new_v4().to_string()).bind(&origin).bind(device.to_string()).bind(&notification_id).bind(format!("result:{}", input.result_id)).bind(hex_digest(notice.as_bytes())).bind(target).bind(&input.title).bind(notice_ciphertext).bind(notice_nonce.to_vec()).bind(&row_id).bind(&row_id).bind(now).bind(input.notification_expires_at).execute(&mut *tx).await.map_err(|_| ResultError::Database)?;
        tx.commit().await.map_err(|_| ResultError::Database)?;
        Ok(ResultReceipt {
            schema_version: 1,
            result_id: input.result_id,
            source_hash: input.source_hash,
            accepted_at: now,
            updated_at: now,
            page_state: PageState::Available,
            page_expires_at: expires,
            notification_id,
            notification_status: "pending_channel".to_owned(),
        })
    }
    pub async fn status(
        &self,
        device: Uuid,
        result_id: &str,
    ) -> Result<ResultReceipt, ResultError> {
        self.receipt_for(&format!("device:{device}"), result_id)
            .await
    }
    pub async fn link(&self, device: Uuid, result_id: &str) -> Result<ResultLink, ResultError> {
        let row = sqlx::query("SELECT r.source_hash,r.page_expires_at,r.revoked_at,s.token_ciphertext,s.token_nonce,s.public_origin FROM results r JOIN result_shares s ON s.result_row_id=r.id WHERE r.origin_key=?1 AND r.result_id=?2").bind(format!("device:{device}")).bind(result_id).fetch_optional(&self.pool).await.map_err(|_| ResultError::Database)?.ok_or(ResultError::NotFound)?;
        let expiry: i64 = row.get("page_expires_at");
        if row.get::<Option<i64>, _>("revoked_at").is_some() || now()? >= expiry {
            return Err(ResultError::Inaccessible);
        }
        let hash: String = row.get("source_hash");
        let cipher = self.cipher.as_ref().ok_or(ResultError::Unavailable)?;
        let plaintext = cipher
            .decrypt(
                &row.get::<Vec<u8>, _>("token_nonce"),
                &row.get::<Vec<u8>, _>("token_ciphertext"),
                &aad(device, result_id, &hash, b"ResultShareToken"),
            )
            .map_err(|_| ResultError::Unavailable)?;
        let token = String::from_utf8(plaintext.to_vec()).map_err(|_| ResultError::Unavailable)?;
        Ok(ResultLink {
            url: format!("{}/r/{token}", row.get::<String, _>("public_origin")),
            expires_at: expiry,
        })
    }
    pub async fn revoke(
        &self,
        device: Uuid,
        result_id: &str,
        request_id: &str,
    ) -> Result<ResultReceipt, ResultError> {
        let row_id: String =
            sqlx::query_scalar("SELECT id FROM results WHERE origin_key=?1 AND result_id=?2")
                .bind(format!("device:{device}"))
                .bind(result_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|_| ResultError::Database)?
                .ok_or(ResultError::NotFound)?;
        self.admin_revoke(&row_id, request_id).await
    }
    pub async fn resend(
        &self,
        device: Uuid,
        result_id: &str,
        request_id: &str,
    ) -> Result<ResultReceipt, ResultError> {
        if request_id.is_empty()
            || request_id.len() > 128
            || request_id.chars().any(char::is_control)
        {
            return Err(ResultError::Validation);
        }
        let origin = format!("device:{device}");
        let now = now()?;
        let mut tx = self.pool.begin().await.map_err(|_| ResultError::Database)?;
        // Acquire the SQLite writer fence before reading revocation/expiry or
        // action state, so a committed revoke cannot be followed by a resend.
        let locked = sqlx::query(
            "UPDATE results SET updated_at=updated_at WHERE origin_key=?1 AND result_id=?2",
        )
        .bind(&origin)
        .bind(result_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| ResultError::Database)?
        .rows_affected();
        if locked != 1 {
            return Err(ResultError::NotFound);
        }
        let result = sqlx::query("SELECT id,page_expires_at,revoked_at FROM results WHERE origin_key=?1 AND result_id=?2")
            .bind(&origin).bind(result_id).fetch_one(&mut *tx).await.map_err(|_|ResultError::Database)?;
        let result_row: String = result.get("id");
        let action: Option<String> = sqlx::query_scalar("SELECT notification_id FROM result_actions WHERE result_row_id=?1 AND action_kind='resend' AND request_id=?2").bind(&result_row).bind(request_id).fetch_optional(&mut *tx).await.map_err(|_|ResultError::Database)?;
        if action.is_some() {
            // The action row prevents duplicate delivery. The receipt remains a
            // projection of the current notification so a delayed A replay
            // cannot move a Desktop client back from a later B resend.
            tx.commit().await.map_err(|_| ResultError::Database)?;
            return self.receipt_for(&origin, result_id).await;
        }
        let expires: i64 = result.get("page_expires_at");
        if result.get::<Option<i64>, _>("revoked_at").is_some() || now >= expires {
            return Err(ResultError::Inaccessible);
        }
        let row=sqlx::query("SELECT r.source_hash,o.notification_id AS initial_notice_id,o.target_account_fingerprint,o.title,o.protected_body_ciphertext,o.protected_body_nonce FROM results r JOIN notification_outbox o ON o.notification_id=r.initial_notification_id AND o.result_row_id=r.id WHERE r.id=?1")
            .bind(&result_row).fetch_one(&mut *tx).await.map_err(|_|ResultError::Database)?;
        let notification_id = Uuid::new_v4().to_string();
        let dedupe = format!("result-resend:{result_row}:{request_id}");
        let cipher = self.cipher.as_ref().ok_or(ResultError::Unavailable)?;
        let hash: String = row.get("source_hash");
        let prior = cipher
            .decrypt(
                &row.get::<Vec<u8>, _>("protected_body_nonce"),
                &row.get::<Vec<u8>, _>("protected_body_ciphertext"),
                &notice_aad(
                    device,
                    result_id,
                    &hash,
                    &row.get::<String, _>("initial_notice_id"),
                ),
            )
            .map_err(|_| ResultError::Unavailable)?;
        let (nonce, ciphertext) = cipher
            .encrypt(
                &prior,
                &notice_aad(device, result_id, &hash, &notification_id),
            )
            .map_err(|_| ResultError::Unavailable)?;
        let outbox_id = Uuid::new_v4().to_string();
        sqlx::query("INSERT OR IGNORE INTO notification_outbox(id,origin_kind,origin_key,origin_device_id,notification_id,dedupe_key,payload_hash,kind,target_account_fingerprint,title,body,body_kind,protected_body_ciphertext,protected_body_nonce,result_row_id,correlation_key,priority,status,not_before,expires_at,created_at,updated_at,sensitive_body) VALUES(?1,'device',?2,?3,?4,?5,?6,'result_link',?7,?8,'[PROTECTED_RESULT_LINK]','protected_result_link',?9,?10,?11,?11,80,'pending_channel',?12,?13,?12,?12,0)").bind(outbox_id).bind(&origin).bind(device.to_string()).bind(&notification_id).bind(&dedupe).bind(hex_digest(&prior)).bind(row.get::<String,_>("target_account_fingerprint")).bind(row.get::<String,_>("title")).bind(ciphertext).bind(nonce.to_vec()).bind(&result_row).bind(now).bind(expires).execute(&mut *tx).await.map_err(|_|ResultError::Database)?;
        sqlx::query("INSERT OR IGNORE INTO result_actions(result_row_id,action_kind,request_id,notification_id,created_at) VALUES(?1,'resend',?2,?3,?4)").bind(&result_row).bind(request_id).bind(&notification_id).bind(now).execute(&mut *tx).await.map_err(|_|ResultError::Database)?;
        let chosen_notification_id: String = sqlx::query_scalar("SELECT notification_id FROM result_actions WHERE result_row_id=?1 AND action_kind='resend' AND request_id=?2").bind(&result_row).bind(request_id).fetch_one(&mut *tx).await.map_err(|_|ResultError::Database)?;
        if chosen_notification_id == notification_id {
            sqlx::query("UPDATE results SET current_notification_id=?1,notification_status='pending_channel',updated_at=CASE WHEN updated_at>=?2 THEN updated_at+1 ELSE ?2 END WHERE id=?3").bind(&notification_id).bind(now).bind(&result_row).execute(&mut *tx).await.map_err(|_|ResultError::Database)?;
        }
        tx.commit().await.map_err(|_| ResultError::Database)?;
        self.receipt_for(&origin, result_id).await
    }
    pub async fn public_body(
        &self,
        token: &str,
    ) -> Result<(String, ResultViewMetadata), ResultError> {
        let decoded = decode_token(token)?;
        let digest = Sha256::digest(decoded);
        let row=sqlx::query("SELECT r.origin_device_id,r.result_id,r.source_hash,r.safe_title,r.completed_at,r.duration_ms,r.body_ciphertext,r.body_nonce,r.page_expires_at,r.revoked_at,d.enabled,d.revoked_at AS device_revoked FROM result_shares s JOIN results r ON r.id=s.result_row_id JOIN devices d ON d.id=r.origin_device_id WHERE s.token_digest=?1 AND s.revoked_at IS NULL").bind(digest.to_vec()).fetch_optional(&self.pool).await.map_err(|_|ResultError::Database)?.ok_or(ResultError::Inaccessible)?;
        if row.get::<i64, _>("enabled") != 1
            || row.get::<Option<i64>, _>("device_revoked").is_some()
            || row.get::<Option<i64>, _>("revoked_at").is_some()
            || now()? >= row.get("page_expires_at")
        {
            return Err(ResultError::Inaccessible);
        }
        let device = Uuid::parse_str(&row.get::<String, _>("origin_device_id"))
            .map_err(|_| ResultError::Database)?;
        let cipher = self.cipher.as_ref().ok_or(ResultError::Unavailable)?;
        let nonce = row
            .try_get::<Option<Vec<u8>>, _>("body_nonce")
            .map_err(|_| ResultError::Database)?
            .ok_or(ResultError::Inaccessible)?;
        let ciphertext = row
            .try_get::<Option<Vec<u8>>, _>("body_ciphertext")
            .map_err(|_| ResultError::Database)?
            .ok_or(ResultError::Inaccessible)?;
        let bytes = cipher
            .decrypt(
                &nonce,
                &ciphertext,
                &aad(
                    device,
                    &row.get::<String, _>("result_id"),
                    &row.get::<String, _>("source_hash"),
                    b"ResultBody",
                ),
            )
            .map_err(|_| ResultError::Unavailable)?;
        if hex_digest(&bytes) != row.get::<String, _>("source_hash") {
            return Err(ResultError::Inaccessible);
        }
        let body = String::from_utf8(bytes.to_vec()).map_err(|_| ResultError::Inaccessible)?;
        Ok((
            body,
            ResultViewMetadata {
                title: row.get("safe_title"),
                page_expires_at: row.get("page_expires_at"),
                completed_at: row.get("completed_at"),
                duration_ms: row.get("duration_ms"),
                source_hash: row.get("source_hash"),
            },
        ))
    }
    pub async fn admin_list_page(
        &self,
        limit: usize,
        after: Option<ResultAdminCursor>,
        page_state: Option<ResultAdminPageState>,
    ) -> Result<ResultAdminPage, ResultError> {
        if limit == 0 || limit > 100 {
            return Err(ResultError::Validation);
        }
        let now = now()?;
        let cursor_timestamp = after.as_ref().map(|cursor| cursor.accepted_at);
        let cursor_row_id = after.as_ref().map(|cursor| cursor.result_row_id.as_str());
        let fetch_limit = i64::try_from(limit + 1).map_err(|_| ResultError::Validation)?;
        let rows = sqlx::query("SELECT r.id AS result_row_id,r.result_id,r.origin_device_id,r.safe_title,r.source,r.content_mode,r.source_hash,r.accepted_at,r.updated_at,r.page_expires_at,r.body_bytes,r.body_purged_at,r.revoked_at,r.current_notification_id AS notification_id,COALESCE(o.status,r.notification_status) AS notification_status,COALESCE(o.attempt_count,0) AS notification_attempt_count,o.last_error_code,o.target_account_fingerprint FROM results r LEFT JOIN notification_outbox o ON o.notification_id=r.current_notification_id AND o.result_row_id=r.id WHERE (?1 IS NULL OR (?1='available' AND r.revoked_at IS NULL AND r.body_purged_at IS NULL AND r.page_expires_at>?2) OR (?1='revoked' AND r.revoked_at IS NOT NULL) OR (?1='expired' AND r.revoked_at IS NULL AND r.page_expires_at<=?2) OR (?1='content_unavailable' AND r.revoked_at IS NULL AND r.page_expires_at>?2 AND r.body_purged_at IS NOT NULL)) AND (?3 IS NULL OR r.accepted_at<?3 OR (r.accepted_at=?3 AND r.id<?4)) ORDER BY r.accepted_at DESC,r.id DESC LIMIT ?5")
            .bind(page_state.map(ResultAdminPageState::as_str))
            .bind(now)
            .bind(cursor_timestamp)
            .bind(cursor_row_id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| ResultError::Database)?;
        let has_more = rows.len() > limit;
        let items = rows
            .into_iter()
            .take(limit)
            .map(result_admin_list_item)
            .collect::<Vec<_>>();
        let next = has_more.then(|| {
            let item = items.last().expect("full result page has a final item");
            ResultAdminCursor {
                accepted_at: item.accepted_at,
                result_row_id: item.result_row_id.clone(),
            }
        });
        Ok(ResultAdminPage { items, next })
    }
    pub async fn admin_revoke(
        &self,
        result_row_id: &str,
        request_id: &str,
    ) -> Result<ResultReceipt, ResultError> {
        if request_id.is_empty()
            || request_id.len() > 128
            || request_id.chars().any(char::is_control)
        {
            return Err(ResultError::Validation);
        }
        let now = now()?;
        let mut tx = self.pool.begin().await.map_err(|_| ResultError::Database)?;
        // First statement takes the writer lock; repeated revocation leaves its
        // original timestamp/version intact. All notice cancellation is atomic.
        sqlx::query("UPDATE results SET revoked_at=?1,updated_at=MAX(updated_at+1,?1) WHERE id=?2 AND revoked_at IS NULL")
            .bind(now).bind(result_row_id).execute(&mut *tx).await.map_err(|_|ResultError::Database)?;
        let row = sqlx::query("SELECT origin_key,result_id FROM results WHERE id=?1")
            .bind(result_row_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ResultError::Database)?
            .ok_or(ResultError::NotFound)?;
        sqlx::query("UPDATE notification_outbox SET status='cancelled',claimed_at=NULL,next_attempt_at=NULL,updated_at=MAX(updated_at+1,?1) WHERE result_row_id=?2 AND claim_token IS NULL AND kind='result_link' AND status IN ('pending_channel','retry_wait','blocked_activation','blocked_reconnect')")
            .bind(now).bind(result_row_id).execute(&mut *tx).await.map_err(|_|ResultError::Database)?;
        sqlx::query("INSERT OR IGNORE INTO result_actions(result_row_id,action_kind,request_id,created_at) VALUES(?1,'revoke',?2,?3)")
            .bind(result_row_id).bind(request_id).bind(now).execute(&mut *tx).await.map_err(|_|ResultError::Database)?;
        tx.commit().await.map_err(|_| ResultError::Database)?;
        self.receipt_for(
            &row.get::<String, _>("origin_key"),
            &row.get::<String, _>("result_id"),
        )
        .await
    }
    pub async fn admin_detail(
        &self,
        result_row_id: &str,
    ) -> Result<ResultAdminDetailItem, ResultError> {
        let row = sqlx::query("SELECT r.id AS result_row_id,r.result_id,r.origin_device_id,r.safe_title,r.source,r.content_mode,r.source_hash,r.accepted_at,r.updated_at,r.page_expires_at,r.body_bytes,r.body_purged_at,r.revoked_at,r.current_notification_id AS notification_id,COALESCE(o.status,r.notification_status) AS notification_status,COALESCE(o.attempt_count,0) AS notification_attempt_count,o.last_error_code,o.target_account_fingerprint,r.correlation_key,r.result_revision,r.started_at,r.completed_at,r.duration_ms FROM results r LEFT JOIN notification_outbox o ON o.notification_id=r.current_notification_id AND o.result_row_id=r.id WHERE r.id=?1")
            .bind(result_row_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| ResultError::Database)?
            .ok_or(ResultError::NotFound)?;
        let item = ResultAdminListItem {
            result_row_id: row.get("result_row_id"),
            result_id: row.get("result_id"),
            owner_device_id: row.get("origin_device_id"),
            safe_title: row.get("safe_title"),
            source: row.get("source"),
            content_mode: row.get("content_mode"),
            source_hash: row.get("source_hash"),
            accepted_at: row.get("accepted_at"),
            updated_at: row.get("updated_at"),
            page_expires_at: row.get("page_expires_at"),
            body_bytes: row.get("body_bytes"),
            body_purged_at: row.get("body_purged_at"),
            revoked_at: row.get("revoked_at"),
            notification_id: row.get("notification_id"),
            notification_status: row.get("notification_status"),
            notification_attempt_count: row.get("notification_attempt_count"),
            notification_last_error_code: row.get("last_error_code"),
            target_account_fingerprint: row.get("target_account_fingerprint"),
        };
        Ok(ResultAdminDetailItem {
            item,
            correlation_key: row.get("correlation_key"),
            result_revision: row.get("result_revision"),
            started_at: row.get("started_at"),
            completed_at: row.get("completed_at"),
            duration_ms: row.get("duration_ms"),
        })
    }
    async fn existing(
        &self,
        origin: &str,
        result_id: &str,
        dedupe: &str,
        digest: &[u8; 32],
    ) -> Result<Option<ResultReceipt>, ResultError> {
        let rows=sqlx::query("SELECT r.result_id,r.request_digest,r.source_hash,r.accepted_at,r.updated_at,r.page_expires_at,r.current_notification_id,COALESCE(o.status,r.notification_status) AS notification_status,r.revoked_at,r.body_purged_at FROM results r LEFT JOIN notification_outbox o ON o.notification_id=r.current_notification_id AND o.result_row_id=r.id WHERE r.origin_key=?1 AND (r.result_id=?2 OR r.dedupe_key=?3)").bind(origin).bind(result_id).bind(dedupe).fetch_all(&self.pool).await.map_err(|_|ResultError::Database)?;
        if rows.is_empty() {
            return Ok(None);
        }
        if rows.len() != 1 || rows[0].get::<Vec<u8>, _>("request_digest") != digest {
            return Err(ResultError::Conflict);
        }
        let r = &rows[0];
        let observed_at = now()?;
        Ok(Some(ResultReceipt {
            schema_version: 1,
            result_id: r.get("result_id"),
            source_hash: r.get("source_hash"),
            accepted_at: r.get("accepted_at"),
            updated_at: r.get::<i64, _>("updated_at").max(
                if observed_at >= r.get::<i64, _>("page_expires_at") {
                    r.get("page_expires_at")
                } else {
                    0
                },
            ),
            page_state: if r.get::<Option<i64>, _>("revoked_at").is_some() {
                PageState::Revoked
            } else if observed_at >= r.get("page_expires_at") {
                PageState::Expired
            } else if r.get::<Option<i64>, _>("body_purged_at").is_some() {
                PageState::ContentUnavailable
            } else {
                PageState::Available
            },
            page_expires_at: r.get("page_expires_at"),
            notification_id: r.get("current_notification_id"),
            notification_status: r.get("notification_status"),
        }))
    }
    async fn receipt_for(
        &self,
        origin: &str,
        result_id: &str,
    ) -> Result<ResultReceipt, ResultError> {
        let rows=sqlx::query("SELECT r.result_id,r.request_digest,r.source_hash,r.accepted_at,r.updated_at,r.page_expires_at,r.current_notification_id,COALESCE(o.status,r.notification_status) AS notification_status,r.revoked_at,r.body_purged_at FROM results r LEFT JOIN notification_outbox o ON o.notification_id=r.current_notification_id AND o.result_row_id=r.id WHERE r.origin_key=?1 AND r.result_id=?2").bind(origin).bind(result_id).fetch_all(&self.pool).await.map_err(|_|ResultError::Database)?;
        if rows.is_empty() {
            return Err(ResultError::NotFound);
        }
        let r = &rows[0];
        let observed_at = now()?;
        Ok(ResultReceipt {
            schema_version: 1,
            result_id: r.get("result_id"),
            source_hash: r.get("source_hash"),
            accepted_at: r.get("accepted_at"),
            updated_at: r.get::<i64, _>("updated_at").max(
                if observed_at >= r.get::<i64, _>("page_expires_at") {
                    r.get("page_expires_at")
                } else {
                    0
                },
            ),
            page_state: if r.get::<Option<i64>, _>("revoked_at").is_some() {
                PageState::Revoked
            } else if observed_at >= r.get("page_expires_at") {
                PageState::Expired
            } else if r.get::<Option<i64>, _>("body_purged_at").is_some() {
                PageState::ContentUnavailable
            } else {
                PageState::Available
            },
            page_expires_at: r.get("page_expires_at"),
            notification_id: r.get("current_notification_id"),
            notification_status: r.get("notification_status"),
        })
    }
    fn digest(&self, input: &ResultPublicationV1) -> Result<[u8; 32], ResultError> {
        let cipher = self.cipher.as_ref().ok_or(ResultError::Unavailable)?;
        let mut framed = b"PromptDock.ResultPublication.v1\0".to_vec();
        framed.extend(serde_json::to_vec(input).map_err(|_| ResultError::Validation)?);
        Ok(cipher.request_digest(&framed))
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultPublicationV1 {
    pub schema_version: i64,
    pub result_id: String,
    pub dedupe_key: String,
    pub kind: String,
    pub content_mode: String,
    pub source: String,
    pub correlation_key: String,
    pub result_revision: String,
    pub title: String,
    pub body: String,
    pub source_hash: String,
    pub created_at: i64,
    pub notification_expires_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
}
impl ResultPublicationV1 {
    fn validate(&self, max: usize) -> Result<(), ResultError> {
        const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
        let valid_time = |value: i64| (0..=MAX_SAFE_INTEGER).contains(&value);
        let valid_key = |value: &str| !value.chars().any(|c| c.is_control() || c.is_whitespace());
        if self.schema_version != 1
            || self.kind != "run_completed"
            || self.content_mode != "full_final"
            || self.source != "codex_stop"
            || self.result_id.is_empty()
            || self.result_id.len() > 128
            || !valid_key(&self.result_id)
            || self.dedupe_key.is_empty()
            || self.dedupe_key.len() > 256
            || !valid_key(&self.dedupe_key)
            || self.correlation_key.is_empty()
            || self.correlation_key.len() > 128
            || !valid_key(&self.correlation_key)
            || self.result_revision.is_empty()
            || self.result_revision.len() > 20
            || !self.result_revision.bytes().all(|x| x.is_ascii_digit())
            || self.result_revision.starts_with('0')
            || self.result_revision.parse::<i64>().is_err()
            || self.title.trim().is_empty()
            || self.title.chars().count() > 128
            || self.title.chars().any(char::is_control)
            || self.body.len() > max
            || self.source_hash.len() != 64
            || !self
                .source_hash
                .bytes()
                .all(|x| x.is_ascii_lowercase() || x.is_ascii_digit())
            || hex_digest(self.body.as_bytes()) != self.source_hash
            || !valid_time(self.created_at)
            || !valid_time(self.notification_expires_at)
            || self.notification_expires_at <= self.created_at
            || self.started_at.is_some_and(|v| !valid_time(v))
            || self.completed_at.is_some_and(|v| !valid_time(v))
            || self.duration_ms.is_some_and(|v| !valid_time(v))
        {
            return Err(ResultError::Validation);
        }
        Ok(())
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultReceipt {
    pub schema_version: i64,
    pub result_id: String,
    pub source_hash: String,
    pub accepted_at: i64,
    pub updated_at: i64,
    pub page_state: PageState,
    pub page_expires_at: i64,
    pub notification_id: String,
    pub notification_status: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultLink {
    pub url: String,
    pub expires_at: i64,
}
#[derive(Clone, Debug)]
pub struct ResultViewMetadata {
    pub title: String,
    pub page_expires_at: i64,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub source_hash: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultAdminListItem {
    pub result_row_id: String,
    pub result_id: String,
    pub owner_device_id: String,
    pub safe_title: String,
    pub source: String,
    pub content_mode: String,
    pub source_hash: String,
    pub accepted_at: i64,
    pub updated_at: i64,
    pub page_expires_at: i64,
    pub body_bytes: i64,
    pub body_purged_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub notification_id: String,
    pub notification_status: String,
    pub notification_attempt_count: i64,
    pub notification_last_error_code: Option<String>,
    pub target_account_fingerprint: Option<String>,
}

pub struct ResultAdminPage {
    pub items: Vec<ResultAdminListItem>,
    pub next: Option<ResultAdminCursor>,
}

#[derive(Clone, Debug)]
pub struct ResultAdminCursor {
    pub accepted_at: i64,
    pub result_row_id: String,
}

#[derive(Clone, Copy, Debug)]
pub enum ResultAdminPageState {
    Available,
    Revoked,
    Expired,
    ContentUnavailable,
}

impl ResultAdminPageState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::ContentUnavailable => "content_unavailable",
        }
    }
}

fn result_admin_list_item(row: sqlx::sqlite::SqliteRow) -> ResultAdminListItem {
    ResultAdminListItem {
        result_row_id: row.get("result_row_id"),
        result_id: row.get("result_id"),
        owner_device_id: row.get("origin_device_id"),
        safe_title: row.get("safe_title"),
        source: row.get("source"),
        content_mode: row.get("content_mode"),
        source_hash: row.get("source_hash"),
        accepted_at: row.get("accepted_at"),
        updated_at: row.get("updated_at"),
        page_expires_at: row.get("page_expires_at"),
        body_bytes: row.get("body_bytes"),
        body_purged_at: row.get("body_purged_at"),
        revoked_at: row.get("revoked_at"),
        notification_id: row.get("notification_id"),
        notification_status: row.get("notification_status"),
        notification_attempt_count: row.get("notification_attempt_count"),
        notification_last_error_code: row.get("last_error_code"),
        target_account_fingerprint: row.get("target_account_fingerprint"),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultAdminDetailItem {
    #[serde(flatten)]
    pub item: ResultAdminListItem,
    pub correlation_key: String,
    pub result_revision: String,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PageState {
    Available,
    Revoked,
    Expired,
    ContentUnavailable,
}
#[derive(Debug, Error)]
pub enum ResultError {
    #[error("validation")]
    Validation,
    #[error("conflict")]
    Conflict,
    #[error("not found")]
    NotFound,
    #[error("inaccessible")]
    Inaccessible,
    #[error("target unavailable")]
    TargetUnavailable,
    #[error("unavailable")]
    Unavailable,
    #[error("capacity")]
    Capacity,
    #[error("database")]
    Database,
    #[error("clock")]
    Clock,
}
fn now() -> Result<i64, ResultError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ResultError::Clock)
        .and_then(|d| i64::try_from(d.as_millis()).map_err(|_| ResultError::Clock))
}
fn aad(device: Uuid, id: &str, hash: &str, purpose: &[u8]) -> Vec<u8> {
    [purpose, device.as_bytes(), id.as_bytes(), hash.as_bytes()].concat()
}
fn notice_aad(device: Uuid, id: &str, hash: &str, notification_id: &str) -> Vec<u8> {
    [
        b"ResultLinkNotice".as_slice(),
        device.as_bytes(),
        id.as_bytes(),
        hash.as_bytes(),
        notification_id.as_bytes(),
    ]
    .concat()
}
fn result_link_notice(title: &str, origin: &str, token: &str) -> String {
    format!(
        "📄 {title}\n\n完整结果已保存，可点击下方链接查看。\n\n查看完整结果：\n{origin}/r/{token}"
    )
}
fn hex_digest(v: &[u8]) -> String {
    format!("{:x}", Sha256::digest(v))
}
fn decode_token(value: &str) -> Result<Vec<u8>, ResultError> {
    if value.len() != 43 {
        return Err(ResultError::Inaccessible);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ResultError::Inaccessible)?;
    if bytes.len() != 32 {
        return Err(ResultError::Inaccessible);
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::DatabaseConfig, db, outbox::BundleContentCipher};

    async fn service() -> (tempfile::TempDir, ResultService, Uuid) {
        let directory = tempfile::tempdir().expect("temp directory");
        let pool = db::open(&DatabaseConfig {
            path: directory.path().join("results.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let device = Uuid::new_v4();
        sqlx::query("INSERT INTO devices(id,name,token_hash,enabled,created_at) VALUES(?1,'RESULTS',?2,1,?3)")
            .bind(device.to_string()).bind(vec![0u8;32]).bind(now().expect("clock"))
            .execute(&pool).await.expect("device");
        let config = ResultsConfig {
            enabled: true,
            public_origin: Some("https://relay.example.test".to_owned()),
            ..ResultsConfig::default()
        };
        (
            directory,
            ResultService::new(pool, config, Some(BundleContentCipher::new([7; 32]))),
            device,
        )
    }
    fn request(id: &str, body: &str) -> ResultPublicationV1 {
        ResultPublicationV1 {
            schema_version: 1,
            result_id: id.to_owned(),
            dedupe_key: format!("dedupe-{id}"),
            kind: "run_completed".to_owned(),
            content_mode: "full_final".to_owned(),
            source: "codex_stop".to_owned(),
            correlation_key: format!("run-{id}"),
            result_revision: "1".to_owned(),
            title: "Codex 最终回答".to_owned(),
            body: body.to_owned(),
            source_hash: hex_digest(body.as_bytes()),
            created_at: 1,
            notification_expires_at: now().expect("clock") + DAY_MS,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }
    }
    #[test]
    fn link_notice_uses_publication_title_and_contains_exactly_one_result_link() {
        let notice = result_link_notice(
            "Codex 本轮输出｜Codex 已用47%/剩余53%",
            "https://relay.example.test",
            "fixture-token",
        );
        assert!(notice.starts_with("📄 Codex 本轮输出｜Codex 已用47%/剩余53%"));
        assert_eq!(notice.matches("https://relay.example.test/r/").count(), 1);
    }
    #[tokio::test]
    async fn revoke_is_atomic_and_retry_keeps_the_first_revocation_version() {
        let (_directory, service, device) = service().await;
        let input = request("atomic-revoke", "revocation body");
        let before = service
            .publish(device, input, Some(format!("wx:{}", "e".repeat(64))))
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER reject_revoke_action BEFORE INSERT ON result_actions WHEN NEW.action_kind='revoke' BEGIN SELECT RAISE(ABORT,'injected action failure'); END")
            .execute(&service.pool).await.unwrap();
        assert!(
            service
                .revoke(device, "atomic-revoke", "same-intent")
                .await
                .is_err()
        );
        let unchanged = service.status(device, "atomic-revoke").await.unwrap();
        assert!(matches!(unchanged.page_state, PageState::Available));
        assert_eq!(unchanged.notification_status, "pending_channel");
        assert_eq!(unchanged.updated_at, before.updated_at);
        sqlx::query("DROP TRIGGER reject_revoke_action")
            .execute(&service.pool)
            .await
            .unwrap();
        let revoked = service
            .revoke(device, "atomic-revoke", "same-intent")
            .await
            .unwrap();
        assert!(matches!(revoked.page_state, PageState::Revoked));
        assert_eq!(revoked.notification_status, "cancelled");
        assert!(revoked.updated_at > before.updated_at);
        let replay = service
            .revoke(device, "atomic-revoke", "same-intent")
            .await
            .unwrap();
        assert_eq!(replay.updated_at, revoked.updated_at);
        let actions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM result_actions")
            .fetch_one(&service.pool)
            .await
            .unwrap();
        assert_eq!(actions, 1);
    }
    #[tokio::test]
    async fn publish_is_owner_scoped_byte_exact_and_replay_does_not_require_current_target() {
        let (_dir, service, device) = service().await;
        let body = "中文\r\n```rust\nlet x = \"</script>\";\n```\n🙂";
        let request = request("result-1", body);
        let first = service
            .publish(
                device,
                request.clone(),
                Some(format!("wx:{}", "a".repeat(64))),
            )
            .await
            .expect("publish");
        let replay = service
            .publish(device, request, None)
            .await
            .expect("replay");
        assert_eq!(first.notification_id, replay.notification_id);
        let link = service.link(device, "result-1").await.expect("link");
        let token = link.url.rsplit('/').next().expect("token");
        assert_eq!(service.public_body(token).await.expect("raw").0, body);
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox WHERE kind='result_link'")
                .fetch_one(&service.pool)
                .await
                .expect("count");
        assert_eq!(
            count, 1,
            "replay must not create another logical notification"
        );
    }
    #[tokio::test]
    async fn revoked_or_disabled_owner_cannot_read_a_share() {
        let (_dir, first_service, first_device) = service().await;
        first_service
            .publish(
                first_device,
                request("result-2", "body"),
                Some(format!("wx:{}", "b".repeat(64))),
            )
            .await
            .expect("publish");
        let token = first_service
            .link(first_device, "result-2")
            .await
            .expect("link")
            .url
            .rsplit('/')
            .next()
            .expect("token")
            .to_owned();
        first_service
            .revoke(first_device, "result-2", "revoke-1")
            .await
            .expect("revoke");
        assert!(matches!(
            first_service.public_body(&token).await,
            Err(ResultError::Inaccessible)
        ));
        let (_dir2, second_service, second_device) = service().await;
        second_service
            .publish(
                second_device,
                request("result-3", "body"),
                Some(format!("wx:{}", "c".repeat(64))),
            )
            .await
            .expect("publish");
        let token = second_service
            .link(second_device, "result-3")
            .await
            .expect("link")
            .url
            .rsplit('/')
            .next()
            .expect("token")
            .to_owned();
        sqlx::query("UPDATE devices SET enabled=0 WHERE id=?1")
            .bind(second_device.to_string())
            .execute(&second_service.pool)
            .await
            .expect("disable");
        assert!(matches!(
            second_service.public_body(&token).await,
            Err(ResultError::Inaccessible)
        ));
    }
    #[tokio::test]
    async fn protected_notice_stays_ciphertext_at_rest_and_worker_sends_one_link() {
        use crate::outbox::{FakeChannel, OutboxService, OutboxWorker};
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;
        let (_dir, service, device) = service().await;
        let pending = service
            .publish(
                device,
                request("result-worker", "secret body"),
                Some(format!("wx:{}", "d".repeat(64))),
            )
            .await
            .expect("publish");
        let row=sqlx::query("SELECT body,body_kind,protected_body_ciphertext,protected_body_nonce FROM notification_outbox WHERE kind='result_link'").fetch_one(&service.pool).await.expect("notice");
        assert_eq!(row.get::<String, _>("body"), "[PROTECTED_RESULT_LINK]");
        assert_eq!(row.get::<String, _>("body_kind"), "protected_result_link");
        assert!(
            row.get::<Vec<u8>, _>("protected_body_ciphertext")
                .windows(b"/r/".len())
                .all(|part| part != b"/r/")
        );
        assert_eq!(row.get::<Vec<u8>, _>("protected_body_nonce").len(), 24);
        let channel = Arc::new(FakeChannel::default());
        let worker = OutboxWorker::new(
            OutboxService::new(service.pool.clone())
                .with_bundle_cipher(BundleContentCipher::new([7; 32])),
            channel.clone(),
        );
        assert!(
            worker
                .run_once(CancellationToken::new())
                .await
                .expect("worker")
        );
        assert_eq!(channel.sent_count(), 1);
        let accepted = service
            .status(device, "result-worker")
            .await
            .expect("accepted receipt");
        assert_eq!(accepted.notification_status, "provider_accepted");
        assert!(accepted.updated_at > pending.updated_at);
    }
    #[tokio::test]
    async fn concurrent_identical_publish_commits_one_result_share_and_notice() {
        let (_dir, service, device) = service().await;
        let request = request("result-race", "body");
        let target = Some(format!("wx:{}", "e".repeat(64)));
        let (left, right) = tokio::join!(
            service.publish(device, request.clone(), target.clone()),
            service.publish(device, request, target)
        );
        let left = left.expect("left");
        let right = right.expect("right");
        assert_eq!(left.notification_id, right.notification_id);
        let results: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM results")
            .fetch_one(&service.pool)
            .await
            .expect("results");
        let shares: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM result_shares")
            .fetch_one(&service.pool)
            .await
            .expect("shares");
        assert_eq!(results, 1);
        assert_eq!(shares, 1);
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox WHERE kind='result_link'")
                .fetch_one(&service.pool)
                .await
                .expect("notice");
        assert_eq!(count, 1);
    }
    #[tokio::test]
    async fn resend_is_request_idempotent_and_late_replay_returns_current_notice() {
        let (_dir, service, device) = service().await;
        service
            .publish(
                device,
                request("result-resend", "body"),
                Some(format!("wx:{}", "f".repeat(64))),
            )
            .await
            .expect("publish");
        let first = service
            .resend(device, "result-resend", "request-a")
            .await
            .expect("first resend");
        let second = service
            .resend(device, "result-resend", "request-b")
            .await
            .expect("second resend");
        let late = service
            .resend(device, "result-resend", "request-a")
            .await
            .expect("late replay");
        assert_ne!(first.notification_id, second.notification_id);
        assert_eq!(late.notification_id, second.notification_id);
        let notices: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox WHERE kind='result_link'")
                .fetch_one(&service.pool)
                .await
                .expect("notice count");
        assert_eq!(notices, 3);
    }
    #[tokio::test]
    async fn failed_notice_insert_rolls_back_result_and_share() {
        let (_dir, service, device) = service().await;
        let failure = service
            .publish(
                device,
                request("result-rollback", "body"),
                Some("not-a-frozen-target".to_owned()),
            )
            .await;
        assert!(matches!(failure, Err(ResultError::Database)));
        let results: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM results")
            .fetch_one(&service.pool)
            .await
            .expect("results");
        let shares: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM result_shares")
            .fetch_one(&service.pool)
            .await
            .expect("shares");
        let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
            .fetch_one(&service.pool)
            .await
            .expect("notices");
        assert_eq!((results, shares, notices), (0, 0, 0));
    }
    #[tokio::test]
    async fn digest_covers_optional_metadata_and_replay_precedes_lowered_limit() {
        let (_dir, mut service, device) = service().await;
        let original = request("result-metadata", "small body");
        service
            .publish(
                device,
                original.clone(),
                Some(format!("wx:{}", "a".repeat(64))),
            )
            .await
            .expect("publish");
        let mut different = original.clone();
        different.completed_at = Some(2);
        different.duration_ms = Some(1);
        assert!(matches!(
            service
                .publish(device, different, Some(format!("wx:{}", "a".repeat(64))))
                .await,
            Err(ResultError::Conflict)
        ));
        service.config.max_body_bytes = 1;
        assert!(service.publish(device, original, None).await.is_ok());
    }
    #[tokio::test]
    async fn publication_rejects_negative_times_and_non_decimal_revision() {
        let (_dir, service, device) = service().await;
        let mut negative = request("result-negative", "body");
        negative.created_at = -1;
        assert!(matches!(
            service
                .publish(device, negative, Some(format!("wx:{}", "a".repeat(64))))
                .await,
            Err(ResultError::Validation)
        ));
        let mut revision = request("result-revision", "body");
        revision.result_revision = "9223372036854775808".to_owned();
        assert!(matches!(
            service
                .publish(device, revision, Some(format!("wx:{}", "a".repeat(64))))
                .await,
            Err(ResultError::Validation)
        ));
    }
}
