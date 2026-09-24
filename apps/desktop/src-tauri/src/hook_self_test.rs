//! Explicit local-only diagnostic. Never enters guided verification or delivery storage.
use serde::Serialize;
use std::{
    io::Write,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfTestReceipt {
    pub outcome: &'static str,
    pub records: usize,
    pub elapsed_ms: u128,
}

pub(crate) fn run() -> Result<SelfTestReceipt, String> {
    run_with_executable(&std::env::current_exe().map_err(|_| "SELF_TEST_EXE")?)
}

#[doc(hidden)]
pub fn run_with_executable(executable: &std::path::Path) -> Result<SelfTestReceipt, String> {
    let temp = std::env::temp_dir().join(format!("promptdock-self-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp).map_err(|_| "SELF_TEST_DIRECTORY")?;
    let result = (|| {
        let inbox = temp.join("diagnostic.jsonl");
        let registration = "33333333-3333-4333-8333-333333333333";
        // Isolated current-registration fixture: no user profile, host config,
        // or historical transcript is read by this component diagnostic.
        std::fs::write(
            temp.join("capture-policy.json"),
            br#"{"schemaVersion":1,"policy":{"observe_turns":true,"notify_started":true,"notify_ended":true,"completion_quiet_ms":2000,"result_content_mode":"status_only","notify_attention":false,"include_task_input":false},"revision":0,"captureGeneration":0}"#,
        )
        .map_err(|_| "SELF_TEST_POLICY")?;
        std::fs::write(
            temp.join("hook-registration.json"),
            serde_json::to_vec(registration).map_err(|_| "SELF_TEST_REGISTRATION")?,
        )
        .map_err(|_| "SELF_TEST_REGISTRATION")?;
        std::fs::write(
            temp.join("hook-target.json"),
            serde_json::to_vec(&temp).map_err(|_| "SELF_TEST_TARGET")?,
        )
        .map_err(|_| "SELF_TEST_TARGET")?;
        let start = Instant::now();
        let mut child = Command::new(executable)
            .args(["--capture-agent-event", "codex-user-prompt", "--inbox"])
            .arg(&inbox)
            .args(["--hook-registration-id", registration])
            .arg(crate::hook_installer::USER_PROMPT_HOOK_MARKER)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "SELF_TEST_SPAWN")?;
        let input = br#"{"session_id":"diagnostic-session","turn_id":"diagnostic-turn","cwd":"C:/isolated","model":"diagnostic","hook_event_name":"UserPromptSubmit","prompt":"PromptDock isolated component check"}"#;
        let written = child
            .stdin
            .take()
            .ok_or("SELF_TEST_STDIN")
            .and_then(|mut pipe| pipe.write_all(input).map_err(|_| "SELF_TEST_STDIN"));
        if written.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("SELF_TEST_STDIN".into());
        }
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("SELF_TEST_WAIT".into());
                }
                Ok(None) if start.elapsed() >= Duration::from_secs(7) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("SELF_TEST_TIMEOUT".into());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let bytes = crate::hook_health::bounded_read(&inbox)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| "SELF_TEST_RECORD")?;
        let records = text
            .lines()
            .map(crate::agent_event_capture::decode_protected_event_line)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "SELF_TEST_DECRYPT")?;
        if records.len() != 1
            || !matches!(&records[0].payload, crate::agent::AgentEventPayloadV2::RunStarted(started) if started.task.is_none())
            || text.contains("isolated component check")
        {
            return Err("SELF_TEST_RECEIPT_MISSING".into());
        }
        Ok(SelfTestReceipt {
            outcome: "local_component_passed",
            records: records.len(),
            elapsed_ms: start.elapsed().as_millis(),
        })
    })();
    // This uniquely created directory contains only this diagnostic's synthetic records.
    let _ = std::fs::remove_dir_all(&temp);
    result
}
