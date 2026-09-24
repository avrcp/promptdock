use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tracing_subscriber::fmt::MakeWriter;

use crate::error::AppError;

const LOG_FILE: &str = "promptdock.log";
const MAX_LOG_BYTES: u64 = 256 * 1024;
const MAX_ARCHIVES: u8 = 3;

/// The in-memory diagnostic contract intentionally contains no operator supplied
/// text.  Add a new variant when a new fact is needed instead of accepting a
/// string from a hook, a path, or an error.
#[allow(dead_code)] // Closed integration contract; W4 callers select variants incrementally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Component {
    Capture,
    Policy,
    Storage,
    Runtime,
    Relay,
    Hook,
    Retention,
    Export,
}

#[allow(dead_code)] // Closed integration contract; W4 callers select variants incrementally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    Snapshot,
    Probe,
    Check,
    Append,
    Sync,
    Export,
}

#[allow(dead_code)] // Closed integration contract; W4 callers select variants incrementally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum SafeCode {
    Ok,
    Disabled,
    NotRun,
    Unavailable,
    Timeout,
    Rejected,
    CapacityBounded,
    LockPoisoned,
    ExportCreated,
    ExportExists,
    ExportDirectoryRejected,
    ExportWriteFailed,
    PolicyPending,
    HoldActive,
    HoldUncertain,
    HookMissing,
    TrustUnconfirmed,
    RelayUnconfigured,
    RelayUnreachable,
    RelayUnauthorized,
    ResultCapabilityMissing,
    QueueBlocked,
    ResetPending,
    ManifestUnavailable,
    ManifestMismatch,
}

const MAX_JS_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_DIAGNOSTIC_EVENTS: usize = 256;
const MAX_DIAGNOSTIC_JSON_BYTES: usize = 64 * 1024;
// Reserve room for the snapshot object keys and an always-safe u64 eviction count.
const MAX_RING_EVENT_ARRAY_BYTES: usize = MAX_DIAGNOSTIC_JSON_BYTES - 256;
const MAX_DURATION_MS: u32 = 24 * 60 * 60 * 1_000;
const MAX_COUNT: u32 = 1_000_000;
const HEALTH_REPORT_FILE: &str = "health-report.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiagnosticEvent {
    pub sequence: u64,
    pub component: Component,
    pub stage: Stage,
    pub safe_code: SafeCode,
    pub duration_ms: u32,
    pub count: u32,
}

impl DiagnosticEvent {
    fn new(
        sequence: u64,
        component: Component,
        stage: Stage,
        safe_code: SafeCode,
        duration_ms: u32,
        count: u32,
    ) -> Option<Self> {
        (sequence <= MAX_JS_SAFE_INTEGER && duration_ms <= MAX_DURATION_MS && count <= MAX_COUNT)
            .then_some(Self {
                sequence,
                component,
                stage,
                safe_code,
                duration_ms,
                count,
            })
    }
}

/// Best-effort process-local observations. Recording always fails open so a
/// poisoned lock, serialization issue, or disabled diagnostic feature cannot
/// affect capture, delivery, or storage work.
#[derive(Clone, Default)]
pub(crate) struct DiagnosticRing {
    inner: Option<Arc<Mutex<DiagnosticRingInner>>>,
}

#[derive(Default)]
struct DiagnosticRingInner {
    events: VecDeque<DiagnosticEvent>,
    json_bytes: usize,
    evicted_count: u64,
    next_sequence: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiagnosticRingSnapshot {
    pub recent_safe_diagnostic_events: Vec<DiagnosticEvent>,
    pub evicted_diagnostic_count: u64,
}

impl DiagnosticRing {
    pub(crate) fn enabled() -> Self {
        Self {
            inner: Some(Arc::new(Mutex::new(DiagnosticRingInner::default()))),
        }
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self { inner: None }
    }

