use base64::Engine as _;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub const USER_PROMPT_HOOK_MARKER: &str = "--promptdock-desktop-agent-event-user-prompt";
pub const STOP_HOOK_MARKER: &str = "--promptdock-desktop-agent-event-stop";
pub const PERMISSION_REQUEST_HOOK_MARKER: &str =
    "--promptdock-desktop-agent-event-permission-request";
const LOCK_TIMEOUT: Duration = Duration::from_millis(1_000);
const LOCK_RETRY: Duration = Duration::from_millis(10);
// Stop capture may spend up to 5.5 seconds on bounded local context/usage work.
// Keep the host deadline above that process-wide bound so Codex cannot terminate
// the handler before the settling event is durably appended.
const HOOK_TIMEOUT_SECONDS: u64 = 8;
const LOCK_SUFFIX: &str = ".promptdock-desktop.lock";
const INTENT_FILENAME: &str = "desktop-hook-installation.intent";
const INTENT_INSTALLED: &[u8] = b"installed\n";
const INTENT_UNINSTALLED: &[u8] = b"uninstalled\n";
const MAX_CODEX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum HookInstallError {
    #[error("无法确定 Codex 配置目录")]
    MissingCodexHome,
    #[error("Hook 配置不是有效的 JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("Hook 配置结构无效: {0}")]
    InvalidShape(String),
    #[error("Hook 命令路径不是有效的 Unicode")]
    InvalidCommandPath,
    #[error("Codex Hook 配置刚被其他程序修改")]
    ConfigChanged,
    #[error("等待 Codex Hook 配置锁超时")]
    ConfigLockTimeout,
    #[error("Hook 配置文件操作失败: {0}")]
    Io(#[from] io::Error),
    #[error("Codex 主配置不是有效的 TOML: {0}")]
    InvalidToml(#[from] toml::de::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookInstallPlanOutcome {
    Installed,
    Repaired,
    NoChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookInstallPlan {
    pub outcome: HookInstallPlanOutcome,
    pub definition_changed: bool,
    pub registration_changed: bool,
    pub review_required: Option<bool>,
    pub external_review_required: bool,
    pub changed_events: Vec<String>,
    pub source_fingerprint: Option<String>,
    pub planned_fingerprint: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookInstallationState {
    Absent,
    Current,
    NeedsRepair { reasons: Vec<String> },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTrustState {
    Trusted,
    Untrusted,
    Modified,
    Disabled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedHookKind {
    UserPrompt,
    Stop,
    PermissionRequest,
}
impl ManagedHookKind {
    fn label(self) -> &'static str {
        match self {
            Self::UserPrompt => "UserPromptSubmit",
            Self::Stop => "Stop",
            Self::PermissionRequest => "PermissionRequest",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedHookSpec {
    pub kind: ManagedHookKind,
    pub event_name: &'static str,
    pub capture_event: &'static str,
    pub marker: &'static str,
    pub command: String,
    pub command_windows: String,
    pub timeout_seconds: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFeatureStatus {
    pub user_prompt: HookInstallationState,
    pub stop: HookInstallationState,
    pub permission_request: HookInstallationState,
    pub foreign_stop_handlers: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookFeatures {
    pub prompt_capture: bool,
    pub run_lifecycle: bool,
    pub attention: bool,
    pub capture_agent_outputs: bool,
}
impl HookFeatures {
    pub const fn for_phase8(notifications_enabled: bool, capture_agent_outputs: bool) -> Self {
        Self {
            prompt_capture: true,
            run_lifecycle: notifications_enabled || capture_agent_outputs,
            attention: false,
            capture_agent_outputs,
        }
    }

    pub const fn for_phase9(
        notifications_enabled: bool,
        attention_enabled: bool,
        capture_agent_outputs: bool,
    ) -> Self {
        Self {
            prompt_capture: true,
            run_lifecycle: notifications_enabled || capture_agent_outputs,
            attention: notifications_enabled && attention_enabled,
            capture_agent_outputs,
        }
    }

    pub const fn all() -> Self {
        Self {
            prompt_capture: true,
            run_lifecycle: true,
            attention: true,
            capture_agent_outputs: true,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookState {
    NotInstalled,
    Configured,
    Active,
    Degraded,
    Unsupported,
    NeedsRepair,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookStatus {
    pub state: HookState,
    pub message: String,
    pub last_updated_at: Option<i64>,
}

#[derive(Clone)]
pub struct HookManager {
    config_path: PathBuf,
    inbox_path: PathBuf,
    executable_path: PathBuf,
    intent_path: PathBuf,
    registration_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookInstallationIntent {
    Automatic,
    Installed,
    Uninstalled,
}

pub struct HookConfigTransaction {
    config_path: PathBuf,
    lock: Option<File>,
    before: Snapshot,
    applied: Snapshot,
    finished: bool,
}

impl HookConfigTransaction {
    pub fn commit(mut self) -> Result<(), HookInstallError> {
        ensure(&self.config_path, &self.applied)?;
        self.finished = true;
        self.unlock()
    }

    pub fn rollback(mut self) -> Result<(), HookInstallError> {
        let restored = self.restore();
        self.finished = true;
        let unlocked = self.unlock();
        restored.and(unlocked)
    }

    fn restore(&self) -> Result<(), HookInstallError> {
        restore_snapshot(&self.config_path, &self.before, &self.applied)
    }

    fn unlock(&mut self) -> Result<(), HookInstallError> {
        if let Some(lock) = self.lock.take() {
            FileExt::unlock(&lock)?;
        }
        Ok(())
    }
}

impl Drop for HookConfigTransaction {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.restore();
        }
        let _ = self.unlock();
    }
}

impl HookManager {
    pub fn for_app(app_data_dir: &Path) -> Result<Self, HookInstallError> {
        let home = std::env::var_os("CODEX_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(".codex"))
            })
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(".codex"))
            })
            .ok_or(HookInstallError::MissingCodexHome)?;
        Self::new(
            home.join("hooks.json"),
            app_data_dir.join("agent-events.jsonl"),
            std::env::current_exe()?,
        )
    }
    pub fn new(
        config_path: PathBuf,
        inbox_path: PathBuf,
        executable_path: PathBuf,
    ) -> Result<Self, HookInstallError> {
        if !inbox_path.is_absolute() || !executable_path.is_absolute() {
            return Err(HookInstallError::InvalidShape(
                "Hook executable 和 inbox 路径必须是绝对路径".into(),
            ));
        }
        Ok(Self {
            config_path,
            intent_path: inbox_path
                .parent()
                .expect("absolute inbox path has a parent")
                .join(INTENT_FILENAME),
            inbox_path,
            executable_path,
            registration_id: None,
        })
    }

    pub fn with_registration_id(mut self, id: String) -> Self {
        self.registration_id = Some(id);
        self
    }
    pub fn installation_state(&self) -> Result<HookInstallationState, HookInstallError> {
        self.installation_state_for_features(HookFeatures::all())
    }
    pub fn installation_state_for_features(
        &self,
        features: HookFeatures,
    ) -> Result<HookInstallationState, HookInstallError> {
        Ok(aggregate_for(
            &self.feature_status(features.capture_agent_outputs)?,
            features,
        ))
    }
    pub fn trust_state_for_features(
        &self,
        features: HookFeatures,
    ) -> Result<HookTrustState, HookInstallError> {
        let root = snapshot(&self.config_path)?.parse()?;
        let states = read_hook_states(
            &self
                .config_path
                .parent()
                .ok_or_else(|| HookInstallError::InvalidShape("Hook 配置路径缺少父目录".into()))?
                .join("config.toml"),
        )?;
        let mut aggregate = HookTrustState::Trusted;
        for spec in self.desired_specs(features)? {
            let location = managed_hook_location(&root, spec.kind)?.ok_or_else(|| {
                HookInstallError::InvalidShape(format!(
                    "缺少 {} PromptDock handler",
                    spec.kind.label()
                ))
            })?;
            let key = format!(
                "{}:{}:{}:{}",
                self.config_path.display(),
                hook_event_key_label(spec.event_name),
                location.group_index,
                location.handler_index
            );
            let state = states.get(&key);
            if state.and_then(|state| state.enabled) == Some(false) {
                return Ok(HookTrustState::Disabled);
            }
            let current_hash = hook_hash(spec.event_name, location.group, location.handler)?;
            aggregate = match state.and_then(|state| state.trusted_hash.as_deref()) {
                Some(hash) if hash == current_hash.as_str() => aggregate,
                Some(_) => HookTrustState::Modified,
                None if aggregate != HookTrustState::Modified => HookTrustState::Untrusted,
                None => aggregate,
            };
        }
        Ok(aggregate)
    }
    pub fn installation_state_for(&self) -> Result<HookFeatureStatus, HookInstallError> {
        self.feature_status(true)
    }
    fn feature_status(
        &self,
        capture_agent_outputs: bool,
    ) -> Result<HookFeatureStatus, HookInstallError> {
        let s = snapshot(&self.config_path)?;
        if s.bytes.is_none() {
            return Ok(absent_status());
        }
        inspect(&s.parse()?, &self.specs(capture_agent_outputs)?)
    }
    pub fn install(&self) -> Result<(), HookInstallError> {
        self.install_features(HookFeatures::all())
    }
    pub fn install_features(&self, features: HookFeatures) -> Result<(), HookInstallError> {
        let transaction = self.begin_reconcile_features(features)?;
        self.finish_explicit_change(transaction, HookInstallationIntent::Installed)
    }

    pub fn install_features_expected(
        &self,
        features: HookFeatures,
        expected_fingerprint: Option<&str>,
    ) -> Result<(), HookInstallError> {
        let lock = self.acquire_lock()?;
        let transaction =
            self.begin_reconcile_features_with_lock(features, lock, Some(expected_fingerprint))?;
        self.finish_explicit_change(transaction, HookInstallationIntent::Installed)
    }

    pub fn plan_reconcile_features(
        &self,
        features: HookFeatures,
    ) -> Result<HookInstallPlan, HookInstallError> {
        let before = snapshot(&self.config_path)?;
        let desired = self.desired_specs(features)?;
        let mut root = before.default()?;
        let registration_changed =
            registration_changed(&root, &desired, self.registration_id.as_deref())?;
        let external_review_required = external_index_impact(&root, &desired)?;
        let changed_events = reconcile_managed_in_place(&mut root, &desired)?;
        let definition_changed = !changed_events.is_empty();
        let planned_fingerprint = if definition_changed {
            Some(fingerprint_bytes(&serialized(&root)?))
        } else {
            fingerprint_snapshot(&before)
        };
        Ok(HookInstallPlan {
            outcome: if !definition_changed {
                HookInstallPlanOutcome::NoChange
            } else if before.bytes.is_none() {
                HookInstallPlanOutcome::Installed
            } else {
                HookInstallPlanOutcome::Repaired
            },
            definition_changed,
            registration_changed,
            review_required: Some(definition_changed),
            external_review_required,
            changed_events,
            source_fingerprint: fingerprint_snapshot(&before),
            planned_fingerprint,
        })
    }
    pub fn reconcile(&self) -> Result<(), HookInstallError> {
        self.reconcile_features(HookFeatures::all())
    }
    pub fn reconcile_features(&self, features: HookFeatures) -> Result<(), HookInstallError> {
        self.begin_reconcile_features(features)?.commit()
    }
    pub fn uninstall(&self) -> Result<(), HookInstallError> {
        let transaction = self.begin_uninstall()?;
        self.finish_explicit_change(transaction, HookInstallationIntent::Uninstalled)
    }

    pub fn begin_settings_reconcile(
        &self,
        features: HookFeatures,
    ) -> Result<Option<HookConfigTransaction>, HookInstallError> {
        let lock = self.acquire_lock()?;
        if self.should_reconcile(features)? {
            self.begin_reconcile_features_with_lock(features, lock, None)
                .map(Some)
        } else {
            FileExt::unlock(&lock)?;
            Ok(None)
        }
    }

    pub fn reconcile_on_startup(&self, features: HookFeatures) -> Result<(), HookInstallError> {
        if let Some(transaction) = self.begin_settings_reconcile(features)? {
            transaction.commit()?;
        }
        Ok(())
    }

    fn begin_reconcile_features(
        &self,
        features: HookFeatures,
    ) -> Result<HookConfigTransaction, HookInstallError> {
        let lock = self.acquire_lock()?;
        self.begin_reconcile_features_with_lock(features, lock, None)
    }

    fn begin_reconcile_features_with_lock(
        &self,
        features: HookFeatures,
        lock: File,
        expected_fingerprint: Option<Option<&str>>,
    ) -> Result<HookConfigTransaction, HookInstallError> {
        let desired = self.desired_specs(features)?;
        self.begin_change_with_lock(lock, |before| {
            if expected_fingerprint
                .is_some_and(|expected| fingerprint_snapshot(before).as_deref() != expected)
            {
                return Err(HookInstallError::ConfigChanged);
            }
            let mut root = before.default()?;
            let original = root.clone();
            reconcile_managed_in_place(&mut root, &desired)?;
            Ok((root != original).then_some(root))
        })
    }

    fn begin_uninstall(&self) -> Result<HookConfigTransaction, HookInstallError> {
        let lock = self.acquire_lock()?;
        self.begin_change_with_lock(lock, |before| {
            if before.bytes.is_none() {
                return Ok(None);
            }
            let mut root = before.parse()?;
            Ok(remove_managed(&mut root)?.then_some(root))
        })
    }

    fn begin_change_with_lock(
        &self,
        lock: File,
        change: impl FnOnce(&Snapshot) -> Result<Option<Value>, HookInstallError>,
    ) -> Result<HookConfigTransaction, HookInstallError> {
        let before = snapshot(&self.config_path)?;
        let applied = match change(&before)? {
            Some(root) => {
                write_atomic(&self.config_path, &root, &before)?;
                serialized_snapshot(&root)?
            }
            None => before.clone(),
        };
        Ok(HookConfigTransaction {
            config_path: self.config_path.clone(),
            lock: Some(lock),
            before,
            applied,
            finished: false,
        })
    }

    fn should_reconcile(&self, features: HookFeatures) -> Result<bool, HookInstallError> {
        match self.installation_intent()? {
            HookInstallationIntent::Installed => Ok(true),
            HookInstallationIntent::Uninstalled => Ok(false),
            HookInstallationIntent::Automatic => Ok(features.run_lifecycle
                || !matches!(self.installation_state(), Ok(HookInstallationState::Absent))),
        }
    }

    fn installation_intent(&self) -> Result<HookInstallationIntent, HookInstallError> {
        match fs::read(&self.intent_path) {
            Ok(bytes) if bytes == INTENT_INSTALLED => Ok(HookInstallationIntent::Installed),
            Ok(bytes) if bytes == INTENT_UNINSTALLED => Ok(HookInstallationIntent::Uninstalled),
            Ok(_) => Err(HookInstallError::InvalidShape(
                "Hook installation intent 无效".into(),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(HookInstallationIntent::Automatic)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn finish_explicit_change(
        &self,
        transaction: HookConfigTransaction,
        intent: HookInstallationIntent,
    ) -> Result<(), HookInstallError> {
        let original_intent = snapshot(&self.intent_path)?;
        let bytes = match intent {
            HookInstallationIntent::Installed => INTENT_INSTALLED,
            HookInstallationIntent::Uninstalled => INTENT_UNINSTALLED,
            HookInstallationIntent::Automatic => unreachable!(),
        };
        if original_intent.bytes.as_deref() == Some(bytes) {
            return transaction.commit();
        }
        if let Err(error) = write_raw_atomic(&self.intent_path, bytes, &original_intent) {
            transaction.rollback()?;
            return Err(error);
        }
        match transaction.commit() {
            Ok(()) => Ok(()),
            Err(error) => {
                let written_intent = Snapshot::from_bytes(bytes.to_vec());
                restore_snapshot(&self.intent_path, &original_intent, &written_intent)?;
                Err(error)
            }
        }
    }
    fn specs(&self, capture_agent_outputs: bool) -> Result<[ManagedHookSpec; 3], HookInstallError> {
        Ok([
            self.spec(
                ManagedHookKind::UserPrompt,
                "UserPromptSubmit",
                "codex-user-prompt",
                USER_PROMPT_HOOK_MARKER,
                false,
            )?,
            self.spec(
                ManagedHookKind::Stop,
                "Stop",
                "codex-stop",
                STOP_HOOK_MARKER,
                capture_agent_outputs,
            )?,
            self.spec(
                ManagedHookKind::PermissionRequest,
                "PermissionRequest",
                "codex-permission-request",
                PERMISSION_REQUEST_HOOK_MARKER,
                false,
            )?,
        ])
    }
    pub(crate) fn desired_specs(
        &self,
        features: HookFeatures,
    ) -> Result<Vec<ManagedHookSpec>, HookInstallError> {
        Ok(self
            .specs(features.capture_agent_outputs)?
            .into_iter()
            .filter(|spec| match spec.kind {
                ManagedHookKind::UserPrompt => features.prompt_capture,
                ManagedHookKind::Stop => features.run_lifecycle,
                ManagedHookKind::PermissionRequest => features.attention,
            })
            .collect())
    }
    fn spec(
        &self,
        kind: ManagedHookKind,
        event_name: &'static str,
        capture_event: &'static str,
        marker: &'static str,
        capture_agent_output: bool,
    ) -> Result<ManagedHookSpec, HookInstallError> {
        let output_flag = if capture_agent_output {
            " --capture-agent-output"
        } else {
            ""
        };
        let registration_flag = match self.registration_id.as_deref() {
            Some(id) => {
                let id = uuid::Uuid::parse_str(id).map_err(|_| {
                    HookInstallError::InvalidShape("Hook registration id 必须是 UUID".into())
                })?;
                format!(" --hook-registration-id {id}")
            }
            None => String::new(),
        };
        let command = format!(
            "{} --capture-agent-event {capture_event} --inbox {}{output_flag}{registration_flag} {marker}",
            quote(&self.executable_path)?,
            quote(&self.inbox_path)?
        );
        let script=format!("$ErrorActionPreference='Stop';try{{& {} --capture-agent-event {capture_event} --inbox {}{output_flag}{registration_flag} {marker} 2>$null;exit 0}}catch{{exit 0}}",ps_quote(&self.executable_path)?,ps_quote(&self.inbox_path)?);
        Ok(ManagedHookSpec {
            kind,
            event_name,
            capture_event,
            marker,
            command,
            command_windows: encoded(&script, marker),
            timeout_seconds: HOOK_TIMEOUT_SECONDS,
        })
    }
    #[cfg(test)]
    fn windows_command(&self, kind: ManagedHookKind) -> String {
        self.specs(true)
            .unwrap()
            .into_iter()
            .find(|s| s.kind == kind)
            .unwrap()
            .command_windows
    }
    fn acquire_lock(&self) -> Result<File, HookInstallError> {
        let parent = self
            .config_path
            .parent()
            .ok_or_else(|| HookInstallError::InvalidShape("配置路径缺少父目录".into()))?;
        fs::create_dir_all(parent)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(sibling(&self.config_path, LOCK_SUFFIX))?;
        lock_timeout(&lock)?;
        Ok(lock)
    }
}
fn absent_status() -> HookFeatureStatus {
    HookFeatureStatus {
        user_prompt: HookInstallationState::Absent,
        stop: HookInstallationState::Absent,
        permission_request: HookInstallationState::Absent,
        foreign_stop_handlers: 0,
    }
}
fn aggregate_for(s: &HookFeatureStatus, features: HookFeatures) -> HookInstallationState {
    let states = [
        (&s.user_prompt, "UserPromptSubmit", features.prompt_capture),
        (&s.stop, "Stop", features.run_lifecycle),
        (
            &s.permission_request,
            "PermissionRequest",
            features.attention,
        ),
    ];
    if states.iter().all(|(state, _, desired)| {
        **state
            == if *desired {
                HookInstallationState::Current
            } else {
                HookInstallationState::Absent
            }
    }) {
        return HookInstallationState::Current;
    }
    if states
        .iter()
        .all(|(x, _, _)| **x == HookInstallationState::Absent)
    {
        return HookInstallationState::Absent;
    }
    let mut r = vec![];
    for (state, event, desired) in states {
        if !desired {
            if *state != HookInstallationState::Absent {
                push(&mut r, &format!("存在不需要的 {event} PromptDock handler"));
            }
            continue;
        }
        match state {
            HookInstallationState::Absent => {
                push(&mut r, &format!("缺少 {event} PromptDock handler"))
            }
            HookInstallationState::NeedsRepair { reasons } => {
                for x in reasons {
                    push(&mut r, &format!("{event}: {x}"))
                }
            }
            HookInstallationState::Current => {}
        }
    }
    HookInstallationState::NeedsRepair { reasons: r }
}

#[derive(Debug, Default, Deserialize)]
struct CodexConfigFile {
    #[serde(default)]
    hooks: CodexHooksConfig,
}

#[derive(Debug, Default, Deserialize)]
struct CodexHooksConfig {
    #[serde(default)]
    state: HashMap<String, CodexHookState>,
}

#[derive(Debug, Default, Deserialize)]
struct CodexHookState {
    enabled: Option<bool>,
    trusted_hash: Option<String>,
}

fn read_hook_states(path: &Path) -> Result<HashMap<String, CodexHookState>, HookInstallError> {
    let metadata = match path.metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_CODEX_CONFIG_BYTES {
        return Err(HookInstallError::InvalidShape(
            "Codex 主配置超过读取上限".into(),
        ));
    }
    let config: CodexConfigFile = toml::from_str(&fs::read_to_string(path)?)?;
    Ok(config.hooks.state)
}

struct ManagedHookLocation<'a> {
    group_index: usize,
    handler_index: usize,
    group: &'a Value,
    handler: &'a Value,
}

fn managed_hook_location(
    root: &Value,
    target: ManagedHookKind,
) -> Result<Option<ManagedHookLocation<'_>>, HookInstallError> {
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
        return Ok(None);
    };
    let mut found = None;
    for (event, groups) in hooks {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for (group_index, group) in groups.iter().enumerate() {
            let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for (handler_index, handler) in handlers.iter().enumerate() {
                if kind(handler) == Some(target) {
                    if found.is_some() {
                        return Err(HookInstallError::InvalidShape(format!(
                            "存在多个 PromptDock {} handler",
                            target.label()
                        )));
                    }
                    if event != target.label() {
                        return Err(HookInstallError::InvalidShape(format!(
                            "PromptDock {} handler 不在 {} 事件中",
                            target.label(),
                            target.label()
                        )));
                    }
                    if target == ManagedHookKind::PermissionRequest
                        && group.get("matcher").is_some()
                    {
                        return Err(HookInstallError::InvalidShape(
                            "PromptDock PermissionRequest matcher 必须省略".into(),
                        ));
                    }
                    found = Some(ManagedHookLocation {
                        group_index,
                        handler_index,
                        group,
                        handler,
                    });
                }
            }
        }
    }
    Ok(found)
}

fn hook_event_key_label(event_name: &str) -> &'static str {
    match event_name {
        "UserPromptSubmit" => "user_prompt_submit",
        "Stop" => "stop",
        "PermissionRequest" => "permission_request",
        _ => unreachable!("managed Hook events are fixed"),
    }
}

pub(crate) fn hook_hash(
    event_name: &str,
    group: &Value,
    handler: &Value,
) -> Result<String, HookInstallError> {
    let command = if cfg!(windows) {
        handler
            .get("commandWindows")
            .and_then(Value::as_str)
            .or_else(|| handler.get("command").and_then(Value::as_str))
    } else {
        handler.get("command").and_then(Value::as_str)
    }
    .ok_or_else(|| HookInstallError::InvalidShape("Hook command 无效".into()))?;
    let timeout = handler
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(600)
        .max(1);
    let mut normalized_handler = Map::new();
    normalized_handler.insert("type".into(), Value::String("command".into()));
    normalized_handler.insert("command".into(), Value::String(command.into()));
    normalized_handler.insert("timeout".into(), Value::Number(timeout.into()));
    normalized_handler.insert(
        "async".into(),
        Value::Bool(
            handler
                .get("async")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
    );
    if let Some(message) = handler.get("statusMessage").and_then(Value::as_str) {
        normalized_handler.insert("statusMessage".into(), Value::String(message.into()));
    }
    let mut identity = Map::new();
    identity.insert(
        "event_name".into(),
        Value::String(hook_event_key_label(event_name).into()),
    );
    if event_name == "PermissionRequest" {
        if let Some(matcher) = group.get("matcher").and_then(Value::as_str) {
            identity.insert("matcher".into(), Value::String(matcher.into()));
        }
    }
    identity.insert(
        "hooks".into(),
        Value::Array(vec![Value::Object(normalized_handler)]),
    );
    let bytes = serde_json::to_vec(&canonical_json(&Value::Object(identity)))?;
    let hash = Sha256::digest(bytes);
    Ok(format!("sha256:{hash:x}"))
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), canonical_json(&map[key])))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}
fn encoded(script: &str, marker: &str) -> String {
    let value = base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    format!("powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {value} && rem {marker}")
}

#[derive(Clone)]
struct Snapshot {
    bytes: Option<Vec<u8>>,
    hash: Option<blake3::Hash>,
}
impl Snapshot {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            hash: Some(blake3::hash(&bytes)),
            bytes: Some(bytes),
        }
    }

    fn parse(&self) -> Result<Value, HookInstallError> {
        parse(self.bytes.as_deref().ok_or_else(|| {
            HookInstallError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "Hook config does not exist",
            ))
        })?)
    }
    fn default(&self) -> Result<Value, HookInstallError> {
        self.bytes
            .as_deref()
            .map(parse)
            .transpose()
            .map(|v| v.unwrap_or_else(|| Value::Object(Map::new())))
    }
}
fn snapshot(path: &Path) -> Result<Snapshot, HookInstallError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Snapshot {
            hash: Some(blake3::hash(&bytes)),
            bytes: Some(bytes),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Snapshot {
            bytes: None,
            hash: None,
        }),
        Err(e) => Err(e.into()),
    }
}

fn fingerprint_snapshot(snapshot: &Snapshot) -> Option<String> {
    snapshot.hash.map(|hash| format!("blake3:{hash}"))
}

fn fingerprint_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes))
}

