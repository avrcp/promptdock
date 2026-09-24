use super::outbox::{NotificationEventKind, NotificationPayloadV2};
use crate::agent::{AttentionKind, CodexUsageSnapshot};
use chrono::{Local, TimeZone};

const UNKNOWN_TIME: &str = "未知时间";
const UNKNOWN_DURATION: &str = "未知";
const MAX_LABEL_CHARS: usize = 80;

/// Formats already-selected, redacted, normalized and bounded content. The
/// builder calls this once before enqueue; the sender only consumes the frozen
/// `payload.body` produced from the result.
pub(crate) fn render_content(
    kind: NotificationEventKind,
    payload: &NotificationPayloadV2,
    task: Option<&str>,
    reasoning_effort: Option<&str>,
    usage: Option<&CodexUsageSnapshot>,
    result: Option<&str>,
    result_requested: bool,
) -> (String, String) {
    let agent = sanitize_label(&payload.agent_label);
    let title = match kind {
        NotificationEventKind::Test => "PromptDock 测试通知".to_owned(),
        NotificationEventKind::RunStarted => format!("{agent} 本轮开始"),
        NotificationEventKind::RunCompleted => format!("{agent} 本轮已结束"),
        NotificationEventKind::RunFailed => format!("{agent} 任务失败"),
        NotificationEventKind::RunInterrupted => format!("{agent} 任务已中断"),
        NotificationEventKind::AttentionRequired => {
            unreachable!("attention notifications use render_attention_content")
        }
    };
    let icon = match kind {
        NotificationEventKind::Test => "✅",
        NotificationEventKind::RunStarted => "🚀",
        NotificationEventKind::RunCompleted => "⏹",
        NotificationEventKind::RunFailed => "❌",
        NotificationEventKind::RunInterrupted => "⏹",
        NotificationEventKind::AttentionRequired => {
            unreachable!("attention notifications use render_attention_content")
        }
    };

    if matches!(kind, NotificationEventKind::Test) {
        return (title, test_message());
    }

    let mut blocks = vec![format!("{icon} {title}")];
    let mut metadata = Vec::new();
    let workspace = sanitize_label(&payload.workspace_label);
    if !workspace.is_empty() {
        metadata.push(format!("项目：{workspace}"));
    }
    if matches!(kind, NotificationEventKind::RunStarted) {
        if let Some(model) = payload
            .model_name
            .as_deref()
            .map(sanitize_label)
            .filter(|model| !model.is_empty())
        {
            metadata.push(format!("模型：{model}"));
        }
        if let Some(reasoning) = reasoning_effort
            .map(sanitize_label)
            .filter(|reasoning| !reasoning.is_empty())
        {
            metadata.push(format!("推理：{reasoning}"));
        }
    } else {
        metadata.push(format!(
            "耗时：{}",
            payload
                .duration_ms
                .map(format_duration)
                .unwrap_or_else(|| UNKNOWN_DURATION.into())
        ));
        let usage_text = format_usage(usage);
        if !usage_text.is_empty() {
            blocks.push(usage_text);
        }
    }
    if !metadata.is_empty() {
        blocks.push(metadata.join("\n"));
    }
    if let Some(task) = task.filter(|text| !text.is_empty()) {
        blocks.push(format!("任务：\n{task}"));
    }
    if result_requested {
        blocks.push(match result.filter(|text| !text.is_empty()) {
            Some(result) => format!("结果摘录：\n{result}"),
            None => "结果摘录：\n未取得结果摘录".to_owned(),
        });
    }
    if matches!(kind, NotificationEventKind::RunFailed) {
        blocks.push("请返回 PromptDock/Codex 查看详情。".to_owned());
    }
    let timestamp = match kind {
        NotificationEventKind::RunStarted => payload.started_at,
        NotificationEventKind::AttentionRequired => {
            unreachable!("attention notifications use render_attention_content")
        }
        _ => payload.completed_at,
    };
    blocks.push(format!("时间：{}", format_local_time(timestamp)));
    (title, blocks.join("\n\n"))
}

pub(crate) fn terminal_result_title(
    agent_label: &str,
    usage: Option<&CodexUsageSnapshot>,
) -> String {
    let agent = sanitize_label(agent_label);
    let usage_suffix = usage
        .and_then(|snapshot| snapshot.used_percent)
        .map(|used| format!("｜账户额度：已用{}%", used))
        .unwrap_or_default();
    format!("{agent} 本轮输出{usage_suffix}")
}

fn format_usage(usage: Option<&CodexUsageSnapshot>) -> String {
    let Some(snapshot) = usage else {
        return String::new();
    };
    let Some(used) = snapshot.used_percent else {
        return String::new();
    };
    let window = snapshot
        .window_duration_mins
        .map(|minutes| {
            if minutes % 1_440 == 0 {
                format!("{} 天窗口", minutes / 1_440)
            } else {
                format!("{} 分钟窗口", minutes)
            }
        })
        .unwrap_or_else(|| "当前窗口".to_owned());
    let reset = snapshot
        .resets_at
        .and_then(|timestamp| Local.timestamp_opt(timestamp, 0).single())
        .map(|datetime| format!("，{} 重置", datetime.format("%m-%d %H:%M")))
        .unwrap_or_default();
    format!("账户额度：{window}已用 {used}%{reset}",)
}

