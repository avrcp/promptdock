//! Read-only activity DTOs.  These types deliberately contain no captured
//! prompt, result body, URL, workspace path, token, or encrypted payload.

use serde::{Deserialize, Serialize};

pub(crate) const MAX_ACTIVITY_PAGE_SIZE: u16 = 100;
pub(crate) const DEFAULT_ACTIVITY_PAGE_SIZE: u16 = 20;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivityFilter {
    #[default]
    All,
    Attention,
    Started,
    Results,
    DeliveryIssues,
    Recent,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ActivityPageRequest {
    #[serde(default)]
    pub filter: ActivityFilter,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u16>,
}

impl ActivityPageRequest {
    pub(crate) fn page_size(&self) -> u16 {
        self.limit
            .unwrap_or(DEFAULT_ACTIVITY_PAGE_SIZE)
            .clamp(1, MAX_ACTIVITY_PAGE_SIZE)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityItem {
    pub run_key: String,
    pub workspace_label: String,
    pub display_title: String,
    pub activity_revision: i64,
    pub phase: String,
    pub started_at: Option<i64>,
    pub last_observed_at: i64,
    pub attention: Option<ActivityAttention>,
    pub result: Option<ActivityResult>,
    pub delivery: Option<ActivityDelivery>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityAttention {
    pub revision: i64,
    pub acknowledged_revision: i64,
    pub label: String,
    pub historical: bool,
    pub observation_expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityResult {
    pub outbox_id: String,
    pub result_revision: i64,
    pub page_state: Option<String>,
    pub expires_at: Option<i64>,
    pub seen_result_revision: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityDelivery {
    pub state: String,
    pub last_error_code: Option<String>,
    pub held_until: Option<i64>,
}

impl ActivityItem {
    pub(crate) fn apply_hold(&mut self, hold: &crate::notification::hold::UserHoldStatus) {
        use crate::notification::hold::UserHoldState;
        if hold.state == UserHoldState::Inactive {
            return;
        }
        if let Some(delivery) = &mut self.delivery {
            // A local control does not revoke evidence of an HTTP handoff.
            if matches!(
                delivery.state.as_str(),
                "pending" | "retry_wait" | "blocked_activation" | "blocked_reconnect"
            ) {
                delivery.state = "user_held".into();
                delivery.held_until = hold.until;
                if hold.state == UserHoldState::Uncertain {
                    delivery.last_error_code = Some("USER_HOLD_UNCERTAIN".into());
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityCounts {
    pub attention: u32,
    pub started: u32,
    pub results: u32,
    pub delivery_issues: u32,
    pub recent: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityPage {
    pub items: Vec<ActivityItem>,
    pub next_cursor: Option<String>,
    pub counts: ActivityCounts,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityEventItem {
    pub id: String,
    pub kind: String,
    pub occurred_at: i64,
    pub observed_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityDeliveryItem {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub page_state: Option<String>,
    pub result_revision: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityDetail {
    pub item: ActivityItem,
    pub events: Vec<ActivityEventItem>,
    pub deliveries: Vec<ActivityDeliveryItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttentionAcknowledgement {
    pub acknowledged_revision: i64,
    pub current_revision: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResultSeen {
    pub seen_result_revision: i64,
    pub current_revision: i64,
}