#[cfg(test)]
fn read_config(path: &Path) -> Result<Value, HookInstallError> {
    parse(&fs::read(path)?)
}
fn parse(bytes: &[u8]) -> Result<Value, HookInstallError> {
    let value: Value = serde_json::from_slice(bytes)?;
    if !value.is_object() {
        return Err(HookInstallError::InvalidShape(
            "顶层必须是 JSON object".into(),
        ));
    }
    Ok(value)
}
fn event_groups<'a>(
    root: &'a mut Value,
    event: &str,
) -> Result<&'a mut Vec<Value>, HookInstallError> {
    let root = root
        .as_object_mut()
        .ok_or_else(|| HookInstallError::InvalidShape("顶层必须是 JSON object".into()))?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| HookInstallError::InvalidShape("hooks 必须是 JSON object".into()))?;
    hooks
        .entry(event)
        .or_insert_with(|| Value::Array(vec![]))
        .as_array_mut()
        .ok_or_else(|| HookInstallError::InvalidShape(format!("hooks.{event} 必须是 JSON array")))
}
fn append(root: &mut Value, s: &ManagedHookSpec) -> Result<(), HookInstallError> {
    event_groups(root, s.event_name)?.push(json!({"hooks":[desired_handler(s)]}));
    Ok(())
}

