use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use serde::Deserialize;

use crate::error::AppError;

use super::{
    model::{DesktopDiscoverySource, DesktopHostCandidate, DesktopLauncherConfig, DesktopProduct},
    proxy::apply_config_to_std_command,
    HostFocusStatus,
};

const APPX_QUERY_TIMEOUT: Duration = Duration::from_secs(15);
const APPX_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const APPX_STDOUT_LIMIT: usize = 64 * 1024;
const APPX_STDERR_LIMIT: usize = 128 * 1024;
// Preserve the donor's cross-entry-point mutex identity.
const STARTUP_LOCK_NAME: &str = "codex-proxy-guard-startup.lock";
const STARTUP_COOLDOWN: Duration = Duration::from_secs(5);

// No user-controlled value is interpolated into this script. Its results are
// returned as candidates; discovery never selects a product for the user.
#[cfg(windows)]
const APPX_QUERY_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$records = @()
foreach ($name in @('OpenAI.Codex', 'OpenAI.ChatGPT-Desktop')) {
  foreach ($package in @(Get-AppxPackage -Name $name | Sort-Object Version -Descending)) {
    $manifest = Get-AppxPackageManifest -Package $package.PackageFullName
    $application = @($manifest.Package.Applications.Application) | Select-Object -First 1
    $records += [PSCustomObject]@{
      package_name = [string]$package.Name
      package_version = [string]$package.Version
      architecture = [string]$package.Architecture
      install_location = [string]$package.InstallLocation
      manifest_executable = [string]$application.Executable
    }
  }
}
$records | ConvertTo-Json -Compress
"#;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppxRecord {
    package_name: String,
    package_version: String,
    architecture: String,
    install_location: PathBuf,
    #[serde(default)]
    manifest_executable: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AppxOutput {
    One(AppxRecord),
    Many(Vec<AppxRecord>),
}

impl AppxOutput {
    fn into_records(self) -> Vec<AppxRecord> {
        match self {
            Self::One(record) => vec![record],
            Self::Many(records) => records,
        }
    }
}

pub(crate) async fn discover_appx() -> Result<Vec<DesktopHostCandidate>, AppError> {
    #[cfg(not(windows))]
    {
        Err(safe_error(
            "DESKTOP_DISCOVERY_UNSUPPORTED",
            "ChatGPT Desktop discovery is only available on Windows.",
        ))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                APPX_QUERY_SCRIPT,
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = bounded_output(
            command,
            APPX_QUERY_TIMEOUT,
            APPX_STDOUT_LIMIT,
            APPX_STDERR_LIMIT,
            "APPX_QUERY_FAILED",
        )
        .await?;
        if !output.status.success() {
            return Err(safe_error(
                "APPX_QUERY_FAILED",
                "ChatGPT Desktop discovery failed.",
            ));
        }
        parse_appx_output(&output.stdout)
    }
}

fn parse_appx_output(stdout: &[u8]) -> Result<Vec<DesktopHostCandidate>, AppError> {
    if stdout.len() > APPX_STDOUT_LIMIT {
        return Err(safe_error(
            "APPX_OUTPUT_TOO_LARGE",
            "ChatGPT Desktop discovery returned too much data.",
        ));
    }
    let text = std::str::from_utf8(stdout).map_err(|_| {
        safe_error(
            "APPX_QUERY_INVALID",
            "ChatGPT Desktop discovery returned invalid data.",
        )
    })?;
    if text.trim().is_empty() {
        return Err(safe_error(
            "DESKTOP_NOT_INSTALLED",
            "ChatGPT Desktop was not found.",
        ));
    }
    let parsed: AppxOutput = serde_json::from_str(text).map_err(|_| {
        safe_error(
            "APPX_QUERY_INVALID",
            "ChatGPT Desktop discovery returned invalid data.",
        )
    })?;
    candidates_from_records(parsed.into_records())
}