pub(crate) fn render_attention_content(
    payload: &NotificationPayloadV2,
    attention_kind: AttentionKind,
    safe_summary: &str,
    occurred_at: i64,
) -> (String, String) {
    let agent = sanitize_label(&payload.agent_label);
    let title = format!("{agent} 正在等待你的操作");
    let mut blocks = vec![format!("⚠️ {title}")];
    let workspace = sanitize_label(&payload.workspace_label);
    if !workspace.is_empty() {
        blocks.push(format!("项目：{workspace}"));
    }
    let prompt = match attention_kind {
        AttentionKind::Permission => "需要授权",
        AttentionKind::UserInput => "需要输入",
        AttentionKind::Confirmation => "需要确认",
    };
    blocks.push(format!("{prompt}：\n{safe_summary}"));
    blocks.push(
        "请返回电脑处理。微信仅发送提醒，不能远程批准，也不能绕过 Agent 权限系统。".to_owned(),
    );
    blocks.push(format!("时间：{}", format_local_time(Some(occurred_at))));
    (title, blocks.join("\n\n"))
}

pub(crate) fn format_duration(duration_ms: i64) -> String {
    let total_seconds = duration_ms.max(0) / 1_000;
    if total_seconds < 60 {
        format!("{total_seconds} 秒")
    } else if total_seconds < 60 * 60 {
        format!("{} 分 {} 秒", total_seconds / 60, total_seconds % 60)
    } else {
        format!(
            "{} 小时 {} 分",
            total_seconds / 3_600,
            (total_seconds % 3_600) / 60
        )
    }
}

fn test_message() -> String {
    "✅ PromptDock 测试通知\n\n微信 ClawBot 通道可用。".into()
}

fn format_local_time(timestamp_ms: Option<i64>) -> String {
    timestamp_ms
        .and_then(|timestamp| Local.timestamp_millis_opt(timestamp).single())
        .map(|datetime| datetime.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| UNKNOWN_TIME.into())
}

fn sanitize_label(value: &str) -> String {
    let filtered = value
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    filtered.trim().chars().take(MAX_LABEL_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification::outbox::NOTIFICATION_PAYLOAD_SCHEMA_VERSION;

    fn payload(body: &str) -> NotificationPayloadV2 {
        NotificationPayloadV2 {
            schema_version: NOTIFICATION_PAYLOAD_SCHEMA_VERSION,
            agent_label: "Fixture Agent".into(),
            workspace_label: "prompt_dock".into(),
            model_name: Some("gpt-5.6-sol".into()),
            title: "Fixture Agent 本轮已结束".into(),
            body: body.into(),
            content_mode: crate::model::ResultContentMode::StatusOnly,
            result_revision: None,
            source_hash: None,
            content_bytes: 0,
            started_at: Some(1_700_000_000_000),
            completed_at: Some(1_700_000_496_000),
            duration_ms: Some(496_000),
            delayed_delivery: false,
        }
    }

    #[test]
    fn builder_formatter_follows_started_and_terminal_layouts() {
        let value = payload("");
        let (started_title, started) = render_content(
            NotificationEventKind::RunStarted,
            &value,
            Some("检查插件边界"),
            Some("medium"),
            None,
            None,
            false,
        );
        assert_eq!(started_title, "Fixture Agent 本轮开始");
        assert!(started
            .starts_with("🚀 Fixture Agent 本轮开始\n\n项目：prompt_dock\n模型：gpt-5.6-sol"));
        assert!(started.contains("任务：\n检查插件边界"));
        assert!(started.contains("推理：medium"));

        let (completed_title, completed) = render_content(
            NotificationEventKind::RunCompleted,
            &value,
            Some("检查插件边界"),
            None,
            None,
            Some("已完成 ✅"),
            true,
        );
        assert_eq!(completed_title, "Fixture Agent 本轮已结束");
        assert!(completed.contains("耗时：8 分 16 秒"));
        assert!(completed.contains("结果摘录：\n已完成 ✅"));
        assert!(!completed.contains("模型："));

        let (_, missing) = render_content(
            NotificationEventKind::RunCompleted,
            &value,
            None,
            None,
            None,
            None,
            true,
        );
        assert!(missing.contains("结果摘录：\n未取得结果摘录"));
    }

    #[test]
    fn duration_boundaries_are_exact_and_negative_is_zero() {
        assert_eq!(format_duration(-1), "0 秒");
        assert_eq!(format_duration(999), "0 秒");
        assert_eq!(format_duration(38_999), "38 秒");
        assert_eq!(format_duration(59_999), "59 秒");
        assert_eq!(format_duration(60_000), "1 分 0 秒");
        assert_eq!(format_duration(496_000), "8 分 16 秒");
        assert_eq!(format_duration(3_599_999), "59 分 59 秒");
        assert_eq!(format_duration(3_600_000), "1 小时 0 分");
        assert_eq!(format_duration(4_320_000), "1 小时 12 分");
    }
}
