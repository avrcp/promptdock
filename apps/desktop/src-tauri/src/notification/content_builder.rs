use super::outbox::{NotificationEventKind, NotificationPayloadV2};
use super::renderer::{render_attention_content, render_content};
use super::safe_content::prepare_content;
use super::{truncate_chars_with_notice, MAX_NOTIFICATION_CHARS};
use crate::agent::{AttentionKind, CodexUsageSnapshot, RunStartedV2};
use crate::error::AppError;
use crate::model::{NotificationSettings, ResultContentMode};
use crate::storage::{AgentRunRecord, StoredResultContent};

const MAX_ATTENTION_SUMMARY_CHARS: usize = 600;

pub(crate) struct BuildNotificationInput<'a> {
    pub event_kind: NotificationEventKind,
    pub run: &'a AgentRunRecord,
    pub output: Option<&'a StoredResultContent>,
    pub start_context: Option<&'a RunStartedV2>,
    pub usage: Option<&'a CodexUsageSnapshot>,
    pub settings: &'a NotificationSettings,
}

pub(crate) struct BuildAttentionNotificationInput<'a> {
    pub attention_kind: AttentionKind,
    pub agent_label: &'a str,
    pub safe_summary: &'a str,
    pub occurred_at: i64,
    pub settings: &'a NotificationSettings,
}

pub(crate) struct RenderedNotification {
    pub title: String,
    pub body: String,
    pub safe_payload: NotificationPayloadV2,
}

impl RenderedNotification {
    pub(crate) fn into_safe_payload(self) -> NotificationPayloadV2 {
        debug_assert_eq!(self.title, self.safe_payload.title);
        debug_assert_eq!(self.body, self.safe_payload.body);
        self.safe_payload
    }
}

pub(crate) struct NotificationContentBuilder;

impl NotificationContentBuilder {
    pub(crate) fn build(
        input: BuildNotificationInput<'_>,
    ) -> Result<RenderedNotification, AppError> {
        let _frozen_settings_epoch = input.settings.content_epoch;
        let terminal = !matches!(input.event_kind, NotificationEventKind::RunStarted);
        let terminal_at = terminal_timestamp(input.event_kind, input.run);
        let duration_ms = input
            .run
            .started_at
            .zip(terminal_at)
            .map(|(started, terminal)| terminal.saturating_sub(started).max(0));
        let workspace_label = input
            .start_context
            .and_then(|start| start.workspace_path.as_deref())
            .and_then(|path| std::path::Path::new(path).file_name())
            .and_then(|name| name.to_str());
        let model_name = input
            .start_context
            .and_then(|start| start.model_name.as_deref());
        let task = input
            .start_context
            .and_then(|start| start.task.as_deref())
            .filter(|_| {
                matches!(input.event_kind, NotificationEventKind::RunStarted)
                    && input.settings.include_task_input
            })
            .and_then(|task| prepare_content(task, 4_000, input.settings.redact_secrets));
        let reasoning_effort = input
            .start_context
            .and_then(|start| start.reasoning_effort.as_deref());
        let mut safe_payload = NotificationPayloadV2::for_run(
            &input.run.agent_label,
            workspace_label,
            model_name,
            input.run.started_at,
            terminal_at,
            duration_ms,
        );
        if terminal {
            if let Some(output) = input.output {
                safe_payload.content_mode = output.content_mode;
                safe_payload.result_revision = Some(output.result_revision);
                safe_payload.source_hash = output.source_hash.clone();
                safe_payload.content_bytes = output.content_bytes;
            }
        }
        let (title, body) = match input.output.filter(|_| terminal) {
            Some(output)
                if output.content_available
                    && matches!(output.content_mode, ResultContentMode::FullFinal) =>
            {
                (
                    super::renderer::terminal_result_title(&safe_payload.agent_label, input.usage),
                    output.text.clone(),
                )
            }
            Some(output) if matches!(output.content_mode, ResultContentMode::RedactedExcerpt) => {
                let (title, rendered) = render_content(
                    input.event_kind,
                    &safe_payload,
                    task.as_deref(),
                    reasoning_effort,
                    input.usage,
                    output.content_available.then_some(output.text.as_str()),
                    true,
                );
                (
                    title,
                    truncate_chars_with_notice(&rendered, MAX_NOTIFICATION_CHARS),
                )
            }
            _ => {
                let (title, rendered) = render_content(
                    input.event_kind,
                    &safe_payload,
                    task.as_deref(),
                    reasoning_effort,
                    input.usage,
                    None,
                    false,
                );
                (
                    title,
                    truncate_chars_with_notice(&rendered, MAX_NOTIFICATION_CHARS),
                )
            }
        };
        safe_payload.title = title.clone();
        safe_payload.body = body.clone();
        safe_payload.validate()?;
        Ok(RenderedNotification {
            title,
            body,
            safe_payload,
        })
    }