fn desired_handler(s: &ManagedHookSpec) -> Value {
    json!({
        "type": "command",
        "command": s.command,
        "commandWindows": s.command_windows,
        "async": false,
        "timeout": s.timeout_seconds
    })
}

fn registration_changed(
    root: &Value,
    desired: &[ManagedHookSpec],
    desired_registration_id: Option<&str>,
) -> Result<bool, HookInstallError> {
    for spec in desired {
        let Some(location) = managed_hook_location(root, spec.kind)? else {
            if desired_registration_id.is_some() {
                return Ok(true);
            }
            continue;
        };
        if handler_registration_id(location.handler) != desired_registration_id {
            return Ok(true);
        }
    }
    Ok(false)
}

fn external_index_impact(
    root: &Value,
    desired: &[ManagedHookSpec],
) -> Result<bool, HookInstallError> {
    let desired_kinds = desired.iter().map(|spec| spec.kind).collect::<Vec<_>>();
    let (found, _) = collect(root)?;
    validate_managed_layout(&found)?;
    Ok(found.iter().any(|existing| {
        !desired_kinds.contains(&existing.kind)
            && existing
                .group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|handlers| {
                    handlers
                        .iter()
                        .skip(existing.handler_index + 1)
                        .any(|handler| kind(handler).is_none())
                })
    }))
}

