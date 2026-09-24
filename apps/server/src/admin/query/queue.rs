use blake3::Hasher;
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};
use thiserror::Error;

pub use relay_admin_api::InboundCommand;

const DEFAULT_PAGE_LIMIT: usize = 50;
const MAX_PAGE_LIMIT: usize = 100;

const DELIVERY_REF_DOMAIN: &str = "promptdock.admin.delivery-ref.v1";
const DEVICE_REF_DOMAIN: &str = "promptdock.admin.delivery-origin-device-ref.v1";
const INTERACTIVE_REPLY_REF_DOMAIN: &str = "promptdock.admin.interactive-reply-ref.v1";
const INBOUND_COMMAND_REF_DOMAIN: &str = "promptdock.admin.inbound-command-ref.v1";
const ORPHAN_COMMAND_REF_DOMAIN: &str = "promptdock.admin.orphan-command-ref.v1";

// Keep the select list explicit. In particular, this query must never fetch notification content,
// upstream identifiers, payload hashes, or provider error messages.
const DELIVERIES_SQL: &str = r#"
WITH all_deliveries AS (
    SELECT 'notification:' || id AS id, origin_kind, origin_device_id, kind,
           priority, status, attempt_count, last_error_code, created_at, updated_at,
           NULL AS segment_count, NULL AS accepted_segments
    FROM notification_outbox
    UNION ALL
    SELECT 'bundle:' || bundle.id AS id, 'device' AS origin_kind,
           bundle.origin_device_id, bundle.kind, 0 AS priority, bundle.status,
           MIN(COALESCE((
               SELECT SUM(segment.attempt_count)
               FROM notification_bundle_segments AS segment
               WHERE segment.bundle_row_id = bundle.id
           ), 0), 4294967295) AS attempt_count,
           CASE bundle.status
               WHEN 'partial_failed' THEN 'BUNDLE_PARTIAL_FAILED'
               WHEN 'blocked_target_changed' THEN 'TARGET_ACCOUNT_CHANGED'
               WHEN 'delivery_unknown' THEN 'BUNDLE_DELIVERY_UNKNOWN'
               ELSE NULL
           END AS last_error_code,
           bundle.created_at, bundle.updated_at,
           bundle.segment_count, bundle.accepted_segments
    FROM notification_bundles AS bundle
)
SELECT id, origin_kind, origin_device_id, kind, priority, status, attempt_count,
       last_error_code, created_at, updated_at, segment_count, accepted_segments
FROM all_deliveries
WHERE (?1 IS NULL OR
       CASE status
           WHEN 'pending_channel' THEN 'queued'
           WHEN 'queued' THEN 'queued'
           WHEN 'sending_channel' THEN 'sending'
           WHEN 'delivering' THEN 'sending'
           WHEN 'retry_wait' THEN 'retrying'
           WHEN 'delivery_unknown' THEN 'blocked'
           WHEN 'blocked_activation' THEN 'blocked'
           WHEN 'blocked_reconnect' THEN 'blocked'
           WHEN 'blocked_target_changed' THEN 'blocked'
           WHEN 'provider_accepted' THEN 'accepted'
           WHEN 'dead_letter' THEN 'failed'
           WHEN 'partial_failed' THEN 'failed'
           WHEN 'cancelled' THEN 'cancelled'
           WHEN 'expired' THEN 'expired'
       END = ?1)
  AND (?2 IS NULL OR origin_kind = ?2)
  AND (?3 IS NULL OR created_at >= ?3)
  AND (?4 IS NULL OR created_at <= ?4)
  AND (?5 IS NULL OR created_at < ?5 OR (created_at = ?5 AND id < ?6))
ORDER BY created_at DESC, id DESC
LIMIT ?7
"#;

// Fingerprints are reduced inside SQLite so a complete fingerprint never crosses the query
// boundary. The inbound join exposes only rowid, which is sufficient to construct the same safe
// command reference returned by the inbound projection without selecting message_key.
const INTERACTIVE_REPLIES_SQL: &str = r#"
SELECT o.id,
       substr(o.target_account_fingerprint, 1, 7) || '…' ||
           substr(o.target_account_fingerprint, -4) AS target_hint,
       o.status, o.updated_at AS occurred_at, o.last_error_code,
       c.rowid AS inbound_row_id
FROM notification_outbox AS o
LEFT JOIN inbound_commands AS c ON c.rowid = (
    SELECT MIN(candidate.rowid)
    FROM inbound_commands AS candidate
    WHERE candidate.reply_notification_id = o.notification_id
)
WHERE o.origin_kind = 'system'
  AND o.kind = 'interactive_reply'
  AND (?1 IS NULL OR
       CASE o.status
           WHEN 'pending_channel' THEN 'queued'
           WHEN 'sending_channel' THEN 'sending'
           WHEN 'retry_wait' THEN 'retrying'
           WHEN 'blocked_activation' THEN 'blocked'
           WHEN 'blocked_reconnect' THEN 'blocked'
           WHEN 'provider_accepted' THEN 'accepted'
           WHEN 'dead_letter' THEN 'failed'
           WHEN 'cancelled' THEN 'cancelled'
           WHEN 'expired' THEN 'expired'
       END = ?1)
  AND (?2 IS NULL OR o.updated_at < ?2 OR (o.updated_at = ?2 AND o.id < ?3))
ORDER BY o.updated_at DESC, o.id DESC
LIMIT ?4
"#;

