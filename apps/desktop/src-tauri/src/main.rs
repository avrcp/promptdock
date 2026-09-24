#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(clippy::print_stderr, clippy::print_stdout)]

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use promptdock_desktop_lib::adapters::codex::hooks::CodexHookEvent;
use promptdock_desktop_lib::agent_event_capture::{record_capture_error, AgentEventCaptureError};
use promptdock_desktop_lib::hook_installer::{
    PERMISSION_REQUEST_HOOK_MARKER, STOP_HOOK_MARKER, USER_PROMPT_HOOK_MARKER,
};

fn main() -> std::process::ExitCode {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref() {
        Some(mode) if mode == OsStr::new("--capture-agent-event") => {
            let args: Vec<OsString> = args.collect();
            let is_stop = args.iter().any(|arg| arg == OsStr::new("codex-stop"));
            run_agent_event_capture(args);
            if is_stop {
                // This is the host's neutral Hook response. Durable reception
                // is recorded only by a successful inbox append, never by this
                // syntactic response or the process exit status.
                use std::io::Write;
                let _ = std::io::stdout().write_all(b"{}");
                let _ = std::io::stdout().flush();
            }
        }
        Some(mode) if mode == OsStr::new("--self-test-desktop-startup") => {
            let scenario = args.next();
            let cleanup_token = args.next();
            if args.next().is_some()
                || !scenario
                    .as_deref()
                    .and_then(OsStr::to_str)
                    .zip(cleanup_token.as_deref().and_then(OsStr::to_str))
                    .is_some_and(|(scenario, token)| {
                        promptdock_desktop_lib::self_test_desktop_startup(scenario, token)
                    })
            {
                return std::process::ExitCode::FAILURE;
            }
        }
        _ => {
            if !promptdock_desktop_lib::run() {
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    std::process::ExitCode::SUCCESS
}

fn run_agent_event_capture(mut args: Vec<OsString>) {
    let registration = if let Some(index) = args.iter().position(|a| a == "--hook-registration-id")
    {
        let Some(value) = args
            .get(index + 1)
            .and_then(|a| a.to_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
        else {
            report_capture_error(
                "HOOK_REGISTRATION_INVALID",
                "missing or invalid registration id",
            );
            return;
        };
        args.drain(index..index + 2);
        Some(value.to_string())
    } else {
        None
    };
    if registration.is_none() {
        report_capture_error(
            "HOOK_REGISTRATION_MISSING",
            "registered capture command required",
        );
        return;
    }
    let (event, inbox, _legacy_capture_flag) = match parse_capture_args(&args) {
        Ok(parsed) => parsed,
        Err(message) => {
            report_capture_error("CAPTURE_ARGUMENTS_INVALID", message);
            return;
        }
    };
    let Some(data_dir) = inbox.parent() else {
        report_capture_error("CAPTURE_INBOX_PATH_INVALID", "missing data directory");
        return;
    };
    // The helper holds the same compatible Windows share handle as the GUI for
    // the whole read/append operation. Reset uses exclusive access and cannot
    // delete the inbox between validation and append.
    let _runtime_access =
        match promptdock_desktop_lib::runtime_access::RuntimeAccessGuard::acquire(data_dir) {
            Ok(guard) => guard,
            Err(error) => {
                // Do not append an error beside data while reset owns exclusive access.
                report_capture_error(error.code, "");
                return;
            }
        };
    // Acquire before waiting for stdin: a helper that starts during reset may
    // not outlive the exclusive interval and then append to the new dataset.
    // The input timeout still precedes any inbox/diagnostic or policy write.
    let bytes = match read_capture_stdin(std::io::stdin(), Duration::from_millis(4_000)) {
        Ok(bytes) => bytes,
        Err(error) => {
            report_capture_error(error.code(), "");
            return;
        }
    };
    let result = promptdock_desktop_lib::desktop_capture::capture_registered(
        event,
        &bytes,
        &inbox,
        registration.as_deref(),
    );
    if let Err(error) = result {
        record_capture_error(&inbox, &error);
    }
}

fn read_capture_stdin(
    reader: impl Read + Send + 'static,
    timeout: Duration,
) -> Result<Vec<u8>, AgentEventCaptureError> {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = reader
            .take(promptdock_desktop_lib::agent_event_capture::MAX_AGENT_EVENT_HOOK_INPUT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| AgentEventCaptureError::InvalidInput("CAPTURE_STDIN_READ_FAILED".into()))
            .and({
                if bytes.len() as u64
                    > promptdock_desktop_lib::agent_event_capture::MAX_AGENT_EVENT_HOOK_INPUT_BYTES
                {
                    Err(AgentEventCaptureError::InputTooLarge)
                } else {
                    Ok(bytes)
                }
            });
        let _ = send.send(result);
    });
    receive
        .recv_timeout(timeout)
        .map_err(|_| AgentEventCaptureError::InvalidInput("CAPTURE_STDIN_TIMEOUT".into()))?
}

fn report_capture_error(code: &str, detail: &str) {
    use std::io::Write;
    // Details can contain operating-system paths or user-supplied content.
    // The host only needs a stable operational code to diagnose the failure.
    let _ = detail;
    let _ = std::io::stderr().write_all(format!("{code}\n").as_bytes());
}

fn parse_capture_args(args: &[OsString]) -> Result<(CodexHookEvent, PathBuf, bool), &'static str> {
    let event_name = args
        .first()
        .and_then(|value| value.to_str())
        .ok_or("missing Codex Hook event")?;
    let event = CodexHookEvent::parse(event_name).ok_or("unsupported Codex Hook event")?;
    let expected_marker = match event {
        CodexHookEvent::UserPrompt => USER_PROMPT_HOOK_MARKER,
        CodexHookEvent::Stop => STOP_HOOK_MARKER,
        CodexHookEvent::PermissionRequest => PERMISSION_REQUEST_HOOK_MARKER,
    };

    let mut inbox = None;
    let mut marker_seen = false;
    let mut capture_agent_output = false;
    let mut index = 1;
    while index < args.len() {
        let argument = &args[index];
        if argument == OsStr::new(expected_marker) {
            if marker_seen {
                return Err("duplicate Codex Hook marker");
            }
            marker_seen = true;
            index += 1;
            continue;
        }
        if argument == OsStr::new("--capture-agent-output") {
            if capture_agent_output || event != CodexHookEvent::Stop {
                return Err("invalid agent output capture flag");
            }
            capture_agent_output = true;
            index += 1;
            continue;
        }
        let Some(path) = args.get(index + 1) else {
            return Err("invalid agent event capture arguments");
        };
        if argument != OsStr::new("--inbox") || inbox.is_some() || is_flag_like(path) {
            return Err("invalid agent event capture arguments");
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("agent event inbox must be absolute");
        }
        inbox = Some(path);
        index += 2;
    }

    if !marker_seen {
        return Err("missing Codex Hook marker");
    }
    inbox
        .map(|path| (event, path, capture_agent_output))
        .ok_or("missing agent event inbox")
}

fn is_flag_like(value: &OsStr) -> bool {
    value.to_string_lossy().starts_with("--")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    struct BlockingReader(mpsc::Receiver<()>);

    impl Read for BlockingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            let _ = self.0.recv();
            Ok(0)
        }
    }

    #[test]
    fn open_stdin_times_out_before_capture_can_touch_runtime_files() {
        let (release, blocked) = mpsc::channel();
        let result = read_capture_stdin(BlockingReader(blocked), Duration::from_millis(5));
        assert!(matches!(
            result,
            Err(AgentEventCaptureError::InvalidInput(code)) if code == "CAPTURE_STDIN_TIMEOUT"
        ));
        // Let the detached reader return so the test does not retain it.
        release.send(()).unwrap();
    }

    #[test]
    fn output_capture_is_opt_in_and_only_valid_for_stop() {
        let inbox = std::env::temp_dir().join("promptdock-agent-events.jsonl");
        let stop = vec![
            OsString::from("codex-stop"),
            OsString::from("--inbox"),
            inbox.clone().into_os_string(),
            OsString::from("--capture-agent-output"),
            OsString::from(STOP_HOOK_MARKER),
        ];
        let (_, parsed_inbox, capture) = parse_capture_args(&stop).expect("parse stop args");
        assert_eq!(parsed_inbox, inbox);
        assert!(capture);

        let prompt = vec![
            OsString::from("codex-user-prompt"),
            OsString::from("--inbox"),
            std::env::temp_dir().join("events.jsonl").into_os_string(),
            OsString::from("--capture-agent-output"),
            OsString::from(USER_PROMPT_HOOK_MARKER),
        ];
        assert!(parse_capture_args(&prompt).is_err());
    }

    #[test]
    fn capture_requires_exactly_one_event_specific_hook_marker() {
        let inbox = std::env::temp_dir()
            .join("promptdock-agent-events.jsonl")
            .into_os_string();
        let base = vec![
            OsString::from("codex-stop"),
            OsString::from("--inbox"),
            inbox,
        ];

        assert_eq!(
            parse_capture_args(&base).unwrap_err(),
            "missing Codex Hook marker"
        );

        let mut wrong = base.clone();
        wrong.push(OsString::from(USER_PROMPT_HOOK_MARKER));
        assert!(parse_capture_args(&wrong).is_err());

        let mut repeated = base;
        repeated.push(OsString::from(STOP_HOOK_MARKER));
        repeated.push(OsString::from(STOP_HOOK_MARKER));
        assert_eq!(
            parse_capture_args(&repeated).unwrap_err(),
            "duplicate Codex Hook marker"
        );
    }
}
