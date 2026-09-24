#![cfg(windows)]

use promptdock_desktop_lib::agent::AgentEventPayloadV2;
use promptdock_desktop_lib::hook_installer::{HookFeatures, HookInstallationState, HookManager};
use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const REGISTRATION_ID: &str = "22222222-2222-4222-8222-222222222222";

fn prepare_registered_capture(dir: &Path) {
    std::fs::write(
        dir.join("capture-policy.json"),
        br#"{"schemaVersion":1,"policy":{"observe_turns":true,"notify_started":true,"notify_ended":true,"completion_quiet_ms":2000,"result_content_mode":"status_only","notify_attention":false,"include_task_input":false},"revision":0,"captureGeneration":0}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("hook-registration.json"),
        serde_json::to_vec(REGISTRATION_ID).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("hook-target.json"),
        serde_json::to_vec(dir).unwrap(),
    )
    .unwrap();
}

fn run_command(command: &str, input: &[u8]) -> (Output, Duration) {
    let started = Instant::now();
    let mut child = Command::new("cmd.exe")
        .args(["/D", "/S", "/C"])
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn generated Hook command");
    child
        .stdin
        .take()
        .expect("capture stdin")
        .write_all(input)
        .expect("write Hook payload");
    (
        child.wait_with_output().expect("wait for Hook command"),
        started.elapsed(),
    )
}

fn command<'a>(root: &'a serde_json::Value, event: &str) -> &'a str {
    root.pointer(&format!("/hooks/{event}/0/hooks/0/commandWindows"))
        .and_then(serde_json::Value::as_str)
        .expect("generated commandWindows")
}

#[test]
fn windows_wrapper_suppresses_a_failing_capture_process() {
    let directory = tempfile::tempdir().expect("tempdir");
    let executable = std::path::PathBuf::from(env!("CARGO_BIN_EXE_promptdock-desktop"));
    let config = directory.path().join("hooks.json");
    prepare_registered_capture(directory.path());
    let manager = HookManager::new(
        config.clone(),
        directory.path().join("agent-events.jsonl"),
        executable,
    )
    .expect("manager")
    .with_registration_id(REGISTRATION_ID.to_owned());
    manager.install().expect("install hooks");
    let root: serde_json::Value = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();

    let (output, _) = run_command(command(&root, "Stop"), b"not-json");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"{}");
    assert!(output.stderr.is_empty());
}