fn handler_registration_id(handler: &Value) -> Option<&str> {
    let command = handler.get("command").and_then(Value::as_str)?;
    command
        .split_whitespace()
        .skip_while(|part| *part != "--hook-registration-id")
        .nth(1)
}

fn reconcile_managed_in_place(
    root: &mut Value,
    desired: &[ManagedHookSpec],
) -> Result<Vec<String>, HookInstallError> {
    let desired_kinds = desired.iter().map(|spec| spec.kind).collect::<Vec<_>>();
    let mut changed_events = Vec::new();
    {
        let (found, _) = collect(root)?;
        validate_managed_layout(&found)?;
        for existing in &found {
            if !desired_kinds.contains(&existing.kind) {
                push(&mut changed_events, existing.event);
            }
        }
    }
    remove_undesired_managed(root, &desired_kinds)?;

    for spec in desired {
        let position = managed_hook_location(root, spec.kind)?
            .map(|location| (location.group_index, location.handler_index));
        match position {
            Some((group_index, handler_index)) => {
                let handler = event_groups(root, spec.event_name)?
                    .get_mut(group_index)
                    .and_then(|group| group.get_mut("hooks"))
                    .and_then(Value::as_array_mut)
                    .and_then(|handlers| handlers.get_mut(handler_index))
                    .ok_or_else(|| {
                        HookInstallError::InvalidShape(format!(
                            "无法定位 PromptDock {} handler",
                            spec.kind.label()
                        ))
                    })?;
                if update_handler(handler, spec)? {
                    push(&mut changed_events, spec.event_name);
                }
            }
            None => {
                append(root, spec)?;
                push(&mut changed_events, spec.event_name);
            }
        }
    }
    Ok(changed_events)
}

fn validate_managed_layout(found: &[Found<'_>]) -> Result<(), HookInstallError> {
    for target in [
        ManagedHookKind::UserPrompt,
        ManagedHookKind::Stop,
        ManagedHookKind::PermissionRequest,
    ] {
        let matches = found
            .iter()
            .filter(|existing| existing.kind == target)
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(HookInstallError::InvalidShape(format!(
                "存在多个 PromptDock {} handler",
                target.label()
            )));
        }
        let Some(existing) = matches.first() else {
            continue;
        };
        if existing.event != target.label() {
            return Err(HookInstallError::InvalidShape(format!(
                "PromptDock {} handler 不在 {} 事件中",
                target.label(),
                target.label()
            )));
        }
        if target == ManagedHookKind::PermissionRequest && existing.group.get("matcher").is_some() {
            return Err(HookInstallError::InvalidShape(
                "PromptDock PermissionRequest matcher 必须省略".into(),
            ));
        }
    }
    Ok(())
}

fn remove_undesired_managed(
    root: &mut Value,
    desired_kinds: &[ManagedHookKind],
) -> Result<(), HookInstallError> {
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(());
    };
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| HookInstallError::InvalidShape("hooks 必须是 JSON object".into()))?;
    for (event, groups) in hooks {
        let known = matches!(
            event.as_str(),
            "UserPromptSubmit" | "Stop" | "PermissionRequest"
        );
        let Some(groups) = groups.as_array_mut() else {
            if known {
                return Err(HookInstallError::InvalidShape(format!(
                    "hooks.{event} 必须是 JSON array"
                )));
            }
            continue;
        };
        let mut group_index = 0;
        while group_index < groups.len() {
            let Some(handlers) = groups[group_index].get_mut("hooks") else {
                group_index += 1;
                continue;
            };
            let Some(handlers) = handlers.as_array_mut() else {
                if known {
                    return Err(HookInstallError::InvalidShape(
                        "matcher group hooks 必须是 array".into(),
                    ));
                }
                group_index += 1;
                continue;
            };
            handlers
                .retain(|handler| kind(handler).is_none_or(|kind| desired_kinds.contains(&kind)));
            // Keep an empty group as an index placeholder. Codex discovery iterates
            // an empty hooks array as zero handlers, while later foreign groups keep
            // the same persistent group indexes and trust keys.
            group_index += 1;
        }
    }
    Ok(())
}

fn update_handler(handler: &mut Value, spec: &ManagedHookSpec) -> Result<bool, HookInstallError> {
    let object = handler
        .as_object_mut()
        .ok_or_else(|| HookInstallError::InvalidShape("Hook handler 必须是 JSON object".into()))?;
    let original = object.clone();
    object.insert("type".into(), Value::String("command".into()));
    object.insert("command".into(), Value::String(spec.command.clone()));
    object.insert(
        "commandWindows".into(),
        Value::String(spec.command_windows.clone()),
    );
    object.insert("async".into(), Value::Bool(false));
    object.insert("timeout".into(), Value::Number(spec.timeout_seconds.into()));
    object.remove("statusMessage");
    object.remove("additionalContextLimit");
    Ok(*object != original)
}

