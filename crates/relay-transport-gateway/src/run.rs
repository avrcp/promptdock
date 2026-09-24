use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const RUN_PAGE_SIZE: u16 = 10;
pub const MAX_RUN_TREE_DEPTH: u8 = 4;
pub const MAX_RUN_TREE_NODES: u16 = 100;
pub const MAX_RUN_CONDITIONS: usize = 16;
pub const MAX_RUN_ATTENTION_COUNT: u32 = 10_000;

const MIN_OPAQUE_HANDLE_BYTES: usize = 32;
const MAX_OPAQUE_HANDLE_BYTES: usize = 128;
const MAX_LABEL_CHARS: usize = 120;
const MAX_LABEL_BYTES: usize = MAX_LABEL_CHARS * 4;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRunFilterV5 {
    All,
    Recent,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunQueryActionV5 {
    ListRuns {
        filter: RemoteRunFilterV5,
        page_size: u16,
        cursor: Option<String>,
    },
    GetRunDetail {
        run_handle: String,
    },
    GetRunTree {
        run_handle: String,
        max_depth: u8,
        max_nodes: u16,
    },
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("invalid Gateway v5 run query action")]
pub struct RunQueryValidationError;

impl RunQueryActionV5 {
    pub fn validate(&self) -> Result<(), RunQueryValidationError> {
        let result = match self {
            Self::ListRuns {
                page_size, cursor, ..
            } => validate_page(*page_size, cursor.as_deref()),
            Self::GetRunDetail { run_handle } => validate_opaque_handle(run_handle),
            Self::GetRunTree {
                run_handle,
                max_depth,
                max_nodes,
            } => validate_opaque_handle(run_handle).and_then(|()| {
                if *max_depth <= MAX_RUN_TREE_DEPTH && (1..=MAX_RUN_TREE_NODES).contains(max_nodes)
                {
                    Ok(())
                } else {
                    Err(())
                }
            }),
        };
        result.map_err(|()| RunQueryValidationError)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunQueryResultV5 {
    ListRuns(RemoteRunPageV5),
    GetRunDetail(RemoteRunDetailV5),
    GetRunTree(RemoteRunTreeV5),
}

impl RunQueryResultV5 {
    pub(super) fn validate(&self) -> Result<(), ()> {
        match self {
            Self::ListRuns(page) => page.validate(),
            Self::GetRunDetail(detail) => detail.validate(),
            Self::GetRunTree(tree) => tree.validate(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDesiredStateV5 {
    Running,
    Stopped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRunPhaseV5 {
    Pending,
    Provisioning,
    Starting,
    Active,
    WaitingInput,
    Stopping,
    Finalizing,
    Finished,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRunOutcomeV5 {
    None,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Blocked,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRunConditionV5 {
    Accepted,
    WorkspaceReady,
    RuntimeReady,
    SessionReady,
    EventStreamReady,
    Running,
    WaitingForInput,
    CancellationRequested,
    TerminalObserved,
    VerificationComplete,
    ArtifactsReady,
    CleanupComplete,
    Degraded,
    Retrying,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteRunSummaryV5 {
    pub run_handle: String,
    pub title: String,
    pub runtime_label: String,
    pub workspace_label: String,
    pub desired_state: RemoteDesiredStateV5,
    pub phase: RemoteRunPhaseV5,
    pub outcome: RemoteRunOutcomeV5,
    pub conditions: Vec<RemoteRunConditionV5>,
    pub attention_count: u32,
    pub child_count: u32,
    pub active_child_count: u32,
    pub started_at: i64,
    pub updated_at: i64,
}

impl RemoteRunSummaryV5 {
    fn validate(&self) -> Result<(), ()> {
        validate_opaque_handle(&self.run_handle)?;
        validate_safe_text(&self.title, MAX_LABEL_CHARS, MAX_LABEL_BYTES)?;
        validate_safe_text(&self.runtime_label, MAX_LABEL_CHARS, MAX_LABEL_BYTES)?;
        validate_safe_text(&self.workspace_label, MAX_LABEL_CHARS, MAX_LABEL_BYTES)?;
        validate_timestamp(self.started_at)?;
        validate_timestamp(self.updated_at)?;
        if self.started_at > self.updated_at
            || self.active_child_count > self.child_count
            || self.attention_count > MAX_RUN_ATTENTION_COUNT
            || !valid_conditions(&self.conditions)
        {
            return Err(());
        }
        if self.phase == RemoteRunPhaseV5::Finished {
            if self.outcome == RemoteRunOutcomeV5::None {
                return Err(());
            }
        } else if self.outcome != RemoteRunOutcomeV5::None {
            return Err(());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteRunPageV5 {
    pub items: Vec<RemoteRunSummaryV5>,
    pub next_cursor: Option<String>,
}

impl RemoteRunPageV5 {
    fn validate(&self) -> Result<(), ()> {
        if self.items.len() > usize::from(RUN_PAGE_SIZE) {
            return Err(());
        }
        for item in &self.items {
            item.validate()?;
        }
        if let Some(cursor) = &self.next_cursor {
            validate_opaque_handle(cursor)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteRunDetailV5 {
    pub summary: RemoteRunSummaryV5,
}

impl RemoteRunDetailV5 {
    fn validate(&self) -> Result<(), ()> {
        self.summary.validate()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteRunTreeV5 {
    pub root: RemoteRunTreeNodeV5,
}

impl RemoteRunTreeV5 {
    fn validate(&self) -> Result<(), ()> {
        let mut nodes = 0_u16;
        self.root.validate(0, &mut nodes)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteRunTreeNodeV5 {
    pub run_handle: String,
    pub label: String,
    pub desired_state: RemoteDesiredStateV5,
    pub phase: RemoteRunPhaseV5,
    pub outcome: RemoteRunOutcomeV5,
    pub conditions: Vec<RemoteRunConditionV5>,
    pub attention_count: u32,
    pub child_count: u32,
    pub active_child_count: u32,
    pub children: Vec<RemoteRunTreeNodeV5>,
}

impl RemoteRunTreeNodeV5 {
    fn validate(&self, depth: u8, nodes: &mut u16) -> Result<(), ()> {
        if depth > MAX_RUN_TREE_DEPTH {
            return Err(());
        }
        *nodes = nodes.checked_add(1).ok_or(())?;
        if *nodes > MAX_RUN_TREE_NODES {
            return Err(());
        }
        validate_opaque_handle(&self.run_handle)?;
        validate_safe_text(&self.label, MAX_LABEL_CHARS, MAX_LABEL_BYTES)?;
        if self.active_child_count > self.child_count
            || usize::try_from(self.child_count).map_err(|_| ())? < self.children.len()
            || self.attention_count > MAX_RUN_ATTENTION_COUNT
            || !valid_conditions(&self.conditions)
        {
            return Err(());
        }
        if self.phase == RemoteRunPhaseV5::Finished {
            if self.outcome == RemoteRunOutcomeV5::None {
                return Err(());
            }
        } else if self.outcome != RemoteRunOutcomeV5::None {
            return Err(());
        }
        for child in &self.children {
            child.validate(depth.saturating_add(1), nodes)?;
        }
        Ok(())
    }
}

fn valid_conditions(conditions: &[RemoteRunConditionV5]) -> bool {
    if conditions.len() > MAX_RUN_CONDITIONS {
        return false;
    }
    let mut seen = Vec::with_capacity(conditions.len());
    for condition in conditions {
        if seen.contains(condition) {
            return false;
        }
        seen.push(*condition);
    }
    true
}

fn validate_page(page_size: u16, cursor: Option<&str>) -> Result<(), ()> {
    if page_size != RUN_PAGE_SIZE {
        return Err(());
    }
    if let Some(cursor) = cursor {
        validate_opaque_handle(cursor)?;
    }
    Ok(())
}

fn validate_opaque_handle(value: &str) -> Result<(), ()> {
    if (MIN_OPAQUE_HANDLE_BYTES..=MAX_OPAQUE_HANDLE_BYTES).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(())
    }
}

fn validate_timestamp(value: i64) -> Result<(), ()> {
    if value >= 0 { Ok(()) } else { Err(()) }
}

fn validate_safe_text(value: &str, max_chars: usize, max_bytes: usize) -> Result<(), ()> {
    let chars = value.chars().count();
    if value.trim() == value
        && (1..=max_chars).contains(&chars)
        && value.len() <= max_bytes
        && value.chars().all(|character| {
            !character.is_control()
                && !matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
    {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> String {
        "run_0123456789abcdef0123456789abcdef".into()
    }

    #[test]
    fn actions_enforce_fixed_page_and_opaque_handles() {
        assert!(
            RunQueryActionV5::ListRuns {
                filter: RemoteRunFilterV5::Recent,
                page_size: RUN_PAGE_SIZE,
                cursor: Some("cursor_0123456789abcdef0123456789abcd".into())
            }
            .validate()
            .is_ok()
        );
        assert!(
            RunQueryActionV5::ListRuns {
                filter: RemoteRunFilterV5::All,
                page_size: RUN_PAGE_SIZE + 1,
                cursor: None
            }
            .validate()
            .is_err()
        );
        assert!(
            RunQueryActionV5::GetRunDetail {
                run_handle: "raw/run/key".into()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn phase_outcome_conditions_and_counts_are_consistent_and_closed() {
        let value = serde_json::json!({
            "runHandle": handle(), "title": "Safe", "runtimeLabel": "Codex",
            "workspaceLabel": "PromptDock", "desiredState": "running", "phase": "active",
            "outcome": "succeeded", "conditions": ["accepted", "running"],
            "attentionCount": 0, "childCount": 0,
            "activeChildCount": 0, "startedAt": 1, "updatedAt": 2
        });
        let summary = serde_json::from_value::<RemoteRunSummaryV5>(value).expect("closed DTO");
        assert!(summary.validate().is_err());
        assert!(serde_json::from_value::<RemoteRunPhaseV5>(serde_json::json!("settling")).is_err());

        let duplicate_conditions = serde_json::json!({
            "runHandle": handle(), "title": "Safe", "runtimeLabel": "Codex",
            "workspaceLabel": "PromptDock", "desiredState": "running", "phase": "active",
            "outcome": "none", "conditions": ["running", "running"],
            "attentionCount": 0, "childCount": 0, "activeChildCount": 0,
            "startedAt": 1, "updatedAt": 2
        });
        let summary =
            serde_json::from_value::<RemoteRunSummaryV5>(duplicate_conditions).expect("closed DTO");
        assert!(summary.validate().is_err());
    }
}