fn candidates_from_records(
    records: Vec<AppxRecord>,
) -> Result<Vec<DesktopHostCandidate>, AppError> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for record in records {
        let Some((product, product_label)) = product_for_package(&record.package_name) else {
            continue;
        };
        let Ok((executable, discovery_source)) = resolve_appx_executable(&record) else {
            continue;
        };
        if !seen.insert(comparable_path(&executable)) {
            continue;
        }
        candidates.push(DesktopHostCandidate {
            executable,
            product,
            product_label: product_label.to_owned(),
            package_name: Some(record.package_name),
            package_version: Some(record.package_version),
            architecture: Some(record.architecture),
            discovery_source,
        });
    }
    if candidates.is_empty() {
        return Err(safe_error(
            "DESKTOP_EXECUTABLE_MISSING",
            "ChatGPT Desktop is installed, but its executable was not found.",
        ));
    }
    Ok(candidates)
}

fn product_for_package(package_name: &str) -> Option<(DesktopProduct, &'static str)> {
    if package_name.eq_ignore_ascii_case("OpenAI.Codex") {
        Some((DesktopProduct::ChatGptDesktop, "ChatGPT Desktop"))
    } else if package_name.eq_ignore_ascii_case("OpenAI.ChatGPT-Desktop") {
        Some((
            DesktopProduct::ChatGptDesktopClassic,
            "ChatGPT Desktop (Classic)",
        ))
    } else {
        None
    }
}

fn resolve_appx_executable(
    record: &AppxRecord,
) -> Result<(PathBuf, DesktopDiscoverySource), AppError> {
    let install = record.install_location.canonicalize().map_err(|_| {
        safe_error(
            "DESKTOP_EXECUTABLE_MISSING",
            "ChatGPT Desktop is installed, but its executable was not found.",
        )
    })?;
    if !record.manifest_executable.trim().is_empty() {
        if let Ok(executable) = contained_executable(&install, &record.manifest_executable) {
            return Ok((executable, DesktopDiscoverySource::AppxManifest));
        }
    }
    for relative in ["app/ChatGPT.exe", "app/Codex.exe"] {
        if let Ok(executable) = contained_executable(&install, relative) {
            return Ok((executable, DesktopDiscoverySource::AppxFallback));
        }
    }
    Err(safe_error(
        "DESKTOP_EXECUTABLE_MISSING",
        "ChatGPT Desktop is installed, but its executable was not found.",
    ))
}

fn contained_executable(install: &Path, relative: &str) -> Result<PathBuf, AppError> {
    let candidate = Path::new(relative);
    if relative.starts_with('/')
        || relative.starts_with('\\')
        || has_windows_drive_prefix(relative)
        || candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        return Err(safe_error(
            "APPX_MANIFEST_EXECUTABLE_INVALID",
            "ChatGPT Desktop package metadata was rejected.",
        ));
    }
    let executable = install.join(candidate).canonicalize().map_err(|_| {
        safe_error(
            "DESKTOP_EXECUTABLE_MISSING",
            "ChatGPT Desktop is installed, but its executable was not found.",
        )
    })?;
    if !executable.is_file() || !path_is_within(&executable, install) {
        return Err(safe_error(
            "APPX_MANIFEST_EXECUTABLE_INVALID",
            "ChatGPT Desktop package metadata was rejected.",
        ));
    }
    Ok(executable)
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn path_is_within(candidate: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let candidate = normalize_windows_path(candidate);
        let root = normalize_windows_path(root);
        candidate.starts_with(&format!("{}\\", root.trim_end_matches('\\')))
    }
    #[cfg(not(windows))]
    {
        candidate.starts_with(root) && candidate.components().count() > root.components().count()
    }
}

