#![forbid(unsafe_code)]

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceScope {
    NotifyWrite,
    NotifyReadOwn,
    ChannelRead,
    ChannelManage,
    GatewayConnect,
    JobQuery,
    JobControl,
}

impl DeviceScope {
    pub const ALL: [Self; 7] = [
        Self::NotifyWrite,
        Self::NotifyReadOwn,
        Self::ChannelRead,
        Self::ChannelManage,
        Self::GatewayConnect,
        Self::JobQuery,
        Self::JobControl,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotifyWrite => "notify:write",
            Self::NotifyReadOwn => "notify:read_own",
            Self::ChannelRead => "channel:read",
            Self::ChannelManage => "channel:manage",
            Self::GatewayConnect => "gateway:connect",
            Self::JobQuery => "job:query",
            Self::JobControl => "job:control",
        }
    }
}

impl fmt::Display for DeviceScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for DeviceScope {
    type Err = UnknownDeviceScope;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|scope| scope.as_str() == value)
            .ok_or(UnknownDeviceScope)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownDeviceScope;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredRunState {
    Running,
    Stopped,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Pending,
    Provisioning,
    Starting,
    Active,
    WaitingInput,
    Stopping,
    Finalizing,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Blocked,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunConditionType {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunCondition {
    #[serde(rename = "type")]
    pub condition_type: RunConditionType,
    pub status: ConditionStatus,
    pub reason_code: String,
    pub observed_at: i64,
}

impl RunCondition {
    pub const MAX_REASON_CODE_BYTES: usize = 64;

    pub fn validate(&self) -> Result<(), ProjectionError> {
        if self.observed_at < 0
            || self.reason_code.is_empty()
            || self.reason_code.len() > Self::MAX_REASON_CODE_BYTES
            || !self
                .reason_code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(ProjectionError::InvalidCondition);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityFacet {
    Lifecycle,
    Cancel,
    ResolveAttention,
    Steer,
    ReconnectLive,
    OpenStored,
    Continue,
    Usage,
    Tools,
    ChildTopology,
    Artifacts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    InvalidCondition,
    TooManyConditions,
    InvalidCounts,
    OutcomeBeforeFinish,
    MissingTerminalOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafeRunProjection {
    pub desired_state: DesiredRunState,
    pub phase: RunPhase,
    pub outcome: Option<RunOutcome>,
    pub conditions: Vec<RunCondition>,
    pub attention_count: u32,
    pub child_count: u32,
    pub active_child_count: u32,
}

impl SafeRunProjection {
    pub const MAX_CONDITIONS: usize = 32;

    pub fn validate(&self) -> Result<(), ProjectionError> {
        if self.conditions.len() > Self::MAX_CONDITIONS {
            return Err(ProjectionError::TooManyConditions);
        }
        for condition in &self.conditions {
            condition.validate()?;
        }
        if self.active_child_count > self.child_count {
            return Err(ProjectionError::InvalidCounts);
        }
        match (self.phase, self.outcome) {
            (RunPhase::Finished, None) => Err(ProjectionError::MissingTerminalOutcome),
            (RunPhase::Finished, Some(_)) | (_, None) => Ok(()),
            (_, Some(_)) => Err(ProjectionError::OutcomeBeforeFinish),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryStatus {
    PendingChannel,
    SendingChannel,
    RetryWait,
    BlockedActivation,
    BlockedReconnect,
    ProviderAccepted,
    Expired,
    Cancelled,
    DeadLetter,
}

impl DeliveryStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::ProviderAccepted | Self::Expired | Self::Cancelled | Self::DeadLetter
        )
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        if self.is_terminal() {
            return self as u8 == next as u8;
        }
        matches!(
            (self, next),
            (
                Self::PendingChannel,
                Self::SendingChannel
                    | Self::BlockedActivation
                    | Self::BlockedReconnect
                    | Self::Expired
                    | Self::Cancelled
            ) | (
                Self::SendingChannel,
                Self::ProviderAccepted
                    | Self::RetryWait
                    | Self::BlockedActivation
                    | Self::BlockedReconnect
                    | Self::Expired
                    | Self::DeadLetter
            ) | (
                Self::RetryWait | Self::BlockedActivation | Self::BlockedReconnect,
                Self::SendingChannel | Self::Expired | Self::Cancelled | Self::DeadLetter
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_scopes_are_closed_and_round_trip() {
        for scope in DeviceScope::ALL {
            assert_eq!(scope.as_str().parse(), Ok(scope));
        }
        assert!("admin:shell".parse::<DeviceScope>().is_err());
    }

    #[test]
    fn safe_projection_requires_terminal_outcome_and_bounded_counts() {
        let projection = SafeRunProjection {
            desired_state: DesiredRunState::Running,
            phase: RunPhase::Finished,
            outcome: None,
            conditions: Vec::new(),
            attention_count: 0,
            child_count: 1,
            active_child_count: 0,
        };
        assert_eq!(
            projection.validate(),
            Err(ProjectionError::MissingTerminalOutcome)
        );
    }

    #[test]
    fn terminal_delivery_state_cannot_reopen() {
        assert!(!DeliveryStatus::ProviderAccepted.can_transition_to(DeliveryStatus::RetryWait));
        assert!(
            DeliveryStatus::ProviderAccepted.can_transition_to(DeliveryStatus::ProviderAccepted)
        );
    }
}