#[test]
fn generated_hooks_use_special_windows_paths_and_one_shared_inbox() {
    let directory = tempfile::tempdir().expect("tempdir");
    let special = directory.path().join("空 格 & (括号) 'single'");
    std::fs::create_dir_all(&special).expect("special directory");
    let executable = special.join("Prompt Dock & (便携) '版'.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_promptdock-desktop"), &executable).expect("copy executable");
    let inbox = special.join("agent 事件 收件箱 & (数据) '原样'.jsonl");
    let config = special
        .join("Codex 配置 & (测试) '目录'")
        .join("hooks.json");
    prepare_registered_capture(&special);
    std::fs::write(
        special.join("capture-policy.json"),
        br#"{"schemaVersion":1,"policy":{"observe_turns":true,"notify_started":true,"notify_ended":true,"completion_quiet_ms":2000,"result_content_mode":"status_only","notify_attention":true,"include_task_input":false},"revision":0,"captureGeneration":0}"#,
    )
    .unwrap();
    let manager = HookManager::new(config.clone(), inbox.clone(), executable)
        .expect("manager")
        .with_registration_id(REGISTRATION_ID.to_owned());

    manager.install().expect("install generated hooks");
    let status = manager.installation_state_for().expect("hook state");
    assert_eq!(status.user_prompt, HookInstallationState::Current);
    assert_eq!(status.stop, HookInstallationState::Current);
    assert_eq!(status.permission_request, HookInstallationState::Current);
    assert_eq!(
        manager.installation_state().unwrap(),
        HookInstallationState::Current
    );

    let root: serde_json::Value = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
    assert!(root["hooks"]["PermissionRequest"][0]
        .get("matcher")
        .is_none());
    let payloads = [
        (
            "UserPromptSubmit",
            json!({
                "session_id":"special-session", "turn_id":"prompt-turn",
                "cwd":Path::new("D:\\工作 空间 & (测试) '").display().to_string(),
                "model":"gpt-5.6-sol", "prompt":"原始 prompt：空格 中文 & ( ) ' 必须完整保留",
                "hook_event_name":"UserPromptSubmit"
            }),
        ),
        (
            "Stop",
            json!({
                "session_id":"special-session", "turn_id":"stop-turn",
                "cwd":Path::new("D:\\工作 空间 & (测试) '").display().to_string(),
                "model":"gpt-5.6-sol", "last_assistant_message":"可保存的输出",
                "hook_event_name":"Stop"
            }),
        ),
        (
            "PermissionRequest",
            json!({
                "session_id":"special-session", "turn_id":"permission-turn",
                "cwd":Path::new("D:\\工作 空间 & (测试) '").display().to_string(),
                "model":"gpt-5.6-sol", "tool_name":"shell", "tool_input":{"cmd":"dir"},
                "hook_event_name":"PermissionRequest"
            }),
        ),
    ];
    for (event, payload) in payloads {
        let (output, elapsed) = run_command(
            command(&root, event),
            &serde_json::to_vec(&payload).unwrap(),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if event == "Stop" {
            assert_eq!(output.stdout, b"{}");
        } else {
            assert!(output.stdout.is_empty());
        }
        assert!(output.stderr.is_empty());
        let deadline = if event == "Stop" {
            Duration::from_secs(6)
        } else {
            Duration::from_secs(3)
        };
        assert!(elapsed < deadline, "{event} hook took {elapsed:?}");
    }
    let envelopes = std::fs::read_to_string(&inbox)
        .expect("shared inbox")
        .lines()
        .map(promptdock_desktop_lib::agent_event_capture::decode_protected_event_line)
        .collect::<Result<Vec<_>, _>>()
        .expect("complete envelopes");
    assert_eq!(envelopes.len(), 4);
    assert_eq!(
        envelopes
            .iter()
            .filter(|event| matches!(
                &event.payload, AgentEventPayloadV2::OutputProduced(output)
                    if !output.content_available && output.text.is_empty()
            ))
            .count(),
        1
    );
    assert_eq!(
        envelopes
            .iter()
            .filter(|event| matches!(&event.payload, AgentEventPayloadV2::AttentionRequired(_)))
            .count(),
        1
    );
    assert!(envelopes
        .iter()
        .any(|event| matches!(&event.payload, AgentEventPayloadV2::RunStarted(_))));
    assert!(envelopes
        .iter()
        .any(|event| matches!(&event.payload, AgentEventPayloadV2::RunSettling(_))));
    let serialized = std::fs::read_to_string(&inbox).unwrap();
    assert!(!serialized.contains("原始 prompt"));
    assert!(!serialized.contains("可保存的输出"));
    assert!(!serialized.contains("D:\\工作 空间"));
}