pub(crate) fn selected_candidate(
    config: &DesktopLauncherConfig,
    candidates: &[DesktopHostCandidate],
) -> Result<Option<DesktopHostCandidate>, AppError> {
    let Some(selected) = config.selected_executable.as_ref() else {
        return Ok(None);
    };
    if !selected.is_absolute() || !selected.is_file() {
        return Err(safe_error(
            "DESKTOP_EXECUTABLE_MISSING",
            "The selected ChatGPT Desktop executable was not found.",
        ));
    }
    let executable = selected.canonicalize().map_err(|_| {
        safe_error(
            "DESKTOP_EXECUTABLE_MISSING",
            "The selected ChatGPT Desktop executable was not found.",
        )
    })?;
    if let Some(candidate) = candidates
        .iter()
        .find(|candidate| comparable_path(&candidate.executable) == comparable_path(&executable))
    {
        return Ok(Some(candidate.clone()));
    }
    Ok(Some(DesktopHostCandidate {
        executable,
        product: DesktopProduct::ExplicitExecutable,
        product_label: "ChatGPT Desktop".to_owned(),
        package_name: None,
        package_version: None,
        architecture: None,
        discovery_source: DesktopDiscoverySource::ExplicitExecutable,
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessRecord {
    pid: u32,
    parent_pid: u32,
    executable: PathBuf,
}

pub(crate) fn desktop_root_pid(executable: &Path) -> Result<Option<u32>, AppError> {
    let records = enumerate_processes()?;
    let target = comparable_path(executable);
    let matching: HashSet<u32> = records
        .iter()
        .filter(|record| comparable_path(&record.executable) == target)
        .map(|record| record.pid)
        .collect();
    Ok(records
        .iter()
        .filter(|record| matching.contains(&record.pid) && !matching.contains(&record.parent_pid))
        .map(|record| record.pid)
        .min())
}

/// Focuses only one visible, unowned top-level window belonging to the exact
/// persisted executable. Windows process handles remain open while windows are
/// enumerated, so a terminated PID cannot be recycled into a different host.
pub(crate) fn focus_host(executable: &Path) -> HostFocusStatus {
    #[cfg(windows)]
    {
        focus_host_windows(executable)
    }
    #[cfg(not(windows))]
    {
        let _ = executable;
        HostFocusStatus::HostNotRunning
    }
}

fn focus_status_for_candidates(
    candidate_count: usize,
    foreground_succeeded: Option<bool>,
) -> HostFocusStatus {
    match candidate_count {
        0 => HostFocusStatus::WindowNotFound,
        1 if foreground_succeeded == Some(true) => HostFocusStatus::Focused,
        1 => HostFocusStatus::ForegroundDenied,
        _ => HostFocusStatus::MultipleCandidates,
    }
}

#[cfg(windows)]
struct HeldProcess {
    pid: u32,
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for HeldProcess {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

#[cfg(windows)]
fn focus_host_windows(executable: &Path) -> HostFocusStatus {
    use std::collections::HashSet;
    use windows_sys::core::BOOL;
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM},
        System::Threading::{GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindow, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
            GW_OWNER,
        },
    };

    let Ok(records) = enumerate_processes() else {
        return HostFocusStatus::HostNotRunning;
    };
    let current_pid = unsafe { GetCurrentProcessId() };
    let target = comparable_path(executable);
    let held: Vec<HeldProcess> = records
        .iter()
        .filter(|record| {
            record.pid != current_pid && !is_descendant_of(record.pid, current_pid, &records)
        })
        .filter_map(|record| {
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, record.pid) };
            if handle.is_null()
                || process_image_for_handle(handle).as_deref() != Some(target.as_str())
            {
                if !handle.is_null() {
                    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
                }
                return None;
            }
            Some(HeldProcess {
                pid: record.pid,
                handle,
            })
        })
        .collect();
    if held.is_empty() {
        return HostFocusStatus::HostNotRunning;
    }

    struct WindowSearch {
        pids: HashSet<u32>,
        windows: Vec<HWND>,
    }
    unsafe extern "system" fn collect_window(hwnd: HWND, value: LPARAM) -> BOOL {
        // The pointer is valid only for this synchronous EnumWindows call.
        let search = unsafe { &mut *(value as *mut WindowSearch) };
        if unsafe { IsWindowVisible(hwnd) } == 0 || !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null()
        {
            return 1;
        }
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if search.pids.contains(&pid) {
            search.windows.push(hwnd);
        }
        1
    }

    let mut search = WindowSearch {
        pids: held.iter().map(|process| process.pid).collect(),
        windows: Vec::new(),
    };
    // `held` intentionally remains alive through SetForegroundWindow.
    if unsafe {
        EnumWindows(
            Some(collect_window),
            &mut search as *mut WindowSearch as LPARAM,
        )
    } == 0
    {
        return HostFocusStatus::WindowNotFound;
    }
    match search.windows.as_slice() {
        [] => HostFocusStatus::WindowNotFound,
        [window] => {
            focus_status_for_candidates(1, Some(unsafe { SetForegroundWindow(*window) } != 0))
        }
        windows => focus_status_for_candidates(windows.len(), None),
    }
}

#[cfg(windows)]
fn process_image_for_handle(handle: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};

    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut length)
    } == 0
    {
        return None;
    }
    buffer.truncate(length as usize);
    Some(comparable_path(Path::new(&String::from_utf16_lossy(
        &buffer,
    ))))
}

