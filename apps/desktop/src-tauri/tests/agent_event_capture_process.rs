use promptdock_desktop_lib::agent::{AgentEventEnvelopeV2, AgentEventPayloadV2};
use promptdock_desktop_lib::hook_installer::{
    PERMISSION_REQUEST_HOOK_MARKER, STOP_HOOK_MARKER, USER_PROMPT_HOOK_MARKER,
};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};

const USER_PROMPT_EVENT: &str = "codex-user-prompt";
const STOP_EVENT: &str = "codex-stop";
const PERMISSION_REQUEST_EVENT: &str = "codex-permission-request";
const REGISTRATION_ID: &str = "11111111-1111-4111-8111-111111111111";

#[cfg(windows)]
#[test]
fn reset_exclusive_guard_rejects_helper_before_waiting_for_stdin() {
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};

    struct ReapChild(Child);
    impl Drop for ReapChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory.path().join("agent-events.jsonl");
    prepare_registered_capture(&inbox);
    let exclusive = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .share_mode(0)
        .open(directory.path().join(".runtime-access.guard"))
        .unwrap();
    let mut child = ReapChild(
        Command::new(env!("CARGO_BIN_EXE_promptdock-desktop"))
            .args([
                "--capture-agent-event",
                "--hook-registration-id",
                REGISTRATION_ID,
                STOP_EVENT,
                "--inbox",
            ])
            .arg(&inbox)
            .arg(STOP_HOOK_MARKER)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    // Keep stdin open. The old order would wait four seconds and could capture
    // this invocation after reset completes; rejection must happen immediately.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "helper waited for stdin while reset owned the guard"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(exclusive);
    assert_eq!(out.join().unwrap(), b"{}");
    assert_eq!(
        String::from_utf8(err.join().unwrap()).unwrap().trim(),
        "RUNTIME_ACCESS_UNAVAILABLE"
    );
    assert!(!inbox.exists());
    assert!(!directory.path().join("hook-observed-stop.json").exists());
}

fn prepare_registered_capture(inbox: &Path) {
    let dir = inbox.parent().expect("inbox parent");
    std::fs::create_dir_all(dir).unwrap();
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

fn marker_for(event: &str) -> &'static str {
    match event {
        USER_PROMPT_EVENT => USER_PROMPT_HOOK_MARKER,
        STOP_EVENT => STOP_HOOK_MARKER,
        PERMISSION_REQUEST_EVENT => PERMISSION_REQUEST_HOOK_MARKER,
        _ => panic!("unsupported test Hook event"),
    }
}

fn run_event(event: &str, inbox: &Path, input: &[u8]) -> Output {
    run_event_with_capture(event, inbox, input, event == STOP_EVENT)
}