#[test]
fn stop_hook_only_persists_raw_output_when_capture_is_explicitly_enabled() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("hooks.json");
    let inbox = directory.path().join("agent-events.jsonl");
    prepare_registered_capture(directory.path());
    let manager = HookManager::new(
        config.clone(),
        inbox.clone(),
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_promptdock-desktop")),
    )
    .expect("manager")
    .with_registration_id(REGISTRATION_ID.to_owned());
    manager
        .reconcile_features(HookFeatures::for_phase8(true, false))
        .expect("install lifecycle-only Stop Hook");
    let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    let secret = "private answer must not reach the inbox";
    let payload = json!({
        "session_id":"privacy-session", "turn_id":"turn-without-output",
        "cwd":"C:/workspace/privacy", "model":"gpt-5.6-sol",
        "last_assistant_message":secret, "hook_event_name":"Stop"
    });
    let (output, _) = run_command(
        command(&root, "Stop"),
        &serde_json::to_vec(&payload).unwrap(),
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{}");
    let contents = std::fs::read_to_string(&inbox).unwrap();
    assert!(!contents.contains(secret));
    let events = contents
        .lines()
        .map(promptdock_desktop_lib::agent_event_capture::decode_protected_event_line)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[1].payload,
        AgentEventPayloadV2::RunSettling(_)
    ));

    manager
        .reconcile_features(HookFeatures::for_phase8(true, true))
        .expect("enable output capture");
    std::fs::write(
        directory.path().join("capture-policy.json"),
        br#"{"schemaVersion":1,"policy":{"observe_turns":true,"notify_started":true,"notify_ended":true,"completion_quiet_ms":2000,"result_content_mode":"full_final","notify_attention":false,"include_task_input":false},"revision":1,"captureGeneration":1}"#,
    )
    .unwrap();
    let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    let captured = "explicitly captured answer";
    let payload = json!({
        "session_id":"privacy-session", "turn_id":"turn-with-output",
        "cwd":"C:/workspace/privacy", "model":"gpt-5.6-sol",
        "last_assistant_message":captured, "hook_event_name":"Stop"
    });
    let (output, _) = run_command(
        command(&root, "Stop"),
        &serde_json::to_vec(&payload).unwrap(),
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{}");
    let contents = std::fs::read_to_string(inbox).unwrap();
    assert!(!contents.contains(captured));
    let events = contents
        .lines()
        .map(promptdock_desktop_lib::agent_event_capture::decode_protected_event_line)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        AgentEventPayloadV2::OutputProduced(output)
            if output.text == captured
    )));
}

#[test]
fn reconcile_is_idempotent_repairs_managed_hooks_and_preserves_foreign_handlers() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("hooks.json");
    let manager = HookManager::new(
        config.clone(),
        directory.path().join("agent-events.jsonl"),
        directory.path().join("PromptDock.exe"),
    )
    .expect("manager");
    std::fs::write(
        &config,
        serde_json::to_vec_pretty(&json!({
            "future": {"kept": true},
            "hooks": {"Stop": [{"matcher":"foreign", "hooks":[{"type":"command", "command":"foreign.exe"}]}]}
        }))
        .unwrap(),
    )
    .unwrap();
    manager.reconcile().unwrap();
    let current = std::fs::read(&config).unwrap();
    manager.reconcile().unwrap();
    assert_eq!(std::fs::read(&config).unwrap(), current);

    let mut root: serde_json::Value = serde_json::from_slice(&current).unwrap();
    root["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"] = json!(4);
    std::fs::write(&config, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
    assert!(matches!(
        manager.installation_state(),
        Ok(HookInstallationState::NeedsRepair { .. })
    ));
    manager.reconcile().unwrap();
    assert_eq!(
        manager.installation_state().unwrap(),
        HookInstallationState::Current
    );
    let repaired: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(repaired["future"]["kept"], true);
    assert_eq!(repaired["hooks"]["Stop"][0]["matcher"], "foreign");
    assert_eq!(
        repaired["hooks"]["Stop"][0]["hooks"][0]["command"],
        "foreign.exe"
    );
    assert_eq!(
        manager
            .installation_state_for()
            .unwrap()
            .foreign_stop_handlers,
        1
    );
}

#[test]
fn uninstall_removes_only_managed_new_hooks() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("hooks.json");
    let manager = HookManager::new(
        config.clone(),
        directory.path().join("agent-events.jsonl"),
        directory.path().join("PromptDock.exe"),
    )
    .unwrap();
    manager.install().unwrap();
    let mut root: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    root["hooks"]["Stop"].as_array_mut().unwrap().insert(
        0,
        json!({"hooks":[{"type":"command", "command":"foreign.exe"}]}),
    );
    root["hooks"]["Other"] = json!([{"hooks":[{"command":"old.exe --promptdock-hook-v1"}]}]);
    std::fs::write(&config, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
    manager.uninstall().unwrap();
    let root: serde_json::Value = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
    assert!(root["hooks"]["UserPromptSubmit"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(root["hooks"]["PermissionRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 1);
    assert_eq!(
        root["hooks"]["Other"][0]["hooks"][0]["command"],
        "old.exe --promptdock-hook-v1"
    );
}
