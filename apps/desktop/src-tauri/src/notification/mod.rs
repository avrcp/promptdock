pub(crate) mod content_builder;
pub(crate) mod hold;
pub(crate) mod outbox;
pub(crate) mod redaction;
pub(crate) mod renderer;
pub(crate) mod runtime;
pub(crate) mod safe_content;

pub(crate) const MAX_NOTIFICATION_CHARS: usize = 6_000;
pub(crate) const CONTENT_TRUNCATION_NOTICE: &str =
    "（内容过长，已截断，请回到 PromptDock 查看完整内容）";

/// A test send reports the durable acceptance boundary it actually reached.
/// Relay acceptance is deliberately not represented as provider delivery.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(
    tag = "acceptanceStage",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TestNotificationResult {
    Relay {
        status: &'static str,
        notification_id: String,
        accepted_at: i64,
    },
}

pub(crate) fn started_notification_policy_fingerprint() -> String {
    blake3::hash(b"promptdock-start-task-model-reasoning-redacted-v2")
        .to_hex()
        .to_string()
}

pub(crate) fn run_notification_policy_fingerprint(
    notifications: &crate::model::NotificationSettings,
    capture_agent_outputs: bool,
) -> String {
    // This fingerprint freezes only content permission. Delivery selection is
    // checked again when a row is claimed: disabling observation or an event
    // kind cancels its unclaimed rows, while unrelated choices keep a frozen
    // payload and content epoch intact.
    let _ = capture_agent_outputs;
    let encoded = serde_json::to_vec(&notifications.content_epoch)
        .expect("notification settings are always serializable");
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"promptdock-run-notification-policy-v2\0");
    hasher.update(&encoded);
    hasher.finalize().to_hex().to_string()
}

pub(crate) fn attention_notification_policy_fingerprint(
    notifications: &crate::model::NotificationSettings,
) -> String {
    let encoded = serde_json::to_vec(&(
        notifications.enabled,
        notifications.attention_enabled,
        notifications.redact_secrets,
    ))
    .expect("notification settings are always serializable");
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"promptdock-attention-notification-policy-v2\0");
    hasher.update(&encoded);
    hasher.finalize().to_hex().to_string()
}

pub(crate) fn truncate_chars_with_notice(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }

    let separator = "\n\n";
    let suffix_chars = separator.chars().count() + CONTENT_TRUNCATION_NOTICE.chars().count();
    if max_chars <= suffix_chars {
        return CONTENT_TRUNCATION_NOTICE.chars().take(max_chars).collect();
    }

    let content_limit = max_chars - suffix_chars;
    let mut truncated = text.chars().take(content_limit).collect::<String>();
    let trimmed_len = truncated.trim_end().len();
    truncated.truncate(trimmed_len);
    truncated.push_str(separator);
    truncated.push_str(CONTENT_TRUNCATION_NOTICE);
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NotificationSettings;

    #[test]
    fn test_notification_result_serializes_the_actual_acceptance_stage() {
        assert_eq!(
            serde_json::to_value(TestNotificationResult::Relay {
                status: "relay_accepted",
                notification_id: "notification-1".into(),
                accepted_at: 123,
            })
            .unwrap(),
            serde_json::json!({
                "acceptanceStage": "relay",
                "status": "relay_accepted",
                "notificationId": "notification-1",
                "acceptedAt": 123
            })
        );
    }

    #[test]
    fn selection_changes_do_not_rewrite_a_frozen_content_fingerprint() {
        let mut settings = NotificationSettings::default();
        let before = run_notification_policy_fingerprint(&settings, false);
        settings.mode = crate::model::NotificationMode::CompletionOnly;
        settings.notify_ended = false;
        settings.completion_quiet_ms = 500;
        settings.result_content_mode = crate::model::ResultContentMode::FullFinal;
        assert_eq!(before, run_notification_policy_fingerprint(&settings, true));
        settings.content_epoch += 1;
        assert_ne!(before, run_notification_policy_fingerprint(&settings, true));
    }

    #[test]
    fn attention_toggle_is_isolated_from_run_policy_but_revokes_attention_policy() {
        let disabled = NotificationSettings {
            enabled: true,
            ..NotificationSettings::default()
        };
        let mut enabled = disabled.clone();
        enabled.attention_enabled = true;

        assert_eq!(
            run_notification_policy_fingerprint(&disabled, false),
            run_notification_policy_fingerprint(&enabled, false)
        );
        assert_ne!(
            attention_notification_policy_fingerprint(&disabled),
            attention_notification_policy_fingerprint(&enabled)
        );
    }
}