#[derive(Clone, Copy)]
struct Found<'a> {
    kind: ManagedHookKind,
    event: &'a str,
    group: &'a Value,
    handler_index: usize,
    handler: &'a Value,
}
fn kind(handler: &Value) -> Option<ManagedHookKind> {
    for c in ["command", "commandWindows"]
        .into_iter()
        .filter_map(|x| handler.get(x).and_then(Value::as_str))
    {
        if c.contains(USER_PROMPT_HOOK_MARKER) {
            return Some(ManagedHookKind::UserPrompt);
        }
        if c.contains(STOP_HOOK_MARKER) {
            return Some(ManagedHookKind::Stop);
        }
        if c.contains(PERMISSION_REQUEST_HOOK_MARKER) {
            return Some(ManagedHookKind::PermissionRequest);
        }
    }
    None
}
fn collect(root: &Value) -> Result<(Vec<Found<'_>>, usize), HookInstallError> {
    let root = root
        .as_object()
        .ok_or_else(|| HookInstallError::InvalidShape("顶层必须是 JSON object".into()))?;
    let Some(hooks) = root.get("hooks") else {
        return Ok((vec![], 0));
    };
    let hooks = hooks
        .as_object()
        .ok_or_else(|| HookInstallError::InvalidShape("hooks 必须是 JSON object".into()))?;
    let mut found = vec![];
    let mut foreign_stop = 0;
    for (event, groups) in hooks {
        let known = matches!(
            event.as_str(),
            "UserPromptSubmit" | "Stop" | "PermissionRequest"
        );
        let Some(groups) = groups.as_array() else {
            if known {
                return Err(HookInstallError::InvalidShape(format!(
                    "hooks.{event} 必须是 JSON array"
                )));
            }
            continue;
        };
        for group in groups {
            let Some(handlers) = group.get("hooks") else {
                continue;
            };
            let Some(handlers) = handlers.as_array() else {
                if known {
                    return Err(HookInstallError::InvalidShape(
                        "matcher group hooks 必须是 array".into(),
                    ));
                }
                continue;
            };
            for (handler_index, h) in handlers.iter().enumerate() {
                if let Some(k) = kind(h) {
                    found.push(Found {
                        kind: k,
                        event,
                        group,
                        handler_index,
                        handler: h,
                    })
                } else if event == "Stop" {
                    foreign_stop += 1
                }
            }
        }
    }
    Ok((found, foreign_stop))
}
fn inspect(
    root: &Value,
    specs: &[ManagedHookSpec; 3],
) -> Result<HookFeatureStatus, HookInstallError> {
    let (found, foreign_stop_handlers) = collect(root)?;
    let state = |kind| one_state(&found, kind, specs.iter().find(|s| s.kind == kind).unwrap());
    Ok(HookFeatureStatus {
        user_prompt: state(ManagedHookKind::UserPrompt),
        stop: state(ManagedHookKind::Stop),
        permission_request: state(ManagedHookKind::PermissionRequest),
        foreign_stop_handlers,
    })
}
fn one_state(
    found: &[Found<'_>],
    kind_: ManagedHookKind,
    s: &ManagedHookSpec,
) -> HookInstallationState {
    let v = found
        .iter()
        .filter(|x| x.kind == kind_)
        .copied()
        .collect::<Vec<_>>();
    if v.is_empty() {
        return HookInstallationState::Absent;
    }
    let mut r = vec![];
    if v.len() > 1 {
        push(
            &mut r,
            &format!("存在多个 PromptDock {} handler", kind_.label()),
        )
    }
    for f in v {
        if f.event != s.event_name {
            push(
                &mut r,
                &format!("PromptDock handler 不在 {} 事件中", s.event_name),
            )
        }
        if kind_ == ManagedHookKind::PermissionRequest && f.group.get("matcher").is_some() {
            push(&mut r, "PermissionRequest matcher 必须省略")
        }
        if f.handler.get("type").and_then(Value::as_str) != Some("command") {
            push(&mut r, "handler type 不是 command")
        }
        if f.handler.get("command").and_then(Value::as_str) != Some(&s.command) {
            push(&mut r, "command 与当前安装不一致")
        }
        if f.handler.get("commandWindows").and_then(Value::as_str) != Some(&s.command_windows) {
            push(&mut r, "commandWindows 与当前安装不一致")
        }
        if f.handler.get("async") != Some(&Value::Bool(false)) {
            push(&mut r, "async 必须为 false")
        }
        if f.handler.get("timeout") != Some(&json!(HOOK_TIMEOUT_SECONDS)) {
            push(&mut r, &format!("timeout 必须为 {HOOK_TIMEOUT_SECONDS}"))
        }
        if f.handler.get("statusMessage").is_some() {
            push(&mut r, "必须移除 statusMessage")
        }
        if f.handler.get("additionalContextLimit").is_some() {
            push(&mut r, "必须移除 additionalContextLimit")
        }
    }
    if r.is_empty() {
        HookInstallationState::Current
    } else {
        HookInstallationState::NeedsRepair { reasons: r }
    }
}
fn push(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.into())
    }
}
fn remove_managed(root: &mut Value) -> Result<bool, HookInstallError> {
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(false);
    };
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| HookInstallError::InvalidShape("hooks 必须是 JSON object".into()))?;
    let mut changed = false;
    for (event, groups) in hooks {
        let known = matches!(
            event.as_str(),
            "UserPromptSubmit" | "Stop" | "PermissionRequest"
        );
        let Some(groups) = groups.as_array_mut() else {
            if known {
                return Err(HookInstallError::InvalidShape(format!(
                    "hooks.{event} 必须是 JSON array"
                )));
            }
            continue;
        };
        let mut i = 0;
        while i < groups.len() {
            let Some(handlers) = groups[i].get_mut("hooks") else {
                i += 1;
                continue;
            };
            let Some(handlers) = handlers.as_array_mut() else {
                if known {
                    return Err(HookInstallError::InvalidShape(
                        "matcher group hooks 必须是 array".into(),
                    ));
                }
                i += 1;
                continue;
            };
            let n = handlers.len();
            handlers.retain(|h| kind(h).is_none());
            let removed = n != handlers.len();
            changed |= removed;
            if removed && handlers.is_empty() {
                groups.remove(i);
            } else {
                i += 1
            }
        }
    }
    Ok(changed)
}
fn quote(path: &Path) -> Result<String, HookInstallError> {
    Ok(format!(
        "\"{}\"",
        path.to_str().ok_or(HookInstallError::InvalidCommandPath)?
    ))
}
fn ps_quote(path: &Path) -> Result<String, HookInstallError> {
    Ok(format!(
        "'{}'",
        path.to_str()
            .ok_or(HookInstallError::InvalidCommandPath)?
            .replace('\'', "''")
    ))
}
fn write_atomic(path: &Path, value: &Value, original: &Snapshot) -> Result<(), HookInstallError> {
    let bytes = serialized(value)?;
    write_raw_atomic_with_backup(path, &bytes, original)
}

fn serialized(value: &Value) -> Result<Vec<u8>, HookInstallError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn serialized_snapshot(value: &Value) -> Result<Snapshot, HookInstallError> {
    Ok(Snapshot::from_bytes(serialized(value)?))
}

fn write_raw_atomic(
    path: &Path,
    bytes: &[u8],
    original: &Snapshot,
) -> Result<(), HookInstallError> {
    write_raw_atomic_inner(path, bytes, original, false)
}

fn write_raw_atomic_with_backup(
    path: &Path,
    bytes: &[u8],
    original: &Snapshot,
) -> Result<(), HookInstallError> {
    write_raw_atomic_inner(path, bytes, original, true)
}

fn write_raw_atomic_inner(
    path: &Path,
    bytes: &[u8],
    original: &Snapshot,
    preserve_backup: bool,
) -> Result<(), HookInstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| HookInstallError::InvalidShape("配置路径缺少父目录".into()))?;
    fs::create_dir_all(parent)?;
    let temp = sibling(
        path,
        &format!(".promptdock-desktop.tmp-{}", uuid::Uuid::new_v4()),
    );
    let r = (|| {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        ensure(path, original)?;
        if preserve_backup {
            if let Some(b) = &original.bytes {
                backup(path, b)?;
                ensure(path, original)?
            }
        }
        replace(&temp, path)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&temp);
    }
    r
}