// command_json is consulted only by closed CASE expressions. It is never selected as a value.
// This preserves list filter semantics while ensuring only a finite admin action can cross the
// database boundary. sender_fingerprint is likewise reduced to a non-reversible display hint in
// SQLite, and message_key is not read at all.
const INBOUND_COMMANDS_SQL: &str = r#"
SELECT rowid AS internal_row_id,
       CASE command_kind
           WHEN 'help' THEN 'help'
           WHEN 'list_devices' THEN 'list_devices'
           WHEN 'list_runs' THEN
               CASE json_extract(command_json, '$.filter')
                   WHEN 'recent' THEN 'list_recent'
                   WHEN 'failed' THEN 'list_failed'
                   ELSE 'list_jobs'
               END
           WHEN 'next_page' THEN 'next_page'
           WHEN 'get_run_status' THEN 'get_status'
           WHEN 'get_run_detail' THEN 'get_detail'
           WHEN 'get_run_tree' THEN 'get_tree'
           ELSE 'unknown'
       END AS admin_action,
       substr(sender_fingerprint, 1, 7) || '…' || substr(sender_fingerprint, -4) AS sender_hint,
       status, attempt_count, last_error_code, created_at, expires_at, updated_at
FROM inbound_commands
WHERE (?1 IS NULL OR
       CASE command_kind
           WHEN 'help' THEN 'help'
           WHEN 'list_devices' THEN 'list_devices'
           WHEN 'list_runs' THEN
               CASE json_extract(command_json, '$.filter')
                   WHEN 'recent' THEN 'list_recent'
                   WHEN 'failed' THEN 'list_failed'
                   ELSE 'list_jobs'
               END
           WHEN 'next_page' THEN 'next_page'
           WHEN 'get_run_status' THEN 'get_status'
           WHEN 'get_run_detail' THEN 'get_detail'
           WHEN 'get_run_tree' THEN 'get_tree'
           ELSE 'unknown'
       END = ?1)
  AND (?2 IS NULL OR status = ?2)
  AND (?3 IS NULL OR created_at >= ?3)
  AND (?4 IS NULL OR created_at < ?4 OR (created_at = ?4 AND rowid < ?5))
ORDER BY created_at DESC, rowid DESC
LIMIT ?6
"#;

#[derive(Clone)]
pub struct AdminQueueQueryService {
    pool: SqlitePool,
    max_page_size: usize,
}

impl AdminQueueQueryService {
    pub fn new(pool: SqlitePool, max_page_size: usize) -> Self {
        Self {
            pool,
            max_page_size: max_page_size.min(MAX_PAGE_LIMIT),
        }
    }

    pub async fn list_deliveries(
        &self,
        query: DeliveryListQuery,
    ) -> Result<QueuePage<DeliveryListItem>, AdminQueueQueryError> {
        validate_limit(query.limit, self.max_page_size)?;
        validate_timestamp_range(query.since_at, query.until_at)?;
        validate_string_cursor(query.cursor.as_ref())?;

        let fetch_limit = fetch_limit(query.limit)?;
        let cursor_timestamp = query.cursor.as_ref().map(|cursor| cursor.sort_timestamp);
        let cursor_tie_break = query
            .cursor
            .as_ref()
            .map(|cursor| cursor.tie_break.as_str());
        let mut rows = sqlx::query_as::<_, DeliveryRow>(DELIVERIES_SQL)
            .bind(query.state.map(DeliveryState::as_str))
            .bind(query.origin.map(DeliveryOrigin::as_str))
            .bind(query.since_at)
            .bind(query.until_at)
            .bind(cursor_timestamp)
            .bind(cursor_tie_break)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(database_error)?;

        let next_cursor_position = string_page_boundary(&mut rows, query.limit, |row| {
            (row.created_at, row.id.clone())
        });
        let items = rows
            .into_iter()
            .map(DeliveryListItem::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QueuePage {
            items,
            next_cursor_position,
        })
    }

    pub async fn list_interactive_replies(
        &self,
        query: InteractiveReplyListQuery,
    ) -> Result<QueuePage<InteractiveReplyListItem>, AdminQueueQueryError> {
        validate_limit(query.limit, self.max_page_size)?;
        validate_string_cursor(query.cursor.as_ref())?;

        let fetch_limit = fetch_limit(query.limit)?;
        let cursor_timestamp = query.cursor.as_ref().map(|cursor| cursor.sort_timestamp);
        let cursor_tie_break = query
            .cursor
            .as_ref()
            .map(|cursor| cursor.tie_break.as_str());
        let mut rows = sqlx::query_as::<_, InteractiveReplyRow>(INTERACTIVE_REPLIES_SQL)
            .bind(query.state.map(DeliveryState::as_str))
            .bind(cursor_timestamp)
            .bind(cursor_tie_break)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(database_error)?;

        let next_cursor_position = string_page_boundary(&mut rows, query.limit, |row| {
            (row.occurred_at, row.id.clone())
        });
        let items = rows
            .into_iter()
            .map(InteractiveReplyListItem::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QueuePage {
            items,
            next_cursor_position,
        })
    }