    pub(crate) fn build_attention(
        input: BuildAttentionNotificationInput<'_>,
    ) -> Result<RenderedNotification, AppError> {
        let content = input.settings.content();
        let summary = prepare_content(
            input.safe_summary,
            MAX_ATTENTION_SUMMARY_CHARS,
            content.redact_secrets,
        )
        .unwrap_or_else(|| "Codex 请求需要你在电脑上处理的操作".to_owned());
        let mut safe_payload = NotificationPayloadV2::for_run(
            input.agent_label,
            None,
            None,
            None,
            Some(input.occurred_at),
            None,
        );
        let (title, rendered_body) = render_attention_content(
            &safe_payload,
            input.attention_kind,
            &summary,
            input.occurred_at,
        );
        let body = truncate_chars_with_notice(&rendered_body, MAX_NOTIFICATION_CHARS);
        safe_payload.title = title.clone();
        safe_payload.body = body.clone();
        safe_payload.validate()?;
        Ok(RenderedNotification {
            title,
            body,
            safe_payload,
        })
    }
}

fn terminal_timestamp(kind: NotificationEventKind, run: &AgentRunRecord) -> Option<i64> {
    match kind {
        NotificationEventKind::RunStarted => None,
        NotificationEventKind::RunCompleted => run.completed_at.or(Some(run.last_event_at)),
        NotificationEventKind::RunFailed => run.failed_at.or(Some(run.last_event_at)),
        NotificationEventKind::RunInterrupted => run.interrupted_at.or(Some(run.last_event_at)),
        NotificationEventKind::Test | NotificationEventKind::AttentionRequired => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{CodexUsageSnapshot, RunStartedV2};

    fn run() -> AgentRunRecord {
        AgentRunRecord {
            run_key: "run-1".into(),
            agent_kind: "codex".into(),
            instance_id: "desktop".into(),
            conversation_key: None,
            parent_run_key: None,
            agent_id: None,
            agent_role: None,
            agent_label: "Codex".into(),
            status: "completed".into(),
            outcome: "unknown".into(),
            completion_confidence: "inferred".into(),
            started_at: Some(100),
            settling_at: Some(150),
            settle_not_before: None,
            settle_generation: 1,
            completed_at: Some(200),
            failed_at: None,
            interrupted_at: None,
            cancelled_at: None,
            last_event_at: 200,
        }
    }

    #[test]
    fn metadata_mode_never_includes_an_available_result() {
        let output = StoredResultContent {
            text: String::new(),
            content_mode: ResultContentMode::StatusOnly,
            content_available: false,
            content_bytes: 0,
            result_revision: 1,
            source_hash: None,
        };
        let rendered = NotificationContentBuilder::build(BuildNotificationInput {
            event_kind: NotificationEventKind::RunCompleted,
            run: &run(),
            output: Some(&output),
            start_context: None,
            usage: None,
            settings: &NotificationSettings::default(),
        })
        .unwrap();
        assert!(!rendered.body.contains("private result"));
        assert!(rendered.body.contains("Codex 本轮已结束"));
    }

    #[test]
    fn frozen_excerpt_and_missing_output_have_distinct_short_bodies() {
        let excerpt = "[REDACTED]";
        let output = StoredResultContent {
            text: excerpt.into(),
            content_mode: ResultContentMode::RedactedExcerpt,
            content_available: true,
            content_bytes: excerpt.len(),
            result_revision: 1,
            source_hash: Some(crate::agent::source_hash_sha256(excerpt)),
        };
        let redacted = NotificationContentBuilder::build(BuildNotificationInput {
            event_kind: NotificationEventKind::RunCompleted,
            run: &run(),
            output: Some(&output),
            start_context: None,
            usage: None,
            settings: &NotificationSettings::default(),
        })
        .unwrap();
        assert!(redacted.body.contains("[REDACTED]"));
        let missing_output = StoredResultContent {
            text: String::new(),
            content_mode: ResultContentMode::RedactedExcerpt,
            content_available: false,
            content_bytes: 0,
            result_revision: 2,
            source_hash: None,
        };
        let missing = NotificationContentBuilder::build(BuildNotificationInput {
            event_kind: NotificationEventKind::RunCompleted,
            run: &run(),
            output: Some(&missing_output),
            start_context: None,
            usage: None,
            settings: &NotificationSettings::default(),
        })
        .unwrap();
        assert!(missing.body.contains("未取得"));
    }

    #[test]
    fn full_final_body_is_the_exact_source_bytes() {
        let exact = "中英 👩🏽‍💻 e\u{301}\r\n\r\n\t```\n  code  \\ path\n```";
        let output = StoredResultContent {
            text: exact.into(),
            content_mode: ResultContentMode::FullFinal,
            content_available: true,
            content_bytes: exact.len(),
            result_revision: 7,
            source_hash: Some(crate::agent::source_hash_sha256(exact)),
        };
        let rendered = NotificationContentBuilder::build(BuildNotificationInput {
            event_kind: NotificationEventKind::RunCompleted,
            run: &run(),
            output: Some(&output),
            start_context: None,
            usage: None,
            settings: &NotificationSettings::default(),
        })
        .unwrap();
        assert_eq!(rendered.body.as_bytes(), exact.as_bytes());
        assert_eq!(rendered.safe_payload.result_revision, Some(7));
        assert_eq!(rendered.safe_payload.content_bytes, exact.len());
        assert_eq!(
            rendered.safe_payload.source_hash.as_deref(),
            Some(crate::agent::source_hash_sha256(exact).as_str())
        );
    }

    #[test]
    fn start_includes_original_task_workspace_model_and_reasoning() {
        let start = RunStartedV2 {
            agent_label: "Codex".into(),
            workspace_path: Some(r"C:\workspace\promptdock-desktop".into()),
            model_name: Some("gpt-5.6-terra".into()),
            task: Some("检查 Authorization: Bearer fixture-secret".into()),
            reasoning_effort: Some("high".into()),
        };
        let rendered = NotificationContentBuilder::build(BuildNotificationInput {
            event_kind: NotificationEventKind::RunStarted,
            run: &run(),
            output: None,
            start_context: Some(&start),
            usage: None,
            settings: &NotificationSettings {
                include_task_input: true,
                ..NotificationSettings::default()
            },
        })
        .unwrap();
        assert!(rendered.body.contains("项目：promptdock-desktop"));
        assert!(rendered.body.contains("模型：gpt-5.6-terra"));
        assert!(rendered.body.contains("推理：high"));
        assert!(rendered
            .body
            .contains("任务：\n检查 Authorization: Bearer [REDACTED]"));
        assert!(!rendered.body.contains("fixture-secret"));
    }

    #[test]
    fn task_input_requires_the_explicit_start_notification_switch() {
        let start = RunStartedV2 {
            agent_label: "Codex".into(),
            workspace_path: None,
            model_name: None,
            task: Some("private task input".into()),
            reasoning_effort: None,
        };
        let rendered = NotificationContentBuilder::build(BuildNotificationInput {
            event_kind: NotificationEventKind::RunStarted,
            run: &run(),
            output: None,
            start_context: Some(&start),
            usage: None,
            settings: &NotificationSettings::default(),
        })
        .unwrap();
        assert!(!rendered.body.contains("private task input"));
        assert!(!rendered.body.contains("任务："));
    }

    #[test]
    fn full_final_keeps_exact_body_and_places_usage_in_single_link_title() {
        let exact = "exact result";
        let output = StoredResultContent {
            text: exact.into(),
            content_mode: ResultContentMode::FullFinal,
            content_available: true,
            content_bytes: exact.len(),
            result_revision: 1,
            source_hash: Some(crate::agent::source_hash_sha256(exact)),
        };
        let usage = CodexUsageSnapshot {
            used_percent: Some(47),
            window_duration_mins: Some(10_080),
            resets_at: Some(1_800_000_000),
        };
        let rendered = NotificationContentBuilder::build(BuildNotificationInput {
            event_kind: NotificationEventKind::RunCompleted,
            run: &run(),
            output: Some(&output),
            start_context: None,
            usage: Some(&usage),
            settings: &NotificationSettings::default(),
        })
        .unwrap();
        assert_eq!(rendered.body, exact);
        assert!(rendered.title.contains("已用47%"));
    }
}