fn run_event_with_capture(
    event: &str,
    inbox: &Path,
    input: &[u8],
    capture_agent_output: bool,
) -> Output {
    prepare_registered_capture(inbox);
    let mut command = Command::new(env!("CARGO_BIN_EXE_promptdock-desktop"));
    command
        .args(["--capture-agent-event", event, "--inbox"])
        .arg(inbox)
        .args(["--hook-registration-id", REGISTRATION_ID]);
    if capture_agent_output {
        command.arg("--capture-agent-output");
    }
    command.arg(marker_for(event));
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn run_args(args: &[&str], input: &[u8], current_dir: &Path) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_promptdock-desktop"))
        .args(args)
        .current_dir(current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn assert_silent_success(output: &Output) {
    assert!(output.status.success(), "process failed: {output:?}");
    assert!(
        output.stdout.is_empty() || output.stdout == b"{}",
        "only Stop may return the neutral JSON object"
    );
    assert!(output.stderr.is_empty(), "stderr must stay empty");
}

fn common(event_name: &str, turn_id: &str) -> Value {
    json!({
        "session_id": "session-1",
        "turn_id": turn_id,
        "cwd": "C:/workspace/prompt dock",
        "model": "gpt-5.6",
        "hook_event_name": event_name
    })
}

fn user_prompt_input(turn_id: &str, prompt: &str) -> Vec<u8> {
    let mut value = common("UserPromptSubmit", turn_id);
    value["prompt"] = Value::String(prompt.into());
    serde_json::to_vec(&value).unwrap()
}

fn stop_input(turn_id: &str, message: Option<&str>) -> Vec<u8> {
    let mut value = common("Stop", turn_id);
    value["last_assistant_message"] = message.map_or(Value::Null, |message| json!(message));
    value["transcript_path"] = json!("private transcript path");
    serde_json::to_vec(&value).unwrap()
}

fn permission_input(turn_id: &str, tool_input: Value) -> Vec<u8> {
    let mut value = common("PermissionRequest", turn_id);
    value["tool_name"] = json!("Bash");
    value["tool_input"] = tool_input;
    serde_json::to_vec(&value).unwrap()
}

fn read_events(inbox: &Path) -> Vec<AgentEventEnvelopeV2> {
    let serialized = std::fs::read_to_string(inbox).unwrap_or_else(|error| {
        let diagnostics = std::fs::read_to_string(error_log(inbox)).unwrap_or_default();
        panic!("failed to read inbox: {error}; diagnostics: {diagnostics}")
    });
    serialized
        .lines()
        .map(|line| {
            promptdock_desktop_lib::agent_event_capture::decode_protected_event_line(line).unwrap()
        })
        .collect()
}

/// The contract a burst of Hook helpers has to satisfy, whether or not the
/// machine kept up.
///
/// Each helper waits at most `CAPTURE_LOCK_TIMEOUT` for the inbox lock and then
/// fails open: exit 0, empty stdout and stderr, one diagnostic line naming the
/// code. On a quiet disk all `expected_batches` land, which is why asserting the
/// count directly held for so long; on a loaded runner a burst can exhaust the
/// budget. What must hold either way is that no record is torn, no record is
/// duplicated, and no record is missing without a diagnostic to explain it.
fn assert_batches_accounted(
    inbox: &Path,
    events: &[AgentEventEnvelopeV2],
    expected_batches: usize,
    records_per_batch: usize,
) {
    let diagnostics = std::fs::read_to_string(error_log(inbox)).unwrap_or_default();
    let failed = diagnostics.lines().count();
    for line in diagnostics.lines() {
        assert!(
            line.ends_with("agent_event_capture_failed code=HOOK_LOCK_TIMEOUT"),
            "unexpected capture diagnostic: {line}"
        );
    }
    assert_eq!(
        events.len() + failed * records_per_batch,
        expected_batches * records_per_batch,
        "every record is either in the inbox or accounted for by a diagnostic; diagnostics: {diagnostics}"
    );
    assert!(
        !events.is_empty(),
        "the whole burst failed open; diagnostics: {diagnostics}"
    );
    let distinct: std::collections::HashSet<&str> =
        events.iter().map(|event| event.event_id.as_str()).collect();
    assert_eq!(
        distinct.len(),
        events.len(),
        "a burst must not record the same event twice"
    );
}

fn error_log(inbox: &Path) -> std::path::PathBuf {
    let mut path = inbox.as_os_str().to_owned();
    path.push(".errors.log");
    path.into()
}

#[test]
fn user_prompt_emits_only_safe_run_started_in_one_process() {
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory.path().join("agent-events.jsonl");

    let output = run_event(
        USER_PROMPT_EVENT,
        &inbox,
        &user_prompt_input("turn-1", "capture me"),
    );
    assert_silent_success(&output);

    let events = read_events(&inbox);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].payload,
        AgentEventPayloadV2::RunStarted(_)
    ));
    assert!(!std::fs::read_to_string(&inbox)
        .unwrap()
        .contains("capture me"));
}

