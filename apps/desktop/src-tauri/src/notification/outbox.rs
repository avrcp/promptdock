use rusqlite::{params, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::db::{now_ms, Db};
use crate::error::AppError;
use crate::model::{NotificationBackendKind, ResultContentMode};
use crate::notification::hold::HoldController;
use crate::notification::{
    attention_notification_policy_fingerprint, run_notification_policy_fingerprint,
    MAX_NOTIFICATION_CHARS,
};
use crate::relay::protocol::RelayRemoteNotificationState;
use crate::settings::SettingsStore;

pub const NOTIFICATION_PAYLOAD_SCHEMA_VERSION: u16 = 2;
const STALE_CLAIM_MS: i64 = 5 * 60 * 1_000;
const RUN_STARTED_TTL_MS: i64 = 10 * 60 * 1_000;
const RUN_TERMINAL_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
const ATTENTION_REQUIRED_TTL_MS: i64 = 30 * 60 * 1_000;
const CONTROL_TTL_MS: i64 = 5 * 60 * 1_000;
const DELAYED_DELIVERY_GRACE_MS: i64 = 60_000;
const WORKER_IDLE_POLL: Duration = Duration::from_millis(250);
const OUTBOX_TERMINAL_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const OUTBOX_DEAD_LETTER_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1_000;
const OUTBOX_RETENTION_BATCH_SIZE: i64 = 500;
const OUTBOX_RETENTION_INTERVAL: Duration = Duration::from_secs(60);

const NETWORK_BACKOFF_MS: &[i64] = &[5_000, 30_000, 120_000, 600_000, 1_800_000];
const RATE_LIMIT_BACKOFF_MS: &[i64] = &[30_000, 120_000, 600_000];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationEventKind {
    Test,
    RunStarted,
    RunCompleted,
    RunFailed,
    RunInterrupted,
    AttentionRequired,
}

impl NotificationEventKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::RunStarted => "run_started",
            Self::RunCompleted => "run_completed",
            Self::RunFailed => "run_failed",
            Self::RunInterrupted => "run_interrupted",
            Self::AttentionRequired => "attention_required",
        }
    }

    fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "test" => Ok(Self::Test),
            "run_started" => Ok(Self::RunStarted),
            "run_completed" => Ok(Self::RunCompleted),
            "run_failed" => Ok(Self::RunFailed),
            "run_interrupted" => Ok(Self::RunInterrupted),
            "attention_required" => Ok(Self::AttentionRequired),
            _ => Err(AppError::new("OUTBOX_CORRUPT", "通知队列包含未知事件")),
        }
    }

    fn priority(self) -> i64 {
        match self {
            Self::AttentionRequired => 120,
            Self::Test => 90,
            Self::RunCompleted | Self::RunFailed | Self::RunInterrupted => 80,
            Self::RunStarted => 50,
        }
    }

    fn ttl_ms(self) -> i64 {
        match self {
            Self::Test => CONTROL_TTL_MS,
            Self::RunStarted => RUN_STARTED_TTL_MS,
            Self::RunCompleted | Self::RunFailed | Self::RunInterrupted => RUN_TERMINAL_TTL_MS,
            Self::AttentionRequired => ATTENTION_REQUIRED_TTL_MS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationPayloadV2 {
    pub schema_version: u16,
    pub agent_label: String,
    pub workspace_label: String,
    pub model_name: Option<String>,
    pub title: String,
    pub body: String,
    pub content_mode: ResultContentMode,
    pub result_revision: Option<i64>,
    pub source_hash: Option<String>,
    pub content_bytes: usize,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub delayed_delivery: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotificationHistoryItem {
    pub id: String,
    pub event_kind: NotificationEventKind,
    pub status: String,
    pub delivery_backend: NotificationBackendKind,
    pub acceptance_stage: Option<TransportAcceptanceStage>,
    pub payload: NotificationPayloadV2,
    pub body_available: bool,
    pub attempt_count: i64,
    pub last_error_code: Option<String>,
    pub provider_message_id: Option<String>,
    pub relay_notification_id: Option<String>,
    pub relay_accepted_at: Option<i64>,
    pub remote_status: Option<String>,
    pub remote_updated_at: Option<i64>,
    pub remote_provider_message_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub delivered_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotificationHistoryPage {
    pub items: Vec<NotificationHistoryItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotificationOutboxStats {
    pub pending_notifications: u32,
    pub blocked_notifications: u32,
    pub pending_count: u32,
    pub sending_count: u32,
    pub retry_count: u32,
    pub blocked_activation_count: u32,
    pub blocked_reconnect_count: u32,
    pub dead_letter_count: u32,
    pub last_delivery_at: Option<i64>,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationDeliveryState {
    pub status: String,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationAcceptanceReceipt {
    pub status: String,
    pub acceptance_stage: Option<TransportAcceptanceStage>,
    pub relay_notification_id: Option<String>,
    pub relay_accepted_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelayReconciliationCandidate {
    pub outbox_id: String,
    pub notification_id: String,
}

pub(crate) struct RelayRemoteStatusUpdate<'a> {
    pub outbox_id: &'a str,
    pub notification_id: &'a str,
    pub status: &'a str,
    pub updated_at: i64,
    pub provider_message_id: Option<&'a str>,
}

impl NotificationPayloadV2 {
    pub(crate) fn for_run(
        agent_label: &str,
        workspace_label: Option<&str>,
        model_name: Option<&str>,
        started_at: Option<i64>,
        completed_at: Option<i64>,
        duration_ms: Option<i64>,
    ) -> Self {
        Self {
            schema_version: NOTIFICATION_PAYLOAD_SCHEMA_VERSION,
            agent_label: sanitize(agent_label),
            workspace_label: workspace_label.map(sanitize).unwrap_or_default(),
            model_name: model_name.and_then(sanitize_optional),
            title: String::new(),
            body: String::new(),
            content_mode: ResultContentMode::StatusOnly,
            result_revision: None,
            source_hash: None,
            content_bytes: 0,
            started_at,
            completed_at,
            duration_ms,
            delayed_delivery: false,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != NOTIFICATION_PAYLOAD_SCHEMA_VERSION
            || self.agent_label.chars().count() > 80
            || self.agent_label.chars().any(char::is_control)
            || self.workspace_label.chars().count() > 80
            || self.workspace_label.chars().any(char::is_control)
            || self.model_name.as_deref().is_some_and(|model| {
                model.chars().count() > 80 || model.chars().any(char::is_control)
            })
            || self.title.chars().count() > 160
            || self.title.chars().any(char::is_control)
            || !self.valid_content_contract()
        {
            return Err(AppError::new(
                "INVALID_NOTIFICATION_PAYLOAD",
                "通知元数据无效",
            ));
        }
        Ok(())
    }

    fn valid_content_contract(&self) -> bool {
        match self.content_mode {
            ResultContentMode::FullFinal if self.source_hash.is_some() => {
                self.result_revision.is_some_and(|revision| revision > 0)
                    && self.content_bytes == self.body.len()
                    && self.body.len() <= crate::agent::MAX_RESULT_CONTENT_BYTES
                    && self.source_hash.as_deref()
                        == Some(crate::agent::source_hash_sha256(&self.body).as_str())
            }
            ResultContentMode::FullFinal => {
                self.result_revision.is_some_and(|revision| revision > 0)
                    && self.content_bytes == 0
                    && valid_notification_text(&self.body, MAX_NOTIFICATION_CHARS)
            }
            ResultContentMode::RedactedExcerpt => {
                self.result_revision.is_some_and(|revision| revision > 0)
                    && self.content_bytes <= crate::agent::MAX_RESULT_CONTENT_BYTES
                    && valid_notification_text(&self.body, MAX_NOTIFICATION_CHARS)
            }
            ResultContentMode::StatusOnly => {
                self.result_revision.is_none_or(|revision| revision > 0)
                    && self.source_hash.is_none()
                    && self.content_bytes == 0
                    && valid_notification_text(&self.body, MAX_NOTIFICATION_CHARS)
            }
        }
    }

    fn validate_for_kind(&self, kind: NotificationEventKind) -> Result<(), AppError> {
        self.validate()?;
        if matches!(
            kind,
            NotificationEventKind::RunStarted
                | NotificationEventKind::RunCompleted
                | NotificationEventKind::RunFailed
                | NotificationEventKind::RunInterrupted
                | NotificationEventKind::AttentionRequired
        ) && (self.title.trim().is_empty()
            || (self.body.trim().is_empty()
                && !(matches!(self.content_mode, ResultContentMode::FullFinal)
                    && self.source_hash.is_some())))
        {
            return Err(AppError::new(
                "INVALID_NOTIFICATION_PAYLOAD",
                "任务通知缺少冻结正文",
            ));
        }
        Ok(())
    }
}

fn valid_notification_text(text: &str, max_chars: usize) -> bool {
    text.chars().count() <= max_chars
        && !text
            .chars()
            .any(|character| character.is_control() && character != '\n')
}

#[derive(Clone)]
pub(crate) struct OutboxRepository {
    db: Arc<Db>,
}

pub(crate) struct RunNotificationInsert<'a> {
    pub agent_run_key: &'a str,
    pub kind: NotificationEventKind,
    pub dedupe_key: &'a str,
    pub payload: &'a NotificationPayloadV2,
    pub content_policy_hash: &'a str,
    pub created_at: i64,
    pub not_before: i64,
}

pub(crate) struct AttentionNotificationInsert<'a> {
    pub agent_run_key: Option<&'a str>,
    pub dedupe_key: &'a str,
    pub payload: &'a NotificationPayloadV2,
    pub content_policy_hash: &'a str,
    pub created_at: i64,
    pub observed_at: i64,
    pub expires_at: Option<i64>,
}

struct NotificationInsert<'a> {
    agent_run_key: Option<&'a str>,
    kind: NotificationEventKind,
    delivery_backend: NotificationBackendKind,
    dedupe_key: &'a str,
    payload: &'a NotificationPayloadV2,
    content_policy_hash: Option<&'a str>,
    created_at: i64,
    not_before: i64,
    expires_at: Option<i64>,
}

impl OutboxRepository {
    pub(crate) fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    #[cfg(test)]
    pub(crate) fn db(&self) -> &Arc<Db> {
        &self.db
    }

    pub(crate) fn hold_controller(&self) -> HoldController {
        HoldController::new(Arc::clone(&self.db))
    }

    pub(crate) fn insert_tx(
        tx: &Transaction<'_>,
        input: RunNotificationInsert<'_>,
    ) -> Result<bool, AppError> {
        let delivery_backend = selected_delivery_backend_tx(tx)?;
        insert_outbox(
            tx,
            NotificationInsert {
                agent_run_key: Some(input.agent_run_key),
                kind: input.kind,
                delivery_backend,
                dedupe_key: input.dedupe_key,
                payload: input.payload,
                content_policy_hash: Some(input.content_policy_hash),
                created_at: input.created_at,
                not_before: input.not_before,
                expires_at: None,
            },
        )
    }

    pub(crate) fn insert_attention_tx(
        tx: &Transaction<'_>,
        input: AttentionNotificationInsert<'_>,
    ) -> Result<bool, AppError> {
        let delivery_backend = selected_delivery_backend_tx(tx)?;
        let expires_at = input
            .expires_at
            .unwrap_or_else(|| input.created_at.saturating_add(ATTENTION_REQUIRED_TTL_MS));
        if expires_at <= input.created_at {
            return Err(AppError::new(
                "INVALID_NOTIFICATION_EXPIRY",
                "操作提醒的有效期无效",
            ));
        }
        let inserted = insert_outbox(
            tx,
            NotificationInsert {
                agent_run_key: input.agent_run_key,
                kind: NotificationEventKind::AttentionRequired,
                delivery_backend,
                dedupe_key: input.dedupe_key,
                payload: input.payload,
                content_policy_hash: Some(input.content_policy_hash),
                created_at: input.created_at,
                not_before: input.created_at,
                expires_at: Some(expires_at),
            },
        )?;
        if inserted && expires_at <= input.observed_at {
            tx.execute(
                "UPDATE notification_outbox
                 SET status = 'expired', last_error_code = 'NOTIFICATION_EXPIRED',
                     last_error_message = 'Attention request expired before ingestion',
                     updated_at = ?
                 WHERE dedupe_key = ? AND status = 'pending'",
                params![input.observed_at, input.dedupe_key],
            )?;
            return Ok(false);
        }
        Ok(inserted)
    }

    pub(crate) fn cancel_pending_run_started_tx(
        tx: &Transaction<'_>,
        run_key: &str,
        now: i64,
        reason_code: &'static str,
        reason_message: &'static str,
    ) -> Result<usize, AppError> {
        tx.execute(
            "UPDATE notification_outbox
             SET status = 'cancelled', claimed_at = NULL, next_attempt_at = NULL,
                 last_error_code = ?, last_error_message = ?, updated_at = ?
             WHERE agent_run_key = ? AND event_kind = 'run_started'
               AND status IN ('pending', 'retry_wait',
                              'blocked_activation', 'blocked_reconnect')",
            params![reason_code, reason_message, now, run_key],
        )
        .map_err(AppError::from)
    }

    pub(crate) fn cancel_pending_run_completed_tx(
        tx: &Transaction<'_>,
        run_key: &str,
        now: i64,
        reason_code: &'static str,
        reason_message: &'static str,
    ) -> Result<usize, AppError> {
        tx.execute(
            "UPDATE notification_outbox
             SET status = 'cancelled', claimed_at = NULL, next_attempt_at = NULL,
                 last_error_code = ?, last_error_message = ?, updated_at = ?
             WHERE agent_run_key = ? AND event_kind = 'run_completed'
               AND status IN ('pending', 'retry_wait',
                              'blocked_activation', 'blocked_reconnect')",
            params![reason_code, reason_message, now, run_key],
        )
        .map_err(AppError::from)
    }

    pub(crate) fn enqueue_control(
        &self,
        kind: NotificationEventKind,
        dedupe_key: &str,
        payload: &NotificationPayloadV2,
        now: i64,
    ) -> Result<bool, AppError> {
        if kind != NotificationEventKind::Test {
            return Err(AppError::new(
                "INVALID_NOTIFICATION_KIND",
                "控制通知类型无效",
            ));
        }
        payload.validate_for_kind(kind)?;
        self.db.with_transaction(|tx| {
            insert_outbox(
                tx,
                NotificationInsert {
                    agent_run_key: None,
                    kind,
                    delivery_backend: NotificationBackendKind::Relay,
                    dedupe_key,
                    payload,
                    content_policy_hash: None,
                    created_at: now,
                    not_before: now,
                    expires_at: None,
                },
            )
        })
    }

    pub(crate) fn stats(&self) -> Result<NotificationOutboxStats, AppError> {
        self.db.with_connection(|conn| {
            conn.query_row(
                "SELECT
                    SUM(CASE WHEN status IN ('pending', 'sending', 'retry_wait') THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status IN ('blocked_activation', 'blocked_reconnect') THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'sending' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'retry_wait' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'blocked_activation' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'blocked_reconnect' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'dead_letter' THEN 1 ELSE 0 END),
                    MAX(delivered_at),
                    (SELECT last_error_code FROM notification_outbox
                     WHERE last_error_code IS NOT NULL
                     ORDER BY updated_at DESC, id DESC LIMIT 1)
                 FROM notification_outbox",
                [],
                |row| {
                    Ok(NotificationOutboxStats {
                        pending_notifications: count_to_u32(row.get::<_, Option<i64>>(0)?),
                        blocked_notifications: count_to_u32(row.get::<_, Option<i64>>(1)?),
                        pending_count: count_to_u32(row.get::<_, Option<i64>>(2)?),
                        sending_count: count_to_u32(row.get::<_, Option<i64>>(3)?),
                        retry_count: count_to_u32(row.get::<_, Option<i64>>(4)?),
                        blocked_activation_count: count_to_u32(row.get::<_, Option<i64>>(5)?),
                        blocked_reconnect_count: count_to_u32(row.get::<_, Option<i64>>(6)?),
                        dead_letter_count: count_to_u32(row.get::<_, Option<i64>>(7)?),
                        last_delivery_at: row.get(8)?,
                        last_error_code: row.get(9)?,
                    })
                },
            )
            .map_err(AppError::from)
        })
    }

    /// Deletes one bounded batch of terminal outbox rows in a single
    /// transaction. Active delivery states are deliberately excluded: a local
    /// retention pass must never discard work that could still be delivered.
    pub(crate) fn prune_retained(&self, now: i64) -> Result<usize, AppError> {
        let terminal_cutoff = now.saturating_sub(OUTBOX_TERMINAL_RETENTION_MS);
        let dead_letter_cutoff = now.saturating_sub(OUTBOX_DEAD_LETTER_RETENTION_MS);
        self.db.with_transaction(|tx| {
            tx.execute(
                "DELETE FROM notification_outbox
                 WHERE id IN (
                     SELECT outbox.id FROM notification_outbox AS outbox
                     LEFT JOIN relay_result_publications AS result ON result.outbox_id = outbox.id
                     WHERE result.pending_resend_request_id IS NULL
                       AND result.pending_revoke_request_id IS NULL
                       AND ((outbox.status = 'delivered' AND outbox.delivered_at <= ?
                             AND (outbox.acceptance_stage IS NULL
                                  OR outbox.acceptance_stage != 'relay'
                                  OR outbox.remote_status IN ('provider_accepted','dead_letter','expired','cancelled','blocked_target_changed')))
                            OR (outbox.status IN ('cancelled', 'expired') AND outbox.updated_at <= ?)
                            OR (outbox.status = 'dead_letter' AND outbox.updated_at <= ?))
                     ORDER BY outbox.updated_at ASC, outbox.id ASC
                     LIMIT ?
                 )",
                params![
                    terminal_cutoff,
                    terminal_cutoff,
                    dead_letter_cutoff,
                    OUTBOX_RETENTION_BATCH_SIZE,
                ],
            )
            .map_err(AppError::from)
        })
    }

    pub(crate) fn history_page(
        &self,
        page_size: u16,
        cursor: Option<&str>,
    ) -> Result<NotificationHistoryPage, AppError> {
        if !(1..=100).contains(&page_size) {
            return Err(AppError::new(
                "INVALID_PAGE_SIZE",
                "通知历史分页大小必须在 1 到 100 之间",
            ));
        }
        let cursor = cursor.map(parse_history_cursor).transpose()?;
        let limit = i64::from(page_size) + 1;
        let mut items = self.db.with_connection(|conn| {
            let query = "SELECT id, event_kind, status, delivery_backend, acceptance_stage,
                                list_title, list_content_mode, list_content_bytes, list_body_available,
                                attempt_count, last_error_code,
                                provider_message_id, relay_notification_id, relay_accepted_at,
                                remote_status, remote_updated_at, remote_provider_message_id,
                                created_at, updated_at, delivered_at
                         FROM notification_outbox
                         WHERE (?1 IS NULL OR created_at < ?1 OR (created_at = ?1 AND id < ?2))
                         ORDER BY created_at DESC, id DESC
                         LIMIT ?3";
            let (cursor_time, cursor_id) = cursor
                .as_ref()
                .map_or((None, None), |(time, id)| (Some(*time), Some(id.as_str())));
            let mut statement = conn.prepare(query)?;
            let rows = statement.query_map(
                params![cursor_time, cursor_id, limit],
                decode_raw_history_item,
            )?;
            rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
        })?;
        let has_more = items.len() > usize::from(page_size);
        if has_more {
            items.pop();
        }
        let items = items
            .into_iter()
            .map(RawHistoryItem::into_item)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if has_more {
            items
                .last()
                .map(|item| format!("{}:{}", item.created_at, item.id))
        } else {
            None
        };
        Ok(NotificationHistoryPage { items, next_cursor })
    }

    pub(crate) fn history_item(
        &self,
        id: &str,
    ) -> Result<Option<NotificationHistoryItem>, AppError> {
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err(AppError::new("INVALID_NOTIFICATION_ID", "通知投递标识无效"));
        }
        let raw = self.db.with_connection(|conn| {
            conn.query_row(
                "SELECT id, event_kind, status, delivery_backend, acceptance_stage,
                        list_title, list_content_mode, list_content_bytes, list_body_available,
                        attempt_count, last_error_code,
                        provider_message_id, relay_notification_id, relay_accepted_at,
                        remote_status, remote_updated_at, remote_provider_message_id,
                        created_at, updated_at, delivered_at
                 FROM notification_outbox WHERE id = ?",
                [id],
                decode_raw_history_item,
            )
            .optional()
            .map_err(AppError::from)
        })?;
        raw.map(RawHistoryItem::into_item).transpose()
    }

    pub(crate) fn delivery_state(
        &self,
        dedupe_key: &str,
    ) -> Result<Option<NotificationDeliveryState>, AppError> {
        if dedupe_key.is_empty() || dedupe_key.len() > 256 {
            return Err(AppError::new(
                "INVALID_NOTIFICATION_KEY",
                "通知投递标识无效",
            ));
        }
        self.db.with_connection(|conn| {
            conn.query_row(
                "SELECT status, last_error_code FROM notification_outbox WHERE dedupe_key = ?",
                params![dedupe_key],
                |row| {
                    Ok(NotificationDeliveryState {
                        status: row.get(0)?,
                        last_error_code: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(AppError::from)
        })
    }

    pub(crate) fn acceptance_receipt(
        &self,
        dedupe_key: &str,
    ) -> Result<Option<NotificationAcceptanceReceipt>, AppError> {
        if dedupe_key.is_empty() || dedupe_key.len() > 256 {
            return Err(AppError::new(
                "INVALID_NOTIFICATION_KEY",
                "通知投递标识无效",
            ));
        }
        self.db.with_connection(|conn| {
            conn.query_row(
                "SELECT status, acceptance_stage, relay_notification_id, relay_accepted_at
                 FROM notification_outbox WHERE dedupe_key = ?",
                params![dedupe_key],
                |row| {
                    let acceptance_stage = row
                        .get::<_, Option<String>>(1)?
                        .as_deref()
                        .map(TransportAcceptanceStage::parse)
                        .transpose()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?;
                    Ok(NotificationAcceptanceReceipt {
                        status: row.get(0)?,
                        acceptance_stage,
                        relay_notification_id: row.get(2)?,
                        relay_accepted_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(AppError::from)
        })
    }

    pub(crate) fn history_change_token(&self) -> u64 {
        self.db.revision()
    }

    pub(crate) fn register_result(
        &self,
        record: &crate::relay::result_store::ResultPublicationRecord,
    ) -> Result<(), AppError> {
        crate::relay::result_store::register(&self.db, record)
    }
    pub(crate) fn result_publication(
        &self,
        id: &str,
    ) -> Result<Option<crate::relay::result_store::ResultPublicationRecord>, AppError> {
        crate::relay::result_store::detail(&self.db, id)
    }
    pub(crate) fn apply_result_receipt(
        &self,
        id: &str,
        receipt: &crate::relay::result_protocol::RelayResultReceipt,
        allow_notification_change: bool,
    ) -> Result<crate::relay::result_store::ResultReceiptApply, AppError> {
        crate::relay::result_store::apply(&self.db, id, receipt, allow_notification_change)
    }

    pub(crate) fn register_result_action(
        &self,
        id: &str,
        action: &str,
        request_id: &str,
    ) -> Result<String, AppError> {
        crate::relay::result_store::register_action(&self.db, id, action, request_id)
    }
    pub(crate) fn apply_result_action_receipt(
        &self,
        id: &str,
        receipt: &crate::relay::result_protocol::RelayResultReceipt,
        action: &str,
        request_id: &str,
    ) -> Result<crate::relay::result_store::ResultReceiptApply, AppError> {
        crate::relay::result_store::apply_action(&self.db, id, receipt, action, request_id)
    }

    #[cfg(test)]
    pub(crate) fn relay_reconciliation_candidates(
        &self,
        limit: u16,
    ) -> Result<Vec<RelayReconciliationCandidate>, AppError> {
        self.relay_reconciliation_candidates_after(limit, None)
    }

    pub(crate) fn relay_reconciliation_candidates_after(
        &self,
        limit: u16,
        after: Option<&str>,
    ) -> Result<Vec<RelayReconciliationCandidate>, AppError> {
        if !(1..=50).contains(&limit) {
            return Err(AppError::new(
                "INVALID_RECONCILIATION_LIMIT",
                "Relay 回执刷新数量必须在 1 到 50 之间",
            ));
        }
        self.db.try_with_connection(|conn| {
            let mut statement = conn.prepare(
                "SELECT notification_outbox.id, notification_outbox.relay_notification_id
                 FROM notification_outbox AS notification_outbox
                 LEFT JOIN relay_result_publications AS result ON result.outbox_id = notification_outbox.id
                 WHERE delivery_backend = 'relay'
                   AND acceptance_stage = 'relay'
                   AND relay_notification_id IS NOT NULL
                   AND ((result.outbox_id IS NOT NULL AND (
                            result.accepted_at IS NULL
                            OR result.notification_status IS NULL
                            OR result.notification_status NOT IN ('provider_accepted', 'dead_letter', 'expired', 'cancelled', 'blocked_target_changed', 'delivery_unknown')
                            OR result.pending_resend_request_id IS NOT NULL
                            OR result.pending_revoke_request_id IS NOT NULL
                        ))
                        OR (result.outbox_id IS NULL AND (remote_status IS NULL OR remote_status NOT IN ('provider_accepted', 'dead_letter', 'expired', 'cancelled', 'blocked_target_changed', 'delivery_unknown'))))
                   AND (?2 IS NULL OR notification_outbox.id > ?2)
                 ORDER BY notification_outbox.id ASC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map(params![i64::from(limit), after], |row| {
                Ok(RelayReconciliationCandidate {
                    outbox_id: row.get(0)?,
                    notification_id: row.get(1)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
        })
    }

    /// Applies only a strictly newer server-owned receipt to the exact Relay
    /// acceptance row. Equal timestamps are idempotent; older callbacks lose.
    pub(crate) fn apply_relay_remote_status(
        &self,
        update: RelayRemoteStatusUpdate<'_>,
    ) -> Result<bool, AppError> {
        if update.updated_at < 0
            || !valid_bounded_identifier(update.outbox_id, 128)
            || !valid_bounded_identifier(update.notification_id, 128)
            || !valid_relay_remote_status(update.status)
            || update
                .provider_message_id
                .is_some_and(|value| !valid_bounded_identifier(value, 512))
        {
            return Err(AppError::new(
                "INVALID_RELAY_NOTIFICATION_STATUS",
                "Relay 通知状态无效",
            ));
        }
        self.db.try_with_connection(|conn| {
            conn.execute(
                "UPDATE notification_outbox
                 SET remote_status = ?, remote_updated_at = ?,
                     remote_provider_message_id = ?
                 WHERE id = ?
                   AND delivery_backend = 'relay'
                   AND acceptance_stage = 'relay'
                   AND relay_notification_id = ?
                   AND (remote_status IS NULL OR remote_status NOT IN
                        ('provider_accepted', 'dead_letter', 'expired', 'cancelled', 'blocked_target_changed', 'delivery_unknown'))
                   AND (remote_updated_at IS NULL OR remote_updated_at < ?)",
                params![
                    update.status,
                    update.updated_at,
                    update.provider_message_id,
                    update.outbox_id,
                    update.notification_id,
                    update.updated_at,
                ],
            )
            .map(|changed| changed == 1)
            .map_err(AppError::from)
        })
    }

    #[cfg(test)]
    pub(crate) fn claim_next(&self, now: i64) -> Result<Option<OutboxClaim>, AppError> {
        self.claim_next_for(NotificationBackendKind::Relay, now)
    }

    #[cfg(test)]
    pub(crate) fn claim_next_for(
        &self,
        backend: NotificationBackendKind,
        now: i64,
    ) -> Result<Option<OutboxClaim>, AppError> {
        self.claim_next_for_with_decoder(backend, now, |protected| {
            crate::content_crypto::unprotect_json(
                crate::content_crypto::ContentPurpose::NotificationPayload,
                protected,
            )
        })
    }

    /// A hold permit and the `sending` transition share one SQLite
    /// transaction.  This is intentionally narrower than HTTP delivery: once
    /// this returns a claim, a later hold treats that request as in flight.
    pub(crate) fn claim_next_for_with_hold(
        &self,
        backend: NotificationBackendKind,
        now: i64,
        hold: &HoldController,
    ) -> Result<Option<OutboxClaim>, AppError> {
        self.claim_next_for_with_decoder_and_gate(
            backend,
            now,
            |tx| hold.permits_send_tx(tx, now),
            |protected| {
                crate::content_crypto::unprotect_json(
                    crate::content_crypto::ContentPurpose::NotificationPayload,
                    protected,
                )
            },
        )
    }

    #[cfg(test)]
    fn claim_next_for_with_decoder(
        &self,
        backend: NotificationBackendKind,
        now: i64,
        decode: impl FnOnce(&str) -> Result<NotificationPayloadV2, AppError>,
    ) -> Result<Option<OutboxClaim>, AppError> {
        self.claim_next_for_with_decoder_and_gate(backend, now, |_| Ok(true), decode)
    }

    fn claim_next_for_with_decoder_and_gate(
        &self,
        backend: NotificationBackendKind,
        now: i64,
        gate: impl FnOnce(&Transaction<'_>) -> Result<bool, AppError>,
        decode: impl FnOnce(&str) -> Result<NotificationPayloadV2, AppError>,
    ) -> Result<Option<OutboxClaim>, AppError> {
        self.expire_due(now)?;
        let raw = self.db.with_transaction(|tx| {
            if !gate(tx)? {
                return Ok(None);
            }
            tx.query_row(
                "UPDATE notification_outbox
                 SET status = 'sending', claimed_at = ?, attempt_count = attempt_count + 1,
                     updated_at = ?
                 WHERE id = (
                     SELECT id FROM notification_outbox
                     WHERE status IN ('pending', 'retry_wait')
                       AND delivery_backend = ?
                       AND not_before <= ? AND COALESCE(next_attempt_at, not_before) <= ?
                       AND expires_at > ?
                     ORDER BY priority DESC, created_at ASC, id ASC
                     LIMIT 1
                 )
                   AND status IN ('pending', 'retry_wait')
                   AND delivery_backend = ?
                   AND not_before <= ? AND COALESCE(next_attempt_at, not_before) <= ?
                   AND expires_at > ?
                  RETURNING id, agent_run_key, event_kind, delivery_backend, dedupe_key,
                            priority, created_at, client_id, payload_json, content_policy_hash,
                            attempt_count, claimed_at, not_before, expires_at, last_error_code",
                params![
                    now,
                    now,
                    backend.as_str(),
                    now,
                    now,
                    now,
                    backend.as_str(),
                    now,
                    now,
                    now
                ],
                RawClaim::from_row,
            )
            .optional()
            .map_err(AppError::from)
        })?;

        let Some(raw) = raw else {
            return Ok(None);
        };
        let raw_id = raw.id.clone();
        match raw.into_claim_with_decoder(now, decode) {
            Ok(claim) => Ok(Some(claim)),
            Err(error) if error.code == "PROTECTED_CONTENT_UNAVAILABLE" => {
                self.finish_sending(
                    &raw_id,
                    "retry_wait",
                    now,
                    Some(now.saturating_add(NETWORK_BACKOFF_MS[0])),
                    "PROTECTED_CONTENT_UNAVAILABLE",
                    "Stored notification content is temporarily unavailable",
                )?;
                Err(error)
            }
            Err(error) => {
                self.finish_sending(
                    &raw_id,
                    "dead_letter",
                    now,
                    None,
                    "OUTBOX_PAYLOAD_CORRUPT",
                    "Stored notification metadata is invalid",
                )?;
                Err(error)
            }
        }
    }

    pub(crate) fn recover_stale_claims(&self, now: i64) -> Result<usize, AppError> {
        self.db.with_connection(|conn| {
            conn.execute(
                "UPDATE notification_outbox
                 SET status = CASE WHEN expires_at <= ? THEN 'expired' ELSE 'retry_wait' END,
                     claimed_at = NULL,
                     next_attempt_at = CASE WHEN expires_at <= ? THEN NULL ELSE ? END,
                     last_error_code = 'STALE_CLAIM_RECOVERED',
                     last_error_message = 'Recovered an interrupted delivery claim', updated_at = ?
                 WHERE status = 'sending' AND claimed_at IS NOT NULL AND claimed_at <= ?",
                params![now, now, now, now, now.saturating_sub(STALE_CLAIM_MS)],
            )
            .map_err(AppError::from)
        })
    }

    pub(crate) fn expire_due(&self, now: i64) -> Result<usize, AppError> {
        self.db.with_connection(|conn| {
            conn.execute(
                "UPDATE notification_outbox
                 SET status = 'expired', claimed_at = NULL, next_attempt_at = NULL,
                     last_error_code = 'NOTIFICATION_EXPIRED',
                     last_error_message = 'Notification delivery window expired', updated_at = ?
                 WHERE expires_at <= ?
                   AND status IN ('pending', 'retry_wait', 'blocked_activation', 'blocked_reconnect')",
                params![now, now],
            )
            .map_err(AppError::from)
        })
    }

    /// Freeze destination changes against enqueues/claims while the profile is saved.
    pub(crate) fn guard_destination_change<T>(
        &self,
        save: impl FnOnce() -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        self.db.with_connection(|conn| {
            let pending: bool = conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM notification_outbox AS outbox
                    LEFT JOIN relay_result_publications AS result ON result.outbox_id = outbox.id
                    WHERE outbox.status NOT IN ('cancelled','expired','dead_letter','delivered')
                       OR (outbox.status = 'delivered' AND outbox.acceptance_stage = 'relay' AND (
                            outbox.remote_status IS NULL
                            OR outbox.remote_status NOT IN ('provider_accepted','dead_letter','expired','cancelled','blocked_target_changed')
                            OR result.pending_resend_request_id IS NOT NULL
                            OR result.pending_revoke_request_id IS NOT NULL
                       ))
                )",
                [],
                |row| row.get(0),
            )?;
            if pending { return Err(AppError::new("RELAY_DESTINATION_HAS_PENDING", "旧目的地仍有未完成投递，请保留配置直到投递结束或明确取消旧队列")); }
            save()
        })
    }

    /// Resume only blocks whose prerequisite became true. Health probes must
    /// not repeatedly reset unrelated blocked rows back into the hot queue.
    pub(crate) fn retry_blocked_for_relay(
        &self,
        now: i64,
        credentials_ready: bool,
        result_pages_ready: bool,
        target_ready: bool,
    ) -> Result<usize, AppError> {
        self.db.with_transaction(|tx| {
            let mut recovered = 0;
            for (enabled, code) in [
                (credentials_ready, "RELAY_CREDENTIALS_REQUIRED"),
                (result_pages_ready, "RELAY_RESULT_PAGES_UNAVAILABLE"),
                (target_ready, "RELAY_TARGET_UNAVAILABLE"),
            ] {
                if !enabled {
                    continue;
                }
                tx.execute(
                    "UPDATE notification_outbox
                     SET status = 'expired', next_attempt_at = NULL,
                         last_error_code = 'NOTIFICATION_EXPIRED',
                         last_error_message = 'Notification delivery window expired', updated_at = ?
                     WHERE expires_at <= ? AND status = 'blocked_reconnect'
                       AND last_error_code = ? AND delivery_backend = 'relay'",
                    params![now, now, code],
                )?;
                recovered += tx.execute(
                    "UPDATE notification_outbox
                     SET status = 'retry_wait', claimed_at = NULL, next_attempt_at = ?,
                         last_error_code = NULL, last_error_message = NULL, updated_at = ?
                     WHERE expires_at > ? AND status = 'blocked_reconnect'
                       AND last_error_code = ? AND delivery_backend = 'relay'",
                    params![now, now, now, code],
                )?;
            }
            Ok(recovered)
        })
    }

    pub(crate) fn complete_delivery(
        &self,
        claim: &OutboxClaim,
        result: DeliveryResult,
        now: i64,
    ) -> Result<(), AppError> {
        if let DeliveryResult::Accepted(acceptance) = &result {
            return self.finish_accepted(&claim.id, acceptance, now);
        }
        if matches!(result, DeliveryResult::Held) {
            return self.release_held_claim(claim, now);
        }
        if now >= claim.expires_at {
            self.finish_sending(
                &claim.id,
                "expired",
                now,
                None,
                "NOTIFICATION_EXPIRED",
                "Notification delivery window expired",
            )?;
            return Ok(());
        }

        match result {
            DeliveryResult::Accepted(_) => {
                unreachable!("provider acceptance is finalized before TTL checks")
            }
            DeliveryResult::Retryable(retry) => match retry.policy {
                TransportRetryPolicy::NetworkBackoff => self.retry_with_schedule(
                    claim,
                    now,
                    NETWORK_BACKOFF_MS,
                    retry.error_code,
                    retry.error_message,
                ),
                TransportRetryPolicy::RateLimitBackoff => self.retry_with_schedule(
                    claim,
                    now,
                    RATE_LIMIT_BACKOFF_MS,
                    retry.error_code,
                    retry.error_message,
                ),
                TransportRetryPolicy::Limited { attempts, delay_ms } => self.retry_with_limit(
                    claim,
                    now,
                    attempts,
                    delay_ms,
                    retry.error_code,
                    retry.error_message,
                ),
            },
            DeliveryResult::Blocked(block) => self.finish_sending(
                &claim.id,
                "blocked_reconnect",
                now,
                None,
                block.error_code,
                block.error_message,
            ),
            DeliveryResult::PermanentFailure(failure) => self.finish_sending(
                &claim.id,
                "dead_letter",
                now,
                None,
                failure.error_code,
                failure.error_message,
            ),
            DeliveryResult::Cancelled => self.finish_sending(
                &claim.id,
                "retry_wait",
                now,
                Some(now),
                "WORKER_CANCELLED",
                "Notification worker stopped before completion",
            ),
            DeliveryResult::PolicyRevoked => self.finish_sending(
                &claim.id,
                "cancelled",
                now,
                None,
                "NOTIFICATION_POLICY_CHANGED",
                "Notification privacy policy changed",
            ),
            DeliveryResult::Held => unreachable!("held claims are restored before TTL handling"),
        }
    }

    /// A sender discovered a hold after its local preparation but before any
    /// HTTP request. Restore exactly this claim and undo its reservation; a
    /// concurrent/stale completion must never touch a newer claim.
    fn release_held_claim(&self, claim: &OutboxClaim, now: i64) -> Result<(), AppError> {
        self.db.with_connection(|conn| {
            let changed = conn.execute(
                "UPDATE notification_outbox
                 SET status = CASE WHEN expires_at <= ?1 THEN 'expired' ELSE 'pending' END,
                     claimed_at = NULL, next_attempt_at = NULL,
                     attempt_count = attempt_count - 1,
                     last_error_code = CASE WHEN expires_at <= ?1 THEN 'NOTIFICATION_EXPIRED' ELSE last_error_code END,
                     updated_at = ?1
                 WHERE id = ?2 AND status = 'sending' AND claimed_at = ?3
                   AND attempt_count = ?4 AND attempt_count > 0",
                params![now, claim.id, claim.claimed_at, claim.attempt_count],
            )?;
            if changed == 0 {
                return Err(AppError::new("OUTBOX_CLAIM_LOST", "通知发送 Claim 已失效"));
            }
            Ok(())
        })
    }

    fn retry_with_schedule(
        &self,
        claim: &OutboxClaim,
        now: i64,
        schedule: &[i64],
        code: &'static str,
        message: &'static str,
    ) -> Result<(), AppError> {
        let index = claim.attempt_count.saturating_sub(1) as usize;
        if let Some(delay) = schedule.get(index) {
            self.finish_sending(
                &claim.id,
                "retry_wait",
                now,
                Some(now.saturating_add(*delay)),
                code,
                message,
            )
        } else {
            self.finish_sending(&claim.id, "dead_letter", now, None, code, message)
        }
    }

    fn retry_with_limit(
        &self,
        claim: &OutboxClaim,
        now: i64,
        limit: i64,
        delay: i64,
        code: &'static str,
        message: &'static str,
    ) -> Result<(), AppError> {
        if claim.attempt_count < limit {
            self.finish_sending(
                &claim.id,
                "retry_wait",
                now,
                Some(now.saturating_add(delay)),
                code,
                message,
            )
        } else {
            self.finish_sending(&claim.id, "dead_letter", now, None, code, message)
        }
    }

    fn finish_sending(
        &self,
        id: &str,
        status: &'static str,
        now: i64,
        next_attempt_at: Option<i64>,
        error_code: &'static str,
        error_message: &'static str,
    ) -> Result<(), AppError> {
        self.db.with_connection(|conn| {
            let changed = conn.execute(
                "UPDATE notification_outbox
                 SET status = ?, claimed_at = NULL, next_attempt_at = ?,
                     last_error_code = NULLIF(?, ''), last_error_message = NULLIF(?, ''),
                     delivered_at = CASE WHEN ? = 'delivered' THEN ? ELSE delivered_at END,
                     updated_at = ?
                 WHERE id = ? AND status = 'sending'",
                params![
                    status,
                    next_attempt_at,
                    error_code,
                    error_message,
                    status,
                    now,
                    now,
                    id
                ],
            )?;
            if changed == 0 {
                return Err(AppError::new("OUTBOX_CLAIM_LOST", "通知发送 Claim 已失效"));
            }
            Ok(())
        })
    }

    fn finish_accepted(
        &self,
        id: &str,
        acceptance: &TransportAcceptance,
        completed_at: i64,
    ) -> Result<(), AppError> {
        let accepted_at = acceptance.accepted_at.unwrap_or(completed_at);
        let (stage, provider_message_id, relay_notification_id, relay_accepted_at) =
            match acceptance.stage {
                TransportAcceptanceStage::Provider => (
                    acceptance.stage.as_str(),
                    acceptance.provider_message_id.as_deref(),
                    None,
                    None,
                ),
                TransportAcceptanceStage::Relay => (
                    acceptance.stage.as_str(),
                    None,
                    acceptance.transport_message_id.as_deref(),
                    Some(accepted_at),
                ),
            };
        self.db.with_connection(|conn| {
            let changed = conn.execute(
                "UPDATE notification_outbox
                 SET status = 'delivered', claimed_at = NULL, next_attempt_at = NULL,
                     last_error_code = NULL, last_error_message = NULL,
                     acceptance_stage = ?, provider_message_id = ?,
                     relay_notification_id = ?, relay_accepted_at = ?,
                     delivered_at = ?, updated_at = ?
                 WHERE id = ? AND status = 'sending'",
                params![
                    stage,
                    provider_message_id,
                    relay_notification_id,
                    relay_accepted_at,
                    accepted_at,
                    completed_at,
                    id
                ],
            )?;
            if changed == 0 {
                return Err(AppError::new("OUTBOX_CLAIM_LOST", "通知发送 Claim 已失效"));
            }
            Ok(())
        })
    }
}

pub(crate) fn selected_delivery_backend_tx(
    _tx: &Transaction<'_>,
) -> Result<NotificationBackendKind, AppError> {
    Ok(NotificationBackendKind::Relay)
}

fn valid_bounded_identifier(value: &str, max_bytes: usize) -> bool {
    value.trim() == value
        && !value.is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(char::is_control)
}

fn valid_relay_remote_status(status: &str) -> bool {
    RelayRemoteNotificationState::parse(status).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutboxRowStatus {
    Pending,
    Sending,
    RetryWait,
    BlockedActivation,
    BlockedReconnect,
    Delivered,
    Expired,
    Cancelled,
    DeadLetter,
}

impl OutboxRowStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "sending" => Some(Self::Sending),
            "retry_wait" => Some(Self::RetryWait),
            "blocked_activation" => Some(Self::BlockedActivation),
            "blocked_reconnect" => Some(Self::BlockedReconnect),
            "delivered" => Some(Self::Delivered),
            "expired" => Some(Self::Expired),
            "cancelled" => Some(Self::Cancelled),
            "dead_letter" => Some(Self::DeadLetter),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::RetryWait => "retry_wait",
            Self::BlockedActivation => "blocked_activation",
            Self::BlockedReconnect => "blocked_reconnect",
            Self::Delivered => "delivered",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::DeadLetter => "dead_letter",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboxClaim {
    pub id: String,
    pub agent_run_key: Option<String>,
    pub event_kind: NotificationEventKind,
    pub delivery_backend: NotificationBackendKind,
    pub dedupe_key: String,
    pub priority: i64,
    pub created_at: i64,
    pub client_id: String,
    pub payload: NotificationPayloadV2,
    pub content_policy_hash: Option<String>,
    pub attempt_count: i64,
    pub claimed_at: i64,
    pub expires_at: i64,
    pub last_error_code: Option<String>,
}

struct RawClaim {
    id: String,
    agent_run_key: Option<String>,
    event_kind: String,
    delivery_backend: String,
    dedupe_key: String,
    priority: i64,
    created_at: i64,
    client_id: String,
    payload_json: String,
    content_policy_hash: Option<String>,
    attempt_count: i64,
    claimed_at: i64,
    not_before: i64,
    expires_at: i64,
    last_error_code: Option<String>,
}

struct RawHistoryItem {
    id: String,
    event_kind: String,
    status: String,
    delivery_backend: String,
    acceptance_stage: Option<String>,
    list_title: String,
    list_content_mode: String,
    list_content_bytes: usize,
    list_body_available: bool,
    attempt_count: i64,
    last_error_code: Option<String>,
    provider_message_id: Option<String>,
    relay_notification_id: Option<String>,
    relay_accepted_at: Option<i64>,
    remote_status: Option<String>,
    remote_updated_at: Option<i64>,
    remote_provider_message_id: Option<String>,
    created_at: i64,
    updated_at: i64,
    delivered_at: Option<i64>,
}

fn decode_raw_history_item(row: &Row<'_>) -> rusqlite::Result<RawHistoryItem> {
    Ok(RawHistoryItem {
        id: row.get(0)?,
        event_kind: row.get(1)?,
        status: row.get(2)?,
        delivery_backend: row.get(3)?,
        acceptance_stage: row.get(4)?,
        list_title: row.get(5)?,
        list_content_mode: row.get(6)?,
        list_content_bytes: row.get(7)?,
        list_body_available: row.get(8)?,
        attempt_count: row.get(9)?,
        last_error_code: row.get(10)?,
        provider_message_id: row.get(11)?,
        relay_notification_id: row.get(12)?,
        relay_accepted_at: row.get(13)?,
        remote_status: row.get(14)?,
        remote_updated_at: row.get(15)?,
        remote_provider_message_id: row.get(16)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
        delivered_at: row.get(19)?,
    })
}

impl RawHistoryItem {
    fn into_item(self) -> Result<NotificationHistoryItem, AppError> {
        let event_kind = NotificationEventKind::parse(&self.event_kind)?;
        let delivery_backend = NotificationBackendKind::parse(&self.delivery_backend)
            .ok_or_else(|| AppError::new("OUTBOX_CORRUPT", "通知队列包含未知后端"))?;
        let acceptance_stage = self
            .acceptance_stage
            .as_deref()
            .map(TransportAcceptanceStage::parse)
            .transpose()?;
        let status = OutboxRowStatus::parse(&self.status)
            .ok_or_else(|| AppError::new("OUTBOX_CORRUPT", "通知队列包含未知本地状态"))?;
        let remote_status = self
            .remote_status
            .as_deref()
            .map(|status| {
                RelayRemoteNotificationState::parse(status)
                    .ok_or_else(|| AppError::new("OUTBOX_CORRUPT", "通知队列包含未知远端状态"))
            })
            .transpose()?;
        let content_mode = parse_list_content_mode(&self.list_content_mode)?;
        let payload = NotificationPayloadV2 {
            schema_version: NOTIFICATION_PAYLOAD_SCHEMA_VERSION,
            agent_label: String::new(),
            workspace_label: String::new(),
            model_name: None,
            title: self.list_title,
            body: String::new(),
            content_mode,
            result_revision: None,
            source_hash: None,
            content_bytes: self.list_content_bytes,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            delayed_delivery: false,
        };
        Ok(NotificationHistoryItem {
            id: self.id,
            event_kind,
            status: status.as_str().to_owned(),
            delivery_backend,
            acceptance_stage,
            payload,
            body_available: self.list_body_available,
            attempt_count: self.attempt_count,
            last_error_code: self.last_error_code,
            provider_message_id: self.provider_message_id,
            relay_notification_id: self.relay_notification_id,
            relay_accepted_at: self.relay_accepted_at,
            remote_status: remote_status.map(|status| status.as_str().to_owned()),
            remote_updated_at: self.remote_updated_at,
            remote_provider_message_id: self.remote_provider_message_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
            delivered_at: self.delivered_at,
        })
    }
}

fn parse_list_content_mode(value: &str) -> Result<ResultContentMode, AppError> {
    match value {
        "status_only" => Ok(ResultContentMode::StatusOnly),
        "redacted_excerpt" => Ok(ResultContentMode::RedactedExcerpt),
        "full_final" => Ok(ResultContentMode::FullFinal),
        _ => Err(AppError::new(
            "OUTBOX_CORRUPT",
            "通知列表投影包含未知正文模式",
        )),
    }
}

fn parse_history_cursor(cursor: &str) -> Result<(i64, String), AppError> {
    if cursor.len() > 200 {
        return Err(AppError::new("INVALID_CURSOR", "通知历史游标无效"));
    }
    let (created_at, id) = cursor
        .split_once(':')
        .ok_or_else(|| AppError::new("INVALID_CURSOR", "通知历史游标无效"))?;
    let created_at = created_at
        .parse::<i64>()
        .map_err(|_| AppError::new("INVALID_CURSOR", "通知历史游标无效"))?;
    if created_at < 0 || id.is_empty() || id.len() > 128 || id.chars().any(char::is_whitespace) {
        return Err(AppError::new("INVALID_CURSOR", "通知历史游标无效"));
    }
    Ok((created_at, id.to_owned()))
}

fn count_to_u32(value: Option<i64>) -> u32 {
    value.unwrap_or_default().clamp(0, i64::from(u32::MAX)) as u32
}

impl RawClaim {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            agent_run_key: row.get(1)?,
            event_kind: row.get(2)?,
            delivery_backend: row.get(3)?,
            dedupe_key: row.get(4)?,
            priority: row.get(5)?,
            created_at: row.get(6)?,
            client_id: row.get(7)?,
            payload_json: row.get(8)?,
            content_policy_hash: row.get(9)?,
            attempt_count: row.get(10)?,
            claimed_at: row.get(11)?,
            not_before: row.get(12)?,
            expires_at: row.get(13)?,
            last_error_code: row.get(14)?,
        })
    }

    fn into_claim_with_decoder(
        self,
        now: i64,
        decode: impl FnOnce(&str) -> Result<NotificationPayloadV2, AppError>,
    ) -> Result<OutboxClaim, AppError> {
        let event_kind = NotificationEventKind::parse(&self.event_kind)?;
        let delivery_backend = NotificationBackendKind::parse(&self.delivery_backend)
            .ok_or_else(|| AppError::new("OUTBOX_PAYLOAD_CORRUPT", "通知投递后端损坏"))?;
        let policy_protected = matches!(
            event_kind,
            NotificationEventKind::RunStarted
                | NotificationEventKind::RunCompleted
                | NotificationEventKind::RunFailed
                | NotificationEventKind::RunInterrupted
                | NotificationEventKind::AttentionRequired
        );
        if policy_protected
            && self.content_policy_hash.as_deref().is_none_or(|hash| {
                hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(AppError::new(
                "OUTBOX_PAYLOAD_CORRUPT",
                "通知隐私策略标识损坏",
            ));
        }
        if !policy_protected && self.content_policy_hash.is_some() {
            return Err(AppError::new(
                "OUTBOX_PAYLOAD_CORRUPT",
                "控制通知包含无效隐私策略标识",
            ));
        }
        let mut payload = decode(&self.payload_json)?;
        payload.validate_for_kind(event_kind)?;
        if matches!(
            event_kind,
            NotificationEventKind::RunCompleted
                | NotificationEventKind::RunFailed
                | NotificationEventKind::RunInterrupted
        ) && (self.attempt_count > 1
            || now > self.not_before.saturating_add(DELAYED_DELIVERY_GRACE_MS))
        {
            payload.delayed_delivery = true;
        }
        Ok(OutboxClaim {
            id: self.id,
            agent_run_key: self.agent_run_key,
            event_kind,
            delivery_backend,
            dedupe_key: self.dedupe_key,
            priority: self.priority,
            created_at: self.created_at,
            client_id: self.client_id,
            payload,
            content_policy_hash: self.content_policy_hash,
            attempt_count: self.attempt_count,
            claimed_at: self.claimed_at,
            expires_at: self.expires_at,
            last_error_code: self.last_error_code,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TransportAcceptanceStage {
    Provider,
    Relay,
}

impl TransportAcceptanceStage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Relay => "relay",
        }
    }

    fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "provider" => Ok(Self::Provider),
            "relay" => Ok(Self::Relay),
            _ => Err(AppError::new("OUTBOX_CORRUPT", "通知队列包含未知接收阶段")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransportAcceptance {
    pub stage: TransportAcceptanceStage,
    pub transport_message_id: Option<String>,
    pub provider_message_id: Option<String>,
    pub accepted_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportRetryPolicy {
    NetworkBackoff,
    RateLimitBackoff,
    Limited { attempts: i64, delay_ms: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransportRetry {
    pub policy: TransportRetryPolicy,
    pub error_code: &'static str,
    pub error_message: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportBlockKind {
    Credentials,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransportBlock {
    pub kind: TransportBlockKind,
    pub error_code: &'static str,
    pub error_message: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransportFailure {
    pub error_code: &'static str,
    pub error_message: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeliveryResult {
    Accepted(TransportAcceptance),
    Retryable(TransportRetry),
    Blocked(TransportBlock),
    PermanentFailure(TransportFailure),
    Cancelled,
    PolicyRevoked,
    /// No request was sent because the final sender-side hold gate rejected it.
    Held,
}

impl DeliveryResult {
    #[cfg(test)]
    #[allow(non_upper_case_globals)]
    pub(crate) const ReconnectRequired: Self = Self::Blocked(TransportBlock {
        kind: TransportBlockKind::Credentials,
        error_code: "RELAY_CREDENTIALS_REQUIRED",
        error_message: "Relay credentials must be refreshed before retry",
    });

    #[cfg(test)]
    #[allow(non_upper_case_globals)]
    pub(crate) const NetworkUnavailable: Self = Self::Retryable(TransportRetry {
        policy: TransportRetryPolicy::NetworkBackoff,
        error_code: "NETWORK_UNAVAILABLE",
        error_message: "Notification transport is unavailable",
    });

    #[cfg(test)]
    #[allow(non_upper_case_globals)]
    pub(crate) const RateLimited: Self = Self::Retryable(TransportRetry {
        policy: TransportRetryPolicy::RateLimitBackoff,
        error_code: "RATE_LIMITED",
        error_message: "Notification service rate limited the request",
    });

    #[cfg(test)]
    #[allow(non_upper_case_globals)]
    pub(crate) const Rejected: Self = Self::Retryable(TransportRetry {
        policy: TransportRetryPolicy::Limited {
            attempts: 3,
            delay_ms: 30_000,
        },
        error_code: "DELIVERY_REJECTED",
        error_message: "Notification service rejected the request",
    });
}

pub(crate) trait DeliveryTransport: Send + Sync + 'static {
    fn deliver(&self, claim: &OutboxClaim, cancellation: &DeliveryCancellation) -> DeliveryResult;
}

pub(crate) trait RunNotificationPolicyProvider: Send + Sync + 'static {
    fn current_policy_fingerprint(
        &self,
        event_kind: NotificationEventKind,
    ) -> Result<Option<String>, AppError>;
}

impl RunNotificationPolicyProvider for SettingsStore {
    fn current_policy_fingerprint(
        &self,
        event_kind: NotificationEventKind,
    ) -> Result<Option<String>, AppError> {
        let snapshot = self.get()?;
        match event_kind {
            NotificationEventKind::RunStarted
                if snapshot.notifications.enabled
                    && matches!(
                        snapshot.notifications.mode,
                        crate::model::NotificationMode::StartAndCompletion
                    ) =>
            {
                Ok(Some(
                    crate::notification::started_notification_policy_fingerprint(),
                ))
            }
            NotificationEventKind::RunCompleted
            | NotificationEventKind::RunFailed
            | NotificationEventKind::RunInterrupted
                if snapshot.notifications.enabled && snapshot.notifications.notify_ended =>
            {
                Ok(Some(run_notification_policy_fingerprint(
                    &snapshot.notifications,
                    snapshot.capture.result_content_mode.captures_text(),
                )))
            }
            NotificationEventKind::AttentionRequired
                if snapshot.notifications.enabled && snapshot.notifications.attention_enabled =>
            {
                Ok(Some(attention_notification_policy_fingerprint(
                    &snapshot.notifications,
                )))
            }
            _ => Ok(None),
        }
    }
}

#[derive(Default)]
struct WorkerSignal {
    state: Mutex<WorkerSignalState>,
    changed: Condvar,
}

#[derive(Default)]
struct WorkerSignalState {
    cancelled: bool,
    generation: u64,
}

impl WorkerSignal {
    fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
    }

    /// Sleeps only while no producer changed the queue generation and shutdown
    /// has not been requested.  Both predicates share the same mutex as the
    /// condvar, so a wake or cancel between claiming and waiting is observed.
    fn wait_while_unchanged(&self, generation: u64) -> bool {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, WORKER_IDLE_POLL, |state| {
                !state.cancelled && state.generation == generation
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.cancelled
    }

    fn wake(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.cancelled = true;
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn is_cancelled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cancelled
    }
}

pub(crate) struct DeliveryCancellation {
    signal: Arc<WorkerSignal>,
}

impl DeliveryCancellation {
    fn new(signal: Arc<WorkerSignal>) -> Self {
        Self { signal }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.signal.is_cancelled()
    }
}

struct WorkerErrorReporter {
    last_code: Option<&'static str>,
    last_reported_at: Instant,
}

impl WorkerErrorReporter {
    fn new() -> Self {
        Self {
            last_code: None,
            last_reported_at: Instant::now() - Duration::from_secs(10),
        }
    }

    fn report(&mut self, operation: &'static str, error: &AppError) {
        let now = Instant::now();
        if self.should_report(error.code, now) {
            tracing::warn!(
                code = error.code,
                operation,
                "notification outbox operation failed"
            );
        }
    }

    fn should_report(&mut self, code: &'static str, now: Instant) -> bool {
        if self.last_code == Some(code)
            && now.duration_since(self.last_reported_at) < Duration::from_secs(10)
        {
            return false;
        }
        self.last_code = Some(code);
        self.last_reported_at = now;
        true
    }
}

pub(crate) struct NotificationDeliveryWorker {
    signal: Arc<WorkerSignal>,
    handle: Option<JoinHandle<()>>,
}

impl NotificationDeliveryWorker {
    #[cfg(test)]
    pub(crate) fn panicked_for_test() -> Self {
        let signal = Arc::new(WorkerSignal::default());
        Self {
            signal,
            handle: Some(thread::spawn(|| panic!("synthetic worker panic"))),
        }
    }

    #[cfg(test)]
    pub(crate) fn start(
        repository: OutboxRepository,
        transport: Arc<dyn DeliveryTransport>,
        policy_provider: Arc<dyn RunNotificationPolicyProvider>,
    ) -> Result<Self, AppError> {
        let hold = repository.hold_controller();
        Self::start_for_backend(
            repository,
            transport,
            policy_provider,
            NotificationBackendKind::Relay,
            hold,
        )
    }

    pub(crate) fn start_for_backend(
        repository: OutboxRepository,
        transport: Arc<dyn DeliveryTransport>,
        policy_provider: Arc<dyn RunNotificationPolicyProvider>,
        backend: NotificationBackendKind,
        hold: HoldController,
    ) -> Result<Self, AppError> {
        let signal = Arc::new(WorkerSignal::default());
        let cancellation = Arc::new(DeliveryCancellation::new(Arc::clone(&signal)));
        let thread_cancellation = Arc::clone(&cancellation);
        let thread_signal = Arc::clone(&signal);
        let handle = thread::Builder::new()
            .name("notification-outbox".into())
            .spawn(move || {
                let mut errors = WorkerErrorReporter::new();
                if let Err(error) = repository.recover_stale_claims(now_ms().unwrap_or(0)) {
                    errors.report("recover_stale_claims", &error);
                }
                let mut next_retention_at = Instant::now();
                let mut hold_was_active = false;
                let mut throttle_after_hold_release = false;
                let mut next_hold_dispatch: Option<Instant> = None;
                // Start at zero so a hold+resume completed before this worker
                // was spawned is still observed as a recovery release.
                let mut observed_hold_release = 0;
                let mut observed_expired_hold_document = false;
                while !thread_cancellation.is_cancelled() {
                    let queue_generation = thread_signal.generation();
                    let now = now_ms().unwrap_or(0);
                    let release_generation = hold.release_generation();
                    if release_generation != observed_hold_release {
                        observed_hold_release = release_generation;
                        throttle_after_hold_release = true;
                        next_hold_dispatch = Some(Instant::now() + Duration::from_secs(1));
                    }
                    match hold.status_at(now) {
                        Ok(status) => match status.state {
                            crate::notification::hold::UserHoldState::Active
                            | crate::notification::hold::UserHoldState::Uncertain => {
                                hold_was_active = true;
                            }
                            crate::notification::hold::UserHoldState::Inactive
                                if hold_was_active =>
                            {
                                hold_was_active = false;
                                throttle_after_hold_release = true;
                                // Hold release can expose a large accumulated queue. Keep
                                // its original TTLs and identities, but start it at most
                                // once per second rather than issuing a catch-up burst.
                                next_hold_dispatch = Some(Instant::now() + Duration::from_secs(1));
                            }
                            crate::notification::hold::UserHoldState::Inactive => {
                                // On a fresh worker process there is no in-memory
                                // release generation. A past durable hold still
                                // means the queued rows were accumulated while
                                // dispatch was paused, so throttle its first drain.
                                if !observed_expired_hold_document
                                    && status.started_at.is_some()
                                    && status.until.is_some_and(|until| now >= until)
                                {
                                    observed_expired_hold_document = true;
                                    throttle_after_hold_release = true;
                                    next_hold_dispatch =
                                        Some(Instant::now() + Duration::from_secs(1));
                                }
                            }
                        },
                        Err(error) => {
                            hold_was_active = true;
                            errors.report("notification_hold", &error);
                        }
                    }
                    if next_hold_dispatch.is_some_and(|deadline| Instant::now() < deadline) {
                        if thread_signal.wait_while_unchanged(queue_generation) {
                            break;
                        }
                        continue;
                    }
                    next_hold_dispatch = None;
                    if Instant::now() >= next_retention_at {
                        if let Err(error) = repository.prune_retained(now) {
                            errors.report("prune_retained", &error);
                        }
                        next_retention_at = Instant::now() + OUTBOX_RETENTION_INTERVAL;
                    }
                    match repository.claim_next_for_with_hold(backend, now, &hold) {
                        Ok(Some(claim)) => {
                            let delivery_started = Instant::now();
                            tracing::debug!(
                                backend = backend.as_str(),
                                stage = "send_started",
                                attempt = claim.attempt_count,
                                duration_ms = 0_u64,
                                code = "OK",
                                "notification delivery stage"
                            );
                            let result = if thread_cancellation.is_cancelled() {
                                cancellation_before_transport_result(&hold, now)
                            } else if let Some(claim_policy) = claim.content_policy_hash.as_deref()
                            {
                                match policy_provider.current_policy_fingerprint(claim.event_kind) {
                                    Ok(Some(current_policy)) if current_policy == claim_policy => {
                                        transport.deliver(&claim, &thread_cancellation)
                                    }
                                    Ok(_) => DeliveryResult::PolicyRevoked,
                                    Err(error) => {
                                        errors.report("notification_policy", &error);
                                        DeliveryResult::PolicyRevoked
                                    }
                                }
                            } else {
                                transport.deliver(&claim, &thread_cancellation)
                            };
                            let held_by_user = matches!(&result, DeliveryResult::Held);
                            let result = preserve_held_when_cancelled(
                                result,
                                thread_cancellation.is_cancelled(),
                            );
                            let (stage, code) = delivery_trace_fields(&result);
                            if let Err(error) = repository.complete_delivery(
                                &claim,
                                result,
                                now_ms().unwrap_or(now),
                            ) {
                                tracing::warn!(
                                    backend = backend.as_str(),
                                    stage = "completion_failed",
                                    attempt = claim.attempt_count,
                                    duration_ms =
                                        bounded_trace_duration_ms(delivery_started.elapsed()),
                                    code = error.code,
                                    "notification delivery stage"
                                );
                                errors.report("complete_delivery", &error);
                            } else {
                                tracing::info!(
                                    backend = backend.as_str(),
                                    stage,
                                    attempt = claim.attempt_count,
                                    duration_ms =
                                        bounded_trace_duration_ms(delivery_started.elapsed()),
                                    code,
                                    "notification delivery stage"
                                );
                            }
                            if held_by_user {
                                hold_was_active = true;
                                throttle_after_hold_release = false;
                            } else if throttle_after_hold_release {
                                // Keep draining a backlog at one outbound request per
                                // second. A later empty poll clears this recovery mode.
                                next_hold_dispatch = Some(Instant::now() + Duration::from_secs(1));
                            }
                        }
                        Ok(None) => {
                            if throttle_after_hold_release {
                                throttle_after_hold_release = false;
                            }
                            if thread_signal.wait_while_unchanged(queue_generation) {
                                break;
                            }
                        }
                        Err(error) => {
                            errors.report("claim_next", &error);
                            if thread_signal.wait_while_unchanged(queue_generation) {
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|error| {
                AppError::internal(
                    "NOTIFICATION_WORKER_START_FAILED",
                    "通知发送后台任务启动失败",
                    error.to_string(),
                )
            })?;
        Ok(Self {
            signal,
            handle: Some(handle),
        })
    }

    pub(crate) fn wake(&self) {
        self.signal.wake();
    }

    pub(crate) fn begin_shutdown(&self) {
        self.signal.cancel();
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), AppError> {
        self.begin_shutdown();
        if let Some(handle) = self.handle.take() {
            handle.join().map_err(|_| {
                AppError::new(
                    "NOTIFICATION_WORKER_JOIN_FAILED",
                    "通知发送后台任务异常退出",
                )
            })?;
        }
        Ok(())
    }
}

impl Drop for NotificationDeliveryWorker {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn delivery_trace_fields(result: &DeliveryResult) -> (&'static str, &'static str) {
    match result {
        DeliveryResult::Accepted(acceptance) => match acceptance.stage {
            TransportAcceptanceStage::Provider => ("provider_accepted", "OK"),
            TransportAcceptanceStage::Relay => ("relay_accepted", "OK"),
        },
        DeliveryResult::Retryable(retry) => ("retry_scheduled", retry.error_code),
        DeliveryResult::Blocked(block) => ("blocked_credentials", block.error_code),
        DeliveryResult::PermanentFailure(failure) => ("permanent_failure", failure.error_code),
        DeliveryResult::Cancelled => ("cancelled", "WORKER_CANCELLED"),
        DeliveryResult::PolicyRevoked => ("policy_revoked", "NOTIFICATION_POLICY_CHANGED"),
        DeliveryResult::Held => ("held", "NOTIFICATION_HOLD_ACTIVE"),
    }
}

fn preserve_held_when_cancelled(result: DeliveryResult, cancelled: bool) -> DeliveryResult {
    if cancelled && !matches!(&result, DeliveryResult::Accepted(_) | DeliveryResult::Held) {
        DeliveryResult::Cancelled
    } else {
        result
    }
}

/// The worker can be cancelled after it has claimed a row but before it calls
/// the transport. Re-enter the shared send gate so a concurrently committed
/// hold returns Held and restores this zero-HTTP claim.
fn cancellation_before_transport_result(hold: &HoldController, now: i64) -> DeliveryResult {
    match hold.acquire_send_permit(now) {
        Ok(Some(_permit)) => DeliveryResult::Cancelled,
        Ok(None) | Err(_) => DeliveryResult::Held,
    }
}

fn bounded_trace_duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn insert_outbox(
    tx: &rusqlite::Transaction<'_>,
    input: NotificationInsert<'_>,
) -> Result<bool, AppError> {
    input.payload.validate_for_kind(input.kind)?;
    let already_recorded: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM notification_outbox WHERE dedupe_key = ?)",
        [input.dedupe_key],
        |row| row.get(0),
    )?;
    if !already_recorded {
        crate::storage::retention::check_capacity(tx, input.payload.content_bytes)?;
    }
    let policy_protected = matches!(
        input.kind,
        NotificationEventKind::RunStarted
            | NotificationEventKind::RunCompleted
            | NotificationEventKind::RunFailed
            | NotificationEventKind::RunInterrupted
            | NotificationEventKind::AttentionRequired
    );
    let valid_policy_hash = input
        .content_policy_hash
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    if policy_protected != valid_policy_hash {
        return Err(AppError::new(
            "INVALID_NOTIFICATION_POLICY",
            "通知隐私策略标识无效",
        ));
    }
    let payload_json = crate::content_crypto::protect_json(
        crate::content_crypto::ContentPurpose::NotificationPayload,
        input.payload,
    )?;
    let id = deterministic_id("outbox", input.dedupe_key);
    let client_id = format!(
        "promptdock-{}",
        deterministic_id("client", input.dedupe_key)
    );
    let expires_at = input
        .expires_at
        .unwrap_or_else(|| input.created_at.saturating_add(input.kind.ttl_ms()));
    tx.execute(
        "INSERT OR IGNORE INTO notification_outbox (
            id, agent_run_key, event_kind, dedupe_key, client_id, payload_json,
            content_policy_hash, list_title, list_content_mode, list_content_bytes,
            list_body_available, list_result_revision, delivery_backend, priority,
            status, not_before, expires_at, created_at, updated_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?)",
        params![
            id,
            input.agent_run_key,
            input.kind.as_str(),
            input.dedupe_key,
            client_id,
            payload_json,
            input.content_policy_hash,
            input.payload.title,
            content_mode_name(input.payload.content_mode),
            input.payload.content_bytes,
            i64::from(input.payload.source_hash.is_some()),
            input.payload.result_revision,
            input.delivery_backend.as_str(),
            input.kind.priority(),
            input.not_before,
            expires_at,
            input.created_at,
            input.created_at,
        ],
    )
    .map(|changed| changed == 1)
    .map_err(AppError::from)
}

fn content_mode_name(mode: ResultContentMode) -> &'static str {
    match mode {
        ResultContentMode::StatusOnly => "status_only",
        ResultContentMode::RedactedExcerpt => "redacted_excerpt",
        ResultContentMode::FullFinal => "full_final",
    }
}

fn deterministic_id(namespace: &str, value: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(namespace.as_bytes());
    hasher.update(&[0]);
    hasher.update(value.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn sanitize_optional(value: &str) -> Option<String> {
    let value = sanitize(value);
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;
    use std::time::{Duration, Instant};

    const START_AT: i64 = 1_000_000;
    const STOP_AT: i64 = START_AT + 38_000;
    const POLICY_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    struct FixedPolicyProvider(&'static str);

    impl RunNotificationPolicyProvider for FixedPolicyProvider {
        fn current_policy_fingerprint(
            &self,
            _event_kind: NotificationEventKind,
        ) -> Result<Option<String>, AppError> {
            Ok(Some(self.0.to_owned()))
        }
    }

    struct DisabledPolicyProvider;

    impl RunNotificationPolicyProvider for DisabledPolicyProvider {
        fn current_policy_fingerprint(
            &self,
            _event_kind: NotificationEventKind,
        ) -> Result<Option<String>, AppError> {
            Ok(None)
        }
    }
    fn setup_repository() -> (Arc<Db>, OutboxRepository) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let repository = OutboxRepository::new(db.clone());
        (db, repository)
    }

    #[test]
    fn temporary_payload_unprotect_failure_returns_claim_to_retry_wait() {
        let (db, repository) = setup_repository();
        let dedupe = "temporary-unprotect";
        enqueue_test(&repository, dedupe, START_AT);
        let id = deterministic_id("outbox", dedupe);

        let error = repository
            .claim_next_for_with_decoder(NotificationBackendKind::Relay, START_AT, |_| {
                Err(AppError::new(
                    "PROTECTED_CONTENT_UNAVAILABLE",
                    "temporary fixture failure",
                ))
            })
            .unwrap_err();
        assert_eq!(error.code, "PROTECTED_CONTENT_UNAVAILABLE");
        assert_eq!(status(&db, &id), "retry_wait");
        assert_eq!(
            scalar(&db, "SELECT next_attempt_at FROM notification_outbox"),
            START_AT + NETWORK_BACKOFF_MS[0]
        );

        let claim = repository
            .claim_next(START_AT + NETWORK_BACKOFF_MS[0])
            .unwrap()
            .unwrap();
        assert_eq!(claim.id, id);
    }

    #[test]
    fn corrupt_ciphertext_is_dead_lettered_without_blocking_the_next_claim() {
        let (db, repository) = setup_repository();
        enqueue_test(&repository, "corrupt-ciphertext", START_AT);
        enqueue_test(&repository, "healthy-after-corrupt", START_AT + 1);
        let corrupt_id = deterministic_id("outbox", "corrupt-ciphertext");
        let corrupt_payload = crate::content_crypto::protect_text(
            crate::content_crypto::ContentPurpose::NotificationPayload,
            b"not a notification payload",
        )
        .unwrap();
        db.with_connection(|connection| {
            connection.execute(
                "UPDATE notification_outbox SET payload_json=? WHERE id=?",
                params![corrupt_payload, corrupt_id],
            )?;
            Ok(())
        })
        .unwrap();

        let error = repository.claim_next(START_AT + 1).unwrap_err();
        assert!(crate::content_crypto::is_protected_content_corrupt(&error));
        assert_eq!(status(&db, &corrupt_id), "dead_letter");
        let next = repository
            .claim_next(START_AT + 1)
            .unwrap()
            .expect("healthy claim after corrupt ciphertext");
        assert_eq!(next.id, deterministic_id("outbox", "healthy-after-corrupt"));
    }

    #[test]
    fn delivery_trace_stages_are_closed_and_identifier_free() {
        assert_eq!(
            delivery_trace_fields(&DeliveryResult::Accepted(TransportAcceptance {
                stage: TransportAcceptanceStage::Relay,
                transport_message_id: Some("must-not-enter-trace".into()),
                provider_message_id: None,
                accepted_at: Some(1),
            })),
            ("relay_accepted", "OK")
        );
        assert_eq!(
            delivery_trace_fields(&DeliveryResult::Retryable(TransportRetry {
                policy: TransportRetryPolicy::NetworkBackoff,
                error_code: "RELAY_TRANSPORT_RETRY",
                error_message: "must-not-enter-trace",
            })),
            ("retry_scheduled", "RELAY_TRANSPORT_RETRY")
        );
    }

    fn accept_relay(
        repository: &OutboxRepository,
        key: &str,
        created_at: i64,
    ) -> RelayReconciliationCandidate {
        repository
            .enqueue_control(NotificationEventKind::Test, key, &payload(), created_at)
            .unwrap();
        let claim = repository
            .claim_next_for(NotificationBackendKind::Relay, created_at)
            .unwrap()
            .unwrap();
        let notification_id = format!("relay-{key}");
        repository
            .complete_delivery(
                &claim,
                DeliveryResult::Accepted(TransportAcceptance {
                    stage: TransportAcceptanceStage::Relay,
                    transport_message_id: Some(notification_id.clone()),
                    provider_message_id: None,
                    accepted_at: Some(created_at + 1),
                }),
                created_at + 1,
            )
            .unwrap();
        RelayReconciliationCandidate {
            outbox_id: claim.id,
            notification_id,
        }
    }

    fn accepted() -> DeliveryResult {
        DeliveryResult::Accepted(TransportAcceptance {
            stage: TransportAcceptanceStage::Provider,
            transport_message_id: None,
            provider_message_id: None,
            accepted_at: None,
        })
    }

    #[test]
    fn hot_reconciliation_stops_after_result_notification_is_provider_accepted() {
        let (_, repository) = setup_repository();
        let candidate = accept_relay(&repository, "accepted-result", START_AT);
        let hash = "a".repeat(64);
        repository
            .register_result(&crate::relay::result_store::ResultPublicationRecord {
                outbox_id: candidate.outbox_id.clone(),
                source_hash: hash.clone(),
                result_revision: 1,
                destination_identity: "fixture-destination".into(),
                request_digest: "b".repeat(64),
                accepted_at: None,
                page_state: None,
                page_expires_at: None,
                notification_id: None,
                notification_status: None,
                updated_at: None,
            })
            .unwrap();
        repository
            .apply_result_receipt(
                &candidate.outbox_id,
                &crate::relay::result_protocol::RelayResultReceipt {
                    schema_version: 1,
                    result_id: candidate.outbox_id.clone(),
                    source_hash: hash,
                    accepted_at: START_AT + 2,
                    updated_at: START_AT + 3,
                    page_state: crate::relay::result_protocol::RelayResultPageState::Available,
                    page_expires_at: START_AT + 100,
                    notification_id: candidate.notification_id.clone(),
                    notification_status: "provider_accepted".into(),
                },
                false,
            )
            .unwrap();
        assert!(repository
            .relay_reconciliation_candidates_after(10, None)
            .unwrap()
            .is_empty());
        repository
            .register_result_action(&candidate.outbox_id, "revoke", "pending-revoke")
            .unwrap();
        assert_eq!(
            repository
                .relay_reconciliation_candidates_after(10, None)
                .unwrap(),
            vec![candidate]
        );
    }

    fn payload() -> NotificationPayloadV2 {
        NotificationPayloadV2 {
            schema_version: NOTIFICATION_PAYLOAD_SCHEMA_VERSION,
            agent_label: "fixture-agent".into(),
            workspace_label: "prompt_dock".into(),
            model_name: Some("gpt-5.6".into()),
            title: "Fixture notification".into(),
            body: "Fixture safe body".into(),
            content_mode: ResultContentMode::StatusOnly,
            result_revision: None,
            source_hash: None,
            content_bytes: 0,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            delayed_delivery: false,
        }
    }

    #[test]
    fn payload_v2_rejects_legacy_missing_and_mixed_contracts() {
        let mut legacy = serde_json::to_value(payload()).unwrap();
        legacy["schemaVersion"] = serde_json::json!(1);
        let decoded: NotificationPayloadV2 = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            decoded.validate().unwrap_err().code,
            "INVALID_NOTIFICATION_PAYLOAD"
        );

        let mut missing_title = serde_json::to_value(payload()).unwrap();
        missing_title.as_object_mut().unwrap().remove("title");
        assert!(serde_json::from_value::<NotificationPayloadV2>(missing_title).is_err());

        let mut missing_body = serde_json::to_value(payload()).unwrap();
        missing_body.as_object_mut().unwrap().remove("body");
        assert!(serde_json::from_value::<NotificationPayloadV2>(missing_body).is_err());

        let mut mixed = serde_json::to_value(payload()).unwrap();
        mixed["taskPreview"] = serde_json::json!("legacy duplicate content");
        assert!(serde_json::from_value::<NotificationPayloadV2>(mixed).is_err());
    }

    fn scalar(db: &Db, sql: &str) -> i64 {
        db.with_connection(|conn| {
            conn.query_row(sql, [], |row| row.get(0))
                .map_err(AppError::from)
        })
        .unwrap()
    }

    fn text(db: &Db, sql: &str) -> String {
        db.with_connection(|conn| {
            conn.query_row(sql, [], |row| row.get(0))
                .map_err(AppError::from)
        })
        .unwrap()
    }

    fn status(db: &Db, id: &str) -> String {
        db.with_connection(|conn| {
            conn.query_row(
                "SELECT status FROM notification_outbox WHERE id = ?",
                params![id],
                |row| row.get(0),
            )
            .map_err(AppError::from)
        })
        .unwrap()
    }

    fn insert_retention_fixture(
        db: &Db,
        id: &str,
        status: &str,
        updated_at: i64,
        delivered_at: Option<i64>,
    ) {
        let payload_json = crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::NotificationPayload,
            &payload(),
        )
        .unwrap();
        db.with_connection(|conn| {
            conn.execute(
                "INSERT INTO notification_outbox (
                    id, agent_run_key, event_kind, dedupe_key, client_id, payload_json,
                    priority, status, not_before, expires_at, created_at, updated_at, delivered_at
                 ) VALUES (?, NULL, 'test', ?, ?, ?, 90, ?, 0, ?, 0, ?, ?)",
                params![
                    id,
                    format!("dedupe-{id}"),
                    format!("client-{id}"),
                    payload_json,
                    status,
                    i64::MAX,
                    updated_at,
                    delivered_at,
                ],
            )
            .map_err(AppError::from)
        })
        .unwrap();
    }

    #[test]
    fn retention_prunes_exact_terminal_boundaries_and_keeps_active_delivery_states() {
        let (db, repository) = setup_repository();
        let now = OUTBOX_DEAD_LETTER_RETENTION_MS + 1_000;
        let terminal_cutoff = now - OUTBOX_TERMINAL_RETENTION_MS;
        let dead_letter_cutoff = now - OUTBOX_DEAD_LETTER_RETENTION_MS;

        insert_retention_fixture(
            &db,
            "provider-accepted-boundary",
            "delivered",
            0,
            Some(terminal_cutoff),
        );
        insert_retention_fixture(
            &db,
            "provider-accepted-fresh",
            "delivered",
            0,
            Some(terminal_cutoff + 1),
        );
        insert_retention_fixture(
            &db,
            "cancelled-boundary",
            "cancelled",
            terminal_cutoff,
            None,
        );
        insert_retention_fixture(&db, "expired-boundary", "expired", terminal_cutoff, None);
        insert_retention_fixture(
            &db,
            "dead-letter-boundary",
            "dead_letter",
            dead_letter_cutoff,
            None,
        );
        insert_retention_fixture(
            &db,
            "dead-letter-fresh",
            "dead_letter",
            dead_letter_cutoff + 1,
            None,
        );
        for status in [
            "pending",
            "retry_wait",
            "blocked_activation",
            "blocked_reconnect",
            "sending",
        ] {
            insert_retention_fixture(&db, &format!("active-{status}"), status, 0, None);
        }

        assert_eq!(repository.prune_retained(now).unwrap(), 4);
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox
                 WHERE id IN ('provider-accepted-boundary', 'cancelled-boundary',
                              'expired-boundary', 'dead-letter-boundary')"
            ),
            0
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox
                 WHERE id IN ('provider-accepted-fresh', 'dead-letter-fresh')"
            ),
            2
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox
                 WHERE status IN ('pending', 'retry_wait', 'blocked_activation',
                                  'blocked_reconnect', 'sending')"
            ),
            5
        );
    }

    #[test]
    fn relay_remote_state_retention_is_fail_closed_except_for_explicit_terminals() {
        let now = OUTBOX_TERMINAL_RETENTION_MS + 1;
        for remote_status in [
            None,
            Some("delivery_unknown"),
            Some("pending_channel"),
            Some("retry_wait"),
            Some("sending_channel"),
            Some("queued"),
            Some("delivering"),
            Some("partial_failed"),
            Some("sending"),
            Some("blocked_activation"),
            Some("future_remote_state"),
        ] {
            let (db, repository) = setup_repository();
            insert_retention_fixture(&db, "relay-row", "delivered", 0, Some(0));
            db.with_connection(|conn| {
                conn.execute(
                    "UPDATE notification_outbox
                     SET acceptance_stage = 'relay', remote_status = ? WHERE id = 'relay-row'",
                    params![remote_status],
                )?;
                Ok(())
            })
            .unwrap();
            assert_eq!(
                repository.prune_retained(now).unwrap(),
                0,
                "{remote_status:?} must remain until a terminal receipt"
            );
            assert_eq!(scalar(&db, "SELECT COUNT(*) FROM notification_outbox"), 1);
        }

        for remote_status in [
            "provider_accepted",
            "dead_letter",
            "expired",
            "cancelled",
            "blocked_target_changed",
        ] {
            let (db, repository) = setup_repository();
            insert_retention_fixture(&db, "relay-row", "delivered", 0, Some(0));
            db.with_connection(|conn| {
                conn.execute(
                    "UPDATE notification_outbox
                     SET acceptance_stage = 'relay', remote_status = ? WHERE id = 'relay-row'",
                    [remote_status],
                )?;
                Ok(())
            })
            .unwrap();
            assert_eq!(
                repository.prune_retained(now).unwrap(),
                1,
                "{remote_status}"
            );
            assert_eq!(
                scalar(&db, "SELECT COUNT(*) FROM notification_outbox"),
                0,
                "{remote_status} may be retained only through the terminal window"
            );
        }
    }

    #[test]
    fn retention_deletes_in_bounded_batches() {
        let (db, repository) = setup_repository();
        let now = OUTBOX_TERMINAL_RETENTION_MS;
        for index in 0..=OUTBOX_RETENTION_BATCH_SIZE {
            insert_retention_fixture(
                &db,
                &format!("retention-batch-{index}"),
                "cancelled",
                0,
                None,
            );
        }

        assert_eq!(
            repository.prune_retained(now).unwrap(),
            OUTBOX_RETENTION_BATCH_SIZE as usize
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE status = 'cancelled'"
            ),
            1
        );
        assert_eq!(repository.prune_retained(now).unwrap(), 1);
    }

    #[test]
    fn retention_keeps_terminal_rows_with_pending_result_actions_until_cleared() {
        let (db, repository) = setup_repository();
        let now = OUTBOX_TERMINAL_RETENTION_MS + 1;
        for (id, column) in [
            ("pending-resend-retention", "pending_resend_request_id"),
            ("pending-revoke-retention", "pending_revoke_request_id"),
        ] {
            insert_retention_fixture(&db, id, "delivered", 0, Some(0));
            db.with_connection(|conn| {
                conn.execute(
                    &format!(
                        "INSERT INTO relay_result_publications(
                            outbox_id, source_hash, result_revision, destination_identity, request_digest, {column}
                         ) VALUES (?1, ?2, 1, 'device', ?3, ?4)"
                    ),
                    params![id, "0".repeat(64), "1".repeat(64), format!("request-{id}")],
                )?;
                Ok(())
            })
            .unwrap();
        }

        assert_eq!(repository.prune_retained(now).unwrap(), 0);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM notification_outbox"), 2);
        db.with_connection(|conn| {
            conn.execute(
                "UPDATE relay_result_publications
                 SET pending_resend_request_id = NULL, pending_revoke_request_id = NULL",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(repository.prune_retained(now).unwrap(), 2);
    }

    fn enqueue_test(repository: &OutboxRepository, dedupe: &str, now: i64) {
        assert!(repository
            .enqueue_control(NotificationEventKind::Test, dedupe, &payload(), now)
            .unwrap());
    }

    fn enqueue_run(
        db: &Db,
        kind: NotificationEventKind,
        dedupe: &str,
        run_key: &str,
        created_at: i64,
        not_before: i64,
    ) {
        db.with_transaction(|tx| {
            insert_run_fixture(tx, run_key)?;
            let mut payload = NotificationPayloadV2::for_run(
                "Fixture Agent",
                Some("prompt_dock"),
                Some("gpt-5.6"),
                Some(START_AT),
                Some(created_at),
                Some(created_at - START_AT),
            );
            payload.title = "Fixture Agent task".into();
            payload.body = "Fixture safe run notification".into();
            assert!(OutboxRepository::insert_tx(
                tx,
                RunNotificationInsert {
                    agent_run_key: run_key,
                    kind,
                    dedupe_key: dedupe,
                    payload: &payload,
                    content_policy_hash: POLICY_HASH,
                    created_at,
                    not_before,
                },
            )?);
            Ok(())
        })
        .unwrap();
    }

    fn insert_run_fixture(tx: &Transaction<'_>, run_key: &str) -> Result<(), AppError> {
        tx.execute(
            "INSERT OR IGNORE INTO agent_runs (
                run_key, agent_kind, instance_id, agent_label,
                status, outcome, completion_confidence, started_at, last_event_at
             ) VALUES (?, 'fixture-agent', 'fixture-instance', 'Fixture Agent',
                       'running', 'unknown', 'provisional', ?, ?)",
            params![run_key, START_AT, START_AT],
        )?;
        Ok(())
    }

    #[test]
    fn migration_creates_backend_and_acceptance_receipt_fields() {
        let (db, repository) = setup_repository();
        assert_eq!(scalar(&db, "PRAGMA user_version"), 7);
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM pragma_table_info('notification_outbox')
                 WHERE name IN ('delivery_backend', 'acceptance_stage',
                                'relay_notification_id', 'relay_accepted_at',
                                'remote_status', 'remote_updated_at',
                                'remote_provider_message_id')",
            ),
            7
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM pragma_table_info('notification_outbox')
                 WHERE name IN ('list_title', 'list_content_mode',
                                'list_content_bytes', 'list_body_available')",
            ),
            4
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('agent_runs', 'notification_outbox')",
            ),
            2
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index'
                 AND name IN ('idx_notification_outbox_ready', 'idx_notification_outbox_agent_run',
                              'idx_agent_runs_recent', 'idx_agent_runs_status_settle',
                              'idx_agent_runs_recent_keyset',
                              'idx_agent_runs_status_recent_keyset',
                              'idx_notification_outbox_retention_closed',
                              'idx_notification_outbox_retention_delivered')",
            ),
            6
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'codex_turn_runs'",
            ),
            0
        );
        enqueue_test(&repository, "migration-fixture", START_AT);
        let client_id = text(&db, "SELECT client_id FROM notification_outbox");
        assert!(client_id.starts_with("promptdock-"));
        assert_eq!(
            client_id,
            format!(
                "promptdock-{}",
                deterministic_id("client", "migration-fixture")
            )
        );
    }

    #[test]
    fn transaction_primitives_cover_every_notifiable_run_outcome() {
        let (db, repository) = setup_repository();
        db.with_transaction(|tx| {
            insert_run_fixture(tx, "fixture-run")?;
            for kind in [
                NotificationEventKind::RunStarted,
                NotificationEventKind::RunCompleted,
                NotificationEventKind::RunFailed,
                NotificationEventKind::RunInterrupted,
            ] {
                let suffix = kind.as_str();
                assert!(OutboxRepository::insert_tx(
                    tx,
                    RunNotificationInsert {
                        agent_run_key: "fixture-run",
                        kind,
                        dedupe_key: &format!("{suffix}:fixture-run"),
                        payload: &payload(),
                        content_policy_hash: POLICY_HASH,
                        created_at: START_AT,
                        not_before: START_AT,
                    },
                )?);
            }
            assert_eq!(
                OutboxRepository::cancel_pending_run_started_tx(
                    tx,
                    "fixture-run",
                    START_AT + 1,
                    "RUN_SETTLING",
                    "Run entered settling before start notification delivery",
                )?,
                1
            );
            Ok(())
        })
        .unwrap();

        let page = repository.history_page(10, None).unwrap();
        let kinds = page
            .items
            .into_iter()
            .map(|item| item.event_kind)
            .collect::<Vec<_>>();
        assert!(kinds.contains(&NotificationEventKind::RunCompleted));
        assert!(kinds.contains(&NotificationEventKind::RunFailed));
        assert!(kinds.contains(&NotificationEventKind::RunInterrupted));
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM notification_outbox WHERE status = 'cancelled'"
            ),
            1
        );
    }

    #[test]
    fn claim_is_single_conditional_and_creation_ordered() {
        let (_, repository) = setup_repository();
        assert!(repository
            .enqueue_control(NotificationEventKind::Test, "test", &payload(), START_AT)
            .unwrap());
        assert!(repository
            .enqueue_control(
                NotificationEventKind::Test,
                "later-test",
                &payload(),
                START_AT + 1
            )
            .unwrap());
        let first = repository.claim_next(START_AT + 1).unwrap().unwrap();
        assert_eq!(first.event_kind, NotificationEventKind::Test);
        assert_eq!(first.delivery_backend, NotificationBackendKind::Relay);
        assert_eq!(first.dedupe_key, "test");
        assert_eq!(first.priority, 90);
        assert_eq!(first.created_at, START_AT);
        repository
            .complete_delivery(&first, accepted(), START_AT + 2)
            .unwrap();
        assert_eq!(
            repository
                .claim_next(START_AT + 2)
                .unwrap()
                .unwrap()
                .event_kind,
            NotificationEventKind::Test
        );

        let (db, repository) = setup_repository();
        enqueue_test(&repository, "only-once", START_AT);
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let repository = repository.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                repository.claim_next(START_AT).unwrap().is_some()
            }));
        }
        barrier.wait();
        let claimed = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|claimed| *claimed)
            .count();
        assert_eq!(claimed, 1);
        assert_eq!(
            scalar(&db, "SELECT attempt_count FROM notification_outbox"),
            1
        );
    }

    #[test]
    fn hold_gate_leaves_rows_unclaimed_and_preserves_attempts_until_resume() {
        let (db, repository) = setup_repository();
        let hold = repository.hold_controller();
        enqueue_test(&repository, "held-notification", START_AT);
        let held = hold
            .hold(
                0,
                crate::notification::hold::HoldDuration::Minutes15,
                START_AT,
            )
            .unwrap();

        assert!(repository
            .claim_next_for_with_hold(NotificationBackendKind::Relay, START_AT + 1, &hold)
            .unwrap()
            .is_none());
        assert_eq!(
            scalar(&db, "SELECT attempt_count FROM notification_outbox"),
            0
        );
        assert_eq!(
            text(&db, "SELECT status FROM notification_outbox"),
            "pending"
        );

        hold.resume(held.state.revision, START_AT + 2).unwrap();
        let claim = repository
            .claim_next_for_with_hold(NotificationBackendKind::Relay, START_AT + 2, &hold)
            .unwrap()
            .unwrap();
        assert_eq!(claim.attempt_count, 1);
    }

    #[test]
    fn committed_hold_rejects_later_claim_without_changing_ttl_or_identity() {
        let (db, repository) = setup_repository();
        let hold = repository.hold_controller();
        enqueue_test(&repository, "hold-race", START_AT);
        let id = deterministic_id("outbox", "hold-race");
        let expires_before = scalar(&db, "SELECT expires_at FROM notification_outbox");
        hold.hold(
            0,
            crate::notification::hold::HoldDuration::Minutes60,
            START_AT,
        )
        .unwrap();

        assert!(repository
            .claim_next_for_with_hold(NotificationBackendKind::Relay, START_AT + 1, &hold)
            .unwrap()
            .is_none());
        assert_eq!(text(&db, "SELECT id FROM notification_outbox"), id);
        assert_eq!(
            scalar(&db, "SELECT expires_at FROM notification_outbox"),
            expires_before
        );
        assert_eq!(
            scalar(&db, "SELECT attempt_count FROM notification_outbox"),
            0
        );
    }

    #[test]
    fn final_hold_rejection_restores_exact_claim_without_extending_ttl() {
        let (db, repository) = setup_repository();
        enqueue_test(&repository, "held-after-prepare", START_AT);
        let claim = repository.claim_next(START_AT).unwrap().unwrap();
        let expires_at = claim.expires_at;
        repository
            .complete_delivery(&claim, DeliveryResult::Held, START_AT + 1)
            .unwrap();
        assert_eq!(
            text(&db, "SELECT status FROM notification_outbox"),
            "pending"
        );
        assert_eq!(
            scalar(&db, "SELECT attempt_count FROM notification_outbox"),
            0
        );
        assert_eq!(
            scalar(&db, "SELECT expires_at FROM notification_outbox"),
            expires_at
        );
    }

    #[test]
    fn final_hold_rejection_after_ttl_expires_without_retrying() {
        let (db, repository) = setup_repository();
        enqueue_test(&repository, "held-after-expiry", START_AT);
        let claim = repository.claim_next(START_AT).unwrap().unwrap();
        repository
            .complete_delivery(&claim, DeliveryResult::Held, claim.expires_at)
            .unwrap();
        assert_eq!(
            text(&db, "SELECT status FROM notification_outbox"),
            "expired"
        );
        assert_eq!(
            scalar(&db, "SELECT attempt_count FROM notification_outbox"),
            0 // No HTTP request started, even though the TTL elapsed during preparation.
        );
    }

    #[test]
    fn worker_shutdown_interleaving_keeps_final_hold_rejection() {
        // A sender may observe the final hold gate at the same instant shutdown
        // flips cancellation. Held wins because no HTTP request was started and
        // the claim must be restored without consuming its attempt.
        assert!(matches!(
            preserve_held_when_cancelled(DeliveryResult::Held, true),
            DeliveryResult::Held
        ));
    }

    #[test]
    fn worker_cancel_after_claim_releases_attempt_when_hold_commits_before_transport() {
        let (db, repository) = setup_repository();
        let hold = repository.hold_controller();
        enqueue_test(&repository, "cancel-hold-before-transport", START_AT);
        let claim = repository.claim_next(START_AT).unwrap().unwrap();
        let receipt = hold
            .hold(
                0,
                crate::notification::hold::HoldDuration::Minutes15,
                START_AT + 1,
            )
            .unwrap();
        assert_eq!(receipt.status, "applied");

        let result = cancellation_before_transport_result(&hold, START_AT + 1);
        assert!(matches!(result, DeliveryResult::Held));
        repository
            .complete_delivery(&claim, result, START_AT + 1)
            .unwrap();
        assert_eq!(
            text(&db, "SELECT status FROM notification_outbox"),
            "pending"
        );
        assert_eq!(
            scalar(&db, "SELECT attempt_count FROM notification_outbox"),
            0
        );
    }

    #[test]
    fn stale_claims_recover_and_expired_items_never_requeue() {
        let (db, repository) = setup_repository();
        enqueue_test(&repository, "stale", START_AT);
        let claim = repository.claim_next(START_AT).unwrap().unwrap();
        assert_eq!(
            repository
                .recover_stale_claims(START_AT + STALE_CLAIM_MS - 1)
                .unwrap(),
            0
        );
        assert_eq!(
            repository
                .recover_stale_claims(START_AT + STALE_CLAIM_MS)
                .unwrap(),
            1
        );
        assert_eq!(status(&db, &claim.id), "expired");
        assert!(repository
            .claim_next(START_AT + STALE_CLAIM_MS)
            .unwrap()
            .is_none());
        assert_eq!(
            repository
                .retry_blocked_for_relay(START_AT + STALE_CLAIM_MS, true, true, true)
                .unwrap(),
            0
        );

        let (db, repository) = setup_repository();
        enqueue_run(
            &db,
            NotificationEventKind::RunCompleted,
            "run_completed:fixture-run",
            "fixture-run",
            STOP_AT,
            STOP_AT + 3_000,
        );
        let claim = repository.claim_next(STOP_AT + 3_000).unwrap().unwrap();
        assert_eq!(
            repository
                .recover_stale_claims(claim.claimed_at + STALE_CLAIM_MS)
                .unwrap(),
            1
        );
        assert_eq!(status(&db, &claim.id), "retry_wait");
        assert_eq!(
            text(&db, "SELECT last_error_code FROM notification_outbox"),
            "STALE_CLAIM_RECOVERED"
        );
    }

    #[test]
    fn retry_and_credential_blocking_store_only_stable_errors() {
        let cases = [
            (
                DeliveryResult::NetworkUnavailable,
                "retry_wait",
                "NETWORK_UNAVAILABLE",
            ),
            (DeliveryResult::RateLimited, "retry_wait", "RATE_LIMITED"),
            (
                DeliveryResult::ReconnectRequired,
                "blocked_reconnect",
                "RELAY_CREDENTIALS_REQUIRED",
            ),
            (DeliveryResult::Rejected, "retry_wait", "DELIVERY_REJECTED"),
            (
                DeliveryResult::PermanentFailure(TransportFailure {
                    error_code: "IDEMPOTENCY_CONFLICT",
                    error_message: "Transport idempotency conflict",
                }),
                "dead_letter",
                "IDEMPOTENCY_CONFLICT",
            ),
            (DeliveryResult::Cancelled, "retry_wait", "WORKER_CANCELLED"),
        ];
        for (index, (result, expected_status, expected_code)) in cases.into_iter().enumerate() {
            let (db, repository) = setup_repository();
            enqueue_test(&repository, &format!("case-{index}"), START_AT);
            let claim = repository.claim_next(START_AT).unwrap().unwrap();
            repository
                .complete_delivery(&claim, result, START_AT + 1)
                .unwrap();
            assert_eq!(status(&db, &claim.id), expected_status);
            assert_eq!(
                text(&db, "SELECT last_error_code FROM notification_outbox"),
                expected_code
            );
            let stored = text(&db, "SELECT last_error_message FROM notification_outbox");
            assert!(!stored.contains("token") && !stored.contains("secret"));
        }
    }

    #[test]
    fn retry_limits_and_completion_delayed_flag_are_enforced() {
        let (db, repository) = setup_repository();
        enqueue_test(&repository, "network-limit", START_AT);
        let mut claim = repository.claim_next(START_AT).unwrap().unwrap();
        claim.attempt_count = 6;
        repository
            .complete_delivery(&claim, DeliveryResult::NetworkUnavailable, START_AT + 1)
            .unwrap();
        assert_eq!(status(&db, &claim.id), "dead_letter");

        let (db, repository) = setup_repository();
        enqueue_run(
            &db,
            NotificationEventKind::RunCompleted,
            "run_completed:fixture-run",
            "fixture-run",
            STOP_AT,
            STOP_AT + 3_000,
        );
        let first = repository.claim_next(STOP_AT + 3_000).unwrap().unwrap();
        repository
            .complete_delivery(&first, DeliveryResult::NetworkUnavailable, STOP_AT + 3_000)
            .unwrap();
        let second = repository.claim_next(STOP_AT + 8_000).unwrap().unwrap();
        assert!(second.payload.delayed_delivery);
        let stored: NotificationPayloadV2 = crate::content_crypto::unprotect_json(
            crate::content_crypto::ContentPurpose::NotificationPayload,
            &text(&db, "SELECT payload_json FROM notification_outbox"),
        )
        .unwrap();
        assert!(!stored.delayed_delivery);
    }

    #[test]
    fn network_and_rate_limit_use_exact_retry_policy() {
        let (db, repository) = setup_repository();
        enqueue_test(&repository, "network-backoff", START_AT);
        let claim = repository.claim_next(START_AT).unwrap().unwrap();
        repository
            .complete_delivery(&claim, DeliveryResult::NetworkUnavailable, START_AT)
            .unwrap();
        assert_eq!(
            scalar(&db, "SELECT next_attempt_at FROM notification_outbox"),
            START_AT + 5_000
        );

        let (db, repository) = setup_repository();
        enqueue_test(&repository, "rate-backoff", START_AT);
        let claim = repository.claim_next(START_AT).unwrap().unwrap();
        repository
            .complete_delivery(&claim, DeliveryResult::RateLimited, START_AT)
            .unwrap();
        assert_eq!(
            scalar(&db, "SELECT next_attempt_at FROM notification_outbox"),
            START_AT + 30_000
        );
    }

    #[test]
    fn confirmed_delivery_wins_over_ttl() {
        let (db, repository) = setup_repository();
        enqueue_run(
            &db,
            NotificationEventKind::RunStarted,
            "run_started:fixture-run",
            "fixture-run",
            START_AT,
            START_AT,
        );
        let claim = repository.claim_next(START_AT).unwrap().unwrap();
        assert_eq!(status(&db, &claim.id), "sending");

        db.with_transaction(|tx| {
            OutboxRepository::cancel_pending_run_started_tx(
                tx,
                "fixture-run",
                STOP_AT,
                "RUN_COMPLETED",
                "Run completed before start notification delivery",
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(status(&db, &claim.id), "sending");
        repository
            .complete_delivery(
                &claim,
                DeliveryResult::Accepted(TransportAcceptance {
                    stage: TransportAcceptanceStage::Provider,
                    transport_message_id: None,
                    provider_message_id: Some("provider-message-123".into()),
                    accepted_at: Some(claim.expires_at + 1),
                }),
                claim.expires_at + 1,
            )
            .unwrap();
        assert_eq!(status(&db, &claim.id), "delivered");
        assert_eq!(
            text(&db, "SELECT provider_message_id FROM notification_outbox"),
            "provider-message-123"
        );
        assert_eq!(
            text(&db, "SELECT acceptance_stage FROM notification_outbox"),
            "provider"
        );
    }

    #[test]
    fn relay_backend_is_claimed_independently_and_records_only_relay_acceptance() {
        let (_, repository) = setup_repository();
        repository
            .enqueue_control(
                NotificationEventKind::Test,
                "relay-test",
                &payload(),
                START_AT,
            )
            .unwrap();
        let claim = repository
            .claim_next_for(NotificationBackendKind::Relay, START_AT)
            .unwrap()
            .unwrap();
        repository
            .complete_delivery(
                &claim,
                DeliveryResult::Accepted(TransportAcceptance {
                    stage: TransportAcceptanceStage::Relay,
                    transport_message_id: Some("relay-notification-1".into()),
                    provider_message_id: None,
                    accepted_at: Some(START_AT + 1),
                }),
                START_AT + 1,
            )
            .unwrap();
        let history = repository.history_page(25, None).unwrap();
        assert_eq!(history.items.len(), 1);
        let item = &history.items[0];
        assert_eq!(item.acceptance_stage, Some(TransportAcceptanceStage::Relay));
        assert_eq!(
            item.relay_notification_id.as_deref(),
            Some("relay-notification-1")
        );
        assert_eq!(item.relay_accepted_at, Some(START_AT + 1));
        assert!(item.provider_message_id.is_none());
    }

    #[test]
    fn relay_reconciliation_is_bounded_monotonic_and_terminal() {
        let (_, repository) = setup_repository();
        let older = accept_relay(&repository, "remote-older", START_AT);
        let newer = accept_relay(&repository, "remote-newer", START_AT + 10);

        let selected = repository.relay_reconciliation_candidates(1).unwrap();
        let mut ordered = [older.clone(), newer.clone()];
        ordered.sort_by(|a, b| a.outbox_id.cmp(&b.outbox_id));
        assert_eq!(selected, ordered[..1]);
        assert_eq!(
            repository
                .relay_reconciliation_candidates_after(1, Some(&ordered[0].outbox_id))
                .unwrap(),
            ordered[1..]
        );
        assert_eq!(
            repository
                .relay_reconciliation_candidates(51)
                .unwrap_err()
                .code,
            "INVALID_RECONCILIATION_LIMIT"
        );
        for legacy in ["accepted", "pending", "sending"] {
            assert_eq!(
                repository
                    .apply_relay_remote_status(RelayRemoteStatusUpdate {
                        outbox_id: &older.outbox_id,
                        notification_id: &older.notification_id,
                        status: legacy,
                        updated_at: START_AT + 19,
                        provider_message_id: None,
                    })
                    .unwrap_err()
                    .code,
                "INVALID_RELAY_NOTIFICATION_STATUS"
            );
        }

        assert!(repository
            .apply_relay_remote_status(RelayRemoteStatusUpdate {
                outbox_id: &older.outbox_id,
                notification_id: &older.notification_id,
                status: "pending_channel",
                updated_at: START_AT + 20,
                provider_message_id: None,
            })
            .unwrap());
        assert!(!repository
            .apply_relay_remote_status(RelayRemoteStatusUpdate {
                outbox_id: &older.outbox_id,
                notification_id: &older.notification_id,
                status: "sending_channel",
                updated_at: START_AT + 20,
                provider_message_id: Some("conflicting-equal-revision"),
            })
            .unwrap());
        assert!(!repository
            .apply_relay_remote_status(RelayRemoteStatusUpdate {
                outbox_id: &older.outbox_id,
                notification_id: "wrong-notification",
                status: "sending_channel",
                updated_at: START_AT + 21,
                provider_message_id: None,
            })
            .unwrap());
        assert!(repository
            .apply_relay_remote_status(RelayRemoteStatusUpdate {
                outbox_id: &older.outbox_id,
                notification_id: &older.notification_id,
                status: "provider_accepted",
                updated_at: START_AT + 30,
                provider_message_id: Some("provider-1"),
            })
            .unwrap());
        assert!(!repository
            .apply_relay_remote_status(RelayRemoteStatusUpdate {
                outbox_id: &older.outbox_id,
                notification_id: &older.notification_id,
                status: "retry_wait",
                updated_at: START_AT + 40,
                provider_message_id: None,
            })
            .unwrap());

        let item = repository
            .history_page(10, None)
            .unwrap()
            .items
            .into_iter()
            .find(|item| item.id == older.outbox_id)
            .unwrap();
        assert_eq!(item.remote_status.as_deref(), Some("provider_accepted"));
        assert_eq!(item.remote_updated_at, Some(START_AT + 30));
        assert_eq!(
            item.remote_provider_message_id.as_deref(),
            Some("provider-1")
        );
        assert!(!repository
            .relay_reconciliation_candidates(50)
            .unwrap()
            .contains(&older));
    }

    #[test]
    fn relay_remote_monotonicity_survives_sqlite_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(temp.path()).unwrap());
        let repository = OutboxRepository::new(db.clone());
        let candidate = accept_relay(&repository, "restart-receipt", START_AT);
        assert!(repository
            .apply_relay_remote_status(RelayRemoteStatusUpdate {
                outbox_id: &candidate.outbox_id,
                notification_id: &candidate.notification_id,
                status: "provider_accepted",
                updated_at: START_AT + 50,
                provider_message_id: Some("provider-before-restart"),
            })
            .unwrap());
        drop(repository);
        drop(db);

        let reopened = OutboxRepository::new(Arc::new(Db::open(temp.path()).unwrap()));
        assert!(!reopened
            .apply_relay_remote_status(RelayRemoteStatusUpdate {
                outbox_id: &candidate.outbox_id,
                notification_id: &candidate.notification_id,
                status: "pending_channel",
                updated_at: START_AT + 100,
                provider_message_id: None,
            })
            .unwrap());
        let item = reopened.history_page(10, None).unwrap().items.remove(0);
        assert_eq!(item.remote_status.as_deref(), Some("provider_accepted"));
        assert_eq!(item.remote_updated_at, Some(START_AT + 50));
        assert_eq!(
            item.remote_provider_message_id.as_deref(),
            Some("provider-before-restart")
        );
    }

    #[test]
    fn destination_change_allows_completed_history_but_not_unfinished_receipts() {
        let (_, repository) = setup_repository();
        let accepted = accept_relay(&repository, "completed", START_AT);
        assert_eq!(
            repository
                .guard_destination_change(|| Ok(()))
                .unwrap_err()
                .code,
            "RELAY_DESTINATION_HAS_PENDING"
        );
        repository
            .apply_relay_remote_status(RelayRemoteStatusUpdate {
                outbox_id: &accepted.outbox_id,
                notification_id: &accepted.notification_id,
                status: "provider_accepted",
                updated_at: START_AT + 20,
                provider_message_id: None,
            })
            .unwrap();
        repository.guard_destination_change(|| Ok(())).unwrap();
    }

    #[test]
    fn retry_blocked_only_recovers_the_block_reason_that_became_ready() {
        let (db, repository) = setup_repository();
        repository
            .enqueue_control(
                NotificationEventKind::Test,
                "relay-blocked-one",
                &payload(),
                START_AT,
            )
            .unwrap();
        repository
            .enqueue_control(
                NotificationEventKind::Test,
                "relay-blocked-two",
                &payload(),
                START_AT,
            )
            .unwrap();
        let first = repository.claim_next(START_AT).unwrap().unwrap();
        repository
            .complete_delivery(&first, DeliveryResult::ReconnectRequired, START_AT + 1)
            .unwrap();
        let second = repository
            .claim_next_for(NotificationBackendKind::Relay, START_AT)
            .unwrap()
            .unwrap();
        repository
            .complete_delivery(&second, DeliveryResult::ReconnectRequired, START_AT + 1)
            .unwrap();

        db.with_connection(|conn| { conn.execute("UPDATE notification_outbox SET last_error_code='BLOCKED_TARGET_CHANGED' WHERE id=?", [&second.id])?; Ok(()) }).unwrap();
        assert_eq!(
            repository
                .retry_blocked_for_relay(START_AT + 2, true, false, false)
                .unwrap(),
            1
        );
        assert_eq!(status(&db, &first.id), "retry_wait");
        assert_eq!(status(&db, &second.id), "blocked_reconnect");
    }

    #[test]
    fn stats_and_history_are_redacted_stable_and_paginated() {
        let (_, repository) = setup_repository();
        for (index, created_at) in [START_AT, START_AT + 1, START_AT + 2]
            .into_iter()
            .enumerate()
        {
            assert!(repository
                .enqueue_control(
                    NotificationEventKind::Test,
                    &format!("history-{index}"),
                    &payload(),
                    created_at,
                )
                .unwrap());
        }
        let delivered = repository.claim_next(START_AT + 2).unwrap().unwrap();
        repository
            .complete_delivery(&delivered, accepted(), START_AT + 3)
            .unwrap();

        let stats = repository.stats().unwrap();
        assert_eq!(stats.pending_notifications, 2);
        assert_eq!(stats.blocked_notifications, 0);
        assert_eq!(stats.last_delivery_at, Some(START_AT + 3));

        let first = repository.history_page(2, None).unwrap();
        assert_eq!(first.items.len(), 2);
        assert!(first.items[0].created_at > first.items[1].created_at);
        assert_eq!(first.items[0].payload.title, "Fixture notification");
        assert_eq!(
            first.items[0].payload.content_mode,
            ResultContentMode::StatusOnly
        );
        assert_eq!(first.items[0].payload.content_bytes, 0);
        assert!(!first.items[0].body_available);
        let public_json = serde_json::to_value(&first.items[0]).unwrap();
        assert_eq!(
            public_json["payload"]["schemaVersion"],
            NOTIFICATION_PAYLOAD_SCHEMA_VERSION
        );
        assert_eq!(public_json["payload"]["body"], "");
        assert_eq!(public_json["deliveryBackend"], "relay");
        assert!(public_json["payload"].get("schema_version").is_none());
        let delivered_item = repository
            .history_page(100, None)
            .unwrap()
            .items
            .into_iter()
            .find(|item| item.status == "delivered")
            .unwrap();
        assert_eq!(
            delivered_item.acceptance_stage,
            Some(TransportAcceptanceStage::Provider)
        );
        assert!(first.next_cursor.is_some());
        let second = repository
            .history_page(2, first.next_cursor.as_deref())
            .unwrap();
        assert_eq!(second.items.len(), 1);
        assert!(second.next_cursor.is_none());
        assert_eq!(
            first.items.len() + second.items.len(),
            repository.history_page(100, None).unwrap().items.len()
        );
        assert_eq!(
            repository
                .history_page(2, Some("not-a-cursor"))
                .unwrap_err()
                .code,
            "INVALID_CURSOR"
        );
    }

    #[test]
    fn history_metadata_and_exact_lookup_survive_unreadable_local_payload() {
        let (db, repository) = setup_repository();
        enqueue_test(&repository, "unreadable-payload", START_AT);
        let id = db
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT id FROM notification_outbox WHERE dedupe_key = 'unreadable-payload'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .map_err(AppError::from)
            })
            .unwrap();
        db.with_connection(|conn| {
            conn.execute(
                "UPDATE notification_outbox SET payload_json = 'not-protected-json' WHERE id = ?",
                [&id],
            )?;
            Ok(())
        })
        .unwrap();

        let page = repository.history_page(10, None).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, id);
        assert_eq!(page.items[0].payload.title, "Fixture notification");
        assert_eq!(
            page.items[0].payload.content_mode,
            ResultContentMode::StatusOnly
        );
        assert!(!page.items[0].body_available);

        let exact = repository.history_item(&id).unwrap().unwrap();
        assert_eq!(exact.id, id);
        assert_eq!(exact.payload.title, "Fixture notification");
        assert!(repository.history_item("missing").unwrap().is_none());
        assert_eq!(
            repository.history_item("").unwrap_err().code,
            "INVALID_NOTIFICATION_ID"
        );
    }

    #[test]
    fn history_fails_closed_for_unknown_local_or_remote_status() {
        let (local_db, local_repository) = setup_repository();
        enqueue_test(&local_repository, "corrupt-local-status", START_AT);
        local_db
            .with_connection(|conn| {
                conn.execute_batch(
                    "PRAGMA ignore_check_constraints = ON;
                     UPDATE notification_outbox SET status = 'future_local_status';
                     PRAGMA ignore_check_constraints = OFF;",
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            local_repository.history_page(10, None).unwrap_err().code,
            "OUTBOX_CORRUPT"
        );

        let (remote_db, remote_repository) = setup_repository();
        remote_repository
            .enqueue_control(
                NotificationEventKind::Test,
                "corrupt-remote-status",
                &payload(),
                START_AT,
            )
            .unwrap();
        remote_db
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE notification_outbox SET remote_status = 'future_remote_status'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            remote_repository.history_page(10, None).unwrap_err().code,
            "OUTBOX_CORRUPT"
        );
    }

    struct BlockingTransport {
        entered: Arc<AtomicUsize>,
    }

    impl DeliveryTransport for BlockingTransport {
        fn deliver(
            &self,
            _claim: &OutboxClaim,
            cancellation: &DeliveryCancellation,
        ) -> DeliveryResult {
            self.entered.fetch_add(1, Ordering::Release);
            while !cancellation.is_cancelled() {
                thread::yield_now();
            }
            DeliveryResult::Cancelled
        }
    }

    struct CountingTransport(Arc<AtomicUsize>);

    impl DeliveryTransport for CountingTransport {
        fn deliver(
            &self,
            _claim: &OutboxClaim,
            _cancellation: &DeliveryCancellation,
        ) -> DeliveryResult {
            self.0.fetch_add(1, Ordering::Release);
            accepted()
        }
    }

    struct TimedTransport(Arc<Mutex<Vec<Instant>>>);

    impl DeliveryTransport for TimedTransport {
        fn deliver(
            &self,
            _claim: &OutboxClaim,
            _cancellation: &DeliveryCancellation,
        ) -> DeliveryResult {
            self.0.lock().unwrap().push(Instant::now());
            accepted()
        }
    }

    #[test]
    fn worker_throttles_backlog_to_one_send_per_second_after_hold_release() {
        let (_, repository) = setup_repository();
        let now = now_ms().unwrap();
        let hold = repository.hold_controller();
        hold.hold(0, crate::notification::hold::HoldDuration::Minutes15, now)
            .unwrap();
        enqueue_test(&repository, "held-throttle-one", now);
        enqueue_test(&repository, "held-throttle-two", now + 1);
        let timestamps = Arc::new(Mutex::new(Vec::new()));
        let mut worker = NotificationDeliveryWorker::start(
            repository,
            Arc::new(TimedTransport(Arc::clone(&timestamps))),
            Arc::new(FixedPolicyProvider(POLICY_HASH)),
        )
        .unwrap();

        // Let the worker observe the durable active state before resume.
        thread::sleep(Duration::from_millis(300));
        hold.resume(1, now_ms().unwrap()).unwrap();
        worker.wake();
        let deadline = Instant::now() + Duration::from_secs(4);
        while timestamps.lock().unwrap().len() < 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        worker.shutdown().unwrap();
        let timestamps = timestamps.lock().unwrap();
        assert_eq!(timestamps.len(), 2);
        assert!(timestamps[1].duration_since(timestamps[0]) >= Duration::from_millis(900));
    }

    #[test]
    fn worker_throttles_fast_hold_resume_even_without_observing_active() {
        let (_, repository) = setup_repository();
        let now = now_ms().unwrap();
        let hold = repository.hold_controller();
        let receipt = hold
            .hold(0, crate::notification::hold::HoldDuration::Minutes15, now)
            .unwrap();
        hold.resume(receipt.state.revision, now + 1).unwrap();
        enqueue_test(&repository, "fast-hold-resume", now);
        let timestamps = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        let mut worker = NotificationDeliveryWorker::start(
            repository,
            Arc::new(TimedTransport(Arc::clone(&timestamps))),
            Arc::new(FixedPolicyProvider(POLICY_HASH)),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while timestamps.lock().unwrap().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        worker.shutdown().unwrap();
        assert!(
            timestamps.lock().unwrap()[0].duration_since(started) >= Duration::from_millis(900)
        );
    }

    #[test]
    fn worker_throttles_expired_hold_on_fresh_start() {
        let (_, repository) = setup_repository();
        let now = now_ms().unwrap();
        let hold = repository.hold_controller();
        hold.hold(
            0,
            crate::notification::hold::HoldDuration::Minutes15,
            now - 15 * 60 * 1_000,
        )
        .unwrap();
        enqueue_test(&repository, "expired-hold-restart", now);
        let timestamps = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        let mut worker = NotificationDeliveryWorker::start(
            repository,
            Arc::new(TimedTransport(Arc::clone(&timestamps))),
            Arc::new(FixedPolicyProvider(POLICY_HASH)),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while timestamps.lock().unwrap().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        worker.shutdown().unwrap();
        assert!(
            timestamps.lock().unwrap()[0].duration_since(started) >= Duration::from_millis(900)
        );
    }

    #[test]
    fn worker_revokes_attention_when_the_current_setting_is_disabled() {
        let (db, repository) = setup_repository();
        let now = now_ms().unwrap();
        db.with_transaction(|tx| {
            assert!(OutboxRepository::insert_attention_tx(
                tx,
                AttentionNotificationInsert {
                    agent_run_key: None,
                    dedupe_key: "disabled-attention-policy",
                    payload: &payload(),
                    content_policy_hash: POLICY_HASH,
                    created_at: now,
                    observed_at: now,
                    expires_at: None,
                },
            )?);
            Ok(())
        })
        .unwrap();
        let deliveries = Arc::new(AtomicUsize::new(0));
        let mut worker = NotificationDeliveryWorker::start(
            repository,
            Arc::new(CountingTransport(Arc::clone(&deliveries))),
            Arc::new(DisabledPolicyProvider),
        )
        .unwrap();
        worker.wake();

        let deadline = Instant::now() + Duration::from_secs(2);
        while text(&db, "SELECT status FROM notification_outbox") != "cancelled"
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        worker.shutdown().unwrap();

        assert_eq!(deliveries.load(Ordering::Acquire), 0);
        assert_eq!(
            text(&db, "SELECT last_error_code FROM notification_outbox"),
            "NOTIFICATION_POLICY_CHANGED"
        );
    }

    #[test]
    fn worker_cancels_stale_policy_payload_without_calling_transport() {
        let (db, repository) = setup_repository();
        let now = now_ms().unwrap();
        enqueue_run(
            &db,
            NotificationEventKind::RunCompleted,
            "stale-policy",
            "stale-policy-run",
            now,
            now,
        );
        let deliveries = Arc::new(AtomicUsize::new(0));
        let mut worker = NotificationDeliveryWorker::start(
            repository,
            Arc::new(CountingTransport(Arc::clone(&deliveries))),
            Arc::new(FixedPolicyProvider(
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            )),
        )
        .unwrap();
        worker.wake();

        let deadline = Instant::now() + Duration::from_secs(2);
        while text(&db, "SELECT status FROM notification_outbox") != "cancelled"
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        worker.shutdown().unwrap();

        assert_eq!(deliveries.load(Ordering::Acquire), 0);
        assert_eq!(
            text(&db, "SELECT status FROM notification_outbox"),
            "cancelled"
        );
        assert_eq!(
            text(&db, "SELECT last_error_code FROM notification_outbox"),
            "NOTIFICATION_POLICY_CHANGED"
        );
    }

    #[test]
    fn worker_is_wakeable_and_shutdown_cancels_inflight_delivery() {
        let (db, repository) = setup_repository();
        let now = now_ms().unwrap();
        enqueue_test(&repository, "worker", now);
        let entered = Arc::new(AtomicUsize::new(0));
        let transport = Arc::new(BlockingTransport {
            entered: entered.clone(),
        });
        let mut worker = NotificationDeliveryWorker::start(
            repository,
            transport,
            Arc::new(FixedPolicyProvider(POLICY_HASH)),
        )
        .unwrap();
        worker.wake();
        let deadline = Instant::now() + Duration::from_secs(2);
        while entered.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(entered.load(Ordering::Acquire), 1);
        worker.shutdown().unwrap();
        assert_eq!(
            text(&db, "SELECT status FROM notification_outbox"),
            "retry_wait"
        );
    }

    #[test]
    fn worker_error_reporting_is_stable_and_rate_limited() {
        let mut reporter = WorkerErrorReporter::new();
        let first = Instant::now();
        assert!(reporter.should_report("OUTBOX_FIXTURE", first));
        assert!(!reporter.should_report("OUTBOX_FIXTURE", first + Duration::from_secs(9)));
        assert!(reporter.should_report("OUTBOX_OTHER", first + Duration::from_secs(9)));
        assert!(reporter.should_report("OUTBOX_OTHER", first + Duration::from_secs(19)));
    }

    #[test]
    fn worker_signal_observes_cancel_before_waiting() {
        let signal = WorkerSignal::default();
        let observed_generation = signal.generation();
        signal.cancel();

        assert!(signal.wait_while_unchanged(observed_generation));
    }

    #[test]
    fn worker_signal_observes_wake_before_waiting() {
        let signal = WorkerSignal::default();
        let observed_generation = signal.generation();
        signal.wake();

        assert!(!signal.wait_while_unchanged(observed_generation));
    }

    #[test]
    fn q20_network_retry_preserves_outbox_identity_and_payload() {
        let (db, repository) = setup_repository();
        enqueue_test(&repository, "q20-retry-identity", START_AT);
        let id = deterministic_id("outbox", "q20-retry-identity");

        let first_claim = repository.claim_next(START_AT).unwrap().unwrap();
        assert_eq!(first_claim.id, id);
        repository
            .complete_delivery(
                &first_claim,
                DeliveryResult::NetworkUnavailable,
                START_AT + 1,
            )
            .unwrap();
        assert_eq!(status(&db, &id), "retry_wait");

        let retry_at = START_AT + 1 + NETWORK_BACKOFF_MS[0];
        let retry_claim = repository.claim_next(retry_at).unwrap().unwrap();
        assert_eq!(retry_claim.id, id);
        assert_eq!(retry_claim.dedupe_key, "q20-retry-identity");
        assert_eq!(retry_claim.attempt_count, 2);
        repository
            .complete_delivery(&retry_claim, accepted(), retry_at + 1)
            .unwrap();
        assert_eq!(status(&db, &id), "delivered");
        assert_eq!(
            scalar(&db, "SELECT attempt_count FROM notification_outbox"),
            2
        );
    }

    #[test]
    fn q19_mock_relay_transport_delivers_and_marks_outbox_accepted() {
        let (db, repository) = setup_repository();
        let now = now_ms().unwrap();
        enqueue_run(
            &db,
            NotificationEventKind::RunCompleted,
            "q19-delivery",
            "q19-run",
            now,
            now,
        );
        let deliveries = Arc::new(AtomicUsize::new(0));
        let mut worker = NotificationDeliveryWorker::start(
            repository,
            Arc::new(CountingTransport(Arc::clone(&deliveries))),
            Arc::new(FixedPolicyProvider(POLICY_HASH)),
        )
        .unwrap();
        worker.wake();

        let deadline = Instant::now() + Duration::from_secs(2);
        while text(&db, "SELECT status FROM notification_outbox") != "delivered"
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        worker.shutdown().unwrap();

        assert_eq!(deliveries.load(Ordering::Acquire), 1);
        assert_eq!(
            text(&db, "SELECT status FROM notification_outbox"),
            "delivered"
        );
    }
}