fn restore_snapshot(
    path: &Path,
    original: &Snapshot,
    applied: &Snapshot,
) -> Result<(), HookInstallError> {
    if original.hash == applied.hash {
        return ensure(path, applied);
    }
    match &original.bytes {
        Some(bytes) => write_raw_atomic(path, bytes, applied),
        None => {
            ensure(path, applied)?;
            match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    Err(HookInstallError::ConfigChanged)
                }
                Err(error) => Err(error.into()),
            }
        }
    }
}
fn ensure(path: &Path, original: &Snapshot) -> Result<(), HookInstallError> {
    if snapshot(path)?.hash != original.hash {
        return Err(HookInstallError::ConfigChanged);
    }
    Ok(())
}
fn backup(path: &Path, bytes: &[u8]) -> Result<(), HookInstallError> {
    let dest = sibling(path, ".promptdock-desktop.bak");
    let temp = sibling(&dest, &format!(".tmp-{}", uuid::Uuid::new_v4()));
    let r = (|| {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        replace(&temp, &dest)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&temp);
    }
    r
}
fn lock_timeout(lock: &File) -> Result<(), HookInstallError> {
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match FileExt::try_lock_exclusive(lock) {
            Ok(()) => return Ok(()),
            Err(e) if contended(&e) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(HookInstallError::ConfigLockTimeout);
                }
                thread::sleep(LOCK_RETRY.min(deadline - now))
            }
            Err(e) => return Err(e.into()),
        }
    }
}
fn contended(e: &io::Error) -> bool {
    let x = fs2::lock_contended_error();
    match (e.raw_os_error(), x.raw_os_error()) {
        (Some(a), Some(b)) => a == b,
        _ => e.kind() == x.kind(),
    }
}
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut x = path.as_os_str().to_owned();
    x.push(suffix);
    PathBuf::from(x)
}
#[cfg(windows)]
fn replace(src: &Path, dst: &Path) -> Result<(), HookInstallError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let a: Vec<u16> = src.as_os_str().encode_wide().chain(Some(0)).collect();
    let b: Vec<u16> = dst.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe {
        MoveFileExW(
            a.as_ptr(),
            b.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(HookInstallError::Io(io::Error::last_os_error()));
    }
    Ok(())
}
#[cfg(not(windows))]
fn replace(src: &Path, dst: &Path) -> Result<(), HookInstallError> {
    fs::rename(src, dst)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    fn manager(d: &tempfile::TempDir) -> HookManager {
        HookManager::new(
            d.path().join("hooks.json"),
            d.path().join("agent-events.jsonl"),
            d.path().join("PromptDock.exe"),
        )
        .unwrap()
    }

    fn file_modified(path: &Path) -> SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }
    fn write_trust_records(
        manager: &HookManager,
        features: HookFeatures,
        hash_override: Option<&str>,
        enabled: Option<bool>,
    ) {
        let root = read_config(&manager.config_path).unwrap();
        let mut states = toml::map::Map::new();
        for spec in manager.desired_specs(features).unwrap() {
            let location = managed_hook_location(&root, spec.kind).unwrap().unwrap();
            let key = format!(
                "{}:{}:{}:{}",
                manager.config_path.display(),
                hook_event_key_label(spec.event_name),
                location.group_index,
                location.handler_index
            );
            let mut state = toml::map::Map::new();
            state.insert(
                "trusted_hash".into(),
                toml::Value::String(hash_override.map(str::to_owned).unwrap_or_else(|| {
                    hook_hash(spec.event_name, location.group, location.handler).unwrap()
                })),
            );
            if let Some(enabled) = enabled {
                state.insert("enabled".into(), toml::Value::Boolean(enabled));
            }
            states.insert(key, toml::Value::Table(state));
        }
        let mut hooks = toml::map::Map::new();
        hooks.insert("state".into(), toml::Value::Table(states));
        let mut config = toml::map::Map::new();
        config.insert("hooks".into(), toml::Value::Table(hooks));
        fs::write(
            manager.config_path.parent().unwrap().join("config.toml"),
            toml::to_string(&toml::Value::Table(config)).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn trust_requires_the_current_hash_for_every_required_handler() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        let features = HookFeatures::for_phase8(true, false);
        m.install_features(features).unwrap();

        assert_eq!(
            m.trust_state_for_features(features).unwrap(),
            HookTrustState::Untrusted
        );
        write_trust_records(&m, features, Some("sha256:stale"), None);
        assert_eq!(
            m.trust_state_for_features(features).unwrap(),
            HookTrustState::Modified
        );
        write_trust_records(&m, features, None, None);
        assert_eq!(
            m.trust_state_for_features(features).unwrap(),
            HookTrustState::Trusted
        );
        write_trust_records(&m, features, None, Some(false));
        assert_eq!(
            m.trust_state_for_features(features).unwrap(),
            HookTrustState::Disabled
        );
    }

    #[test]
    fn trust_keys_use_the_actual_group_index_after_foreign_hooks() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        fs::write(
            &m.config_path,
            br#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"foreign.exe"}]}]}}"#,
        )
        .unwrap();
        let features = HookFeatures::for_phase8(true, false);
        m.install_features(features).unwrap();
        write_trust_records(&m, features, None, None);
        assert_eq!(
            m.trust_state_for_features(features).unwrap(),
            HookTrustState::Trusted
        );
    }
    #[test]
    fn exact_three_hooks_and_permission_has_no_matcher() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        m.install().unwrap();
        let r = read_config(&m.config_path).unwrap();
        for (e, ev, mark) in [
            (
                "UserPromptSubmit",
                "codex-user-prompt",
                USER_PROMPT_HOOK_MARKER,
            ),
            ("Stop", "codex-stop", STOP_HOOK_MARKER),
            (
                "PermissionRequest",
                "codex-permission-request",
                PERMISSION_REQUEST_HOOK_MARKER,
            ),
        ] {
            let h = &r["hooks"][e][0]["hooks"][0];
            assert!(h["command"]
                .as_str()
                .unwrap()
                .contains(&format!("--capture-agent-event {ev} --inbox")));
            assert!(h["command"].as_str().unwrap().ends_with(mark));
            assert_eq!(h["async"], false);
            assert_eq!(h["timeout"], HOOK_TIMEOUT_SECONDS)
        }
        assert!(r["hooks"]["PermissionRequest"][0].get("matcher").is_none())
    }
    #[test]
    fn hook_deadline_allows_bounded_local_input_and_guard_acquisition() {
        // The helper has no second process-kill deadline. The host timeout is
        // the outer bound; input bytes and runtime-access acquisition are
        // bounded locally before a durable append.
        const {
            assert!(HOOK_TIMEOUT_SECONDS >= 8);
            assert!(crate::agent_event_capture::MAX_AGENT_EVENT_HOOK_INPUT_BYTES > 0);
        }
    }
    #[test]
    fn normalized_hash_matches_the_codex_canonical_contract() {
        let group = json!({
            "matcher": "ignored-for-stop",
            "hooks": [{
                "type": "command",
                "command": "posix",
                "commandWindows": "win",
                "timeout": 3,
                "async": false
            }]
        });
        let handler = &group["hooks"][0];
        let expected = if cfg!(windows) {
            "sha256:627992fdd3f95209595e4ab42304dcceb40039ac1a1005ee4b872d1a44f30de7"
        } else {
            "sha256:4355879148a5c1528c9f020538d43d1eb0adab247abe5b80068f2e8a3ba26f5e"
        };
        assert_eq!(hook_hash("Stop", &group, handler).unwrap(), expected);
    }
    #[test]
    fn phase_eight_desired_state_matrix_never_enables_attention() {
        assert_eq!(
            HookFeatures::for_phase8(false, false),
            HookFeatures {
                prompt_capture: true,
                run_lifecycle: false,
                attention: false,
                capture_agent_outputs: false,
            }
        );
        for (notifications, capture) in [(true, false), (false, true), (true, true)] {
            let features = HookFeatures::for_phase8(notifications, capture);
            assert!(features.prompt_capture);
            assert!(features.run_lifecycle);
            assert!(!features.attention);
            assert_eq!(features.capture_agent_outputs, capture);
        }
    }
    #[test]
    fn phase_nine_installs_permission_hook_only_for_enabled_attention_notifications() {
        assert!(!HookFeatures::for_phase9(false, true, false).attention);
        assert!(!HookFeatures::for_phase9(true, false, false).attention);
        let enabled = HookFeatures::for_phase9(true, true, false);
        assert!(enabled.prompt_capture);
        assert!(enabled.run_lifecycle);
        assert!(enabled.attention);
        assert!(!enabled.capture_agent_outputs);
    }
    #[test]
    fn phase_eight_features_keep_prompt_always_and_make_stop_output_capture_explicit() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        let prompt_only = HookFeatures {
            prompt_capture: true,
            run_lifecycle: false,
            attention: false,
            capture_agent_outputs: false,
        };
        m.reconcile_features(prompt_only).unwrap();
        let root = read_config(&m.config_path).unwrap();
        assert_eq!(
            root["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
            1
        );
        assert!(root["hooks"]["Stop"].as_array().is_none_or(Vec::is_empty));
        assert!(root["hooks"]["PermissionRequest"]
            .as_array()
            .is_none_or(Vec::is_empty));
        assert_eq!(
            m.installation_state_for_features(prompt_only).unwrap(),
            HookInstallationState::Current
        );

        let lifecycle_without_output = HookFeatures {
            run_lifecycle: true,
            ..prompt_only
        };
        m.reconcile_features(lifecycle_without_output).unwrap();
        let root = read_config(&m.config_path).unwrap();
        let stop = root["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(!stop.contains("--capture-agent-output"));
        assert_eq!(
            m.installation_state_for_features(lifecycle_without_output)
                .unwrap(),
            HookInstallationState::Current
        );

        let lifecycle_with_output = HookFeatures {
            capture_agent_outputs: true,
            ..lifecycle_without_output
        };
        m.reconcile_features(lifecycle_with_output).unwrap();
        let root = read_config(&m.config_path).unwrap();
        let stop = root["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(stop.contains("--capture-agent-output"));
        assert_eq!(
            m.installation_state_for_features(lifecycle_with_output)
                .unwrap(),
            HookInstallationState::Current
        );
    }
    #[test]
    fn idempotent_repair_foreign_and_uninstall() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        m.install().unwrap();
        let before = fs::read(&m.config_path).unwrap();
        m.reconcile().unwrap();
        assert_eq!(fs::read(&m.config_path).unwrap(), before);
        let mut r = read_config(&m.config_path).unwrap();
        r["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"hooks":[{"command":"foreign.exe"}]}));
        r["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"] = json!(5);
        fs::write(&m.config_path, serde_json::to_vec_pretty(&r).unwrap()).unwrap();
        assert_eq!(m.installation_state_for().unwrap().foreign_stop_handlers, 1);
        assert!(matches!(
            m.installation_state(),
            Ok(HookInstallationState::NeedsRepair { .. })
        ));
        m.reconcile().unwrap();
        assert_eq!(
            m.installation_state().unwrap(),
            HookInstallationState::Current
        );
        m.uninstall().unwrap();
        let r = read_config(&m.config_path).unwrap();
        assert_eq!(r["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert!(r["hooks"]["PermissionRequest"]
            .as_array()
            .unwrap()
            .is_empty())
    }

    #[test]
    fn no_op_reconcile_preserves_bytes_mtime_and_mixed_group_indexes() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        m.install().unwrap();
        let mut root = read_config(&m.config_path).unwrap();
        let managed = root["hooks"]["UserPromptSubmit"][0]["hooks"][0].clone();
        root["hooks"]["UserPromptSubmit"] = json!([
            {"matcher":"before", "hooks":[{"type":"command", "command":"before.exe"}]},
            {
                "matcher":"kept-on-mixed-group",
                "futureGroupField":{"kept":true},
                "hooks":[
                    {"type":"command", "command":"foreign-before.exe"},
                    managed,
                    {"type":"command", "command":"foreign-after.exe"}
                ]
            },
            {"matcher":"after", "hooks":[{"type":"command", "command":"after.exe"}]}
        ]);
        let mut bytes = serde_json::to_vec(&root).unwrap();
        bytes.extend_from_slice(b"  \r\n");
        fs::write(&m.config_path, &bytes).unwrap();
        thread::sleep(Duration::from_millis(1_100));
        let modified = file_modified(&m.config_path);
        let fingerprint = blake3::hash(&bytes);

        m.reconcile().unwrap();

        let after = fs::read(&m.config_path).unwrap();
        assert_eq!(after, bytes);
        assert_eq!(blake3::hash(&after), fingerprint);
        assert_eq!(file_modified(&m.config_path), modified);
        let after = read_config(&m.config_path).unwrap();
        assert!(after["hooks"]["UserPromptSubmit"][1]["hooks"][1]["command"]
            .as_str()
            .unwrap()
            .contains(USER_PROMPT_HOOK_MARKER));
    }

    #[test]
    fn repair_updates_the_unique_managed_handler_in_place() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        m.install().unwrap();
        let mut root = read_config(&m.config_path).unwrap();
        let mut managed = root["hooks"]["Stop"][0]["hooks"][0].clone();
        managed["timeout"] = json!(4);
        root["hooks"]["Stop"] = json!([
            {"matcher":"before", "hooks":[{"type":"command", "command":"before.exe"}]},
            {
                "futureGroupField":"keep",
                "hooks":[
                    {"type":"command", "command":"foreign-before.exe"},
                    managed,
                    {"type":"command", "command":"foreign-after.exe"}
                ]
            },
            {"matcher":"after", "hooks":[{"type":"command", "command":"after.exe"}]}
        ]);
        fs::write(&m.config_path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();

        m.reconcile().unwrap();

        let repaired = read_config(&m.config_path).unwrap();
        let groups = repaired["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0]["matcher"], "before");
        assert_eq!(groups[1]["futureGroupField"], "keep");
        assert_eq!(groups[1]["hooks"][0]["command"], "foreign-before.exe");
        assert!(groups[1]["hooks"][1]["command"]
            .as_str()
            .unwrap()
            .contains(STOP_HOOK_MARKER));
        assert_eq!(groups[1]["hooks"][1]["timeout"], HOOK_TIMEOUT_SECONDS);
        assert_eq!(groups[1]["hooks"][2]["command"], "foreign-after.exe");
        assert_eq!(groups[2]["matcher"], "after");
    }

    #[test]
    fn missing_handlers_are_appended_without_moving_existing_managed_handlers() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        let features = HookFeatures::all();
        m.install_features(features).unwrap();
        let mut root = read_config(&m.config_path).unwrap();
        let stop = root["hooks"]["Stop"][0]["hooks"][0].clone();
        root["hooks"]["Stop"] = json!([
            {"hooks":[{"type":"command", "command":"foreign.exe"}]},
            {"hooks":[stop, {"type":"command", "command":"foreign-after.exe"}]}
        ]);
        root["hooks"]["PermissionRequest"] = json!([]);
        fs::write(&m.config_path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();

        m.reconcile_features(features).unwrap();

        let repaired = read_config(&m.config_path).unwrap();
        assert!(repaired["hooks"]["Stop"][1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(STOP_HOOK_MARKER));
        assert_eq!(
            repaired["hooks"]["Stop"][1]["hooks"][1]["command"],
            "foreign-after.exe"
        );
        assert_eq!(
            repaired["hooks"]["PermissionRequest"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn duplicate_managed_handlers_are_an_explicit_conflict_and_are_not_rewritten() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        m.install().unwrap();
        let mut root = read_config(&m.config_path).unwrap();
        let duplicate = root["hooks"]["Stop"][0]["hooks"][0].clone();
        root["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(json!({"hooks":[duplicate]}));
        let bytes = serde_json::to_vec_pretty(&root).unwrap();
        fs::write(&m.config_path, &bytes).unwrap();

        let error = m.reconcile().unwrap_err();

        assert!(error.to_string().contains("多个 PromptDock Stop handler"));
        assert_eq!(fs::read(&m.config_path).unwrap(), bytes);
    }

    #[test]
    fn wrong_event_and_permission_matcher_are_explicit_conflicts() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        m.install().unwrap();
        let mut wrong_event = read_config(&m.config_path).unwrap();
        let permission = wrong_event["hooks"]["PermissionRequest"][0]["hooks"][0].clone();
        wrong_event["hooks"]["PermissionRequest"] = json!([]);
        wrong_event["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(json!({"hooks":[permission]}));
        let bytes = serde_json::to_vec_pretty(&wrong_event).unwrap();
        fs::write(&m.config_path, &bytes).unwrap();
        let error = m.reconcile().unwrap_err();
        assert!(error
            .to_string()
            .contains("PermissionRequest handler 不在 PermissionRequest 事件中"));
        assert_eq!(fs::read(&m.config_path).unwrap(), bytes);

        let mut matcher = read_config(&m.config_path).unwrap();
        matcher["hooks"]["Stop"].as_array_mut().unwrap().pop();
        matcher["hooks"]["PermissionRequest"] = json!([{
            "matcher":"unexpected",
            "hooks":[permission]
        }]);
        let bytes = serde_json::to_vec_pretty(&matcher).unwrap();
        fs::write(&m.config_path, &bytes).unwrap();
        let error = m.reconcile().unwrap_err();
        assert!(error.to_string().contains("PermissionRequest matcher"));
        assert_eq!(fs::read(&m.config_path).unwrap(), bytes);
    }

    #[test]
    fn failed_intent_write_rolls_back_the_config_change() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        fs::create_dir(&m.intent_path).unwrap();

        assert!(m.install().is_err());
        assert!(!m.config_path.exists());
    }

    #[test]
    fn plan_reports_changed_events_and_no_change_fingerprints() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        let features = HookFeatures::for_phase9(true, true, false);

        let initial = m.plan_reconcile_features(features).unwrap();
        assert_eq!(initial.outcome, HookInstallPlanOutcome::Installed);
        assert!(initial.definition_changed);
        assert_eq!(
            initial.changed_events,
            ["UserPromptSubmit", "Stop", "PermissionRequest"]
        );
        assert!(initial.source_fingerprint.is_none());
        assert!(initial.planned_fingerprint.is_some());
        assert_eq!(initial.review_required, Some(true));
        assert!(!initial.external_review_required);

        m.install_features_expected(features, initial.source_fingerprint.as_deref())
            .unwrap();
        let current = m.plan_reconcile_features(features).unwrap();
        assert_eq!(current.outcome, HookInstallPlanOutcome::NoChange);
        assert!(!current.definition_changed);
        assert!(current.changed_events.is_empty());
        assert_eq!(current.source_fingerprint, current.planned_fingerprint);
        assert_eq!(current.review_required, Some(false));
        assert!(!current.external_review_required);
    }

    #[test]
    fn disabling_a_standalone_optional_handler_keeps_later_foreign_group_indexes() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        m.install().unwrap();
        let mut root = read_config(&m.config_path).unwrap();
        root["hooks"]["PermissionRequest"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "matcher":"foreign-after",
                "hooks":[{"type":"command", "command":"foreign-after.exe"}]
            }));
        fs::write(&m.config_path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
        let features = HookFeatures::for_phase8(true, false);

        let plan = m.plan_reconcile_features(features).unwrap();
        assert!(!plan.external_review_required);
        m.reconcile_features(features).unwrap();

        let after = read_config(&m.config_path).unwrap();
        let groups = after["hooks"]["PermissionRequest"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert!(groups[0]["hooks"].as_array().unwrap().is_empty());
        assert_eq!(groups[1]["matcher"], "foreign-after");
        assert_eq!(groups[1]["hooks"][0]["command"], "foreign-after.exe");
    }

    #[test]
    fn disabling_a_mixed_optional_handler_reports_foreign_handler_index_impact() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        m.install().unwrap();
        let mut root = read_config(&m.config_path).unwrap();
        let managed = root["hooks"]["PermissionRequest"][0]["hooks"][0].clone();
        root["hooks"]["PermissionRequest"] = json!([
            {
                "hooks":[
                    {"type":"command", "command":"foreign-before.exe"},
                    managed,
                    {"type":"command", "command":"foreign-after.exe"}
                ]
            },
            {
                "matcher":"later-group",
                "hooks":[{"type":"command", "command":"later-group.exe"}]
            }
        ]);
        fs::write(&m.config_path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
        let features = HookFeatures::for_phase8(true, false);

        let plan = m.plan_reconcile_features(features).unwrap();
        assert!(plan.external_review_required);
        m.reconcile_features(features).unwrap();

        let after = read_config(&m.config_path).unwrap();
        let groups = after["hooks"]["PermissionRequest"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["hooks"][0]["command"], "foreign-before.exe");
        assert_eq!(groups[0]["hooks"][1]["command"], "foreign-after.exe");
        assert_eq!(groups[1]["matcher"], "later-group");
        assert_eq!(groups[1]["hooks"][0]["command"], "later-group.exe");
    }

    #[test]
    fn expected_fingerprint_rejects_an_external_change_without_overwriting_it() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        let features = HookFeatures::for_phase8(true, false);
        let plan = m.plan_reconcile_features(features).unwrap();
        let external = br#"{"external":true}"#;
        fs::write(&m.config_path, external).unwrap();

        assert!(matches!(
            m.install_features_expected(features, plan.source_fingerprint.as_deref()),
            Err(HookInstallError::ConfigChanged)
        ));
        assert_eq!(fs::read(&m.config_path).unwrap(), external);
        assert!(!m.intent_path.exists());
    }

    #[test]
    fn registration_id_is_stable_in_both_generated_commands() {
        let d = tempfile::tempdir().unwrap();
        let id = "018f6f16-2f84-7c00-aef0-111111111111";
        let m = manager(&d).with_registration_id(id.into());
        let spec = m
            .desired_specs(HookFeatures::for_phase8(true, false))
            .unwrap()
            .into_iter()
            .find(|spec| spec.kind == ManagedHookKind::Stop)
            .unwrap();

        assert!(spec
            .command
            .contains(&format!("--hook-registration-id {id}")));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(
                spec.command_windows
                    .split("-EncodedCommand ")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap(),
            )
            .unwrap();
        let script = String::from_utf16(
            &decoded
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[lo, hi]| u16::from_le_bytes([lo, hi]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(script.contains(&format!("--hook-registration-id {id}")));
    }

    #[test]
    fn plan_distinguishes_registration_changes_from_other_definition_repairs() {
        let d = tempfile::tempdir().unwrap();
        let id = "018f6f16-2f84-7c00-aef0-222222222222";
        let first = manager(&d).with_registration_id(id.into());
        let features = HookFeatures::for_phase8(true, false);
        let initial = first.plan_reconcile_features(features).unwrap();
        assert!(initial.registration_changed);
        first.install_features(features).unwrap();

        let moved = HookManager::new(
            first.config_path.clone(),
            first.inbox_path.clone(),
            d.path().join("PromptDock-updated.exe"),
        )
        .unwrap()
        .with_registration_id(id.into());
        let moved_plan = moved.plan_reconcile_features(features).unwrap();
        assert!(moved_plan.definition_changed);
        assert!(!moved_plan.registration_changed);

        let rotated = moved.with_registration_id("018f6f16-2f84-7c00-aef0-333333333333".into());
        assert!(
            rotated
                .plan_reconcile_features(features)
                .unwrap()
                .registration_changed
        );
    }
    #[test]
    fn settings_transaction_rollback_restores_absence_and_exact_existing_bytes() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        let features = HookFeatures::for_phase8(true, true);

        let transaction = m.begin_settings_reconcile(features).unwrap().unwrap();
        assert!(m.config_path.exists());
        transaction.rollback().unwrap();
        assert!(!m.config_path.exists());

        m.reconcile_features(HookFeatures::for_phase8(false, false))
            .unwrap();
        let mut root = read_config(&m.config_path).unwrap();
        root["hooks"]["Stop"] = json!([{
            "matcher": "foreign",
            "hooks": [{"type": "command", "command": "foreign.exe"}]
        }]);
        let mut original = serde_json::to_vec(&root).unwrap();
        original.extend_from_slice(b"  \r\n");
        fs::write(&m.config_path, &original).unwrap();

        let transaction = m.begin_settings_reconcile(features).unwrap().unwrap();
        assert_ne!(fs::read(&m.config_path).unwrap(), original);
        transaction.rollback().unwrap();
        assert_eq!(fs::read(&m.config_path).unwrap(), original);
    }

    #[test]
    fn transaction_rollback_never_overwrites_an_external_change() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        let transaction = m
            .begin_settings_reconcile(HookFeatures::for_phase8(true, false))
            .unwrap()
            .unwrap();
        let external = br#"{"external":true}"#;
        fs::write(&m.config_path, external).unwrap();

        assert!(matches!(
            transaction.rollback(),
            Err(HookInstallError::ConfigChanged)
        ));
        assert_eq!(fs::read(&m.config_path).unwrap(), external);
    }

    #[test]
    fn startup_repairs_absent_required_hooks_but_honors_explicit_uninstall() {
        let automatic = tempfile::tempdir().unwrap();
        let automatic_manager = manager(&automatic);
        let required = HookFeatures::for_phase8(true, false);
        automatic_manager.reconcile_on_startup(required).unwrap();
        assert_eq!(
            automatic_manager
                .installation_state_for_features(required)
                .unwrap(),
            HookInstallationState::Current
        );

        let capture = tempfile::tempdir().unwrap();
        let capture_manager = manager(&capture);
        let capture_required = HookFeatures::for_phase8(false, true);
        capture_manager
            .reconcile_on_startup(capture_required)
            .unwrap();
        assert_eq!(
            capture_manager
                .installation_state_for_features(capture_required)
                .unwrap(),
            HookInstallationState::Current
        );

        let explicitly_installed = tempfile::tempdir().unwrap();
        let installed_manager = manager(&explicitly_installed);
        let prompt_only = HookFeatures::for_phase8(false, false);
        installed_manager.install_features(prompt_only).unwrap();
        fs::remove_file(&installed_manager.config_path).unwrap();
        manager(&explicitly_installed)
            .reconcile_on_startup(prompt_only)
            .unwrap();
        assert!(installed_manager.config_path.exists());

        let opted_out = tempfile::tempdir().unwrap();
        let opted_out_manager = manager(&opted_out);
        opted_out_manager.uninstall().unwrap();
        assert!(manager(&opted_out)
            .begin_settings_reconcile(required)
            .unwrap()
            .is_none());
        manager(&opted_out).reconcile_on_startup(required).unwrap();
        assert!(!opted_out_manager.config_path.exists());
        assert_eq!(
            opted_out_manager
                .installation_state_for_features(required)
                .unwrap(),
            HookInstallationState::Absent
        );
    }
    #[test]
    fn windows_silent_and_special_paths_are_safe() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(&d);
        let c = m.windows_command(ManagedHookKind::Stop);
        assert!(!c.contains('"'));
        let enc = c
            .split("-EncodedCommand ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let b = base64::engine::general_purpose::STANDARD
            .decode(enc)
            .unwrap();
        let s = String::from_utf16(
            &b.as_chunks::<2>()
                .0
                .iter()
                .map(|&[lo, hi]| u16::from_le_bytes([lo, hi]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(s.contains("--capture-agent-event codex-stop --inbox"));
        assert!(s.contains("catch{exit 0}"));
        assert!(s.contains("2>$null"));
        assert!(!s.contains("*>$null"));
        assert!(!s.contains("LASTEXITCODE"));
        assert!(!s.contains("Error.WriteLine"));
        fs::write(&m.config_path, b"bad").unwrap();
        assert!(m.install().is_err());
        assert_eq!(fs::read(&m.config_path).unwrap(), b"bad");
        assert!(matches!(
            HookManager::new(
                d.path().join("x"),
                PathBuf::from("relative"),
                d.path().join("exe")
            ),
            Err(HookInstallError::InvalidShape(_))
        ))
    }
}