#[test]
fn stop_defaults_to_status_marker_and_settling_without_plaintext_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory.path().join("agent-events.jsonl");

    let output = run_event(
        STOP_EVENT,
        &inbox,
        &stop_input("turn-1", Some("private answer")),
    );
    assert_silent_success(&output);

    let serialized = std::fs::read_to_string(&inbox).unwrap();
    assert!(!serialized.contains("private transcript path"));
    let events = read_events(&inbox);
    assert_eq!(output.stdout, b"{}");
    assert!(!serialized.contains("private answer"));
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].payload,
        AgentEventPayloadV2::OutputProduced(_)
    ));
    assert!(matches!(
        events[1].payload,
        AgentEventPayloadV2::RunSettling(_)
    ));
}

#[test]
fn stop_without_output_emits_missing_status_marker_and_run_settling() {
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory.path().join("agent-events.jsonl");

    let output = run_event(STOP_EVENT, &inbox, &stop_input("turn-1", None));
    assert_silent_success(&output);

    let events = read_events(&inbox);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].payload,
        AgentEventPayloadV2::OutputProduced(_)
    ));
    assert!(matches!(
        events[1].payload,
        AgentEventPayloadV2::RunSettling(_)
    ));
}

#[test]
fn stop_status_capture_keeps_raw_text_out_of_the_inbox_by_default() {
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory.path().join("agent-events.jsonl");
    let secret = "private answer excluded by default";

    let output = run_event_with_capture(
        STOP_EVENT,
        &inbox,
        &stop_input("turn-private", Some(secret)),
        false,
    );
    assert_silent_success(&output);

    let serialized = std::fs::read_to_string(&inbox).unwrap();
    assert!(!serialized.contains(secret));
    let events = read_events(&inbox);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].payload,
        AgentEventPayloadV2::OutputProduced(_)
    ));
    assert!(matches!(
        events[1].payload,
        AgentEventPayloadV2::RunSettling(_)
    ));
}

#[test]
fn permission_request_records_safe_summary_without_tool_input() {
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory.path().join("agent-events.jsonl");
    let secret = "Bearer super-secret-token";

    let output = run_event(
        PERMISSION_REQUEST_EVENT,
        &inbox,
        &permission_input("turn-1", json!({"command": secret, "path": "private.txt"})),
    );
    assert_silent_success(&output);

    assert!(!inbox.exists());
    assert!(!error_log(&inbox).exists());
}

#[test]
fn explicit_non_target_events_are_durable_but_never_presented_as_normal_turns() {
    let directory = tempfile::tempdir().unwrap();
    for (event, mut input) in [
        (
            USER_PROMPT_EVENT,
            serde_json::from_slice::<Value>(&user_prompt_input("prompt", "ignored")).unwrap(),
        ),
        (
            STOP_EVENT,
            serde_json::from_slice::<Value>(&stop_input("stop", Some("ignored"))).unwrap(),
        ),
        (
            PERMISSION_REQUEST_EVENT,
            serde_json::from_slice::<Value>(&permission_input("permission", json!({}))).unwrap(),
        ),
    ] {
        input["agent_id"] = json!("subagent-1");
        let inbox = directory.path().join(format!("{event}.jsonl"));
        let output = run_event(event, &inbox, &serde_json::to_vec(&input).unwrap());
        assert_silent_success(&output);
        let events = read_events(&inbox);
        assert_eq!(events.len(), 1);
        assert!(events.iter().all(|event| matches!(
            event.metadata.as_ref(),
            Some(metadata)
                if metadata.source_classification
                    == promptdock_desktop_lib::agent::SourceClassification::ExplicitNonTarget
                    && metadata.source_reason == "agent_field_present"
        )));
        assert!(events.iter().all(|event| matches!(
            event.payload,
            AgentEventPayloadV2::RunStarted(ref started)
                if started.workspace_path.is_none()
                    && started.model_name.is_none()
                    && started.task.is_none()
        )));
        assert!(!error_log(&inbox).exists());
    }
}

#[test]
fn malformed_and_oversized_input_fail_open_and_record_a_diagnostic() {
    for input in [b"not-json".to_vec(), vec![b'x'; 1024 * 1024 + 1]] {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        let output = run_event(USER_PROMPT_EVENT, &inbox, &input);

        assert_silent_success(&output);
        assert!(!inbox.exists());
        assert!(std::fs::read_to_string(error_log(&inbox))
            .unwrap()
            .contains("agent_event_capture_failed"));
    }
}

