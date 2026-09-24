use serde::{Deserialize, Serialize};

use crate::source::SourceDescriptor;

pub(crate) const COMPLETION_QUIET_MS_MIN: i64 = 500;
pub(crate) const COMPLETION_QUIET_MS_MAX: i64 = 20_000;
pub(crate) const DEFAULT_COMPLETION_QUIET_MS: i64 = 2_000;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationMode {
    CompletionOnly,
    #[default]
    StartAndCompletion,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationBackendKind {
    #[default]
    Relay,
}

impl NotificationBackendKind {
    pub(crate) const fn as_str(self) -> &'static str {
        "relay"
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        (value == "relay").then_some(Self::Relay)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResultContentMode {
    #[default]
    StatusOnly,
    RedactedExcerpt,
    FullFinal,
}

impl ResultContentMode {
    pub const fn captures_text(self) -> bool {
        !matches!(self, Self::StatusOnly)
    }

    pub const fn rank(self) -> u8 {
        match self {
            Self::StatusOnly => 0,
            Self::RedactedExcerpt => 1,
            Self::FullFinal => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationContentSettings {
    pub include_task_input: bool,
    pub content_mode: ResultContentMode,
    pub max_result_chars: u16,
    pub redact_secrets: bool,
    pub attention_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationSettings {
    #[serde(default)]
    pub policy_revision: i64,
    #[serde(default)]
    pub include_task_input: bool,
    pub enabled: bool,
    pub mode: NotificationMode,
    #[serde(default = "default_notify_ended")]
    pub notify_ended: bool,
    #[serde(default = "default_completion_quiet_ms")]
    pub completion_quiet_ms: i64,
    #[serde(default)]
    pub content_epoch: i64,
    pub result_content_mode: ResultContentMode,
    pub max_result_chars: u16,
    pub redact_secrets: bool,
    pub attention_enabled: bool,
}

impl NotificationSettings {
    pub fn validate(&self) -> bool {
        (200..=5000).contains(&self.max_result_chars)
            && (COMPLETION_QUIET_MS_MIN..=COMPLETION_QUIET_MS_MAX)
                .contains(&self.completion_quiet_ms)
    }

    pub fn content(&self) -> NotificationContentSettings {
        NotificationContentSettings {
            include_task_input: self.include_task_input,
            content_mode: self.result_content_mode,
            max_result_chars: self.max_result_chars,
            redact_secrets: self.redact_secrets,
            attention_enabled: self.attention_enabled,
        }
    }
}

const fn default_notify_ended() -> bool {
    true
}

const fn default_completion_quiet_ms() -> i64 {
    DEFAULT_COMPLETION_QUIET_MS
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            policy_revision: 0,
            include_task_input: false,
            mode: NotificationMode::StartAndCompletion,
            notify_ended: true,
            completion_quiet_ms: DEFAULT_COMPLETION_QUIET_MS,
            content_epoch: 0,
            result_content_mode: ResultContentMode::StatusOnly,
            max_result_chars: 1200,
            redact_secrets: true,
            attention_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureSettings {
    pub result_content_mode: ResultContentMode,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsSnapshot {
    pub capture: CaptureSettings,
    pub notifications: NotificationSettings,
}

#[derive(Debug, Clone)]
pub struct SourceCheckpointUpdate {
    pub source: SourceDescriptor,
    pub cursor_json: String,
    pub source_revision: Option<String>,
    pub status: String,
    pub last_error_code: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct SourceHealth {
    pub status: String,
    pub last_error_code: Option<String>,
    pub updated_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_content_mode_is_the_single_capture_and_notification_authority() {
        let settings = NotificationSettings {
            result_content_mode: ResultContentMode::FullFinal,
            ..NotificationSettings::default()
        };
        assert_eq!(
            settings.content().content_mode,
            ResultContentMode::FullFinal
        );
    }

    #[test]
    fn backend_parser_rejects_removed_local_provider() {
        assert_eq!(
            NotificationBackendKind::parse("relay"),
            Some(NotificationBackendKind::Relay)
        );
        assert_eq!(NotificationBackendKind::parse("local_wechat"), None);
    }

    #[test]
    fn completion_quiet_ms_defaults_and_validates_configured_bounds() {
        let mut settings = NotificationSettings::default();
        assert_eq!(settings.completion_quiet_ms, DEFAULT_COMPLETION_QUIET_MS);
        assert!(settings.validate());

        for value in [COMPLETION_QUIET_MS_MIN, COMPLETION_QUIET_MS_MAX] {
            settings.completion_quiet_ms = value;
            assert!(settings.validate(), "quiet value {value} should be valid");
        }
        for value in [COMPLETION_QUIET_MS_MIN - 1, COMPLETION_QUIET_MS_MAX + 1] {
            settings.completion_quiet_ms = value;
            assert!(
                !settings.validate(),
                "quiet value {value} should be invalid"
            );
        }
    }

    #[test]
    fn notification_settings_deserialization_defaults_completion_quiet_ms() {
        let mut value = serde_json::to_value(NotificationSettings::default()).unwrap();
        value.as_object_mut().unwrap().remove("completionQuietMs");
        let parsed: NotificationSettings = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.completion_quiet_ms, DEFAULT_COMPLETION_QUIET_MS);
    }
}
