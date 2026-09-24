use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

pub struct EphemeralInboundText {
    pub message_key: String,
    pub sender_fingerprint: String,
    pub text: Zeroizing<String>,
    pub received_at: i64,
}

impl fmt::Debug for EphemeralInboundText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EphemeralInboundText")
            .field("message_key", &self.message_key)
            .field("sender_fingerprint", &self.sender_fingerprint)
            .field("text", &"[REDACTED]")
            .field("received_at", &self.received_at)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFilter {
    All,
    Recent,
    Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum InboundCommandV5 {
    Help,
    ListDevices,
    SelectDevice {
        slot: u16,
    },
    ListRuntimes,
    SelectRuntime {
        slot: u16,
    },
    ListWorkspaces,
    SelectWorkspace {
        slot: u16,
    },
    ListHarnessProfiles,
    SelectHarnessProfile {
        slot: u16,
    },
    ListTaskPresets,
    StartRun {
        slot: u16,
    },
    /// Only a server-generated opaque id is durable.  The six-digit code is
    /// verified by the sink and is never serialized into `command_json`.
    Confirm {
        confirmation_id: String,
    },
    CancelConfirmation,
    ListRuns {
        filter: RunFilter,
    },
    NextPage,
    GetRunStatus {
        slot: u16,
    },
    GetRunDetail {
        slot: u16,
    },
    GetRunTree {
        slot: u16,
    },
    CancelRun {
        slot: u16,
    },
    Unknown,
}

impl InboundCommandV5 {
    pub(super) const fn kind(&self) -> &'static str {
        match self {
            Self::Help => "help",
            Self::ListDevices => "list_devices",
            Self::SelectDevice { .. } => "select_device",
            Self::ListRuntimes => "list_runtimes",
            Self::SelectRuntime { .. } => "select_runtime",
            Self::ListWorkspaces => "list_workspaces",
            Self::SelectWorkspace { .. } => "select_workspace",
            Self::ListHarnessProfiles => "list_harness_profiles",
            Self::SelectHarnessProfile { .. } => "select_harness_profile",
            Self::ListTaskPresets => "list_task_presets",
            Self::StartRun { .. } => "start_run",
            Self::Confirm { .. } => "confirm",
            Self::CancelConfirmation => "cancel_confirmation",
            Self::ListRuns { .. } => "list_runs",
            Self::NextPage => "next_page",
            Self::GetRunStatus { .. } => "get_run_status",
            Self::GetRunDetail { .. } => "get_run_detail",
            Self::GetRunTree { .. } => "get_run_tree",
            Self::CancelRun { .. } => "cancel_run",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboundAcceptOutcome {
    Fresh,
    ExactReplay,
    Discarded,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum InboundMessageError {
    #[error("inbound message acceptance was cancelled")]
    Cancelled,
    #[error("inbound message key conflicts with an existing payload")]
    IdempotencyConflict,
    #[error("inbound message database operation failed")]
    Database,
}

pub(super) struct ClaimedInboundCommand {
    pub message_key: String,
    pub claim_token: String,
    pub sender_fingerprint: String,
    pub command_kind: String,
    pub command_json: String,
    pub last_error_code: Option<String>,
    pub expires_at: i64,
}

pub(super) struct RenderedInboundReply {
    pub title: String,
    pub body: String,
    pub job_page_selection: Option<PendingJobPageSelection>,
    pub pending_confirmation: Option<PendingConfirmation>,
}

/// The code is intentionally retained only in memory until the reply and the
/// hash-only confirmation row are committed in one transaction.
pub(super) struct PendingConfirmation {
    pub sender_fingerprint: String,
    pub confirmation_id: String,
    pub confirmation_digest: String,
    pub confirmation_nonce: String,
    pub device_id: uuid::Uuid,
    pub action_kind: ConfirmationActionKind,
    pub runtime_handle: Option<String>,
    pub workspace_handle: Option<String>,
    pub harness_profile_handle: Option<String>,
    pub task_preset_handle: Option<String>,
    pub run_handle: Option<String>,
    pub intent_id: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ControlDispatchOutcome {
    pub accepted: bool,
    pub run_handle: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ControlDispatchRecord {
    pub confirmation: super::repository::ConfirmationRecord,
    pub outcome: Option<ControlDispatchOutcome>,
    pub error_code: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConfirmationActionKind {
    StartRun,
    CancelRun,
}

impl ConfirmationActionKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::StartRun => "start_run",
            Self::CancelRun => "cancel_run",
        }
    }
}

pub(super) struct PendingJobPageSelection {
    pub selected_device: uuid::Uuid,
    pub filter: RunFilter,
    pub next_cursor: Option<String>,
    pub entries: Vec<SelectionEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectionItemKind {
    Device,
    Run,
    Runtime,
    Workspace,
    HarnessProfile,
    TaskPreset,
}

impl SelectionItemKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Run => "run",
            Self::Runtime => "runtime",
            Self::Workspace => "workspace",
            Self::HarnessProfile => "harness_profile",
            Self::TaskPreset => "task_preset",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectionEntry {
    pub slot: u16,
    pub device_id: uuid::Uuid,
    pub client_opaque_handle: Option<String>,
    pub item_kind: SelectionItemKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectionPageContext {
    pub selected_device: uuid::Uuid,
    pub filter: RunFilter,
    pub next_cursor: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectedCatalogContext {
    pub device_id: uuid::Uuid,
    pub runtime_handle: Option<String>,
    pub workspace_handle: Option<String>,
    pub harness_profile_handle: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectionLookupError {
    Missing,
    Expired,
    WrongKind,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum InboundWorkerError {
    #[error("inbound command database operation failed")]
    Database,
    #[error("inbound command outbox operation failed")]
    Outbox,
    #[error("inbound command device directory operation failed")]
    DeviceDirectory,
    #[error("system clock is invalid")]
    Clock,
}
