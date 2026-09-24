use super::{InboundCommandV5, RunFilter};

pub const MAX_INBOUND_TEXT_CHARS: usize = 4_096;
const MAX_JOB_SLOT: u16 = 999;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedInboundCommand {
    pub command: InboundCommandV5,
    pub(super) error_code: Option<&'static str>,
}

pub fn parse_inbound_command(text: &str) -> ParsedInboundCommand {
    if text.chars().count() > MAX_INBOUND_TEXT_CHARS {
        return ParsedInboundCommand {
            command: InboundCommandV5::Unknown,
            error_code: Some("INPUT_TOO_LARGE"),
        };
    }
    let command = if matches_any_normalized(text, &["帮助", "help", "Help", "HELP", "/help"]) {
        InboundCommandV5::Help
    } else if matches_any_normalized(
        text,
        &["设备", "device", "devices", "list_devices", "/devices"],
    ) {
        InboundCommandV5::ListDevices
    } else if matches_any_normalized(text, &["运行时", "runtimes", "runtime", "/runtimes"]) {
        InboundCommandV5::ListRuntimes
    } else if matches_any_normalized(text, &["工作区", "workspaces", "workspace", "/workspaces"])
    {
        InboundCommandV5::ListWorkspaces
    } else if matches_any_normalized(text, &["配置", "profiles", "profile", "/profiles"]) {
        InboundCommandV5::ListHarnessProfiles
    } else if matches_any_normalized(text, &["预设", "presets", "preset", "/presets"]) {
        InboundCommandV5::ListTaskPresets
    } else if matches_any_normalized(text, &["任务", "jobs", "/jobs"]) {
        InboundCommandV5::ListRuns {
            filter: RunFilter::All,
        }
    } else if matches_any_normalized(text, &["最近", "recent", "/recent"]) {
        InboundCommandV5::ListRuns {
            filter: RunFilter::Recent,
        }
    } else if matches_any_normalized(text, &["失败", "failed", "/failed"]) {
        InboundCommandV5::ListRuns {
            filter: RunFilter::Failed,
        }
    } else if matches_any_normalized(text, &["下一页", "next", "next_page", "/next"]) {
        InboundCommandV5::NextPage
    } else {
        parse_closed_command(text.trim_matches(char::is_whitespace))
            .unwrap_or(InboundCommandV5::Unknown)
    };
    ParsedInboundCommand {
        command,
        error_code: None,
    }
}

fn matches_any_normalized(text: &str, aliases: &[&str]) -> bool {
    let trimmed = text.trim_matches(char::is_whitespace);
    aliases.iter().any(|alias| {
        trimmed
            .chars()
            .map(|character| {
                if character == '\u{3000}' {
                    ' '
                } else {
                    character
                }
            })
            .eq(alias.chars())
    })
}

fn parse_closed_command(value: &str) -> Option<InboundCommandV5> {
    let mut parts = value.split_whitespace();
    let first = parts.next()?;
    let second = parts.next();
    let third = parts.next();
    if third.is_some() {
        return None;
    }

    // The sink intercepts valid confirmation commands before this parser so
    // the code cannot become part of a durable command.
    if matches!(first, "确认" | "confirm") {
        return None;
    }
    if second.is_none() {
        return match first {
            "取消" | "cancel" | "/cancel" => Some(InboundCommandV5::CancelConfirmation),
            _ => None,
        };
    }
    let (action, slot) = if let Ok(slot) = first.parse::<u16>() {
        (second?, slot)
    } else {
        let slot = second?.parse::<u16>().ok()?;
        (first, slot)
    };
    if slot == 0 || slot > MAX_JOB_SLOT {
        return None;
    }
    match action {
        "选择设备" | "select_device" => Some(InboundCommandV5::SelectDevice { slot }),
        "选择运行时" | "select_runtime" => Some(InboundCommandV5::SelectRuntime { slot }),
        "选择工作区" | "select_workspace" => Some(InboundCommandV5::SelectWorkspace { slot }),
        "选择配置" | "select_profile" => Some(InboundCommandV5::SelectHarnessProfile { slot }),
        "启动" | "start" => Some(InboundCommandV5::StartRun { slot }),
        "状态" | "status" => Some(InboundCommandV5::GetRunStatus { slot }),
        "详情" | "detail" => Some(InboundCommandV5::GetRunDetail { slot }),
        "子运行" | "tree" => Some(InboundCommandV5::GetRunTree { slot }),
        "停止" | "stop" => Some(InboundCommandV5::CancelRun { slot }),
        _ => None,
    }
}

pub(super) fn parse_confirmation_code(text: &str) -> Option<&str> {
    let mut parts = text.trim_matches(char::is_whitespace).split_whitespace();
    let command = parts.next()?;
    let code = parts.next()?;
    if parts.next().is_some() || !matches!(command, "确认" | "confirm") {
        return None;
    }
    (code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit())).then_some(code)
}
