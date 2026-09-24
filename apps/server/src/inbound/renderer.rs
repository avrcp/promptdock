use std::fmt::Write as _;

use crate::gateway::{
    GatewayRequestError, RemoteRunDetailV5, RemoteRunOutcomeV5, RemoteRunPageV5, RemoteRunPhaseV5,
    RemoteRunTreeNodeV5, RemoteRunTreeV5,
};

use super::model::RenderedInboundReply;

const MAX_REPLY_CHARS: usize = 6_000;
const HELP_BODY: &str = "PromptDock 命令\n• 帮助 / 设备 / 选择设备 N\n• 运行时 / 选择运行时 N\n• 工作区 / 选择工作区 N\n• 配置 / 选择配置 N\n• 预设 / 启动 N\n• 确认 6位码 / 取消\n• 任务 / 最近 / 失败 / 下一页\n• 状态 N / 详情 N / 子运行 N / 停止 N\n\n仅允许上述固定命令；不会执行 shell、git、文件或自由文本操作。";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConsoleDevice {
    pub id: uuid::Uuid,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SafeCatalogItem {
    pub handle: String,
    pub label: String,
    pub sensitivity: Option<String>,
    pub enabled: Option<bool>,
    pub remote_start_enabled: Option<bool>,
}

pub(super) fn render_help() -> RenderedInboundReply {
    reply(HELP_BODY.to_owned())
}

pub(super) fn render_unknown() -> RenderedInboundReply {
    reply(format!("未识别该命令。\n\n{HELP_BODY}"))
}

pub(super) fn render_devices(devices: &[ConsoleDevice]) -> RenderedInboundReply {
    if devices.is_empty() {
        return reply("当前没有在线且具备只读任务查询权限的设备。".to_owned());
    }
    let mut body = String::from("可查询设备：");
    for (index, device) in devices.iter().enumerate() {
        let _ = write!(body, "\n{}. {}", index + 1, device.name);
    }
    if devices.len() == 1 {
        body.push_str("\n\n已自动选择该设备，可发送“任务”“最近”或“失败”。");
    } else {
        body.push_str("\n\n发送“<序号> 状态”选择设备，然后查询任务。");
    }
    reply(body)
}

pub(super) fn render_device_selected(name: &str) -> RenderedInboundReply {
    reply(format!("已选择设备：{name}\n可发送“任务”“最近”或“失败”。"))
}

pub(super) fn render_device_selection_required() -> RenderedInboundReply {
    reply("有多个可查询设备。请先发送“设备”，再发送“<序号> 状态”选择设备。".to_owned())
}

pub(super) fn render_catalog(
    kind: &str,
    items: &[SafeCatalogItem],
    truncated: bool,
) -> RenderedInboundReply {
    let title = match kind {
        "runtime" => "可用运行时：",
        "workspace" => "可用工作区：",
        "profile" => "可用配置：",
        "preset" => "可用预设：",
        _ => "可用项目：",
    };
    if items.is_empty() {
        let noun = match kind {
            "runtime" => "运行时",
            "workspace" => "工作区",
            "profile" => "配置",
            "preset" => "预设",
            _ => "项目",
        };
        return reply(format!("当前没有可用的{noun}。"));
    }
    let mut body = title.to_owned();
    for (index, item) in items.iter().enumerate() {
        let _ = write!(body, "\n{}. {}", index + 1, item.label);
        if kind == "workspace" {
            if let Some(sensitivity) = item.sensitivity.as_deref() {
                let _ = write!(body, "（{sensitivity}）");
            }
            if item.remote_start_enabled == Some(false) {
                body.push_str("（远程启动已禁用）");
            }
        } else if item.enabled == Some(false) {
            body.push_str("（已禁用）");
        }
    }
    if truncated {
        body.push_str(
            "\n\n列表超过单页上限，仅展示前 10 项；仍有更多项目未展示。请缩小范围后重试。",
        );
    }
    reply(body)
}

pub(super) fn render_catalog_selected(kind: &str, label: &str) -> RenderedInboundReply {
    let noun = match kind {
        "runtime" => "运行时",
        "workspace" => "工作区",
        "profile" => "配置",
        "preset" => "预设",
        _ => "项目",
    };
    reply(format!("已选择{noun}：{label}"))
}

pub(super) fn render_confirmation_prompt(
    action: &str,
    device: &str,
    workspace: Option<&str>,
    profile: Option<&str>,
    preset: Option<&str>,
    job: Option<&str>,
    code: &str,
) -> RenderedInboundReply {
    let mut body = match action {
        "start_preset" => "即将启动 Night Run".to_owned(),
        "cancel_run" => "即将停止任务".to_owned(),
        _ => "即将执行受保护操作".to_owned(),
    };
    let _ = write!(body, "\n设备：{device}");
    if let Some(workspace) = workspace {
        let _ = write!(body, "\n工作区：{workspace}");
    }
    if let Some(profile) = profile {
        let _ = write!(body, "\n配置：{profile}");
    }
    if let Some(preset) = preset {
        let _ = write!(body, "\n预设：{preset}");
    }
    if let Some(job) = job {
        let _ = write!(body, "\n任务：{job}");
    }
    let _ = write!(
        body,
        "\n确认码：{code}\n确认码 2 分钟内有效。发送“确认 {code}”继续，或发送“取消”。"
    );
    reply(body)
}

pub(super) fn render_start_accepted(
    device: &str,
    label: &str,
    status: &str,
) -> RenderedInboundReply {
    reply(format!(
        "Night Run 已接管\n设备：{device}\n预设：{label}\n状态：{}",
        safe_status(status)
    ))
}

pub(super) fn render_cancel_accepted(
    device: &str,
    label: &str,
    status: &str,
) -> RenderedInboundReply {
    reply(format!(
        "已请求停止\n设备：{device}\n任务：{label}\n状态：{}\n请用“状态”查看最终结果",
        safe_status(status)
    ))
}

pub(super) fn render_start_rejected(
    device: &str,
    label: &str,
    status: &str,
) -> RenderedInboundReply {
    reply(format!(
        "Night Run 未接受\n设备：{device}\n预设：{label}\n状态：{}\n请检查设备状态后重试",
        safe_status(status)
    ))
}

pub(super) fn render_cancel_rejected(
    device: &str,
    label: &str,
    status: &str,
) -> RenderedInboundReply {
    reply(format!(
        "停止请求未接受\n设备：{device}\n任务：{label}\n状态：{}\n请用“状态”查看当前结果",
        safe_status(status)
    ))
}

pub(super) fn render_confirmation_error(error: &str) -> RenderedInboundReply {
    let body = match error {
        "expired" => "确认已过期，请重新发起操作。",
        "replay" => "确认已使用，请重新发起操作。",
        "wrong_scope" => "确认上下文不匹配，请重新发起操作。",
        "wrong_code" => "确认码错误，请重新发送正确的六位确认码。",
        _ => "没有找到有效确认，请先发起受保护操作。",
    };
    reply(body.to_owned())
}

pub(super) fn render_cancelled_confirmation() -> RenderedInboundReply {
    reply("已取消待确认操作。".to_owned())
}

fn safe_status(status: &str) -> &str {
    match status {
        "queued" => "排队",
        "accepted" => "已接收",
        "running" => "运行中",
        "cancelling" => "停止中",
        "cancelled" => "已取消",
        "completed" => "已完成",
        "failed" => "失败",
        "offline" => "离线",
        _ => "未知",
    }
}

pub(super) fn render_selection_error(expired: bool) -> RenderedInboundReply {
    reply(if expired {
        "选择已过期。请重新发送“设备”或任务列表命令。".to_owned()
    } else {
        "该序号不在当前列表中。请重新发送“设备”或任务列表命令。".to_owned()
    })
}

pub(super) fn render_interrupted_restart() -> RenderedInboundReply {
    reply("服务重启中断了本次只读查询，未重复执行。请重新发送命令。".to_owned())
}

pub(super) fn render_gateway_error(error: GatewayRequestError) -> RenderedInboundReply {
    let body = match error {
        GatewayRequestError::Offline | GatewayRequestError::Disconnected => {
            "目标设备当前离线，请稍后重试。"
        }
        GatewayRequestError::Expired => "设备查询超时，请重新发送命令。",
        GatewayRequestError::PendingLimit | GatewayRequestError::SlowConsumer => {
            "设备查询繁忙，请稍后重试。"
        }
        GatewayRequestError::Unauthorized | GatewayRequestError::AuthorizationUnavailable => {
            "目标设备当前没有可用的只读任务查询授权。"
        }
        GatewayRequestError::UnsupportedCapability => "目标设备版本不支持只读任务查询。",
        GatewayRequestError::Rejected => "目标设备拒绝了本次只读查询。",
        GatewayRequestError::InvalidRequest | GatewayRequestError::ProtocolMismatch => {
            "设备返回了不兼容的只读查询结果，请更新客户端后重试。"
        }
    };
    reply(body.to_owned())
}

pub(super) fn render_control_error(error: &str) -> RenderedInboundReply {
    let body = match error {
        "offline"
        | "disconnected"
        | "expired"
        | "pending_limit"
        | "slow_consumer"
        | "control_retry_pending" => "目标设备暂时无法处理受保护操作，请稍后重试。",
        "unauthorized" | "authorization_unavailable" => "目标设备当前没有可用的受保护操作授权。",
        "unsupported_capability" => "目标设备不支持该受保护操作。",
        "rejected" => "目标设备拒绝了本次受保护操作。",
        _ => "设备返回了不兼容的受保护操作结果，请更新客户端后重试。",
    };
    reply(body.to_owned())
}

pub(super) fn render_job_page(device: &str, page: &RemoteRunPageV5) -> RenderedInboundReply {
    let mut body = format!("设备：{device}\n任务：");
    if page.items.is_empty() {
        body.push_str("\n• 暂无匹配任务");
    } else {
        for (index, item) in page.items.iter().enumerate() {
            let _ = write!(
                body,
                "\n{}. {}（{}）",
                index + 1,
                item.title,
                status_label(item.phase, item.outcome)
            );
        }
        body.push_str("\n\n发送“<序号> 状态 / 详情 / 子运行”继续查看。");
    }
    if page.next_cursor.is_some() {
        body.push_str("\n发送“下一页”查看后续 10 项。");
    }
    reply(body)
}

pub(super) fn render_job_detail(
    device: &str,
    detail: &RemoteRunDetailV5,
    compact: bool,
) -> RenderedInboundReply {
    let summary = &detail.summary;
    let mut body = format!(
        "{}\n设备：{device}\n状态：{}",
        summary.title,
        status_label(summary.phase, summary.outcome)
    );
    let _ = write!(body, "\n运行时：{}", summary.runtime_label);
    let _ = write!(body, "\n工作区：{}", summary.workspace_label);
    if !compact {
        body.push_str("\n\n为保护隐私，微信端不展示或保存 Prompt、Output 和内部运行标识。");
    }
    reply(body)
}

pub(super) fn render_job_tree(device: &str, tree: &RemoteRunTreeV5) -> RenderedInboundReply {
    let mut body = format!("设备：{device}\n子运行：");
    render_tree_node(&mut body, &tree.root, 0);
    reply(body)
}

fn render_tree_node(body: &mut String, node: &RemoteRunTreeNodeV5, depth: usize) {
    if body.chars().count() >= MAX_REPLY_CHARS.saturating_sub(160) {
        return;
    }
    let indent = "  ".repeat(depth.min(4));
    let _ = write!(
        body,
        "\n{indent}• {}（{}，子运行 {}，活动 {}）",
        node.label,
        status_label(node.phase, node.outcome),
        node.child_count,
        node.active_child_count
    );
    for child in &node.children {
        render_tree_node(body, child, depth.saturating_add(1));
    }
}

fn status_label(phase: RemoteRunPhaseV5, outcome: RemoteRunOutcomeV5) -> &'static str {
    if phase == RemoteRunPhaseV5::Finished {
        return match outcome {
            RemoteRunOutcomeV5::Succeeded => "已成功",
            RemoteRunOutcomeV5::Failed => "失败",
            RemoteRunOutcomeV5::Cancelled => "已取消",
            RemoteRunOutcomeV5::Interrupted => "已中断",
            RemoteRunOutcomeV5::Blocked => "已阻塞",
            RemoteRunOutcomeV5::Unknown | RemoteRunOutcomeV5::None => "未知",
        };
    }
    match phase {
        RemoteRunPhaseV5::Pending => "排队中",
        RemoteRunPhaseV5::Provisioning => "准备中",
        RemoteRunPhaseV5::Starting => "启动中",
        RemoteRunPhaseV5::Active => "运行中",
        RemoteRunPhaseV5::WaitingInput => "等待输入",
        RemoteRunPhaseV5::Stopping | RemoteRunPhaseV5::Finalizing => "取消中",
        RemoteRunPhaseV5::Finished => unreachable!(),
    }
}