    pub async fn list_inbound_commands(
        &self,
        query: InboundCommandListQuery,
    ) -> Result<QueuePage<InboundCommandListItem>, AdminQueueQueryError> {
        validate_limit(query.limit, self.max_page_size)?;
        validate_nonnegative_timestamp(query.since_at)?;
        let cursor = parse_rowid_cursor(query.cursor.as_ref())?;

        let fetch_limit = fetch_limit(query.limit)?;
        let cursor_timestamp = cursor.as_ref().map(|cursor| cursor.sort_timestamp);
        let cursor_row_id = cursor.as_ref().map(|cursor| cursor.row_id);
        let mut rows = sqlx::query_as::<_, InboundCommandRow>(INBOUND_COMMANDS_SQL)
            .bind(query.command.map(InboundCommand::as_str))
            .bind(query.state.map(InboundState::as_str))
            .bind(query.since_at)
            .bind(cursor_timestamp)
            .bind(cursor_row_id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(database_error)?;

        let next_cursor_position = rowid_page_boundary(&mut rows, query.limit);
        let items = rows
            .into_iter()
            .map(InboundCommandListItem::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QueuePage {
            items,
            next_cursor_position,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct QueueCursorPosition {
    pub sort_timestamp: i64,
    pub tie_break: String,
}

impl std::fmt::Debug for QueueCursorPosition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QueueCursorPosition")
            .field("sort_timestamp", &self.sort_timestamp)
            .field("tie_break", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuePage<T> {
    pub items: Vec<T>,
    pub next_cursor_position: Option<QueueCursorPosition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryListQuery {
    pub state: Option<DeliveryState>,
    pub origin: Option<DeliveryOrigin>,
    pub since_at: Option<i64>,
    pub until_at: Option<i64>,
    pub limit: usize,
    pub cursor: Option<QueueCursorPosition>,
}

impl Default for DeliveryListQuery {
    fn default() -> Self {
        Self {
            state: None,
            origin: None,
            since_at: None,
            until_at: None,
            limit: DEFAULT_PAGE_LIMIT,
            cursor: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractiveReplyListQuery {
    pub state: Option<DeliveryState>,
    pub limit: usize,
    pub cursor: Option<QueueCursorPosition>,
}

impl Default for InteractiveReplyListQuery {
    fn default() -> Self {
        Self {
            state: None,
            limit: DEFAULT_PAGE_LIMIT,
            cursor: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundCommandListQuery {
    pub command: Option<InboundCommand>,
    pub state: Option<InboundState>,
    pub since_at: Option<i64>,
    pub limit: usize,
    pub cursor: Option<QueueCursorPosition>,
}

impl Default for InboundCommandListQuery {
    fn default() -> Self {
        Self {
            command: None,
            state: None,
            since_at: None,
            limit: DEFAULT_PAGE_LIMIT,
            cursor: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryListItem {
    pub id: String,
    pub origin: DeliveryOrigin,
    pub origin_label: String,
    pub kind: DeliveryKind,
    pub state: DeliveryState,
    pub priority: i64,
    pub attempt_count: i64,
    pub error_code: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_segments: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOrigin {
    Device,
    System,
    Admin,
}

impl DeliveryOrigin {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::System => "system",
            Self::Admin => "admin",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    RunEvent,
    Test,
    InteractiveReply,
    Activation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Queued,
    Sending,
    Retrying,
    Blocked,
    Accepted,
    Failed,
    Cancelled,
    Expired,
}

impl DeliveryState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sending => "sending",
            Self::Retrying => "retrying",
            Self::Blocked => "blocked",
            Self::Accepted => "accepted",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractiveReplyListItem {
    pub id: String,
    pub command_ref: String,
    pub target_fingerprint: String,
    pub state: DeliveryState,
    pub occurred_at: i64,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundCommandListItem {
    pub id: String,
    pub command: InboundCommand,
    pub sender_hint: String,
    pub state: InboundState,
    pub attempt_count: i64,
    pub error_code: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InboundState {
    Received,
    Dispatching,
    WaitingGateway,
    ReplyQueued,
    Expired,
    DeadLetter,
}

impl InboundState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Dispatching => "dispatching",
            Self::WaitingGateway => "waiting_gateway",
            Self::ReplyQueued => "reply_queued",
            Self::Expired => "expired",
            Self::DeadLetter => "dead_letter",
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AdminQueueQueryError {
    #[error("admin queue query is invalid")]
    InvalidQuery,
    #[error("admin queue cursor position is invalid")]
    InvalidCursor,
    #[error("admin queue projection is invalid")]
    InvalidProjection,
    #[error("admin queue database query failed")]
    Database,
}

#[derive(FromRow)]
struct DeliveryRow {
    id: String,
    origin_kind: String,
    origin_device_id: Option<String>,
    kind: String,
    priority: i64,
    status: String,
    attempt_count: i64,
    last_error_code: Option<String>,
    created_at: i64,
    updated_at: i64,
    segment_count: Option<i64>,
    accepted_segments: Option<i64>,
}

impl TryFrom<DeliveryRow> for DeliveryListItem {
    type Error = AdminQueueQueryError;

    fn try_from(row: DeliveryRow) -> Result<Self, Self::Error> {
        let origin = delivery_origin(&row.origin_kind)?;
        let origin_label = match (origin, row.origin_device_id.as_deref()) {
            (DeliveryOrigin::System, None) => "system".to_owned(),
            (DeliveryOrigin::Admin, None) => "admin".to_owned(),
            (DeliveryOrigin::Device, Some(device_id)) => {
                let safe_ref = domain_ref(DEVICE_REF_DOMAIN, device_id.as_bytes());
                format!("device:{}", &safe_ref[..12])
            }
            _ => return Err(AdminQueueQueryError::InvalidProjection),
        };
        validate_delivery_numbers(
            row.priority,
            row.attempt_count,
            row.created_at,
            row.updated_at,
        )?;
        match (row.segment_count, row.accepted_segments) {
            (Some(segment_count), Some(accepted_segments))
                if (1..=128).contains(&segment_count)
                    && (0..=segment_count).contains(&accepted_segments) => {}
            (None, None) => {}
            _ => return Err(AdminQueueQueryError::InvalidProjection),
        }

        Ok(Self {
            id: format!(
                "delivery_{}",
                domain_ref(DELIVERY_REF_DOMAIN, row.id.as_bytes())
            ),
            origin,
            origin_label,
            kind: delivery_kind(&row.kind),
            state: delivery_state(&row.status)?,
            priority: row.priority,
            attempt_count: row.attempt_count,
            error_code: safe_error_code(row.last_error_code),
            created_at: row.created_at,
            updated_at: row.updated_at,
            segment_count: row.segment_count,
            accepted_segments: row.accepted_segments,
        })
    }
}

#[derive(FromRow)]
struct InteractiveReplyRow {
    id: String,
    target_hint: String,
    status: String,
    occurred_at: i64,
    last_error_code: Option<String>,
    inbound_row_id: Option<i64>,
}

impl TryFrom<InteractiveReplyRow> for InteractiveReplyListItem {
    type Error = AdminQueueQueryError;

    fn try_from(row: InteractiveReplyRow) -> Result<Self, Self::Error> {
        if row.occurred_at < 0 || !valid_fingerprint_hint(&row.target_hint) {
            return Err(AdminQueueQueryError::InvalidProjection);
        }
        let command_ref = match row.inbound_row_id {
            Some(row_id) if row_id > 0 => inbound_command_ref(row_id),
            Some(_) => return Err(AdminQueueQueryError::InvalidProjection),
            None => format!(
                "command_{}",
                domain_ref(ORPHAN_COMMAND_REF_DOMAIN, row.id.as_bytes())
            ),
        };
        Ok(Self {
            id: format!(
                "reply_{}",
                domain_ref(INTERACTIVE_REPLY_REF_DOMAIN, row.id.as_bytes())
            ),
            command_ref,
            target_fingerprint: row.target_hint,
            state: delivery_state(&row.status)?,
            occurred_at: row.occurred_at,
            error_code: safe_error_code(row.last_error_code),
        })
    }
}

#[derive(FromRow)]
struct InboundCommandRow {
    internal_row_id: i64,
    admin_action: String,
    sender_hint: String,
    status: String,
    attempt_count: i64,
    last_error_code: Option<String>,
    created_at: i64,
    expires_at: i64,
    updated_at: i64,
}

impl TryFrom<InboundCommandRow> for InboundCommandListItem {
    type Error = AdminQueueQueryError;

    fn try_from(row: InboundCommandRow) -> Result<Self, Self::Error> {
        if row.internal_row_id <= 0
            || row.attempt_count < 0
            || row.created_at < 0
            || row.updated_at < row.created_at
            || row.expires_at <= row.created_at
            || !valid_fingerprint_hint(&row.sender_hint)
        {
            return Err(AdminQueueQueryError::InvalidProjection);
        }
        Ok(Self {
            id: inbound_command_ref(row.internal_row_id),
            command: inbound_command(&row.admin_action)?,
            sender_hint: row.sender_hint,
            state: inbound_state(&row.status)?,
            attempt_count: row.attempt_count,
            error_code: safe_error_code(row.last_error_code),
            created_at: row.created_at,
            expires_at: row.expires_at,
            updated_at: row.updated_at,
        })
    }
}

fn delivery_origin(value: &str) -> Result<DeliveryOrigin, AdminQueueQueryError> {
    match value {
        "device" => Ok(DeliveryOrigin::Device),
        "system" => Ok(DeliveryOrigin::System),
        "admin" => Ok(DeliveryOrigin::Admin),
        _ => Err(AdminQueueQueryError::InvalidProjection),
    }
}

fn delivery_kind(value: &str) -> DeliveryKind {
    match value {
        "test" => DeliveryKind::Test,
        "interactive_reply" => DeliveryKind::InteractiveReply,
        "activation" => DeliveryKind::Activation,
        _ => DeliveryKind::RunEvent,
    }
}

fn delivery_state(value: &str) -> Result<DeliveryState, AdminQueueQueryError> {
    match value {
        "pending_channel" | "queued" => Ok(DeliveryState::Queued),
        "sending_channel" | "delivering" => Ok(DeliveryState::Sending),
        "retry_wait" => Ok(DeliveryState::Retrying),
        "blocked_activation"
        | "blocked_reconnect"
        | "blocked_target_changed"
        | "delivery_unknown" => Ok(DeliveryState::Blocked),
        "provider_accepted" => Ok(DeliveryState::Accepted),
        "dead_letter" | "partial_failed" => Ok(DeliveryState::Failed),
        "cancelled" => Ok(DeliveryState::Cancelled),
        "expired" => Ok(DeliveryState::Expired),
        _ => Err(AdminQueueQueryError::InvalidProjection),
    }
}

fn inbound_command(value: &str) -> Result<InboundCommand, AdminQueueQueryError> {
    match value {
        "help" => Ok(InboundCommand::Help),
        "list_devices" => Ok(InboundCommand::ListDevices),
        "list_jobs" => Ok(InboundCommand::ListJobs),
        "list_recent" => Ok(InboundCommand::ListRecent),
        "list_failed" => Ok(InboundCommand::ListFailed),
        "next_page" => Ok(InboundCommand::NextPage),
        "get_status" => Ok(InboundCommand::GetStatus),
        "get_detail" => Ok(InboundCommand::GetDetail),
        "get_tree" => Ok(InboundCommand::GetTree),
        "unknown" => Ok(InboundCommand::Unknown),
        _ => Err(AdminQueueQueryError::InvalidProjection),
    }
}

fn inbound_state(value: &str) -> Result<InboundState, AdminQueueQueryError> {
    match value {
        "received" => Ok(InboundState::Received),
        "dispatching" => Ok(InboundState::Dispatching),
        "waiting_gateway" => Ok(InboundState::WaitingGateway),
        "reply_queued" => Ok(InboundState::ReplyQueued),
        "expired" => Ok(InboundState::Expired),
        "dead_letter" => Ok(InboundState::DeadLetter),
        _ => Err(AdminQueueQueryError::InvalidProjection),
    }
}

fn validate_limit(limit: usize, max_page_size: usize) -> Result<(), AdminQueueQueryError> {
    if (1..=max_page_size).contains(&limit) {
        Ok(())
    } else {
        Err(AdminQueueQueryError::InvalidQuery)
    }
}

fn validate_nonnegative_timestamp(value: Option<i64>) -> Result<(), AdminQueueQueryError> {
    if value.is_some_and(|value| value < 0) {
        Err(AdminQueueQueryError::InvalidQuery)
    } else {
        Ok(())
    }
}

fn validate_timestamp_range(
    since_at: Option<i64>,
    until_at: Option<i64>,
) -> Result<(), AdminQueueQueryError> {
    validate_nonnegative_timestamp(since_at)?;
    validate_nonnegative_timestamp(until_at)?;
    if since_at
        .zip(until_at)
        .is_some_and(|(since, until)| since > until)
    {
        Err(AdminQueueQueryError::InvalidQuery)
    } else {
        Ok(())
    }
}

fn validate_string_cursor(
    cursor: Option<&QueueCursorPosition>,
) -> Result<(), AdminQueueQueryError> {
    if cursor.is_some_and(|cursor| {
        cursor.sort_timestamp < 0
            || cursor.tie_break.is_empty()
            || cursor.tie_break.len() > 256
            || cursor.tie_break.chars().any(char::is_control)
    }) {
        Err(AdminQueueQueryError::InvalidCursor)
    } else {
        Ok(())
    }
}

#[derive(Debug)]
struct RowidCursor {
    sort_timestamp: i64,
    row_id: i64,
}

fn parse_rowid_cursor(
    cursor: Option<&QueueCursorPosition>,
) -> Result<Option<RowidCursor>, AdminQueueQueryError> {
    cursor
        .map(|cursor| {
            let row_id = cursor
                .tie_break
                .parse::<i64>()
                .map_err(|_| AdminQueueQueryError::InvalidCursor)?;
            if cursor.sort_timestamp < 0 || row_id <= 0 {
                return Err(AdminQueueQueryError::InvalidCursor);
            }
            Ok(RowidCursor {
                sort_timestamp: cursor.sort_timestamp,
                row_id,
            })
        })
        .transpose()
}

fn fetch_limit(limit: usize) -> Result<i64, AdminQueueQueryError> {
    i64::try_from(limit.saturating_add(1)).map_err(|_| AdminQueueQueryError::InvalidQuery)
}

fn string_page_boundary<T>(
    rows: &mut Vec<T>,
    limit: usize,
    position: impl Fn(&T) -> (i64, String),
) -> Option<QueueCursorPosition> {
    if rows.len() <= limit {
        return None;
    }
    rows.truncate(limit);
    rows.last().map(|row| {
        let (sort_timestamp, tie_break) = position(row);
        QueueCursorPosition {
            sort_timestamp,
            tie_break,
        }
    })
}

fn rowid_page_boundary(
    rows: &mut Vec<InboundCommandRow>,
    limit: usize,
) -> Option<QueueCursorPosition> {
    if rows.len() <= limit {
        return None;
    }
    rows.truncate(limit);
    rows.last().map(|row| QueueCursorPosition {
        sort_timestamp: row.created_at,
        tie_break: row.internal_row_id.to_string(),
    })
}

fn validate_delivery_numbers(
    priority: i64,
    attempt_count: i64,
    created_at: i64,
    updated_at: i64,
) -> Result<(), AdminQueueQueryError> {
    if !(0..=255).contains(&priority)
        || attempt_count < 0
        || created_at < 0
        || updated_at < created_at
    {
        Err(AdminQueueQueryError::InvalidProjection)
    } else {
        Ok(())
    }
}

fn valid_fingerprint_hint(value: &str) -> bool {
    let Some((prefix, suffix)) = value.split_once('…') else {
        return false;
    };
    prefix.len() == 7
        && prefix.starts_with("wx:")
        && suffix.len() == 4
        && prefix[3..]
            .bytes()
            .chain(suffix.bytes())
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn safe_error_code(value: Option<String>) -> Option<String> {
    value.map(|value| {
        if !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            value
        } else {
            "REDACTED_ERROR_CODE".to_owned()
        }
    })
}

fn inbound_command_ref(row_id: i64) -> String {
    format!(
        "inbound_{}",
        domain_ref(INBOUND_COMMAND_REF_DOMAIN, &row_id.to_be_bytes())
    )
}

fn domain_ref(domain: &'static str, identity: &[u8]) -> String {
    let mut hasher = Hasher::new_derive_key(domain);
    hasher.update(identity);
    hasher.finalize().to_hex().to_string()
}

fn database_error<T>(_error: T) -> AdminQueueQueryError {
    AdminQueueQueryError::Database
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::{config::DatabaseConfig, db};

    const CREATED_AT: i64 = 1_787_652_000_000;
    const DEVICE_ID: &str = "11111111-1111-4111-8111-111111111111";
    const OUTBOX_ID: &str = "private-outbox-row-id";
    const MESSAGE_KEY: &str = "privacy-sentinel-message-key";
    const TITLE: &str = "privacy-sentinel-title";
    const BODY: &str = "privacy-sentinel-body";
    const PAYLOAD_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PROVIDER_MESSAGE_ID: &str = "privacy-sentinel-provider-id";
    const ERROR_MESSAGE: &str = "privacy-sentinel-provider-error-message";
    const FINGERPRINT: &str = "wx:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

    async fn service() -> (TempDir, SqlitePool, AdminQueueQueryService) {
        let directory = TempDir::new().expect("temp directory");
        let pool = db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("test database");
        let service = AdminQueueQueryService::new(pool.clone(), MAX_PAGE_LIMIT);
        (directory, pool, service)
    }

    async fn insert_device(pool: &SqlitePool) {
        sqlx::query(
            "INSERT INTO devices(id, name, token_hash, created_at)
             VALUES (?1, 'private device name', ?2, ?3)",
        )
        .bind(DEVICE_ID)
        .bind(vec![7_u8; 32])
        .bind(CREATED_AT)
        .execute(pool)
        .await
        .expect("device");
    }

    struct OutboxFixture<'a> {
        id: &'a str,
        notification_id: &'a str,
        origin_kind: &'a str,
        origin_device_id: Option<&'a str>,
        kind: &'a str,
        fingerprint: Option<&'a str>,
        status: &'a str,
        created_at: i64,
    }

    async fn insert_outbox(pool: &SqlitePool, fixture: OutboxFixture<'_>) {
        let origin_key = match (fixture.origin_kind, fixture.origin_device_id) {
            ("device", Some(device_id)) => format!("device:{device_id}"),
            ("system", None) => "system:interactive".to_owned(),
            ("admin", None) => "admin:singleton".to_owned(),
            _ => panic!("invalid outbox fixture origin"),
        };
        sqlx::query(
            "INSERT INTO notification_outbox(
                id, origin_kind, origin_key, origin_device_id, notification_id, dedupe_key,
                payload_hash, kind, target_account_fingerprint, title, body, correlation_key,
                priority, status, not_before, expires_at, attempt_count, last_error_code,
                last_error_message, provider_message_id, provider_accepted_at, created_at,
                updated_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'private-correlation',
                200, ?12, ?13, ?14, 2, 'SAFE_ERROR_CODE', ?15, ?16, ?17, ?13, ?18
             )",
        )
        .bind(fixture.id)
        .bind(fixture.origin_kind)
        .bind(origin_key)
        .bind(fixture.origin_device_id)
        .bind(fixture.notification_id)
        .bind(format!("private-dedupe-{}", fixture.id))
        .bind(PAYLOAD_HASH)
        .bind(fixture.kind)
        .bind(fixture.fingerprint)
        .bind(TITLE)
        .bind(BODY)
        .bind(fixture.status)
        .bind(fixture.created_at)
        .bind(fixture.created_at + 60_000)
        .bind(ERROR_MESSAGE)
        .bind(PROVIDER_MESSAGE_ID)
        .bind((fixture.status == "provider_accepted").then_some(fixture.created_at + 20))
        .bind(fixture.created_at + 20)
        .execute(pool)
        .await
        .expect("outbox row");
    }

    async fn insert_bundle_parent(pool: &SqlitePool, created_at: i64) {
        sqlx::query(
            "INSERT INTO notification_bundles(
                id, origin_key, origin_device_id, bundle_id, dedupe_key,
                request_digest, kind, content_mode, source, correlation_key,
                result_revision, title, body_ciphertext, body_nonce, body_bytes,
                source_hash, target_account_fingerprint, status, segment_count,
                accepted_segments, accepted_at, created_at, expires_at, updated_at,
                body_retain_until
             ) VALUES (
                'privacy-sentinel-bundle-row', 'device:' || ?1, ?1,
                'privacy-sentinel-bundle-id', 'privacy-sentinel-bundle-dedupe', ?2,
                'run_completed', 'full_final', 'codex_stop',
                'privacy-sentinel-bundle-correlation', 1,
                'privacy-sentinel-bundle-title', ?3, ?4, 12, ?5, ?6,
                'delivery_unknown', 3, 2, ?7, ?7, ?8, ?9, ?10
             )",
        )
        .bind(DEVICE_ID)
        .bind(vec![3_u8; 32])
        .bind(vec![4_u8; 28])
        .bind(vec![5_u8; 24])
        .bind("b".repeat(64))
        .bind(FINGERPRINT)
        .bind(created_at)
        .bind(created_at + 60_000)
        .bind(created_at + 20)
        .bind(created_at + 86_400_000)
        .execute(pool)
        .await
        .expect("bundle parent");
        for (index, status) in ["provider_accepted", "provider_accepted", "delivery_unknown"]
            .into_iter()
            .enumerate()
        {
            sqlx::query(
                "INSERT INTO notification_bundle_segments(
                    id, bundle_row_id, segment_index, byte_start, byte_end,
                    content_hash, rendered_header, client_id, status, not_before,
                    expires_at, attempt_count, last_error_code, provider_message_id,
                    provider_accepted_at, created_at, updated_at
                 ) VALUES (
                    ?1, 'privacy-sentinel-bundle-row', ?2, ?3, ?4, ?5, '[x/3]',
                    ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12, ?8, ?13
                 )",
            )
            .bind(format!("privacy-sentinel-segment-{index}"))
            .bind(i64::try_from(index).expect("index"))
            .bind(i64::try_from(index * 4).expect("start"))
            .bind(i64::try_from(index * 4 + 4).expect("end"))
            .bind("c".repeat(64))
            .bind(format!("privacy-sentinel-client-{index}"))
            .bind(status)
            .bind(created_at)
            .bind(created_at + 60_000)
            .bind((status == "delivery_unknown").then_some("AMBIGUOUS_MINUS_TWO"))
            .bind((status == "provider_accepted").then_some("privacy-provider-id"))
            .bind((status == "provider_accepted").then_some(created_at + 10))
            .bind(created_at + 20)
            .execute(pool)
            .await
            .expect("bundle segment");
        }
    }

    async fn insert_inbound(
        pool: &SqlitePool,
        message_key: &str,
        notification_id: Option<&str>,
        filter: &str,
        status: &str,
        created_at: i64,
    ) {
        let command_json = format!(r#"{{"action":"list_runs","filter":"{filter}"}}"#);
        sqlx::query(
            "INSERT INTO inbound_commands(
                message_key, sender_fingerprint, command_kind, command_json, payload_hash,
                status, reply_notification_id, expires_at, attempt_count, last_error_code,
                created_at, updated_at
             ) VALUES (?1, ?2, 'list_runs', ?3, ?4, ?5, ?6, ?7, 3,
                       'SAFE_INBOUND_ERROR', ?8, ?9)",
        )
        .bind(message_key)
        .bind(FINGERPRINT)
        .bind(command_json)
        .bind(PAYLOAD_HASH)
        .bind(status)
        .bind(notification_id)
        .bind(created_at + 60_000)
        .bind(created_at)
        .bind(created_at + 20)
        .execute(pool)
        .await
        .expect("inbound row");
    }

    #[tokio::test]
    async fn safe_projections_never_serialize_privacy_sentinels() {
        let (_directory, pool, service) = service().await;
        insert_device(&pool).await;
        insert_outbox(
            &pool,
            OutboxFixture {
                id: "device-outbox-id",
                notification_id: "device-notification-id",
                origin_kind: "device",
                origin_device_id: Some(DEVICE_ID),
                kind: "run_completed",
                fingerprint: None,
                status: "dead_letter",
                created_at: CREATED_AT,
            },
        )
        .await;
        insert_inbound(
            &pool,
            MESSAGE_KEY,
            Some("interactive-notification-id"),
            "recent",
            "reply_queued",
            CREATED_AT + 1,
        )
        .await;
        insert_outbox(
            &pool,
            OutboxFixture {
                id: OUTBOX_ID,
                notification_id: "interactive-notification-id",
                origin_kind: "system",
                origin_device_id: None,
                kind: "interactive_reply",
                fingerprint: Some(FINGERPRINT),
                status: "provider_accepted",
                created_at: CREATED_AT + 1,
            },
        )
        .await;

        let deliveries = service
            .list_deliveries(DeliveryListQuery::default())
            .await
            .expect("deliveries");
        let replies = service
            .list_interactive_replies(InteractiveReplyListQuery::default())
            .await
            .expect("replies");
        let inbound = service
            .list_inbound_commands(InboundCommandListQuery::default())
            .await
            .expect("inbound");

        assert_eq!(replies.items[0].command_ref, inbound.items[0].id);
        assert_eq!(replies.items[0].target_fingerprint, "wx:1234…cdef");
        assert_eq!(inbound.items[0].sender_hint, "wx:1234…cdef");
        assert_eq!(inbound.items[0].command, InboundCommand::ListRecent);

        let serialized = serde_json::to_string(&(deliveries.items, replies.items, inbound.items))
            .expect("serialize projections");
        for sentinel in [
            DEVICE_ID,
            OUTBOX_ID,
            MESSAGE_KEY,
            TITLE,
            BODY,
            PAYLOAD_HASH,
            PROVIDER_MESSAGE_ID,
            ERROR_MESSAGE,
            FINGERPRINT,
            "private-correlation",
            "private-dedupe",
            "private device name",
        ] {
            assert!(
                !serialized.contains(sentinel),
                "leaked sentinel: {sentinel}"
            );
        }
    }

    #[tokio::test]
    async fn keyset_pages_are_stable_and_limited_to_one_hundred() {
        let (_directory, pool, service) = service().await;
        insert_device(&pool).await;
        for index in 0..3 {
            let id = format!("row-{index}");
            let notification_id = format!("notification-{index}");
            insert_outbox(
                &pool,
                OutboxFixture {
                    id: &id,
                    notification_id: &notification_id,
                    origin_kind: "device",
                    origin_device_id: Some(DEVICE_ID),
                    kind: "test",
                    fingerprint: None,
                    status: "pending_channel",
                    created_at: CREATED_AT,
                },
            )
            .await;
        }

        let first = service
            .list_deliveries(DeliveryListQuery {
                limit: 2,
                ..DeliveryListQuery::default()
            })
            .await
            .expect("first page");
        assert_eq!(first.items.len(), 2);
        assert!(first.next_cursor_position.is_some());
        let second = service
            .list_deliveries(DeliveryListQuery {
                limit: 2,
                cursor: first.next_cursor_position,
                ..DeliveryListQuery::default()
            })
            .await
            .expect("second page");
        assert_eq!(second.items.len(), 1);
        assert!(second.next_cursor_position.is_none());
        assert_ne!(first.items[0].id, first.items[1].id);
        assert_ne!(first.items[0].id, second.items[0].id);
        assert_ne!(first.items[1].id, second.items[0].id);

        assert_eq!(
            service
                .list_deliveries(DeliveryListQuery {
                    limit: 101,
                    ..DeliveryListQuery::default()
                })
                .await,
            Err(AdminQueueQueryError::InvalidQuery)
        );
    }

    #[tokio::test]
    async fn bundle_parents_share_deterministic_delivery_pages_without_exposing_content() {
        let (_directory, pool, service) = service().await;
        insert_device(&pool).await;
        insert_outbox(
            &pool,
            OutboxFixture {
                id: "same-time-notification-row",
                notification_id: "same-time-notification-id",
                origin_kind: "device",
                origin_device_id: Some(DEVICE_ID),
                kind: "run_completed",
                fingerprint: None,
                status: "pending_channel",
                created_at: CREATED_AT,
            },
        )
        .await;
        insert_bundle_parent(&pool, CREATED_AT).await;

        let first = service
            .list_deliveries(DeliveryListQuery {
                limit: 1,
                ..DeliveryListQuery::default()
            })
            .await
            .expect("first mixed page");
        let second = service
            .list_deliveries(DeliveryListQuery {
                limit: 1,
                cursor: first.next_cursor_position.clone(),
                ..DeliveryListQuery::default()
            })
            .await
            .expect("second mixed page");
        assert_eq!(first.items.len(), 1);
        assert_eq!(second.items.len(), 1);
        assert_ne!(first.items[0].id, second.items[0].id);
        assert!(second.next_cursor_position.is_none());

        let blocked = service
            .list_deliveries(DeliveryListQuery {
                state: Some(DeliveryState::Blocked),
                origin: Some(DeliveryOrigin::Device),
                ..DeliveryListQuery::default()
            })
            .await
            .expect("blocked bundle");
        assert_eq!(blocked.items.len(), 1);
        assert_eq!(blocked.items[0].state, DeliveryState::Blocked);
        assert_eq!(
            blocked.items[0].error_code.as_deref(),
            Some("BUNDLE_DELIVERY_UNKNOWN")
        );
        assert_eq!(blocked.items[0].segment_count, Some(3));
        assert_eq!(blocked.items[0].accepted_segments, Some(2));
        let json = serde_json::to_string(&blocked.items[0]).expect("bundle admin JSON");
        for secret in [
            "privacy-sentinel-bundle-row",
            "privacy-sentinel-bundle-id",
            "privacy-sentinel-bundle-dedupe",
            "privacy-sentinel-bundle-correlation",
            "privacy-sentinel-bundle-title",
            "privacy-sentinel-segment",
            "privacy-sentinel-client",
            "privacy-provider-id",
            FINGERPRINT,
        ] {
            assert!(!json.contains(secret), "leaked bundle sentinel: {secret}");
        }
        pool.close().await;
    }

    #[tokio::test]
    async fn filters_use_only_closed_mappings() {
        let (_directory, pool, service) = service().await;
        insert_device(&pool).await;
        insert_outbox(
            &pool,
            OutboxFixture {
                id: "blocked-row",
                notification_id: "blocked-notification",
                origin_kind: "device",
                origin_device_id: Some(DEVICE_ID),
                kind: "run_failed",
                fingerprint: None,
                status: "blocked_reconnect",
                created_at: CREATED_AT,
            },
        )
        .await;
        insert_inbound(
            &pool,
            "failed-list-message",
            None,
            "failed",
            "received",
            CREATED_AT,
        )
        .await;

        let deliveries = service
            .list_deliveries(DeliveryListQuery {
                state: Some(DeliveryState::Blocked),
                origin: Some(DeliveryOrigin::Device),
                ..DeliveryListQuery::default()
            })
            .await
            .expect("filtered deliveries");
        assert_eq!(deliveries.items.len(), 1);
        assert_eq!(deliveries.items[0].state, DeliveryState::Blocked);
        assert_eq!(deliveries.items[0].kind, DeliveryKind::RunEvent);

        let inbound = service
            .list_inbound_commands(InboundCommandListQuery {
                command: Some(InboundCommand::ListFailed),
                state: Some(InboundState::Received),
                ..InboundCommandListQuery::default()
            })
            .await
            .expect("filtered inbound");
        assert_eq!(inbound.items.len(), 1);
        assert_eq!(inbound.items[0].command, InboundCommand::ListFailed);
    }

    #[tokio::test]
    async fn admin_test_origin_projects_and_filters_without_raw_identity() {
        let (_directory, pool, service) = service().await;
        insert_outbox(
            &pool,
            OutboxFixture {
                id: "private-admin-outbox-id",
                notification_id: "private-admin-notification-id",
                origin_kind: "admin",
                origin_device_id: None,
                kind: "test",
                fingerprint: None,
                status: "pending_channel",
                created_at: CREATED_AT,
            },
        )
        .await;

        let page = service
            .list_deliveries(DeliveryListQuery {
                origin: Some(DeliveryOrigin::Admin),
                ..DeliveryListQuery::default()
            })
            .await
            .expect("Admin delivery projection");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].origin, DeliveryOrigin::Admin);
        assert_eq!(page.items[0].origin_label, "admin");
        assert_eq!(page.items[0].kind, DeliveryKind::Test);
        let serialized = serde_json::to_string(&page.items).expect("delivery JSON");
        assert!(!serialized.contains("private-admin-outbox-id"));
        assert!(!serialized.contains("private-admin-notification-id"));
        pool.close().await;
    }

    #[test]
    fn sql_select_lists_do_not_expose_forbidden_raw_values() {
        for forbidden in [
            "title",
            "body",
            "body_ciphertext",
            "body_nonce",
            "bundle_id",
            "source_hash",
            "payload_hash",
            "dedupe_key",
            "correlation_key",
            "provider_message_id",
            "last_error_message",
        ] {
            assert!(!DELIVERIES_SQL.contains(forbidden));
        }

        let reply_select = INTERACTIVE_REPLIES_SQL
            .split_once("FROM")
            .expect("reply select")
            .0;
        let inbound_select = INBOUND_COMMANDS_SQL
            .split_once("FROM")
            .expect("inbound select")
            .0;
        for select in [reply_select, inbound_select] {
            for forbidden_alias in [
                "message_key AS",
                "command_json AS",
                "payload_hash AS",
                "provider_message_id AS",
                "last_error_message AS",
                "sender_fingerprint AS",
                "target_account_fingerprint AS",
            ] {
                assert!(!select.contains(forbidden_alias));
            }
        }
    }

    #[test]
    fn domain_separation_prevents_cross_projection_reference_reuse() {
        let identity = b"same-private-identity";
        assert_ne!(
            domain_ref(DELIVERY_REF_DOMAIN, identity),
            domain_ref(INTERACTIVE_REPLY_REF_DOMAIN, identity)
        );
        assert_ne!(
            domain_ref(DEVICE_REF_DOMAIN, identity),
            domain_ref(DELIVERY_REF_DOMAIN, identity)
        );
    }

    #[test]
    fn errors_and_debug_output_remain_safe() {
        let cursor = QueueCursorPosition {
            sort_timestamp: CREATED_AT,
            tie_break: MESSAGE_KEY.to_owned(),
        };
        let error = parse_rowid_cursor(Some(&cursor)).expect_err("invalid rowid cursor");
        let output = format!("{error:?} {error}");
        assert!(!output.contains(MESSAGE_KEY));

        let value = serde_json::to_value(DeliveryListItem {
            id: "delivery_safe".to_owned(),
            origin: DeliveryOrigin::System,
            origin_label: "system".to_owned(),
            kind: DeliveryKind::Test,
            state: DeliveryState::Queued,
            priority: 0,
            attempt_count: 0,
            error_code: None,
            created_at: 0,
            updated_at: 0,
            segment_count: None,
            accepted_segments: None,
        })
        .expect("delivery json");
        assert_eq!(value.get("priority"), Some(&Value::from(0)));
    }
}