    /// Appends one safe fact and assigns its process-local monotonic sequence.
    /// Invalid bounds, a disabled ring, or a poisoned lock are all fail-open.
    pub(crate) fn push(
        &self,
        component: Component,
        stage: Stage,
        safe_code: SafeCode,
        duration_ms: u32,
        count: u32,
    ) {
        let Some(inner) = &self.inner else {
            return;
        };
        let Ok(mut guard) = inner.lock() else {
            return;
        };
        let Some(event) = DiagnosticEvent::new(
            guard.next_sequence,
            component,
            stage,
            safe_code,
            duration_ms,
            count,
        ) else {
            return;
        };
        guard.next_sequence = guard.next_sequence.saturating_add(1);
        record_locked(&mut guard, event);
    }

    pub(crate) fn snapshot(&self) -> DiagnosticRingSnapshot {
        let Some(inner) = &self.inner else {
            return DiagnosticRingSnapshot::default();
        };
        let Ok(guard) = inner.lock() else {
            return DiagnosticRingSnapshot::default();
        };
        DiagnosticRingSnapshot {
            recent_safe_diagnostic_events: guard.events.iter().cloned().collect(),
            evicted_diagnostic_count: guard.evicted_count.min(MAX_JS_SAFE_INTEGER),
        }
    }
}

fn record_locked(guard: &mut DiagnosticRingInner, event: DiagnosticEvent) {
    let Ok(event_bytes) = serde_json::to_vec(&event) else {
        return;
    };
    // An event is only six fixed fields. If this ever stops fitting, keep
    // the business path fail-open rather than exporting a partial payload.
    if event_bytes.len() + 2 > MAX_RING_EVENT_ARRAY_BYTES {
        increment_evicted_count(guard);
        return;
    }
    while guard.events.len() >= MAX_DIAGNOSTIC_EVENTS
        || json_array_len(guard.json_bytes, guard.events.len(), event_bytes.len())
            > MAX_RING_EVENT_ARRAY_BYTES
    {
        let Some(old) = guard.events.pop_front() else {
            break;
        };
        guard.json_bytes = guard.json_bytes.saturating_sub(serialized_event_len(&old));
        increment_evicted_count(guard);
    }
    guard.json_bytes = guard.json_bytes.saturating_add(event_bytes.len());
    guard.events.push_back(event);
}

fn increment_evicted_count(guard: &mut DiagnosticRingInner) {
    guard.evicted_count = guard
        .evicted_count
        .saturating_add(1)
        .min(MAX_JS_SAFE_INTEGER);
}

fn serialized_event_len(event: &DiagnosticEvent) -> usize {
    serde_json::to_vec(event)
        .map(|value| value.len())
        .unwrap_or(0)
}

fn json_array_len(event_bytes: usize, event_count: usize, incoming_bytes: usize) -> usize {
    // `[]` plus one comma between every existing event and the incoming event.
    2usize
        .saturating_add(event_bytes)
        .saturating_add(incoming_bytes)
        .saturating_add(event_count)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReportVersion {
    HealthReportV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContractReference {
    pub result_page_contract: &'static str,
}

impl ContractReference {
    const fn pinned() -> Self {
        Self {
            result_page_contract: "result_pages_v1",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RedactionPolicyVersion {
    DiagnosticsRedactionV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckStatus {
    Pass,
    Fail,
    NotRun,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckResult {
    pub component: Component,
    pub stage: Stage,
    pub status: CheckStatus,
    pub safe_code: SafeCode,
    pub duration_ms: u32,
}

impl CheckResult {
    pub(crate) fn new(
        component: Component,
        stage: Stage,
        status: CheckStatus,
        safe_code: SafeCode,
        duration_ms: u32,
    ) -> Option<Self> {
        (duration_ms <= MAX_DURATION_MS).then_some(Self {
            component,
            stage,
            status,
            safe_code,
            duration_ms,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueueCounts {
    pub pending: u32,
    pub sending: u32,
    pub retry: u32,
    pub blocked: u32,
    pub dead_letter: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetentionPressure {
    pub retained_records: u32,
    pub record_limit: u32,
    pub retained_content_bytes: u64,
    pub content_byte_limit: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HealthReport {
    pub report_version: ReportVersion,
    pub app_build: SafeBuild,
    pub contract_reference: ContractReference,
    pub runtime_epoch: u64,
    pub redaction_policy_version: RedactionPolicyVersion,
    pub check_results: Vec<CheckResult>,
    pub queue_counts: QueueCounts,
    pub retention_pressure: RetentionPressure,
    pub recent_safe_diagnostic_events: Vec<DiagnosticEvent>,
    pub evicted_diagnostic_count: u64,
    pub explicit_not_run: Vec<Component>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct SafeBuild(String);

impl SafeBuild {
    pub(crate) fn parse(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (value.len() <= 96
            && !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
        .then_some(Self(value))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HealthReportInput {
    pub app_build: SafeBuild,
    pub runtime_epoch: u64,
    pub check_results: Vec<CheckResult>,
    pub queue_counts: QueueCounts,
    pub retention_pressure: RetentionPressure,
    pub explicit_not_run: Vec<Component>,
    pub ring_snapshot: DiagnosticRingSnapshot,
}

impl HealthReport {
    /// Creates a report only from caller-provided safe aggregates. It performs
    /// no filesystem, database, credential, or network reads.
    pub(crate) fn from_safe_input(input: HealthReportInput) -> Option<Self> {
        (input.runtime_epoch <= MAX_JS_SAFE_INTEGER
            && input.check_results.len() <= MAX_DIAGNOSTIC_EVENTS
            && input.explicit_not_run.len() <= 32
            && input.ring_snapshot.recent_safe_diagnostic_events.len() <= MAX_DIAGNOSTIC_EVENTS)
            .then_some(Self {
                report_version: ReportVersion::HealthReportV1,
                app_build: input.app_build,
                contract_reference: ContractReference::pinned(),
                runtime_epoch: input.runtime_epoch,
                redaction_policy_version: RedactionPolicyVersion::DiagnosticsRedactionV1,
                check_results: input.check_results,
                queue_counts: input.queue_counts,
                retention_pressure: input.retention_pressure,
                recent_safe_diagnostic_events: input.ring_snapshot.recent_safe_diagnostic_events,
                evicted_diagnostic_count: input.ring_snapshot.evicted_diagnostic_count,
                explicit_not_run: input.explicit_not_run,
            })
    }
}

/// Read-only UI DTO assembled by the desktop integration from safe aggregates.
/// The export path intentionally accepts only its `report` member.
#[allow(dead_code)] // UI integration owns the fixed text and calls this incrementally.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HealthSnapshot {
    pub steps: Vec<HealthStep>,
    pub report: HealthReport,
    pub test: Option<TestProbe>,
}

#[allow(dead_code)] // UI copy is a fixed local contract, never hook or error text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HealthStep {
    pub id: &'static str,
    pub label: &'static str,
    pub status: &'static str,
    pub code: String,
    pub detail: String,
    pub next_action: &'static str,
}

#[allow(dead_code)] // Explicit local test status; no network request is made here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestProbe {
    pub probe_id: String,
    pub outbox_id: String,
    pub status: String,
    pub remote_status: Option<String>,
    pub last_error_code: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiagnosticExportError {
    DirectoryRejected,
    AlreadyExists,
    WriteFailed,
    SerializationFailed,
}

/// Writes one UTF-8 JSON document to a caller-owned, controlled directory.
/// The fixed filename and `create_new` avoid clobbering an existing report.
pub(crate) fn export_health_report_json(
    controlled_dir: &Path,
    report: &HealthReport,
) -> Result<PathBuf, DiagnosticExportError> {
    validate_controlled_directory(controlled_dir)?;
    let bytes =
        serde_json::to_vec(report).map_err(|_| DiagnosticExportError::SerializationFailed)?;
    if bytes.len() > MAX_DIAGNOSTIC_JSON_BYTES {
        return Err(DiagnosticExportError::SerializationFailed);
    }
    let output = controlled_dir.join(HEALTH_REPORT_FILE);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => DiagnosticExportError::AlreadyExists,
            _ => DiagnosticExportError::WriteFailed,
        })?;
    file.write_all(&bytes)
        .and_then(|_| file.flush())
        .map_err(|_| DiagnosticExportError::WriteFailed)?;
    Ok(output)
}

fn validate_controlled_directory(path: &Path) -> Result<(), DiagnosticExportError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| DiagnosticExportError::DirectoryRejected)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(DiagnosticExportError::DirectoryRejected);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(DiagnosticExportError::DirectoryRejected);
        }
    }
    Ok(())
}

#[derive(Clone)]
struct RollingLogWriter {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

struct RollingLogFile {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl RollingLogWriter {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }
}

impl<'a> MakeWriter<'a> for RollingLogWriter {
    type Writer = RollingLogFile;

    fn make_writer(&'a self) -> Self::Writer {
        RollingLogFile {
            path: self.path.clone(),
            lock: Arc::clone(&self.lock),
        }
    }
}

impl Write for RollingLogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("log writer lock poisoned"))?;
        rotate_if_needed(&self.path, buf.len() as u64)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn init_logging(app_data_dir: &Path) -> Result<(), AppError> {
    std::fs::create_dir_all(app_data_dir).map_err(|error| {
        AppError::internal("LOGGING_UNAVAILABLE", "日志初始化失败", error.to_string())
    })?;
    tracing_subscriber::fmt()
        .json()
        .with_writer(RollingLogWriter::new(app_data_dir.join(LOG_FILE)))
        .try_init()
        .map_err(|error| {
            AppError::internal("LOGGING_UNAVAILABLE", "日志初始化失败", error.to_string())
        })
}

fn rotate_if_needed(path: &Path, incoming: u64) -> io::Result<()> {
    if file_len(path).saturating_add(incoming) <= MAX_LOG_BYTES {
        return Ok(());
    }
    for index in (1..=MAX_ARCHIVES).rev() {
        let source = if index == 1 {
            path.to_path_buf()
        } else {
            archive_sibling(path, index - 1)
        };
        let destination = archive_sibling(path, index);
        if !source.exists() {
            continue;
        }
        if destination.exists() {
            std::fs::remove_file(&destination)?;
        }
        std::fs::rename(source, destination)?;
    }
    Ok(())
}

fn archive_sibling(path: &Path, index: u8) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(format!(".{index}"));
    PathBuf::from(value)
}

fn file_len(path: &Path) -> u64 {
    path.metadata().map(|metadata| metadata.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_writer_rotates_without_losing_the_new_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join(LOG_FILE);
        std::fs::write(&log, vec![b'x'; MAX_LOG_BYTES as usize]).expect("seed log");
        let mut writer = RollingLogFile {
            path: log.clone(),
            lock: Arc::new(Mutex::new(())),
        };
        writer.write_all(b"next").expect("write log");
        assert!(archive_sibling(&log, 1).exists());
        assert_eq!(std::fs::read(&log).unwrap(), b"next");
    }

    fn event(sequence: u64) -> DiagnosticEvent {
        DiagnosticEvent::new(
            sequence,
            Component::Runtime,
            Stage::Check,
            SafeCode::Ok,
            1,
            1,
        )
        .expect("fixed event is valid")
    }

    fn report(snapshot: DiagnosticRingSnapshot) -> HealthReport {
        HealthReport::from_safe_input(HealthReportInput {
            app_build: SafeBuild::parse("0.1.0-test").unwrap(),
            runtime_epoch: 42,
            check_results: vec![CheckResult::new(
                Component::Runtime,
                Stage::Check,
                CheckStatus::NotRun,
                SafeCode::NotRun,
                0,
            )
            .unwrap()],
            queue_counts: QueueCounts::default(),
            retention_pressure: RetentionPressure::default(),
            explicit_not_run: vec![Component::Hook],
            ring_snapshot: snapshot,
        })
        .unwrap()
    }

    #[test]
    fn diagnostic_event_rejects_unbounded_values_and_never_accepts_sentinel_text() {
        assert!(DiagnosticEvent::new(
            MAX_JS_SAFE_INTEGER + 1,
            Component::Capture,
            Stage::Append,
            SafeCode::Ok,
            0,
            0,
        )
        .is_none());
        assert!(DiagnosticEvent::new(
            1,
            Component::Capture,
            Stage::Append,
            SafeCode::Ok,
            MAX_DURATION_MS + 1,
            0,
        )
        .is_none());
        assert!(SafeBuild::parse("SENTINEL_prompt_C:\\private\\token").is_none());

        let encoded = serde_json::to_string(&event(1)).unwrap();
        assert!(!encoded.contains("SENTINEL"));
        assert!(!encoded.contains("private"));
        assert!(!encoded.contains("token"));
    }

    #[test]
    fn ring_evicts_oldest_at_the_256_event_limit_and_stays_within_json_budget() {
        let ring = DiagnosticRing::enabled();
        for _ in 0..=MAX_DIAGNOSTIC_EVENTS as u64 {
            ring.push(Component::Runtime, Stage::Check, SafeCode::Ok, 1, 1);
        }
        let snapshot = ring.snapshot();
        assert_eq!(
            snapshot.recent_safe_diagnostic_events.len(),
            MAX_DIAGNOSTIC_EVENTS
        );
        assert_eq!(
            snapshot
                .recent_safe_diagnostic_events
                .first()
                .unwrap()
                .sequence,
            1
        );
        assert_eq!(snapshot.evicted_diagnostic_count, 1);
        assert!(serde_json::to_vec(&snapshot).unwrap().len() <= MAX_DIAGNOSTIC_JSON_BYTES);
        assert!(json_array_len(MAX_RING_EVENT_ARRAY_BYTES - 2, 1, 1) > MAX_RING_EVENT_ARRAY_BYTES);
    }

    #[test]
    fn ring_push_owns_a_monotonic_js_safe_sequence_and_clamps_evictions() {
        let ring = DiagnosticRing::enabled();
        ring.push(Component::Capture, Stage::Append, SafeCode::Ok, 0, 0);
        ring.push(Component::Capture, Stage::Append, SafeCode::Ok, 0, 0);
        assert_eq!(
            ring.snapshot()
                .recent_safe_diagnostic_events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        let inner = Arc::clone(ring.inner.as_ref().unwrap());
        inner.lock().unwrap().evicted_count = u64::MAX;
        assert_eq!(
            ring.snapshot().evicted_diagnostic_count,
            MAX_JS_SAFE_INTEGER
        );
    }

    #[test]
    fn disabled_or_poisoned_ring_does_not_change_business_control_flow() {
        let disabled = DiagnosticRing::disabled();
        disabled.push(Component::Runtime, Stage::Check, SafeCode::Ok, 0, 0);
        assert!(disabled.snapshot().recent_safe_diagnostic_events.is_empty());

        let ring = DiagnosticRing::enabled();
        let inner = Arc::clone(ring.inner.as_ref().unwrap());
        let _ = std::panic::catch_unwind(|| {
            let _guard = inner.lock().unwrap();
            panic!("test-only poison");
        });
        ring.push(Component::Runtime, Stage::Check, SafeCode::Ok, 0, 0);
        assert!(ring.snapshot().recent_safe_diagnostic_events.is_empty());
    }

    #[test]
    fn export_is_utf8_create_new_and_rejects_uncontrolled_directories() {
        let dir = tempfile::tempdir().unwrap();
        let value = report(DiagnosticRingSnapshot {
            recent_safe_diagnostic_events: vec![event(1)],
            evicted_diagnostic_count: 0,
        });
        let output = export_health_report_json(dir.path(), &value).unwrap();
        let bytes = std::fs::read(&output).unwrap();
        assert!(std::str::from_utf8(&bytes).is_ok());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["runtimeEpoch"],
            42
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["contractReference"]
                ["resultPageContract"],
            "result_pages_v1"
        );
        assert_eq!(
            export_health_report_json(dir.path(), &value),
            Err(DiagnosticExportError::AlreadyExists)
        );
        assert_eq!(
            export_health_report_json(&output, &value),
            Err(DiagnosticExportError::DirectoryRejected)
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_rejects_symlink_directory() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let link = parent.path().join("link");
        symlink(target.path(), &link).unwrap();
        assert_eq!(
            export_health_report_json(&link, &report(DiagnosticRingSnapshot::default())),
            Err(DiagnosticExportError::DirectoryRejected)
        );
    }
}
