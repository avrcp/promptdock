//! Bounded local verification. A nonce correlates a user-selected workflow, not a host signature.
use crate::adapters::codex::hooks::CodexHookEvent;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerificationView {
    pub state: VerificationState,
    pub verification_id: Option<String>,
    pub validated_at: Option<i64>,
    pub validated_definition_fingerprint: Option<String>,
    pub host_attribution: String,
    pub instruction: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VerificationState {
    #[default]
    NotStarted,
    WaitingSubmit,
    SubmitSeen,
    Paired,
    TimedOut,
    Invalidated,
    Inconclusive,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Session {
    id: String,
    nonce: String,
    registration: String,
    fingerprint: String,
    build: String,
    helper_build: String,
    scope: String,
    expires_at: i64,
    sequence: u64,
    watermark: u64,
    session: Option<String>,
    turn: Option<String>,
    state: VerificationState,
    validated_at: Option<i64>,
}
const FILE: &str = "hook-verification.json";
const DEFINITION_FILE: &str = "hook-definition.json";
const DEFINITION_SCHEMA_VERSION: u16 = 1;
const RECOGNITION_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
const INSTRUCTION_PREFIX: &str =
    "这是 PromptDock 通知验证。不要修改文件，也不要调用外部服务；请只回复：";

fn verification_instruction(nonce: &str) -> String {
    format!("{INSTRUCTION_PREFIX}{nonce}")
}

fn nonce_token(value: &str) -> Option<&str> {
    let value = value.trim();
    let hex = value.strip_prefix("PDVERIFY_")?;
    (hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(value)
}

fn instruction_nonce(value: &str) -> Option<&str> {
    nonce_token(value.trim().strip_prefix(INSTRUCTION_PREFIX)?.trim())
}

fn matches_instruction(value: &str, nonce: &str) -> bool {
    instruction_nonce(value) == Some(nonce)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DefinitionEvidence {
    schema_version: u16,
    registration_id: String,
    definition_fingerprint: String,
}

/// A read-only capture classification. State changes happen only after the
/// corresponding Hook input has been durably appended to the inbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureVerification {
    Normal,
    Verification,
    RejectedUnavailable,
}

impl CaptureVerification {
    pub(crate) fn is_verification(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Observation {
    pub observed_at: i64,
    pub registration_id: String,
    pub definition_fingerprint: Option<String>,
    pub helper_build: Option<String>,
}
fn observation_file(event: &str) -> Option<&'static str> {
    match event {
        "UserPromptSubmit" => Some("hook-observed-submit.json"),
        "Stop" => Some("hook-observed-stop.json"),
        "PermissionRequest" => Some("hook-observed-permission.json"),
        _ => None,
    }
}
pub(crate) fn last_observation(dir: &Path, event: &str) -> Option<Observation> {
    let bytes = crate::hook_health::bounded_read(&dir.join(observation_file(event)?)).ok()?;
    serde_json::from_slice(&bytes).ok()
}
pub(crate) fn record_observation(dir: &Path, event: CodexHookEvent, registration: &str) {
    let name = match event {
        CodexHookEvent::UserPrompt => "UserPromptSubmit",
        CodexHookEvent::Stop => "Stop",
        CodexHookEvent::PermissionRequest => "PermissionRequest",
    };
    let Some(file) = observation_file(name) else {
        return;
    };
    let observation = Observation {
        observed_at: crate::db::now_ms().unwrap_or(0),
        registration_id: registration.into(),
        definition_fingerprint: current_definition(dir).map(|e| e.definition_fingerprint),
        helper_build: build_identity(),
    };
    if let Ok(bytes) = serde_json::to_vec(&observation) {
        let _ = crate::atomic_file::write(
            &dir.join(file),
            &bytes,
            "HOOK_OBSERVATION_WRITE",
            "无法记录 Hook 观测",
        );
    }
}
pub(crate) fn build_identity() -> Option<String> {
    let commit = env!("PROMPTDOCK_BUILD_COMMIT");
    Some(format!(
        "promptdock_desktop_{}_{}",
        env!("CARGO_PKG_VERSION").replace('.', "_"),
        commit
    ))
}
fn file_identity(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > 256 * 1024 * 1024 {
        return None;
    }
    let mut reader = file.take(256 * 1024 * 1024 + 1);
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    let mut total = 0;
    loop {
        let count = reader.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        total += count;
        if total > 256 * 1024 * 1024 {
            return None;
        }
        hash.update(&buffer[..count]);
    }
    Some(format!("sha256:{:x}", hash.finalize()))
}
/// Installation-owned projection of the normalized managed definition. This
/// is not a host trust grant: it lets a short-lived helper compare the current
/// registration/definition without scanning unrelated Hook entries or hashing
/// the executable.
pub(crate) fn write_definition_evidence(
    dir: &Path,
    registration: &str,
    fingerprint: &str,
) -> Result<(), String> {
    if registration.is_empty()
        || registration.len() > 128
        || fingerprint.is_empty()
        || fingerprint.len() > 128
    {
        return Err("HOOK_DEFINITION_INVALID".into());
    }
    let bytes = serde_json::to_vec(&DefinitionEvidence {
        schema_version: DEFINITION_SCHEMA_VERSION,
        registration_id: registration.into(),
        definition_fingerprint: fingerprint.into(),
    })
    .map_err(|_| "HOOK_DEFINITION_ENCODE")?;
    crate::atomic_file::write(
        &dir.join(DEFINITION_FILE),
        &bytes,
        "HOOK_DEFINITION_WRITE",
        "无法保存 Hook 定义证据",
    )
    .map_err(|error| error.message)
}

fn current_definition(dir: &Path) -> Option<DefinitionEvidence> {
    let bytes = crate::hook_health::bounded_read(&dir.join(DEFINITION_FILE)).ok()?;
    let evidence: DefinitionEvidence = serde_json::from_slice(&bytes).ok()?;
    (evidence.schema_version == DEFINITION_SCHEMA_VERSION
        && !evidence.registration_id.is_empty()
        && evidence.registration_id.len() <= 128
        && !evidence.definition_fingerprint.is_empty()
        && evidence.definition_fingerprint.len() <= 128)
        .then_some(evidence)
}

fn current_scope(dir: &Path) -> Option<String> {
    let target = crate::hook_health::bounded_read(&dir.join("hook-target.json")).ok()?;
    // The launcher and proxy settings are unrelated to the managed Hook
    // definition. Full-file fingerprints stay in installer CAS only.
    let path: std::path::PathBuf = serde_json::from_slice(&target).ok()?;
    Some(crate::hook_health::fingerprint(
        path.to_string_lossy()
            .replace('\\', "/")
            .to_lowercase()
            .as_bytes(),
    ))
}
fn load(dir: &Path) -> Option<Session> {
    let bytes = crate::hook_health::bounded_read(&dir.join(FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}
fn save(dir: &Path, session: &Session) -> Result<(), String> {
    let bytes = serde_json::to_vec(session).map_err(|_| "VERIFICATION_ENCODE")?;
    crate::atomic_file::write(
        &dir.join(FILE),
        &bytes,
        "VERIFICATION_WRITE",
        "无法保存验证状态",
    )
    .map_err(|e| e.message)
}
fn lock(dir: &Path) -> Result<std::fs::File, String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("hook-verification.lock"))
        .map_err(|_| "VERIFICATION_LOCK")?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|_| "VERIFICATION_BUSY")?;
    Ok(file)
}
pub(crate) fn begin(
    dir: &Path,
    registration: &str,
    fingerprint: &str,
) -> Result<VerificationView, String> {
    let _lock = lock(dir)?;
    let seq = load(dir).map(|s| s.sequence).unwrap_or(0);
    let session = Session {
        id: uuid::Uuid::new_v4().to_string(),
        nonce: format!("PDVERIFY_{}", uuid::Uuid::new_v4().simple()),
        registration: registration.into(),
        fingerprint: fingerprint.into(),
        build: std::env::current_exe()
            .ok()
            .and_then(|path| file_identity(&path))
            .ok_or("HELPER_BUILD_UNREADABLE")?,
        helper_build: build_identity().ok_or("HELPER_BUILD_UNREADABLE")?,
        scope: current_scope(dir).ok_or("HOOK_SCOPE_UNREADABLE")?,
        expires_at: crate::db::now_ms().unwrap_or(0) + 120_000,
        sequence: seq,
        watermark: seq,
        session: None,
        turn: None,
        state: VerificationState::WaitingSubmit,
        validated_at: None,
    };
    save(dir, &session)?;
    Ok(view(&session))
}
fn view(s: &Session) -> VerificationView {
    VerificationView {
        state: s.state.clone(),
        verification_id: Some(s.id.clone()),
        validated_at: s.validated_at,
        validated_definition_fingerprint: s.validated_at.map(|_| s.fingerprint.clone()),
        host_attribution: "shared_scope_unknown".into(),
        instruction: matches!(
            s.state,
            VerificationState::WaitingSubmit | VerificationState::SubmitSeen
        )
        .then(|| verification_instruction(&s.nonce)),
    }
}
pub(crate) fn status(
    dir: &Path,
    registration: Option<&str>,
    fingerprint: Option<&str>,
) -> VerificationView {
    let Some(mut s) = load(dir) else {
        return Default::default();
    };
    if Some(s.registration.as_str()) != registration
        || Some(s.fingerprint.as_str()) != fingerprint
        || Some(s.scope.clone()) != current_scope(dir)
        || Some(s.helper_build.clone()) != build_identity()
    {
        s.state = VerificationState::Invalidated;
    } else if matches!(
        s.state,
        VerificationState::WaitingSubmit | VerificationState::SubmitSeen
    ) && crate::db::now_ms().unwrap_or(0) >= s.expires_at
    {
        s.state = VerificationState::TimedOut;
    }
    view(&s)
}
fn accept(
    s: &mut Session,
    event: CodexHookEvent,
    input: &serde_json::Value,
    registration: &str,
    now: i64,
) -> bool {
    if s.registration != registration
        || now >= s.expires_at
        || !matches!(
            s.state,
            VerificationState::WaitingSubmit
                | VerificationState::SubmitSeen
                | VerificationState::Paired
        )
    {
        return false;
    }
    if input.get("agent_id").is_some_and(|v| !v.is_null())
        || input.get("agent_type").is_some_and(|v| !v.is_null())
    {
        return false;
    }
    let (Some(session), Some(turn)) = (input["session_id"].as_str(), input["turn_id"].as_str())
    else {
        return false;
    };
    if session.is_empty() || turn.is_empty() {
        return false;
    }
    let Some(next) = s.sequence.checked_add(1) else {
        s.state = VerificationState::Inconclusive;
        return false;
    };
    s.sequence = next;
    if s.sequence <= s.watermark {
        return false;
    }
    if event == CodexHookEvent::UserPrompt
        && input["prompt"]
            .as_str()
            .is_some_and(|p| matches_instruction(p, &s.nonce))
    {
        if s.state != VerificationState::WaitingSubmit {
            return s.session.as_deref() == Some(session) && s.turn.as_deref() == Some(turn);
        }
        s.session = Some(session.into());
        s.turn = Some(turn.into());
        s.state = VerificationState::SubmitSeen;
        return true;
    }
    if event == CodexHookEvent::Stop
        && s.session.as_deref() == Some(session)
        && s.turn.as_deref() == Some(turn)
        && matches!(
            s.state,
            VerificationState::SubmitSeen | VerificationState::Paired
        )
    {
        s.state = VerificationState::Paired;
        s.validated_at.get_or_insert(now);
        return true;
    }
    false
}
/// Classify a candidate without taking the state lock or mutating files. A
/// nonce-shaped submit and its paired Stop cannot fall back to a normal
/// notification just because verification state is unavailable.
pub(crate) fn classify(
    dir: &Path,
    _registration: &str,
    event: CodexHookEvent,
    bytes: &[u8],
) -> CaptureVerification {
    let Ok(input) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return CaptureVerification::Normal;
    };
    let nonce_shaped = input
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .and_then(instruction_nonce)
        .is_some();
    let exact_reply = event == CodexHookEvent::Stop
        && input
            .get("last_assistant_message")
            .and_then(serde_json::Value::as_str)
            .and_then(nonce_token)
            .is_some();
    let Some(s) = load(dir) else {
        return if nonce_shaped || exact_reply {
            CaptureVerification::RejectedUnavailable
        } else {
            CaptureVerification::Normal
        };
    };
    let paired_turn = s.session.is_some()
        && s.turn.is_some()
        && crate::db::now_ms().unwrap_or(i64::MAX)
            <= s.expires_at.saturating_add(RECOGNITION_RETENTION_MS)
        && input["session_id"].as_str() == s.session.as_deref()
        && input["turn_id"].as_str() == s.turn.as_deref();
    let exact_instruction = event == CodexHookEvent::UserPrompt
        && input["prompt"]
            .as_str()
            .is_some_and(|value| matches_instruction(value, &s.nonce));
    if nonce_shaped || paired_turn || exact_instruction || exact_reply {
        CaptureVerification::Verification
    } else {
        CaptureVerification::Normal
    }
}

/// Called only after a durable capture. No raw text is persisted here.
pub(crate) fn commit_classification(
    dir: &Path,
    registration: &str,
    event: CodexHookEvent,
    bytes: &[u8],
    classification: CaptureVerification,
) {
    if !classification.is_verification() {
        return;
    }
    let Ok(_lock) = lock(dir) else { return };
    let Some(mut s) = load(dir) else { return };
    if Some(s.scope.clone()) != current_scope(dir) {
        s.state = VerificationState::Invalidated;
        let _ = save(dir, &s);
        return;
    }
    if Some(s.helper_build.clone()) != build_identity() {
        return;
    }
    let current_registration =
        crate::hook_health::bounded_read(&dir.join("hook-registration.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<String>(&b).ok());
    let definition = current_definition(dir);
    if current_registration.as_deref() != Some(registration)
        || definition.as_ref().is_none_or(|e| {
            e.registration_id != registration || e.definition_fingerprint != s.fingerprint
        })
    {
        s.state = VerificationState::Invalidated;
        let _ = save(dir, &s);
        return;
    }
    let Ok(input) = serde_json::from_slice(bytes) else {
        return;
    };
    let matched = accept(
        &mut s,
        event,
        &input,
        registration,
        crate::db::now_ms().unwrap_or(0),
    );
    if matched {
        let _ = save(dir, &s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("hook-target.json"),
            serde_json::to_vec(&dir.path().join("codex")).unwrap(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn marker_mentions_are_normal_but_bounded_verification_instructions_are_not() {
        let dir = fixture();
        let nonce = "PDVERIFY_0123456789abcdef0123456789abcdef";
        let normal = serde_json::json!({"prompt":format!("请解释 {nonce} 的实现")});
        assert_eq!(
            classify(
                dir.path(),
                "r",
                CodexHookEvent::UserPrompt,
                &serde_json::to_vec(&normal).unwrap()
            ),
            CaptureVerification::Normal
        );
        let verification =
            serde_json::json!({"prompt":format!("{}\r\n",verification_instruction(nonce))});
        assert_eq!(
            classify(
                dir.path(),
                "r",
                CodexHookEvent::UserPrompt,
                &serde_json::to_vec(&verification).unwrap()
            ),
            CaptureVerification::RejectedUnavailable
        );
        let late_stop = serde_json::json!({"last_assistant_message":nonce});
        assert_eq!(
            classify(
                dir.path(),
                "r",
                CodexHookEvent::Stop,
                &serde_json::to_vec(&late_stop).unwrap()
            ),
            CaptureVerification::RejectedUnavailable
        );
    }
    #[test]
    fn verification_build_identity_detects_replacement_but_launcher_change_does_not_invalidate() {
        let d = fixture();
        let path = d.path().join("helper");
        std::fs::write(&path, b"aaaa").unwrap();
        let first = file_identity(&path).unwrap();
        std::fs::write(&path, b"bbbb").unwrap();
        assert_ne!(file_identity(&path).unwrap(), first);
        begin(d.path(), "r", "f").unwrap();
        std::fs::write(d.path().join("launcher.json"), b"new host selection").unwrap();
        assert_eq!(
            status(d.path(), Some("r"), Some("f")).state,
            VerificationState::WaitingSubmit
        );
    }
    #[test]
    fn only_current_root_submit_then_stop_pairs_and_expiry_invalidates() {
        let d = fixture();
        begin(d.path(), "new", "hash").unwrap();
        let mut s = load(d.path()).unwrap();
        let now = s.expires_at - 1;
        let prompt = serde_json::json!({"session_id":"s","turn_id":"t","prompt":view(&s).instruction.unwrap()});
        let stop = serde_json::json!({"session_id":"s","turn_id":"t"});
        assert!(!accept(&mut s, CodexHookEvent::Stop, &stop, "new", now));
        assert!(!accept(
            &mut s,
            CodexHookEvent::UserPrompt,
            &prompt,
            "old",
            now
        ));
        let mut child = prompt.clone();
        child["agent_id"] = "child".into();
        assert!(!accept(
            &mut s,
            CodexHookEvent::UserPrompt,
            &child,
            "new",
            now
        ));
        assert!(accept(
            &mut s,
            CodexHookEvent::UserPrompt,
            &prompt,
            "new",
            now
        ));
        let mut other = stop.clone();
        other["turn_id"] = "other".into();
        assert!(!accept(&mut s, CodexHookEvent::Stop, &other, "new", now));
        assert!(accept(&mut s, CodexHookEvent::Stop, &stop, "new", now));
        assert_eq!(s.state, VerificationState::Paired);
        assert!(!accept(&mut s, CodexHookEvent::Stop, &stop, "new", now + 1));
        save(d.path(), &s).unwrap();
        assert_eq!(
            status(d.path(), Some("changed"), Some("hash")).state,
            VerificationState::Invalidated
        );
        let text = std::fs::read_to_string(d.path().join(FILE)).unwrap();
        assert!(!text.contains("请只回复"));
    }

    #[test]
    fn nonce_shaped_input_is_suppressed_when_verification_state_is_unavailable() {
        let d = fixture();
        std::fs::write(d.path().join(FILE), b"not json").unwrap();
        let input = serde_json::to_vec(&serde_json::json!({
            "session_id":"opaque-session", "turn_id":"opaque-turn",
            "prompt": verification_instruction("PDVERIFY_0123456789abcdef0123456789abcdef")
        }))
        .unwrap();
        assert_eq!(
            classify(d.path(), "registration", CodexHookEvent::UserPrompt, &input),
            CaptureVerification::RejectedUnavailable
        );
    }

    #[test]
    fn paired_stop_remains_a_verification_event_after_expiry() {
        let d = fixture();
        begin(d.path(), "registration", "semantic-definition").unwrap();
        let mut session = load(d.path()).unwrap();
        session.session = Some("opaque-session".into());
        session.turn = Some("opaque-turn".into());
        session.state = VerificationState::SubmitSeen;
        session.expires_at = crate::db::now_ms().unwrap() - 1;
        save(d.path(), &session).unwrap();
        let input = br#"{"session_id":"opaque-session","turn_id":"opaque-turn"}"#;
        assert_eq!(
            classify(d.path(), "registration", CodexHookEvent::Stop, input),
            CaptureVerification::Verification
        );
    }

    #[test]
    fn definition_evidence_is_bounded_and_observation_uses_its_semantic_fingerprint() {
        let d = fixture();
        write_definition_evidence(d.path(), "registration", "semantic-fingerprint").unwrap();
        record_observation(d.path(), CodexHookEvent::Stop, "registration");
        let observation = last_observation(d.path(), "Stop").unwrap();
        assert_eq!(
            observation.definition_fingerprint.as_deref(),
            Some("semantic-fingerprint")
        );
        assert!(write_definition_evidence(d.path(), "", "semantic-fingerprint").is_err());
    }
}