fn is_descendant_of(pid: u32, ancestor: u32, records: &[ProcessRecord]) -> bool {
    let mut current = pid;
    for _ in 0..64 {
        let Some(record) = records.iter().find(|record| record.pid == current) else {
            return false;
        };
        if record.parent_pid == ancestor {
            return true;
        }
        if record.parent_pid == 0 || record.parent_pid == current {
            return false;
        }
        current = record.parent_pid;
    }
    false
}

fn comparable_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        normalize_windows_path(path)
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
}

#[cfg(windows)]
fn normalize_windows_path(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('/', "\\").to_lowercase();
    if let Some(rest) = normalized.strip_prefix(r"\\?\unc\") {
        return format!(r"\\{rest}");
    }
    normalized
        .strip_prefix(r"\\?\")
        .or_else(|| normalized.strip_prefix(r"\??\"))
        .unwrap_or(&normalized)
        .to_owned()
}

#[cfg(windows)]
fn enumerate_processes() -> Result<Vec<ProcessRecord>, AppError> {
    use std::mem::size_of;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(safe_error(
            "PROCESS_QUERY_FAILED",
            "Desktop process status is unavailable.",
        ));
    }
    struct HandleGuard(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    let _snapshot_guard = HandleGuard(snapshot);
    let mut records = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    let mut success = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while success {
        let process =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID) };
        if !process.is_null() {
            let process_guard = HandleGuard(process);
            let mut buffer = vec![0_u16; 32_768];
            let mut length = buffer.len() as u32;
            if unsafe {
                QueryFullProcessImageNameW(
                    process_guard.0,
                    PROCESS_NAME_WIN32,
                    buffer.as_mut_ptr(),
                    &mut length,
                )
            } != 0
            {
                buffer.truncate(length as usize);
                records.push(ProcessRecord {
                    pid: entry.th32ProcessID,
                    parent_pid: entry.th32ParentProcessID,
                    executable: PathBuf::from(String::from_utf16_lossy(&buffer)),
                });
            }
        }
        success = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    if unsafe { windows_sys::Win32::Foundation::GetLastError() } != ERROR_NO_MORE_FILES {
        return Err(safe_error(
            "PROCESS_QUERY_FAILED",
            "Desktop process status is unavailable.",
        ));
    }
    Ok(records)
}

#[cfg(not(windows))]
fn enumerate_processes() -> Result<Vec<ProcessRecord>, AppError> {
    Ok(Vec::new())
}

#[derive(Debug)]
struct StartupLock {
    file: File,
}

impl StartupLock {
    fn acquire() -> Result<Self, AppError> {
        Self::acquire_at(std::env::temp_dir().join(STARTUP_LOCK_NAME))
    }

    fn acquire_at(path: PathBuf) -> Result<Self, AppError> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|_| safe_error("LAUNCH_BUSY", "Another launcher is starting Desktop."))?;
        file.try_lock_exclusive()
            .map_err(|_| safe_error("LAUNCH_BUSY", "Another launcher is starting Desktop."))?;
        let mut timestamp = String::new();
        file.read_to_string(&mut timestamp)
            .map_err(|_| safe_error("LAUNCH_BUSY", "Desktop launcher state is unavailable."))?;
        let now = now_millis();
        if timestamp
            .trim()
            .parse::<u128>()
            .ok()
            .is_some_and(|last| now.saturating_sub(last) < STARTUP_COOLDOWN.as_millis())
        {
            return Err(safe_error(
                "LAUNCH_BUSY",
                "Desktop was launched recently. Please wait and try again.",
            ));
        }
        Ok(Self { file })
    }

    fn note_spawned(&mut self) -> Result<(), AppError> {
        self.file
            .set_len(0)
            .and_then(|()| self.file.seek(SeekFrom::Start(0)).map(|_| ()))
            .and_then(|()| write!(self.file, "{}", now_millis()))
            .and_then(|()| self.file.sync_all())
            .map_err(|_| {
                safe_error(
                    "DESKTOP_LAUNCH_FAILED",
                    "Desktop process could not be started.",
                )
            })
    }
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub(crate) fn spawn_desktop(
    candidate: &DesktopHostCandidate,
    refuse_if_running: bool,
    proxy: &super::model::LauncherProxyConfig,
) -> Result<(u32, Option<String>), AppError> {
    let mut startup_lock = StartupLock::acquire()?;
    if refuse_if_running && desktop_root_pid(&candidate.executable)?.is_some() {
        return Err(safe_error(
            "DESKTOP_ALREADY_RUNNING",
            "ChatGPT Desktop is already running. Restart it to apply new proxy settings.",
        ));
    }
    if !candidate.executable.is_file() {
        return Err(safe_error(
            "DESKTOP_EXECUTABLE_MISSING",
            "The selected ChatGPT Desktop executable was not found.",
        ));
    }
    let mut command = std::process::Command::new(&candidate.executable);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // This validates an enabled proxy before `spawn`; an invalid proxy can
    // never degrade into a direct launch.
    let proxy_endpoint = apply_config_to_std_command(&mut command, proxy)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    let child = command.spawn().map_err(|_| {
        safe_error(
            "DESKTOP_LAUNCH_FAILED",
            "ChatGPT Desktop process could not be started.",
        )
    })?;
    startup_lock.note_spawned()?;
    Ok((child.id(), proxy_endpoint))
}

#[cfg(windows)]
struct BoundedOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    #[allow(dead_code)]
    stderr: Vec<u8>,
}