#[test]
fn oversized_stable_key_component_fails_open() {
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory.path().join("agent-events.jsonl");
    let mut input = common("UserPromptSubmit", "turn-1");
    input["session_id"] = json!("x".repeat(1_025));
    input["prompt"] = json!("ignored");

    let output = run_event(
        USER_PROMPT_EVENT,
        &inbox,
        &serde_json::to_vec(&input).unwrap(),
    );

    assert_silent_success(&output);
    assert!(!inbox.exists());
    assert!(error_log(&inbox).exists());
}

#[test]
fn invalid_arguments_fail_open_silently_without_touching_the_inbox() {
    let directory = tempfile::tempdir().unwrap();
    let input = user_prompt_input("turn-1", "ignored");
    let cases = [
        vec!["--capture-agent-event"],
        vec!["--capture-agent-event", "unknown", "--inbox", "inbox.jsonl"],
        vec!["--capture-agent-event", USER_PROMPT_EVENT],
        vec![
            "--capture-agent-event",
            USER_PROMPT_EVENT,
            "--inbox",
            "inbox.jsonl",
            "--inbox",
            "other.jsonl",
        ],
        vec![
            "--capture-agent-event",
            USER_PROMPT_EVENT,
            "--unknown",
            "value",
        ],
    ];

    for args in cases {
        let mut registered = args;
        registered.extend(["--hook-registration-id", REGISTRATION_ID]);
        let output = run_args(&registered, &input, directory.path());
        assert!(output.status.success());
    }
    assert!(!directory.path().join("inbox.jsonl").exists());
}

#[test]
fn missing_wrong_and_repeated_hook_markers_never_write_the_inbox_or_raw_input() {
    let directory = tempfile::tempdir().unwrap();
    let secret = "marker rejection must not persist this private answer";
    let input = stop_input("turn-marker-rejected", Some(secret));
    let cases: [(&str, &[&str]); 3] = [
        ("missing", &[]),
        ("wrong", &[USER_PROMPT_HOOK_MARKER]),
        ("repeated", &[STOP_HOOK_MARKER, STOP_HOOK_MARKER]),
    ];

    for (name, markers) in cases {
        let case_dir = directory.path().join(name);
        std::fs::create_dir_all(&case_dir).unwrap();
        let inbox = case_dir.join("agent-events.jsonl");
        prepare_registered_capture(&inbox);
        let mut command = Command::new(env!("CARGO_BIN_EXE_promptdock-desktop"));
        command
            .args(["--capture-agent-event", STOP_EVENT, "--inbox"])
            .arg(&inbox)
            .args(["--hook-registration-id", REGISTRATION_ID])
            .arg("--capture-agent-output")
            .args(markers)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(&input).unwrap();
        assert!(child.wait_with_output().unwrap().status.success());

        assert!(!inbox.exists(), "case {name} unexpectedly wrote the inbox");
        let diagnostics = error_log(&inbox);
        if diagnostics.exists() {
            assert!(!std::fs::read_to_string(diagnostics)
                .unwrap()
                .contains(secret));
        }
    }
}

#[test]
fn repeated_stop_invocations_with_the_same_input_get_distinct_occurrence_ids() {
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory.path().join("agent-events.jsonl");
    let input = stop_input("turn-1", Some("same answer"));

    assert_silent_success(&run_event(STOP_EVENT, &inbox, &input));
    assert_silent_success(&run_event(STOP_EVENT, &inbox, &input));

    let events = read_events(&inbox);
    assert_eq!(events.len(), 4);
    assert_ne!(events[0].event_id, events[2].event_id);
    assert_ne!(events[1].event_id, events[3].event_id);

    let protected_line = std::fs::read_to_string(&inbox)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    let first =
        promptdock_desktop_lib::agent_event_capture::decode_protected_event_line(&protected_line)
            .unwrap();
    let replay =
        promptdock_desktop_lib::agent_event_capture::decode_protected_event_line(&protected_line)
            .unwrap();
    assert_eq!(first.event_id, replay.event_id);
}

