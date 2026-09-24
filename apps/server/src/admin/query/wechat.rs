use std::collections::BTreeSet;

use serde::Serialize;
use thiserror::Error;

use crate::state::AppState;

use super::{QueueSummary, overview::queue_summary};
use crate::admin::{
    ADMIN_SCHEMA_VERSION,
    event_log::{OperationalEventKind, OperationalEventLog},
};

#[derive(Clone)]
pub struct AdminWechatQueryService {
    app: AppState,
    events: OperationalEventLog,
    max_page_size: usize,
}

impl AdminWechatQueryService {
    pub fn new(app: AppState, events: OperationalEventLog, max_page_size: usize) -> Self {
        Self {
            app,
            events,
            max_page_size,
        }
    }

    pub async fn status(&self) -> Result<WechatAdminStatus, WechatQueryError> {
        let generated_at = unix_timestamp_ms()?;
        let status = self
            .app
            .wechat_status()
            .await
            .map_err(|_| WechatQueryError::Database)?;
        let queue = queue_summary(&self.app.db)
            .await
            .map_err(|_| WechatQueryError::Database)?;
        Ok(WechatAdminStatus {
            schema_version: ADMIN_SCHEMA_VERSION,
            generated_at,
            state: status.state,
            account_hint: status.account_fingerprint,
            last_poll_at: status.last_poll_at,
            last_context_at: status.last_context_at,
            last_provider_accepted_at: status.last_provider_accepted_at,
            queue,
            last_error_code: status.last_error_code,
        })
    }

    pub async fn events(
        &self,
        query: ChannelEventQuery,
    ) -> Result<ChannelEventPage, WechatQueryError> {
        if query.limit == 0
            || query.limit > self.max_page_size
            || query.since_at.is_some_and(|value| value < 0)
        {
            return Err(WechatQueryError::InvalidQuery);
        }
        let mut items = self
            .events
            .snapshot()
            .await
            .into_iter()
            .filter_map(|event| {
                let kind = ChannelEventKind::from_operational(event.kind)?;
                (query.kinds.is_empty() || query.kinds.contains(&kind)).then_some(
                    ChannelEventItem {
                        id: event.id,
                        kind,
                        occurred_at: event.occurred_at,
                        safe_message: kind.safe_message(),
                    },
                )
            })
            .filter(|event| {
                query
                    .since_at
                    .is_none_or(|since| event.occurred_at >= since)
            })
            .filter(|event| {
                query.before.as_ref().is_none_or(|before| {
                    (event.occurred_at, event.id.as_str())
                        < (before.occurred_at, before.id.as_str())
                })
            })
            .collect::<Vec<_>>();
        if let Some(monitor) = &self.app.wechat_monitor {
            let status = monitor.safe_status();
            if let Some(occurred_at) = status.last_poll_at {
                items.push(ChannelEventItem::observed(
                    format!("monitor-poll-{occurred_at}"),
                    ChannelEventKind::Poll,
                    occurred_at,
                ));
                if status.error_code.is_some() {
                    items.push(ChannelEventItem::observed(
                        format!("monitor-reconnect-{occurred_at}"),
                        ChannelEventKind::Reconnect,
                        occurred_at,
                    ));
                }
            }
            if let Some(occurred_at) = monitor
                .snapshot_bundle()
                .and_then(|bundle| bundle.session.context_token_updated_at)
            {
                items.push(ChannelEventItem::observed(
                    format!("monitor-context-{occurred_at}"),
                    ChannelEventKind::Context,
                    occurred_at,
                ));
            }
        }
        items.retain(|event| {
            (query.kinds.is_empty() || query.kinds.contains(&event.kind))
                && query
                    .since_at
                    .is_none_or(|since| event.occurred_at >= since)
                && query.before.as_ref().is_none_or(|before| {
                    (event.occurred_at, event.id.as_str())
                        < (before.occurred_at, before.id.as_str())
                })
        });
        items.sort_by(|left, right| {
            (right.occurred_at, &right.id).cmp(&(left.occurred_at, &left.id))
        });
        let has_more = items.len() > query.limit;
        items.truncate(query.limit);
        Ok(ChannelEventPage { items, has_more })
    }
}

#[derive(Clone, Debug)]
pub struct ChannelEventQuery {
    pub kinds: BTreeSet<ChannelEventKind>,
    pub since_at: Option<i64>,
    pub limit: usize,
    pub before: Option<ChannelEventPosition>,
}

#[derive(Clone, Debug)]
pub struct ChannelEventPosition {
    pub occurred_at: i64,
    pub id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatAdminStatus {
    pub schema_version: u32,
    pub generated_at: i64,
    pub state: crate::state::WechatConnectionState,
    pub account_hint: Option<String>,
    pub last_poll_at: Option<i64>,
    pub last_context_at: Option<i64>,
    pub last_provider_accepted_at: Option<i64>,
    pub queue: QueueSummary,
    pub last_error_code: Option<&'static str>,
}

#[derive(Debug)]
pub struct ChannelEventPage {
    pub items: Vec<ChannelEventItem>,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelEventItem {
    pub id: String,
    pub kind: ChannelEventKind,
    pub occurred_at: i64,
    pub safe_message: &'static str,
}

impl ChannelEventItem {
    fn observed(id: String, kind: ChannelEventKind, occurred_at: i64) -> Self {
        Self {
            id,
            kind,
            occurred_at,
            safe_message: kind.safe_message(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelEventKind {
    Poll,
    Context,
    Login,
    LoginCancelled,
    LoginExpired,
    LoginFailed,
    Disconnect,
    Reconnect,
    Test,
}

impl ChannelEventKind {
    fn from_operational(kind: OperationalEventKind) -> Option<Self> {
        match kind {
            OperationalEventKind::WechatLoginStarted
            | OperationalEventKind::WechatVerifySubmitted => Some(Self::Login),
            OperationalEventKind::WechatLoginCancelled => Some(Self::LoginCancelled),
            OperationalEventKind::WechatDisconnected => Some(Self::Disconnect),
            OperationalEventKind::WechatTestEnqueued => Some(Self::Test),
            _ => None,
        }
    }

    fn safe_message(self) -> &'static str {
        match self {
            Self::Poll => "WeChat polling completed",
            Self::Context => "WeChat context was refreshed",
            Self::Login => "WeChat login state changed",
            Self::LoginCancelled => "WeChat login was cancelled",
            Self::LoginExpired => "WeChat login expired",
            Self::LoginFailed => "WeChat login failed",
            Self::Disconnect => "WeChat was disconnected",
            Self::Reconnect => "WeChat reconnect state changed",
            Self::Test => "WeChat test notification was queued",
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WechatQueryError {
    #[error("channel event query is invalid")]
    InvalidQuery,
    #[error("wechat admin query is unavailable")]
    Database,
    #[error("system clock is unavailable")]
    Clock,
}

fn unix_timestamp_ms() -> Result<i64, WechatQueryError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .ok_or(WechatQueryError::Clock)
}
