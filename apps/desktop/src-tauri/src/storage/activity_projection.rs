//! Only called within the event/finalizer transaction. Reading state lives elsewhere.
use rusqlite::{params, OptionalExtension, Transaction};

use crate::{
    agent::{AgentEventEnvelopeV2, AgentEventPayloadV2},
    error::AppError,
};

const MAX_REVISION: i64 = 9_007_199_254_740_991;
const ATTENTION_COOLDOWN_MS: i64 = 60_000;

fn ensure(tx: &Transaction<'_>, run_key: &str, now: i64) -> Result<(), AppError> {
    tx.execute(
        "INSERT OR IGNORE INTO run_presentation(run_key,updated_at) VALUES (?,?)",
        params![run_key, now],
    )?;
    Ok(())
}

pub(crate) fn advance(tx: &Transaction<'_>, run_key: &str, now: i64) -> Result<(), AppError> {
    ensure(tx, run_key, now)?;
    if tx.execute("UPDATE run_presentation SET activity_revision=activity_revision+1, updated_at=max(updated_at,?) WHERE run_key=? AND activity_revision < ?", params![now,run_key,MAX_REVISION])? != 1 {
        return Err(AppError::new("ACTIVITY_REVISION_OVERFLOW", "活动版本已达上限"));
    }
    Ok(())
}

pub(crate) fn record_run(
    tx: &Transaction<'_>,
    event: &AgentEventEnvelopeV2,
) -> Result<(), AppError> {
    let Some(key) = event.correlation.run_key.as_deref() else {
        return Ok(());
    };
    advance(tx, key, event.observed_at)?;
    if let AgentEventPayloadV2::RunStarted(start) = &event.payload {
        let label = workspace_label(start.workspace_path.as_deref());
        if !label.is_empty() {
            tx.execute(
                "UPDATE run_presentation SET workspace_label=? WHERE run_key=?",
                params![label, key],
            )?;
        }
    }
    Ok(())
}

fn workspace_label(path: Option<&str>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    let basename = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    if basename == "." || basename == ".." || basename.contains(':') || basename.contains('@') {
        return String::new();
    }
    crate::notification::redaction::redact_secrets(basename)
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect()
}

/// Persist the new observation before deciding whether it may create a notice.
/// Missing runs retain the adapter's old diagnostic behavior but create no card.
pub(crate) fn record_attention(
    tx: &Transaction<'_>,
    event: &AgentEventEnvelopeV2,
    event_row_id: &str,
    now: i64,
) -> Result<bool, AppError> {
    let Some(key) = event.correlation.run_key.as_deref() else {
        return Ok(true);
    };
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM agent_runs WHERE run_key=?",
            [key],
            |r| r.get(0),
        )
        .optional()?;
    let Some(status) = status else {
        return Ok(true);
    };
    advance(tx, key, now)?;
    if tx.execute("UPDATE run_presentation SET attention_revision=attention_revision+1,last_attention_event_id=?,last_attention_at=? WHERE run_key=? AND attention_revision < ?",params![event_row_id,now,key,MAX_REVISION])? != 1 {
        return Err(AppError::new("ATTENTION_REVISION_OVERFLOW", "提醒版本已达上限"));
    }
    let previous: Option<i64> = tx.query_row(
        "SELECT last_notice_at FROM run_presentation WHERE run_key=?",
        [key],
        |r| r.get(0),
    )?;
    Ok(status == "running"
        && previous.is_none_or(|at| now.saturating_sub(at) >= ATTENTION_COOLDOWN_MS))
}

pub(crate) fn notice_created(
    tx: &Transaction<'_>,
    key: Option<&str>,
    now: i64,
) -> Result<(), AppError> {
    if let Some(key) = key {
        tx.execute(
            "UPDATE run_presentation SET last_notice_at=? WHERE run_key=?",
            params![now, key],
        )?;
    }
    Ok(())
}

/// A sent or possibly submitted request cannot be recalled. Cancel only rows
/// that have never obtained a sending claim, precisely within this root run.
pub(crate) fn cancel_unsent_attention(
    tx: &Transaction<'_>,
    key: &str,
    now: i64,
) -> Result<(), AppError> {
    tx.execute("UPDATE notification_outbox SET status='cancelled',updated_at=?,last_error_code='ATTENTION_HISTORICAL',last_error_message='该轮已结束或正在确认结束；旧权限观察仅保留历史' WHERE agent_run_key=? AND event_kind='attention_required' AND status IN ('pending','retry_wait','blocked_activation','blocked_reconnect') AND attempt_count=0 AND relay_notification_id IS NULL AND acceptance_stage IS NULL",params![now,key])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_projection_is_only_a_bounded_safe_basename() {
        assert_eq!(workspace_label(Some(r"D:\private\AstroLink")), "AstroLink");
        assert_eq!(workspace_label(Some("/private/项目/")), "项目");
        assert_eq!(
            workspace_label(Some("https://example.test")),
            "example.test"
        );
        assert_eq!(workspace_label(Some("D:")), "");
        assert_eq!(workspace_label(Some("..")), "");
        assert_eq!(workspace_label(Some(&"x".repeat(300))).len(), 120);
    }
}