fn reply(body: String) -> RenderedInboundReply {
    RenderedInboundReply {
        title: "PromptDock".to_owned(),
        body: truncate_chars(body, MAX_REPLY_CHARS),
        job_page_selection: None,
        pending_confirmation: None,
    }
}

fn truncate_chars(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value
    } else {
        value
            .chars()
            .take(max_chars.saturating_sub(1))
            .chain(['…'])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{RemoteDesiredStateV5, RemoteRunConditionV5, RemoteRunSummaryV5};

    #[test]
    fn help_is_closed_and_exposes_only_typed_control() {
        let body = render_help().body;
        assert!(body.contains("子运行 N"));
        assert!(body.contains("确认 6位码"));
        assert!(body.contains("不会执行 shell、git、文件或自由文本操作"));
    }

    #[test]
    fn detail_omits_all_content_and_internal_handles() {
        let internal = "0123456789abcdef0123456789abcdef";
        let detail = RemoteRunDetailV5 {
            summary: RemoteRunSummaryV5 {
                run_handle: format!("run_{internal}"),
                title: "安全标题".to_owned(),
                runtime_label: "Codex".to_owned(),
                workspace_label: "PromptDock".to_owned(),
                desired_state: RemoteDesiredStateV5::Running,
                phase: RemoteRunPhaseV5::Active,
                outcome: RemoteRunOutcomeV5::None,
                conditions: vec![
                    RemoteRunConditionV5::Accepted,
                    RemoteRunConditionV5::Running,
                ],
                attention_count: 0,
                started_at: 1,
                updated_at: 2,
                child_count: 0,
                active_child_count: 0,
            },
        };
        let body = render_job_detail("HOME-PC", &detail, false).body;
        assert!(!body.contains(internal));
        assert!(body.contains("为保护隐私"));
    }

    #[test]
    fn catalog_truncation_is_explicit_and_keeps_the_reply_bounded() {
        let items = (0..10)
            .map(|index| SafeCatalogItem {
                handle: format!("workspace_{index:032x}"),
                label: format!("工作区 {index}"),
                sensitivity: None,
                enabled: None,
                remote_start_enabled: Some(true),
            })
            .collect::<Vec<_>>();
        let body = render_catalog("workspace", &items, true).body;
        assert!(body.contains("仅展示前 10 项"));
        assert!(body.contains("仍有更多项目未展示"));
        assert!(body.chars().count() <= MAX_REPLY_CHARS);
    }

    #[test]
    fn rejected_control_replies_never_claim_acceptance() {
        let start = render_start_rejected("HOME-PC", "已选预设", "unknown").body;
        let cancel = render_cancel_rejected("HOME-PC", "已选任务", "unknown").body;
        assert!(start.contains("未接受"));
        assert!(cancel.contains("未接受"));
        assert!(!start.contains("已接管"));
        assert!(!cancel.contains("已请求停止"));
    }
}