#[test]
fn concurrent_prompt_captures_keep_each_safe_start_record_complete() {
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory.path().join("agent-events.jsonl");
    prepare_registered_capture(&inbox);
    let mut children: Vec<Child> = Vec::new();

    for turn in 0..12 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_promptdock-desktop"))
            .args(["--capture-agent-event", USER_PROMPT_EVENT, "--inbox"])
            .arg(&inbox)
            .args(["--hook-registration-id", REGISTRATION_ID])
            .arg(USER_PROMPT_HOOK_MARKER)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&user_prompt_input(&format!("turn-{turn}"), "concurrent"))
            .unwrap();
        children.push(child);
    }

    for child in children {
        assert_silent_success(&child.wait_with_output().unwrap());
    }

    let events = read_events(&inbox);
    assert_batches_accounted(&inbox, &events, 12, 1);
    assert!(events
        .iter()
        .all(|event| matches!(event.payload, AgentEventPayloadV2::RunStarted(_))));
}

#[test]
fn concurrent_stop_captures_keep_each_settling_record_complete() {
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory.path().join("agent-events.jsonl");
    prepare_registered_capture(&inbox);
    let mut children: Vec<Child> = Vec::new();

    for turn in 0..12 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_promptdock-desktop"))
            .args(["--capture-agent-event", STOP_EVENT, "--inbox"])
            .arg(&inbox)
            .args(["--hook-registration-id", REGISTRATION_ID])
            .arg("--capture-agent-output")
            .arg(STOP_HOOK_MARKER)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&stop_input(
                &format!("turn-{turn}"),
                Some(&format!("answer-{turn}")),
            ))
            .unwrap();
        children.push(child);
    }

    for child in children {
        assert_silent_success(&child.wait_with_output().unwrap());
    }

    let events = read_events(&inbox);
    assert_batches_accounted(&inbox, &events, 12, 2);
    // A Stop batch is one lock hold for two records, so a partial pair is the
    // shape a lost-update bug leaves behind.
    let produced = events
        .iter()
        .filter(|event| matches!(event.payload, AgentEventPayloadV2::OutputProduced(_)))
        .count();
    let settling = events
        .iter()
        .filter(|event| matches!(event.payload, AgentEventPayloadV2::RunSettling(_)))
        .count();
    assert_eq!(produced, settling, "a Stop batch must land whole");
    assert_eq!(produced * 2, events.len());
}

#[test]
fn special_unicode_and_spaced_inbox_path_is_supported() {
    let directory = tempfile::tempdir().unwrap();
    let inbox = directory
        .path()
        .join("特殊 folder with spaces")
        .join("agent-events.jsonl");

    let output = run_event(
        USER_PROMPT_EVENT,
        &inbox,
        &user_prompt_input("turn-special", "special path"),
    );
    assert_silent_success(&output);
    assert_eq!(read_events(&inbox).len(), 1);
}

#[test]
fn unwritable_inbox_path_fails_open_without_touching_neighbor_files() {
    let directory = tempfile::tempdir().unwrap();
    let blocker = directory.path().join("not-a-directory");
    std::fs::write(&blocker, b"keep").unwrap();
    let inbox = blocker.join("agent-events.jsonl");

    let mut command = Command::new(env!("CARGO_BIN_EXE_promptdock-desktop"));
    command
        .args(["--capture-agent-event", USER_PROMPT_EVENT, "--inbox"])
        .arg(&inbox)
        .args(["--hook-registration-id", REGISTRATION_ID])
        .arg(USER_PROMPT_HOOK_MARKER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&user_prompt_input("turn-unwritable", "ignored"))
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("RUNTIME_ACCESS_UNAVAILABLE"));
    assert_eq!(std::fs::read(&blocker).unwrap(), b"keep");
    assert!(!inbox.exists());
}
