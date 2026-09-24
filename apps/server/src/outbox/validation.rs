use sha2::{Digest as _, Sha256};

use super::{NotificationBundleV1, OutboxError, RelayNotificationV1};

pub(super) const MAX_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
pub const MAX_BUNDLE_BODY_BYTES: usize = 256 * 1024;
pub const MAX_BUNDLE_SEGMENTS: usize = 128;
pub const MAX_RENDERED_SEGMENT_BYTES: usize = 3_500;

const ALLOWED_KINDS: [&str; 8] = [
    "run_started",
    "run_completed",
    "run_failed",
    "run_interrupted",
    "attention_required",
    "test",
    "generic",
    "interactive_reply",
];

pub(super) fn validate_notification(
    notification: &RelayNotificationV1,
    accepted_at: i64,
) -> Result<(), OutboxError> {
    let valid = notification.schema_version == 1
        && byte_length_between(&notification.notification_id, 1, 128)
        && byte_length_between(&notification.dedupe_key, 1, 256)
        && ALLOWED_KINDS.contains(&notification.kind.as_str())
        && (0..=255).contains(&notification.priority)
        && char_length_between(&notification.title, 1, 128)
        && char_length_between(&notification.body, 1, 6_000)
        && notification
            .correlation_key
            .as_ref()
            .is_none_or(|value| byte_length_between(value, 1, 128))
        && notification.created_at >= 0
        && notification.expires_at > notification.created_at
        && notification.expires_at > accepted_at
        && notification.expires_at - accepted_at <= MAX_TTL_MS;
    if valid {
        Ok(())
    } else {
        Err(OutboxError::Validation)
    }
}

pub(super) fn validate_bundle(
    bundle: &NotificationBundleV1,
    accepted_at: i64,
) -> Result<(), OutboxError> {
    let source_hash = Sha256::digest(bundle.body.as_bytes());
    let expected_hash = format!("{source_hash:x}");
    let valid = bundle.schema_version == 1
        && byte_length_between(&bundle.bundle_id, 1, 128)
        && byte_length_between(&bundle.dedupe_key, 1, 256)
        && bundle.kind == "run_completed"
        && bundle.content_mode == "full_final"
        && bundle.source == "codex_stop"
        && byte_length_between(&bundle.correlation_key, 1, 128)
        && bundle.result_revision > 0
        && char_length_between(&bundle.title, 1, 128)
        && !bundle.body.is_empty()
        && bundle.body.len() <= MAX_BUNDLE_BODY_BYTES
        && bundle.source_hash == expected_hash
        && bundle.source_hash.len() == 64
        && bundle
            .source_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && bundle.created_at >= 0
        && bundle.expires_at > bundle.created_at
        && bundle.expires_at > accepted_at
        && bundle.expires_at - accepted_at <= MAX_TTL_MS;
    if valid {
        Ok(())
    } else {
        Err(OutboxError::Validation)
    }
}

pub(super) fn validate_target_account_fingerprint(value: &str) -> Result<(), OutboxError> {
    let valid = value.strip_prefix("wx:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if valid {
        Ok(())
    } else {
        Err(OutboxError::Validation)
    }
}

fn byte_length_between(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len())
}

fn char_length_between(value: &str, minimum: usize, maximum: usize) -> bool {
    let length = value.chars().count();
    (minimum..=maximum).contains(&length)
}