#[cfg(windows)]
enum CapturedStream {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

#[cfg(windows)]
async fn bounded_output(
    mut command: tokio::process::Command,
    deadline: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
    error_code: &'static str,
) -> Result<BoundedOutput, AppError> {
    use tokio::io::AsyncReadExt;

    let mut child = command.spawn().map_err(|_| {
        safe_error(
            error_code,
            "ChatGPT Desktop discovery could not be started.",
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        safe_error(
            error_code,
            "ChatGPT Desktop discovery could not be started.",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        safe_error(
            error_code,
            "ChatGPT Desktop discovery could not be started.",
        )
    })?;
    let mut readers = tokio::task::JoinSet::new();
    readers.spawn(async move {
        let mut bytes = Vec::new();
        stdout
            .take((stdout_limit + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map(|_| CapturedStream::Stdout(bytes))
    });
    readers.spawn(async move {
        let mut bytes = Vec::new();
        stderr
            .take((stderr_limit + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map(|_| CapturedStream::Stderr(bytes))
    });
    let deadline = tokio::time::Instant::now() + deadline;
    let status = match tokio::time::timeout_at(deadline, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => {
            abort_and_drain(&mut readers).await;
            kill_and_reap(&mut child).await;
            return Err(safe_error(error_code, "ChatGPT Desktop discovery failed."));
        }
        Err(_) => {
            abort_and_drain(&mut readers).await;
            kill_and_reap(&mut child).await;
            return Err(safe_error(
                "APPX_QUERY_TIMEOUT",
                "ChatGPT Desktop discovery timed out.",
            ));
        }
    };
    let captured = tokio::time::timeout_at(deadline, async {
        let mut stdout = None;
        let mut stderr = None;
        while let Some(result) = readers.join_next().await {
            match result {
                Ok(Ok(CapturedStream::Stdout(bytes))) => stdout = Some(bytes),
                Ok(Ok(CapturedStream::Stderr(bytes))) => stderr = Some(bytes),
                Ok(Err(_)) | Err(_) => return None,
            }
        }
        Some((stdout?, stderr?))
    })
    .await;
    let (stdout, stderr) = match captured {
        Ok(Some(captured)) => captured,
        Ok(None) => {
            abort_and_drain(&mut readers).await;
            kill_and_reap(&mut child).await;
            return Err(safe_error(error_code, "ChatGPT Desktop discovery failed."));
        }
        Err(_) => {
            abort_and_drain(&mut readers).await;
            kill_and_reap(&mut child).await;
            return Err(safe_error(
                "APPX_QUERY_TIMEOUT",
                "ChatGPT Desktop discovery timed out.",
            ));
        }
    };
    if stdout.len() > stdout_limit || stderr.len() > stderr_limit {
        return Err(safe_error(
            "APPX_OUTPUT_TOO_LARGE",
            "ChatGPT Desktop discovery returned too much data.",
        ));
    }
    Ok(BoundedOutput {
        status,
        stdout,
        stderr,
    })
}

#[cfg(windows)]
async fn abort_and_drain<T: 'static>(tasks: &mut tokio::task::JoinSet<T>) {
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

#[cfg(windows)]
async fn kill_and_reap(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(APPX_REAP_TIMEOUT, child.wait()).await;
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn safe_error(code: &'static str, message: &'static str) -> AppError {
    AppError::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(root: &Path, package: &str, version: &str) -> AppxRecord {
        AppxRecord {
            package_name: package.into(),
            package_version: version.into(),
            architecture: "x64".into(),
            install_location: root.into(),
            manifest_executable: "app/ChatGPT.exe".into(),
        }
    }

    fn fixture_package() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("app")).unwrap();
        std::fs::write(root.path().join("app/ChatGPT.exe"), b"").unwrap();
        root
    }

    #[test]
    fn multiple_products_remain_candidates_without_cross_product_selection() {
        let current = fixture_package();
        let classic = fixture_package();
        let candidates = candidates_from_records(vec![
            record(classic.path(), "OpenAI.ChatGPT-Desktop", "999"),
            record(current.path(), "OpenAI.Codex", "1"),
        ])
        .unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].package_version.as_deref(), Some("999"));
        assert_eq!(candidates[1].package_version.as_deref(), Some("1"));
    }

    #[test]
    fn no_selection_does_not_auto_choose_a_discovered_candidate() {
        let current = fixture_package();
        let candidates =
            candidates_from_records(vec![record(current.path(), "OpenAI.Codex", "1")]).unwrap();
        assert_eq!(
            selected_candidate(&DesktopLauncherConfig::default(), &candidates).unwrap(),
            None
        );
    }

    #[test]
    fn selected_candidate_preserves_discovered_identity() {
        let current = fixture_package();
        let candidates =
            candidates_from_records(vec![record(current.path(), "OpenAI.Codex", "1")]).unwrap();
        let config = DesktopLauncherConfig {
            selected_executable: Some(candidates[0].executable.clone()),
            refuse_if_running: true,
        };
        assert_eq!(
            selected_candidate(&config, &candidates)
                .unwrap()
                .unwrap()
                .package_name
                .as_deref(),
            Some("OpenAI.Codex")
        );
    }

    #[test]
    fn manifest_rejects_parent_absolute_and_drive_prefixed_paths() {
        let root = tempfile::tempdir().unwrap();
        assert!(contained_executable(root.path(), "../escape.exe").is_err());
        assert!(contained_executable(root.path(), "/absolute.exe").is_err());
        assert!(contained_executable(root.path(), r"C:\absolute.exe").is_err());
    }

    #[test]
    fn output_limits_and_empty_output_are_stable_errors() {
        assert_eq!(
            parse_appx_output(&vec![b'x'; APPX_STDOUT_LIMIT + 1])
                .unwrap_err()
                .code,
            "APPX_OUTPUT_TOO_LARGE"
        );
        assert_eq!(
            parse_appx_output(b" ").unwrap_err().code,
            "DESKTOP_NOT_INSTALLED"
        );
    }

    #[test]
    fn process_root_matching_uses_full_path_and_parent_relationship() {
        let target = PathBuf::from(r"C:\Program Files\OpenAI\ChatGPT.exe");
        let records = [
            ProcessRecord {
                pid: 10,
                parent_pid: 1,
                executable: target.clone(),
            },
            ProcessRecord {
                pid: 11,
                parent_pid: 10,
                executable: target.clone(),
            },
            ProcessRecord {
                pid: 12,
                parent_pid: 1,
                executable: PathBuf::from(r"D:\Other\ChatGPT.exe"),
            },
        ];
        let comparable = comparable_path(&target);
        let matching: HashSet<u32> = records
            .iter()
            .filter(|record| comparable_path(&record.executable) == comparable)
            .map(|record| record.pid)
            .collect();
        let root = records
            .iter()
            .filter(|record| {
                matching.contains(&record.pid) && !matching.contains(&record.parent_pid)
            })
            .map(|record| record.pid)
            .min();
        assert_eq!(root, Some(10));
    }

    #[test]
    fn focus_selection_reports_zero_one_many_and_denied_without_native_windows() {
        // NOT_RUN: real EnumWindows/SetForegroundWindow cases require an
        // isolated Windows Desktop host and are intentionally not simulated.
        assert_eq!(
            focus_status_for_candidates(0, None),
            HostFocusStatus::WindowNotFound
        );
        assert_eq!(
            focus_status_for_candidates(1, Some(true)),
            HostFocusStatus::Focused
        );
        assert_eq!(
            focus_status_for_candidates(1, Some(false)),
            HostFocusStatus::ForegroundDenied
        );
        assert_eq!(
            focus_status_for_candidates(2, None),
            HostFocusStatus::MultipleCandidates
        );
    }

    #[test]
    fn descendant_detection_excludes_our_child_agent_processes() {
        let records = [
            ProcessRecord {
                pid: 20,
                parent_pid: 10,
                executable: PathBuf::from("host.exe"),
            },
            ProcessRecord {
                pid: 30,
                parent_pid: 20,
                executable: PathBuf::from("child.exe"),
            },
        ];
        assert!(is_descendant_of(30, 10, &records));
        assert!(!is_descendant_of(10, 30, &records));
    }

    #[test]
    fn startup_lock_excludes_concurrent_launchers() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("startup.lock");
        let _first = StartupLock::acquire_at(path.clone()).unwrap();
        assert_eq!(
            StartupLock::acquire_at(path).unwrap_err().code,
            "LAUNCH_BUSY"
        );
    }
}
