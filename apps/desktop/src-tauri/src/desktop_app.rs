//! Product entry point. Donor application assembly is deliberately unreachable.
use crate::desktop_capture::CapturePolicy;
use crate::hook_installer::{HookFeatures, HookInstallationState, HookManager};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use tauri::Manager;
use tauri_plugin_autostart::ManagerExt as _;

struct DesktopState {
    dir: PathBuf,
    startup_probe: bool,
    _runtime_access: crate::runtime_access::RuntimeAccessGuard,
    settings_lock: Arc<Mutex<()>>,
    health: Arc<Mutex<HealthCache>>,
}

struct HealthCache {
    epoch: String,
    revision: u64,
    semantic_snapshot: Option<serde_json::Value>,
}

fn activity_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 256 || key.chars().any(char::is_control) {
        return Err("ACTIVITY_KEY_INVALID".into());
    }
    Ok(())
}

#[tauri::command]
async fn desktop_health_check(
    state: tauri::State<'_, DesktopState>,
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
) -> Result<crate::diagnostics::HealthSnapshot, String> {
    let dir = state.dir.clone();
    let health = state.health.clone();
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::desktop_health::snapshot(&dir, &runtime, health_snapshot(&dir, &health))
    })
    .await
    .map_err(|_| "HEALTH_WORKER_FAILED")?
}

#[tauri::command]
async fn desktop_health_test_status(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
) -> Result<Option<crate::diagnostics::TestProbe>, String> {
    let db = runtime.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::desktop_health::test_status(&db).map_err(|e| e.code.to_owned())
    })
    .await
    .map_err(|_| "HEALTH_WORKER_FAILED")?
}

#[tauri::command]
async fn desktop_health_test_start(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    request_id: String,
) -> Result<crate::diagnostics::TestProbe, String> {
    let runtime = runtime.inner().clone();
    let _operation = runtime.operation.lock().await;
    let work = runtime.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::desktop_health::start_test(&work, &request_id).map_err(|e| e.code.to_owned())
    })
    .await
    .map_err(|_| "HEALTH_WORKER_FAILED")?
}

#[derive(Serialize)]
struct HealthExportReceipt {
    path: String,
}

#[tauri::command]
async fn desktop_health_export(
    state: tauri::State<'_, DesktopState>,
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
) -> Result<HealthExportReceipt, String> {
    let dir = state.dir.clone();
    let health = state.health.clone();
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let snapshot =
            crate::desktop_health::snapshot(&dir, &runtime, health_snapshot(&dir, &health))?;
        let base = dir.join("exports");
        if !base.exists() {
            std::fs::create_dir(&base).map_err(|_| "DIAGNOSTIC_EXPORT_DIRECTORY")?;
        }
        let metadata =
            std::fs::symlink_metadata(&base).map_err(|_| "DIAGNOSTIC_EXPORT_DIRECTORY")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("DIAGNOSTIC_EXPORT_DIRECTORY".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("DIAGNOSTIC_EXPORT_DIRECTORY".into());
            }
        }
        let directory = base.join(format!("diagnostics-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).map_err(|_| "DIAGNOSTIC_EXPORT_DIRECTORY")?;
        let path = crate::diagnostics::export_health_report_json(&directory, &snapshot.report)
            .map_err(|_| "DIAGNOSTIC_EXPORT_FAILED")?;
        runtime.diagnostics.push(
            crate::diagnostics::Component::Export,
            crate::diagnostics::Stage::Export,
            crate::diagnostics::SafeCode::ExportCreated,
            0,
            1,
        );
        Ok(HealthExportReceipt {
            path: path.to_string_lossy().into_owned(),
        })
    })
    .await
    .map_err(|_| "HEALTH_WORKER_FAILED")?
}

#[tauri::command]
async fn desktop_host_focus(
    launcher: tauri::State<'_, crate::launcher::LauncherState>,
) -> Result<crate::launcher::HostFocusResult, String> {
    let launcher = launcher.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        launcher.focus_host().map_err(|e| e.code.to_owned())
    })
    .await
    .map_err(|_| "HOST_FOCUS_WORKER_FAILED")?
}

#[tauri::command]
async fn desktop_notification_hold_status(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
) -> Result<crate::notification::hold::UserHoldStatus, String> {
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .delivery
            .user_hold_status(crate::db::now_ms().map_err(|e| e.code.to_owned())?)
            .map_err(|e| e.code.to_owned())
    })
    .await
    .map_err(|_| "HOLD_WORKER_FAILED")?
}

#[tauri::command]
async fn desktop_notification_hold_set(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    app: tauri::AppHandle,
    minutes: u16,
    expected_revision: u64,
) -> Result<crate::notification::hold::HoldReceipt, String> {
    use crate::notification::hold::HoldDuration;
    let duration = match minutes {
        15 => HoldDuration::Minutes15,
        60 => HoldDuration::Minutes60,
        _ => return Err("HOLD_DURATION_INVALID".into()),
    };
    let runtime = runtime.inner().clone();
    let receipt = tauri::async_runtime::spawn_blocking(move || {
        runtime
            .delivery
            .set_user_hold(
                expected_revision,
                duration,
                crate::db::now_ms().map_err(|e| e.code.to_owned())?,
            )
            .map_err(|e| e.code.to_owned())
    })
    .await
    .map_err(|_| "HOLD_WORKER_FAILED")??;
    use tauri::Emitter;
    let _ = app.emit("notification-hold-changed", ());
    Ok(receipt)
}

#[tauri::command]
async fn desktop_notification_hold_resume(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    app: tauri::AppHandle,
    expected_revision: u64,
) -> Result<crate::notification::hold::HoldReceipt, String> {
    let runtime = runtime.inner().clone();
    let receipt = tauri::async_runtime::spawn_blocking(move || {
        runtime
            .delivery
            .resume_user_hold(
                expected_revision,
                crate::db::now_ms().map_err(|e| e.code.to_owned())?,
            )
            .map_err(|e| e.code.to_owned())
    })
    .await
    .map_err(|_| "HOLD_WORKER_FAILED")??;
    use tauri::Emitter;
    let _ = app.emit("notification-hold-changed", ());
    Ok(receipt)
}

fn activity_revision(value: i64) -> Result<(), String> {
    if !(0..=9_007_199_254_740_991).contains(&value) {
        return Err("ACTIVITY_REVISION_INVALID".into());
    }
    Ok(())
}

#[tauri::command]
async fn desktop_activity_page(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    filter: Option<crate::activity::ActivityFilter>,
    cursor: Option<String>,
    limit: Option<u16>,
) -> Result<crate::activity::ActivityPage, String> {
    let db = runtime.db.clone();
    let delivery = runtime.delivery.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let hold = delivery.user_hold_status(crate::db::now_ms()?)?;
        let mut page = db.with_connection(|conn| {
            crate::storage::activity_repository::ActivityRepository::page(
                conn,
                &crate::activity::ActivityPageRequest {
                    filter: filter.unwrap_or_default(),
                    cursor,
                    limit,
                },
            )
        })?;
        for item in &mut page.items {
            item.apply_hold(&hold);
        }
        Ok::<_, crate::error::AppError>(page)
    })
    .await
    .map_err(|_| "ACTIVITY_WORKER_FAILED")?
    .map_err(|e| e.code.to_owned())
}

#[tauri::command]
async fn desktop_activity_detail(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    run_key: String,
) -> Result<crate::activity::ActivityDetail, String> {
    activity_key(&run_key)?;
    let db = runtime.db.clone();
    let delivery = runtime.delivery.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let hold = delivery.user_hold_status(crate::db::now_ms()?)?;
        let mut detail = db.with_connection(|conn| {
            crate::storage::activity_repository::ActivityRepository::detail(conn, &run_key)?
                .ok_or_else(|| {
                    crate::error::AppError::new(
                        "ACTIVITY_RUN_NOT_FOUND",
                        "该轮记录已不在本机保留范围内",
                    )
                })
        })?;
        detail.item.apply_hold(&hold);
        Ok::<_, crate::error::AppError>(detail)
    })
    .await
    .map_err(|_| "ACTIVITY_WORKER_FAILED")?
    .map_err(|e| e.code.to_owned())
}

#[tauri::command]
async fn desktop_attention_ack(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    run_key: String,
    observed_revision: i64,
) -> Result<crate::activity::AttentionAcknowledgement, String> {
    activity_key(&run_key)?;
    activity_revision(observed_revision)?;
    let db = runtime.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        db.with_transaction(|tx| {
            crate::storage::activity_repository::ActivityRepository::acknowledge_attention(
                tx,
                &run_key,
                observed_revision,
            )
        })
        .map_err(|e| e.code.to_owned())
    })
    .await
    .map_err(|_| "ACTIVITY_WORKER_FAILED")?
}

#[tauri::command]
async fn desktop_result_mark_seen(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    run_key: String,
    result_revision: i64,
) -> Result<crate::activity::ResultSeen, String> {
    activity_key(&run_key)?;
    activity_revision(result_revision)?;
    let db = runtime.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        db.with_transaction(|tx| {
            crate::storage::activity_repository::ActivityRepository::mark_result_seen(
                tx,
                &run_key,
                result_revision,
            )
        })
        .map_err(|e| e.code.to_owned())
    })
    .await
    .map_err(|_| "ACTIVITY_WORKER_FAILED")?
}

#[tauri::command]
async fn desktop_hook_self_test() -> Result<crate::hook_self_test::SelfTestReceipt, String> {
    tauri::async_runtime::spawn_blocking(crate::hook_self_test::run)
        .await
        .map_err(|_| "SELF_TEST_WORKER")?
}

fn registration(dir: &Path) -> Option<String> {
    crate::hook_health::bounded_read(&dir.join("hook-registration.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<String>(&b).ok())
        .filter(|s| uuid::Uuid::parse_str(s).is_ok())
}

fn health_snapshot(
    dir: &Path,
    cache: &Mutex<HealthCache>,
) -> Result<crate::hook_health::HookHealthSnapshot, String> {
    health_snapshot_with_resolver(
        dir,
        cache,
        &crate::hook_health::HookTrustResolver::from_environment(),
    )
}

fn health_snapshot_with_resolver(
    dir: &Path,
    cache: &Mutex<HealthCache>,
    resolver: &crate::hook_health::HookTrustResolver,
) -> Result<crate::hook_health::HookHealthSnapshot, String> {
    let mut cache = cache.lock().map_err(|_| "HOOK_HEALTH_BUSY")?;
    let policy_result = crate::desktop_policy::read_authority(&dir.join("agent-events.jsonl"));
    let (policy, policy_error) = match policy_result {
        Ok(state) => (state.policy, None),
        // A default is used only to shape the inspection request.  It never
        // authorizes a green health result when the durable policy is absent
        // or corrupt.
        Err(_) => (CapturePolicy::default(), Some("CAPTURE_POLICY_UNAVAILABLE")),
    };
    let home_result = read_optional_metadata(&dir.join("hook-target.json")).and_then(|bytes| {
        bytes
            .map(|b| {
                serde_json::from_slice::<PathBuf>(&b).map_err(|_| "HOOK_TARGET_INVALID".to_string())
            })
            .transpose()
    });
    let registration_result =
        read_optional_metadata(&dir.join("hook-registration.json")).and_then(|bytes| {
            bytes
                .map(|b| {
                    serde_json::from_slice::<String>(&b)
                        .map_err(|_| "HOOK_REGISTRATION_INVALID".to_string())
                        .and_then(|id| {
                            uuid::Uuid::parse_str(&id)
                                .map(|_| id)
                                .map_err(|_| "HOOK_REGISTRATION_INVALID".to_string())
                        })
                })
                .transpose()
        });
    let metadata_error = home_result
        .as_ref()
        .err()
        .or(registration_result.as_ref().err())
        .cloned();
    let home = home_result.unwrap_or(None);
    let source = home.as_ref().map(|h| h.join("hooks.json"));
    let manager = home.as_ref().and_then(|h| manager(dir, h).ok());
    let mut snapshot = crate::hook_health::inspect(
        source.as_deref(),
        manager.as_ref(),
        resolver,
        policy.observe_turns,
        policy.notify_attention,
    );
    if let Some(code) = metadata_error {
        snapshot.fresh = false;
        snapshot.diagnostic_code = Some(code);
        snapshot.installation = "checking";
    }
    if let Some(code) = policy_error {
        snapshot.fresh = false;
        snapshot.diagnostic_code = Some(code.into());
    }
    if snapshot.fresh {
        snapshot.registration_id = registration_result.unwrap_or(None);
    }
    for handler in &mut snapshot.handlers {
        if let Some(observed) = crate::hook_verification::last_observation(dir, &handler.event) {
            handler.observation_current = Some(&observed.registration_id)
                == snapshot.registration_id.as_ref()
                && observed.definition_fingerprint.is_some()
                && observed.definition_fingerprint == snapshot.definition_fingerprint
                && observed.helper_build.is_some()
                && observed.helper_build == crate::hook_verification::build_identity();
            handler.last_observed_at = Some(observed.observed_at);
            handler.observed_registration_id = Some(observed.registration_id);
        }
    }
    snapshot.verification = crate::hook_verification::status(
        dir,
        snapshot.registration_id.as_deref(),
        snapshot.definition_fingerprint.as_deref(),
    );
    // Checking time is not a health transition.  Keep the revision stable for
    // semantically identical observations so one application-level controller
    // can safely ignore duplicate polls without concealing stale evidence.
    let mut semantic = serde_json::to_value(&snapshot).map_err(|_| "HOOK_DTO_ENCODE")?;
    if let Some(object) = semantic.as_object_mut() {
        object.remove("runtimeEpoch");
        object.remove("revision");
        object.remove("observedAt");
    }
    if cache.semantic_snapshot.as_ref() != Some(&semantic) {
        cache.revision = cache
            .revision
            .checked_add(1)
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or("HOOK_REVISION_OVERFLOW")?;
        cache.semantic_snapshot = Some(semantic);
    }
    snapshot.runtime_epoch = cache.epoch.clone();
    snapshot.revision = cache.revision;
    Ok(snapshot)
}

#[tauri::command]
async fn desktop_hook_health(
    state: tauri::State<'_, DesktopState>,
) -> Result<crate::hook_health::HookHealthSnapshot, String> {
    let dir = state.dir.clone();
    let cache = state.health.clone();
    tauri::async_runtime::spawn_blocking(move || health_snapshot(&dir, &cache))
        .await
        .map_err(|_| "HOOK_WORKER_FAILED")?
}

#[tauri::command]
async fn desktop_hook_verify(
    state: tauri::State<'_, DesktopState>,
) -> Result<crate::hook_verification::VerificationView, String> {
    let dir = state.dir.clone();
    let cache = state.health.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let health = health_snapshot(&dir, &cache)?;
        if !health.observation_enabled || !health.fresh || health.installation != "current" {
            return Err("HOOK_VERIFICATION_NOT_READY: 请先完成配置并恢复观察".into());
        }
        crate::hook_verification::begin(
            &dir,
            health
                .registration_id
                .as_deref()
                .ok_or("HOOK_REGISTRATION_MISSING")?,
            health
                .definition_fingerprint
                .as_deref()
                .ok_or("HOOK_DEFINITION_MISSING")?,
        )
    })
    .await
    .map_err(|_| "HOOK_WORKER_FAILED")?
}

#[tauri::command]
fn desktop_relay_status(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
) -> DesktopRelayStatus {
    DesktopRelayStatus {
        status: runtime.relay.status(),
        base_url: runtime.relay.configured_base_url(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopRelayStatus {
    #[serde(flatten)]
    status: crate::relay::state::RelayStatus,
    base_url: Option<String>,
}

#[tauri::command]
async fn desktop_relay_configure(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    base_url: String,
    device_token: String,
) -> Result<(), String> {
    runtime
        .relay
        .configure(base_url, zeroize::Zeroizing::new(device_token))
        .await
        .map_err(|error| error.message)?;
    runtime
        .delivery
        .reconcile()
        .map_err(|error| error.message)?;
    Ok(())
}

#[tauri::command]
async fn desktop_relay_probe(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
) -> Result<(), String> {
    runtime
        .relay
        .probe_only()
        .await
        .map_err(|error| error.message)?;
    Ok(())
}

#[tauri::command]
async fn desktop_relay_test(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
) -> Result<crate::notification::TestNotificationResult, String> {
    runtime
        .relay
        .send_test_notification(&runtime.delivery)
        .await
        .map_err(|error| error.message)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopHistoryItem {
    #[serde(flatten)]
    item: crate::notification::outbox::NotificationHistoryItem,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<DesktopResultProjection>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopResultProjection {
    result_id: String,
    source_hash: String,
    page_state: Option<String>,
    page_expires_at: Option<i64>,
    notification_id: Option<String>,
    notification_status: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopHistoryPage {
    items: Vec<DesktopHistoryItem>,
    next_cursor: Option<String>,
}

#[tauri::command]
async fn desktop_deliveries(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    limit: Option<u16>,
    cursor: Option<String>,
) -> Result<DesktopHistoryPage, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let page = runtime
            .outbox
            .history_page(limit.unwrap_or(20), cursor.as_deref())
            .map_err(|error| error.message)?;
        let items = page
            .items
            .into_iter()
            .map(|item| {
                let result = runtime
                    .outbox
                    .result_publication(&item.id)
                    .map_err(|error| error.message)?
                    .map(|value| {
                        let expired = value.page_state.as_deref() == Some("available")
                            && value
                                .page_expires_at
                                .is_some_and(|at| at <= crate::db::now_ms().unwrap_or(0));
                        DesktopResultProjection {
                            result_id: value.outbox_id,
                            source_hash: value.source_hash,
                            page_state: expired
                                .then_some("expired".to_string())
                                .or(value.page_state),
                            page_expires_at: value.page_expires_at,
                            notification_id: value.notification_id,
                            notification_status: value.notification_status,
                        }
                    });
                Ok(DesktopHistoryItem { item, result })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(DesktopHistoryPage {
            items,
            next_cursor: page.next_cursor,
        })
    })
    .await
    .map_err(|_| "DELIVERIES_WORKER_FAILED".to_string())?
}

#[tauri::command]
async fn desktop_result_link(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    id: String,
) -> Result<crate::relay::result_protocol::RelayResultLink, String> {
    runtime
        .relay
        .result_link(&id)
        .await
        .map_err(|error| error.message)
}

#[tauri::command]
async fn desktop_delivery_metadata(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    id: String,
) -> Result<DesktopHistoryItem, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let item = runtime
            .outbox
            .history_item(&id)
            .map_err(|e| e.message)?
            .ok_or("NOTIFICATION_NOT_FOUND")?;
        let result = runtime
            .outbox
            .result_publication(&id)
            .map_err(|e| e.message)?
            .map(|value| DesktopResultProjection {
                result_id: value.outbox_id,
                source_hash: value.source_hash,
                page_state: if value.page_state.as_deref() == Some("available")
                    && value
                        .page_expires_at
                        .is_some_and(|at| at <= crate::db::now_ms().unwrap_or(0))
                {
                    Some("expired".into())
                } else {
                    value.page_state
                },
                page_expires_at: value.page_expires_at,
                notification_id: value.notification_id,
                notification_status: value.notification_status,
            });
        Ok(DesktopHistoryItem { item, result })
    })
    .await
    .map_err(|_| "DELIVERY_METADATA_WORKER_FAILED".to_string())?
}
#[tauri::command]
async fn desktop_result_open(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    id: String,
) -> Result<(), String> {
    let link = runtime
        .relay
        .result_link(&id)
        .await
        .map_err(|error| error.message)?;
    open_result_url(&link.url)
}
#[tauri::command]
async fn desktop_result_revoke(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    id: String,
    request_id: String,
) -> Result<(), String> {
    runtime
        .relay
        .result_action(&id, "revoke", &request_id)
        .await
        .map(|_| ())
        .map_err(|error| format!("{}: {}", error.code, error.message))
}
#[tauri::command]
async fn desktop_result_resend(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    id: String,
    request_id: String,
) -> Result<(), String> {
    runtime
        .relay
        .result_action(&id, "resend", &request_id)
        .await
        .map(|_| ())
        .map_err(|error| format!("{}: {}", error.code, error.message))
}

#[cfg(target_os = "windows")]
pub(crate) fn open_result_url(url: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = std::ffi::OsStr::new(url)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let operation: Vec<u16> = std::ffi::OsStr::new("open")
        .encode_wide()
        .chain(Some(0))
        .collect();
    let value = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if value as isize <= 32 {
        return Err("无法打开安全的结果链接".into());
    }
    Ok(())
}
#[cfg(not(target_os = "windows"))]
pub(crate) fn open_result_url(_: &str) -> Result<(), String> {
    Err("结果链接只能在 Windows 桌面应用中打开".into())
}

#[tauri::command]
async fn desktop_delivery_detail(
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    id: String,
) -> Result<crate::desktop_results::DeliveryDetail, String> {
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || {
        crate::desktop_results::detail(&runtime.db, &id).map_err(|error| error.message)
    })
    .await
    .map_err(|_| "DELIVERY_DETAIL_WORKER_FAILED".to_string())?
}

#[tauri::command]
async fn desktop_export_delivery(
    state: tauri::State<'_, DesktopState>,
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    id: String,
) -> Result<String, String> {
    let runtime = Arc::clone(runtime.inner());
    let dir = state.dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::desktop_results::export(&runtime.db, &dir, &id).map_err(|error| error.message)
    })
    .await
    .map_err(|_| "DELIVERY_EXPORT_WORKER_FAILED".to_string())?
}

#[tauri::command]
async fn desktop_launcher_status(
    state: tauri::State<'_, crate::launcher::LauncherState>,
    desktop: tauri::State<'_, DesktopState>,
) -> Result<crate::launcher::LauncherStatus, String> {
    if desktop.startup_probe {
        // The native startup probe loads the real UI with no installed-host
        // discovery, process queries or selected host in its temporary profile.
        return Ok(crate::launcher::LauncherStatus::default());
    }
    state.status().await.map_err(|error| error.message)
}

/// AppX enumeration is intentionally explicit: ordinary settings mutations
/// consume the cached status and do not launch PowerShell.
#[tauri::command]
async fn desktop_launcher_refresh(
    state: tauri::State<'_, crate::launcher::LauncherState>,
) -> Result<crate::launcher::LauncherStatus, String> {
    state
        .discover_candidates()
        .await
        .map_err(|error| error.message)?;
    state.status().await.map_err(|error| error.message)
}

#[tauri::command]
fn desktop_launcher_save(
    state: tauri::State<'_, crate::launcher::LauncherState>,
    config: crate::launcher::LauncherConfig,
    expected_config: crate::launcher::LauncherConfig,
) -> Result<crate::launcher::LauncherConfig, String> {
    state
        .replace_config_checked(config, Some(&expected_config))
        .map_err(|error| format!("{}: {}", error.code, error.message))
}

#[tauri::command]
async fn desktop_launch(
    state: tauri::State<'_, crate::launcher::LauncherState>,
) -> Result<crate::launcher::DesktopLaunchReceipt, String> {
    state.launch_selected().await.map_err(|error| error.message)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopStatus {
    policy: CapturePolicy,
    policy_revision: i64,
    policy_apply_status: &'static str,
    data_directory: String,
    hook_home: Option<String>,
    inbox_present: bool,
}

fn features(policy: &CapturePolicy) -> HookFeatures {
    HookFeatures {
        prompt_capture: true,
        run_lifecycle: true,
        attention: policy.notify_attention,
        capture_agent_outputs: false,
    }
}

fn manager(dir: &Path, home: &Path) -> Result<HookManager, String> {
    if !home.is_absolute() {
        return Err("请选择绝对路径的 Codex 配置目录".into());
    }
    let manager = HookManager::new(
        home.join("hooks.json"),
        dir.join("agent-events.jsonl"),
        std::env::current_exe().map_err(|_| "无法定位应用")?,
    )
    .map_err(|_| "无法创建 Hook 配置操作".to_string())?;
    Ok(if let Some(id) = registration(dir) {
        manager.with_registration_id(id)
    } else {
        manager
    })
}

fn selected_home(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("hook-target.json");
    if path.metadata().ok()?.len() > 32768 {
        return None;
    }
    serde_json::from_slice::<PathBuf>(&std::fs::read(path).ok()?).ok()
}

fn write_json(dir: &Path, name: &str, value: &impl serde::Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| "配置无法编码")?;
    crate::atomic_file::write(
        &dir.join(name),
        &bytes,
        "DESKTOP_CONFIG_WRITE",
        "无法保存配置",
    )
    .map_err(|_| "无法保存配置".into())
}

#[tauri::command]
async fn desktop_status(
    state: tauri::State<'_, DesktopState>,
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
) -> Result<DesktopStatus, String> {
    let dir = state.dir.clone();
    let projection_pending = runtime.settings.pending();
    tauri::async_runtime::spawn_blocking(move || desktop_status_local(&dir, projection_pending))
        .await
        .map_err(|_| "HOOK_WORKER_FAILED")?
}

fn desktop_status_local(dir: &Path, projection_pending: bool) -> Result<DesktopStatus, String> {
    let inbox = dir.join("agent-events.jsonl");
    let policy_state =
        crate::desktop_policy::read_authority(&inbox).map_err(|error| error.message)?;
    let home = selected_home(dir);
    Ok(DesktopStatus {
        policy: policy_state.policy,
        policy_revision: policy_state.revision,
        policy_apply_status: if projection_pending {
            "pending"
        } else {
            "saved"
        },
        data_directory: dir.to_string_lossy().into_owned(),
        hook_home: home.map(|home| home.to_string_lossy().into_owned()),
        inbox_present: inbox.exists(),
    })
}

#[tauri::command]
fn desktop_autostart_status(
    app: tauri::AppHandle,
    desktop: tauri::State<'_, DesktopState>,
) -> Result<bool, String> {
    if desktop.startup_probe {
        return Ok(false);
    }
    app.autolaunch()
        .is_enabled()
        .map_err(|_| "无法读取开机自启状态".into())
}

#[tauri::command]
fn desktop_autostart_set(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|_| "无法更新开机自启状态".to_string())?;
    manager
        .is_enabled()
        .map_err(|_| "无法确认开机自启状态".into())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopDiagnostics {
    schema_version: u16,
    hook_configured: bool,
    relay_configured: bool,
    relay_ready: bool,
    pending_notifications: u32,
    sending_notifications: u32,
    retry_notifications: u32,
    blocked_notifications: u32,
    dead_letter_notifications: u32,
    last_delivery_at: Option<i64>,
    last_error_code: Option<String>,
    retention: crate::storage::retention::RetentionSummary,
    suppressed_unknown_events: i64,
    verification_events: i64,
    last_unknown_observed_at: Option<i64>,
    last_verification_observed_at: Option<i64>,
}

#[tauri::command]
async fn desktop_diagnostics(
    state: tauri::State<'_, DesktopState>,
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
) -> Result<DesktopDiagnostics, String> {
    let dir = state.dir.clone();
    let runtime = Arc::clone(runtime.inner());
    tauri::async_runtime::spawn_blocking(move || desktop_diagnostics_local(&dir, &runtime))
        .await
        .map_err(|_| "DIAGNOSTICS_WORKER_FAILED".to_string())?
}

fn desktop_diagnostics_local(
    dir: &Path,
    runtime: &crate::desktop_runtime::DesktopRuntime,
) -> Result<DesktopDiagnostics, String> {
    let policy = crate::desktop_policy::read_authority(&dir.join("agent-events.jsonl"))
        .map_err(|error| error.message)?
        .policy;
    let hook_configured = selected_home(dir)
        .and_then(|home| manager(dir, &home).ok())
        .and_then(|manager| {
            manager
                .installation_state_for_features(features(&policy))
                .ok()
        })
        .is_some_and(|status| matches!(status, HookInstallationState::Current));
    let relay = runtime.relay.status();
    let outbox = runtime.delivery.stats().map_err(|error| error.message)?;
    let (
        retention,
        suppressed_unknown_events,
        verification_events,
        last_unknown_observed_at,
        last_verification_observed_at,
    ) = runtime
        .db
        .with_transaction(|tx| {
            let retention = crate::storage::retention::summary(tx)?;
            let facts: (i64, i64, Option<i64>, Option<i64>) = tx
                .query_row(
                    "SELECT
                COALESCE(SUM(CASE WHEN retention_class = 'unknown' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN retention_class = 'verification' THEN 1 ELSE 0 END), 0),
                MAX(CASE WHEN retention_class = 'unknown' THEN observed_at END),
                MAX(CASE WHEN retention_class = 'verification' THEN observed_at END)
             FROM agent_events",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(crate::error::AppError::from)?;
            Ok((retention, facts.0, facts.1, facts.2, facts.3))
        })
        .map_err(|error| error.message)?;
    Ok(DesktopDiagnostics {
        schema_version: 1,
        hook_configured,
        relay_configured: relay.configured,
        relay_ready: runtime.relay.notification_delivery_ready(),
        pending_notifications: outbox.pending_count,
        sending_notifications: outbox.sending_count,
        retry_notifications: outbox.retry_count,
        blocked_notifications: outbox.blocked_notifications,
        dead_letter_notifications: outbox.dead_letter_count,
        last_delivery_at: outbox.last_delivery_at,
        last_error_code: outbox.last_error_code,
        retention,
        suppressed_unknown_events,
        verification_events,
        last_unknown_observed_at,
        last_verification_observed_at,
    })
}

#[tauri::command]
async fn desktop_save_policy(
    state: tauri::State<'_, DesktopState>,
    runtime: tauri::State<'_, Arc<crate::desktop_runtime::DesktopRuntime>>,
    policy: CapturePolicy,
    expected_revision: i64,
) -> Result<crate::desktop_policy::PolicySaveReceipt, String> {
    runtime
        .save_policy(
            &state.dir.join("agent-events.jsonl"),
            policy,
            expected_revision,
        )
        .await
        .map_err(|error| error.message)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopHookPlan {
    #[serde(flatten)]
    plan: crate::hook_installer::HookInstallPlan,
    registration_id: String,
    operation_id: String,
    expected_policy: CapturePolicy,
}

#[tauri::command]
fn desktop_hook_plan(
    state: tauri::State<'_, DesktopState>,
    home: String,
) -> Result<DesktopHookPlan, String> {
    let id = registration(&state.dir).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let policy = crate::desktop_capture::read_policy(&state.dir.join("agent-events.jsonl"));
    let hook = manager(&state.dir, Path::new(&home))?.with_registration_id(id.clone());
    let plan = hook
        .plan_reconcile_features(features(&policy))
        .map_err(|e| e.to_string())?;
    Ok(DesktopHookPlan {
        plan,
        registration_id: id,
        operation_id: uuid::Uuid::new_v4().to_string(),
        expected_policy: policy,
    })
}

#[tauri::command]
async fn desktop_install_hook(
    state: tauri::State<'_, DesktopState>,
    home: String,
    registration_id: String,
    expected_fingerprint: Option<String>,
    expected_policy: CapturePolicy,
) -> Result<DesktopHookPlan, String> {
    let dir = state.dir.clone();
    let settings_lock = state.settings_lock.clone();
    tauri::async_runtime::spawn_blocking(move || {
        install_hook_local(
            &dir,
            &settings_lock,
            home,
            registration_id,
            expected_fingerprint,
            expected_policy,
        )
    })
    .await
    .map_err(|_| "HOOK_WORKER_FAILED")?
}

fn install_hook_local(
    dir: &Path,
    settings_lock: &Mutex<()>,
    home: String,
    registration_id: String,
    expected_fingerprint: Option<String>,
    expected_policy: CapturePolicy,
) -> Result<DesktopHookPlan, String> {
    let _lock = settings_lock.lock().map_err(|_| "配置暂时不可用")?;
    uuid::Uuid::parse_str(&registration_id).map_err(|_| "HOOK_REGISTRATION_INVALID")?;
    if registration(dir).is_some_and(|id| id != registration_id) {
        return Err("HOOK_REGISTRATION_CHANGED".into());
    }
    let home = PathBuf::from(home);
    ensure_no_legacy_hook_owner(&home)?;
    if selected_home(dir).is_some_and(|previous| previous != home) {
        return Err("请先卸载当前选定目录的 Hook，再更换作用域".into());
    }
    let policy = crate::desktop_capture::read_policy(&dir.join("agent-events.jsonl"));
    if policy != expected_policy {
        return Err("HOOK_PLAN_STALE: 采集设置已变化，请重新查看安装计划".into());
    }
    let hook = manager(dir, &home)?.with_registration_id(registration_id.clone());
    let plan = hook
        .plan_reconcile_features(features(&policy))
        .map_err(|e| e.to_string())?;
    if plan.source_fingerprint != expected_fingerprint {
        return Err("HOOK_CONFIG_CHANGED: 配置已变化，请重新查看安装计划".into());
    }
    // Metadata is persisted before the external definition; restore it on a failed CAS/write.
    let target_path = dir.join("hook-target.json");
    let registration_path = dir.join("hook-registration.json");
    let before_target = read_optional_metadata(&target_path)?;
    let before_registration = read_optional_metadata(&registration_path)?;
    let operation = (|| {
        if selected_home(dir).as_ref() != Some(&home) {
            write_json(dir, "hook-target.json", &home)?;
        }
        if registration(dir).as_deref() != Some(&registration_id) {
            write_json(dir, "hook-registration.json", &registration_id)?;
        }
        hook.install_features_expected(features(&policy), expected_fingerprint.as_deref())
            .map_err(|e| e.to_string())
    })();
    if let Err(error) = operation {
        let written_target =
            serde_json::to_vec_pretty(&home).map_err(|_| "HOOK_ROLLBACK_FAILED")?;
        let written_registration =
            serde_json::to_vec_pretty(&registration_id).map_err(|_| "HOOK_ROLLBACK_FAILED")?;
        rollback_metadata([
            (&target_path, before_target, written_target),
            (
                &registration_path,
                before_registration,
                written_registration,
            ),
        ])?;
        return Err(error);
    }
    // The capture helper cannot safely reread the host Hook definition on
    // every stdin event.  Record the managed definition identity beside the
    // registration after a successful CAS/no-op/repair.  A write failure does
    // not erase the installation fact, but it prevents later observations
    // from being classified as current.
    let evidence = crate::hook_health::inspect(
        Some(&home.join("hooks.json")),
        Some(&hook),
        &crate::hook_health::HookTrustResolver::from_environment(),
        policy.observe_turns,
        policy.notify_attention,
    );
    if let Some(fingerprint) = evidence.definition_fingerprint {
        let _ = crate::hook_verification::write_definition_evidence(
            dir,
            &registration_id,
            &fingerprint,
        );
    }
    Ok(DesktopHookPlan {
        plan,
        registration_id,
        operation_id: uuid::Uuid::new_v4().to_string(),
        expected_policy: policy,
    })
}
fn read_optional_metadata(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= 32768 => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => Err("HOOK_METADATA_UNREADABLE".into()),
    }
}
type MetadataRollback<'a> = (&'a PathBuf, Option<Vec<u8>>, Vec<u8>);
fn rollback_metadata<const N: usize>(entries: [MetadataRollback<'_>; N]) -> Result<(), String> {
    let mut failure = None;
    for (path, previous, written) in entries {
        let current = match read_optional_metadata(path) {
            Ok(v) => v,
            Err(e) => {
                failure = Some(e);
                continue;
            }
        };
        if current == previous {
            continue;
        }
        if current.as_deref() != Some(written.as_slice()) {
            failure = Some("HOOK_ROLLBACK_CONFLICT".into());
            continue;
        }
        let restored = match previous {
            Some(bytes) => {
                crate::atomic_file::write(path, &bytes, "HOOK_ROLLBACK", "无法恢复安装目标")
                    .map_err(|_| "HOOK_ROLLBACK_FAILED".to_string())
            }
            None => std::fs::remove_file(path).map_err(|_| "HOOK_ROLLBACK_FAILED".to_string()),
        };
        if let Err(error) = restored {
            failure = Some(error);
        }
    }
    failure.map_or(Ok(()), Err)
}
fn ensure_no_legacy_hook_owner(home: &Path) -> Result<(), String> {
    let path = home.join("hooks.json");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("无法读取现有 Hook 配置".into()),
    };
    if bytes.len() > 1024 * 1024 {
        return Err("现有 Hook 配置过大，无法安全检查".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "现有 Hook 配置不是有效 JSON")?;
    if json_contains_legacy_hook_marker(&value) {
        return Err("检测到旧 PromptDock Hook；请先停用旧捕获，避免同一事件被双写".into());
    }
    Ok(())
}

fn json_contains_legacy_hook_marker(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(value) => value.contains("--promptdock-agent-event"),
        serde_json::Value::Array(values) => values.iter().any(json_contains_legacy_hook_marker),
        serde_json::Value::Object(values) => values.values().any(json_contains_legacy_hook_marker),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_target_keeps_stale_evidence_instead_of_claiming_absent() {
        let dir = tempfile::tempdir().unwrap();
        crate::capture_policy::write_policy_state(
            &dir.path().join("agent-events.jsonl"),
            &crate::capture_policy::CapturePolicyState::default(),
        )
        .unwrap();
        let home = dir.path().join("codex");
        std::fs::create_dir(&home).unwrap();
        write_json(dir.path(), "hook-target.json", &home).unwrap();
        manager(dir.path(), &home)
            .unwrap()
            .install_features(features(&CapturePolicy::default()))
            .unwrap();
        let cache = Mutex::new(HealthCache {
            epoch: "test".into(),
            revision: 0,
            semantic_snapshot: None,
        });
        let resolver = crate::hook_health::HookTrustResolver {
            user_config: Some(dir.path().join("isolated-user.toml")),
        };
        let first = health_snapshot_with_resolver(dir.path(), &cache, &resolver).unwrap();
        assert_eq!(first.installation, "current");
        std::fs::write(dir.path().join("hook-target.json"), b"invalid").unwrap();
        let stale = health_snapshot_with_resolver(dir.path(), &cache, &resolver).unwrap();
        assert!(!stale.fresh);
        assert_eq!(stale.installation, "checking");
        assert!(stale.revision > first.revision);
        assert_ne!(stale.definition_fingerprint, first.definition_fingerprint);
        assert_eq!(
            stale.diagnostic_code.as_deref(),
            Some("HOOK_TARGET_INVALID")
        );
    }

    #[test]
    fn stale_policy_plan_is_rejected_before_any_installation_write() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("codex");
        std::fs::create_dir(&home).unwrap();
        let expected = CapturePolicy {
            notify_attention: true,
            ..CapturePolicy::default()
        };
        assert!(install_hook_local(
            dir.path(),
            &Mutex::new(()),
            home.to_string_lossy().into_owned(),
            uuid::Uuid::new_v4().to_string(),
            None,
            expected
        )
        .unwrap_err()
        .contains("HOOK_PLAN_STALE"));
        assert!(!home.join("hooks.json").exists());
        assert!(!dir.path().join("hook-registration.json").exists());
    }

    #[test]
    fn metadata_rollback_preserves_conflict_and_still_restores_other_entry() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let registration = dir.path().join("registration");
        std::fs::write(&target, b"external").unwrap();
        std::fs::write(&registration, b"ours").unwrap();
        assert!(rollback_metadata([
            (&target, None, b"ours".to_vec()),
            (&registration, None, b"ours".to_vec())
        ])
        .is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"external");
        assert!(!registration.exists());
    }

    #[test]
    fn result_excerpt_toggle_does_not_change_the_installed_hook_definition() {
        let mut policy = CapturePolicy::default();
        let without_excerpt = features(&policy);
        policy.result_content_mode = crate::model::ResultContentMode::FullFinal;
        assert_eq!(features(&policy), without_excerpt);
        assert!(!without_excerpt.capture_agent_outputs);
    }

    #[test]
    fn hook_state_trust_records_are_not_misclassified_as_inline_hooks() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("config.toml"),
            "[hooks.state]\n\"C:/Users/test/.codex/hooks.json:stop:hash\" = true\n",
        )
        .unwrap();
        std::fs::write(
            directory.path().join("hooks.json"),
            br#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"foreign.exe"}]}]}}"#,
        )
        .unwrap();
        assert!(ensure_no_legacy_hook_owner(directory.path()).is_ok());
    }

    #[test]
    fn legacy_promptdock_owner_is_rejected_without_modifying_hooks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hooks.json");
        let original = br#"{"hooks":{"Stop":[{"hooks":[{"command":"old.exe --promptdock-agent-event stop"}]}]}}"#;
        std::fs::write(&path, original).unwrap();
        let error = ensure_no_legacy_hook_owner(directory.path()).unwrap_err();
        assert!(error.contains("旧 PromptDock Hook"));
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn desktop_status_exposes_the_resolved_data_directory() {
        let status = DesktopStatus {
            policy: CapturePolicy::default(),
            policy_revision: 4,
            policy_apply_status: "saved",
            data_directory: r"C:\Users\测试 User\.promptdock-desktop".into(),
            hook_home: None,
            inbox_present: false,
        };

        let value = serde_json::to_value(status).unwrap();
        assert_eq!(
            value["dataDirectory"],
            r"C:\Users\测试 User\.promptdock-desktop"
        );
    }

    #[test]
    fn diagnostics_dto_contains_only_aggregate_safe_facts() {
        let value = serde_json::to_value(DesktopDiagnostics {
            schema_version: 1,
            hook_configured: false,
            relay_configured: false,
            relay_ready: false,
            pending_notifications: 0,
            sending_notifications: 0,
            retry_notifications: 0,
            blocked_notifications: 0,
            dead_letter_notifications: 0,
            last_delivery_at: None,
            last_error_code: None,
            retention: crate::storage::retention::RetentionSummary::default(),
            suppressed_unknown_events: 2,
            verification_events: 1,
            last_unknown_observed_at: Some(12),
            last_verification_observed_at: Some(13),
        })
        .unwrap();
        assert_eq!(value["suppressedUnknownEvents"], 2);
        assert_eq!(value["retention"]["eventCount"], 0);
        for forbidden in ["metadataJson", "payload", "body", "deviceToken", "shareUrl"] {
            assert!(
                value.get(forbidden).is_none(),
                "{forbidden} must not escape diagnostics"
            );
        }
    }
}

#[tauri::command]
fn desktop_uninstall_hook(state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    let _lock = state.settings_lock.lock().map_err(|_| "配置暂时不可用")?;
    if let Some(home) = selected_home(&state.dir) {
        manager(&state.dir, &home)?
            .uninstall()
            .map_err(|_| "Hook 卸载失败")?;
        std::fs::remove_file(state.dir.join("hook-target.json")).map_err(|_| "无法清除配置目标")?;
    }
    Ok(())
}

pub fn run() -> bool {
    run_with_probe(None)
}

pub(crate) fn self_test_startup(scenario: &str, cleanup_token: &str) -> bool {
    let Ok(cleanup_token) = uuid::Uuid::parse_str(cleanup_token) else {
        return false;
    };
    match crate::desktop_startup::NativeStartupProbe::new(scenario, cleanup_token) {
        Ok(mut probe) => {
            probe.retain_for_parent_cleanup();
            run_with_probe(Some(Arc::new(probe)))
        }
        Err(_) => false,
    }
}

fn run_with_probe(probe: Option<Arc<crate::desktop_startup::NativeStartupProbe>>) -> bool {
    use crate::desktop_startup::{show_failure, StartupFailure, StartupState};
    use tauri::Listener;
    let startup = Arc::new(StartupState::default());
    let mut context = tauri::generate_context!();
    // Window construction belongs inside our error boundary too. Tauri creates
    // configured windows before calling the user setup closure by default.
    let mut main_config = context.config().app.windows[0].clone();
    main_config.visible = false;
    for window in &mut context.config_mut().app.windows {
        window.create = false;
    }
    let mut builder = tauri::Builder::default();
    if probe.is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    }
    let setup_state = startup.clone();
    let page_state = startup.clone();
    let setup_probe = probe.clone();
    let result = builder
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--start-hidden"]),
        ))
        .on_page_load(move |webview, payload| {
            if webview.label() == "main"
                && matches!(payload.event(), tauri::webview::PageLoadEvent::Finished)
            {
                page_state.update(|state| state.page_loaded = true);
            }
        })
        .setup(move |app| {
            // Tauri executes setup in RunEvent::Ready and panics on Err. Never
            // propagate an expected startup failure across that event callback.
            let setup_result = (|| -> Result<(), Box<dyn std::error::Error>> {
                let dir = if let Some(probe) = &setup_probe {
                    probe.data_dir()
                } else {
                    let profile = app.path().home_dir().map_err(|error| {
                        crate::error::AppError::internal(
                            "PRODUCT_DATA_ROOT_UNAVAILABLE",
                            "无法定位用户配置目录，应用已停止启动",
                            error.to_string(),
                        )
                    })?;
                    crate::stable_data_root::from_profile(&profile)?
                };
                setup_state.update(|state| state.data_dir = Some(dir.clone()));
                let runtime_access = crate::runtime_access::RuntimeAccessGuard::acquire(&dir)?;
                crate::diagnostics::init_logging(&dir)?;
                let mut window_builder =
                    tauri::WebviewWindowBuilder::from_config(app, &main_config)?;
                if let Some(probe) = &setup_probe {
                    // No additional event permission is granted to ordinary
                    // launches. The probe receives a readiness acknowledgement
                    // only after Vue mounts and its initial IPC calls finish.
                    app.add_capability(r#"{"identifier":"native-startup-probe","windows":["main"],"permissions":["core:event:allow-emit"]}"#)?;
                    let frontend_state = setup_state.clone();
                    app.listen("promptdock://native-startup-ready", move |event| {
                        let ready = serde_json::from_str::<serde_json::Value>(event.payload())
                            .ok()
                            .and_then(|value| value.get("ready").and_then(|ready| ready.as_bool()));
                        if ready == Some(true) {
                            frontend_state.update(|state| state.frontend_ready = true);
                        } else {
                            frontend_state.fail(StartupFailure {
                                code: "DESKTOP_FRONTEND_INIT_FAILED",
                                message: "桌面界面未完成初始状态读取".into(),
                            });
                        }
                    });
                    window_builder = window_builder
                        .data_directory(probe.webview_dir())
                        .initialization_script("window.__PROMPTDOCK_NATIVE_PROBE__ = true;");
                }
                let window = window_builder.build()?;
                // Complete all fallible UI setup before starting runtime workers.
                // A later builder error must never release the data guard while a
                // worker may still be accessing the data directory.
                app.manage(crate::launcher::LauncherState::open(
                    dir.join("launcher.json"),
                )?);
                crate::desktop_tray::install(app)?;
                app.manage(Arc::new(crate::desktop_runtime::DesktopRuntime::start(
                    app.handle().clone(),
                    &dir,
                )?));
                app.manage(DesktopState {
                    dir,
                    startup_probe: setup_probe.is_some(),
                    _runtime_access: runtime_access,
                    settings_lock: Arc::new(Mutex::new(())),
                    health: Arc::new(Mutex::new(HealthCache {
                        epoch: uuid::Uuid::new_v4().to_string(),
                        revision: 0,
                        semantic_snapshot: None,
                    })),
                });
                if setup_probe.is_none() && !std::env::args_os().any(|arg| arg == "--start-hidden")
                {
                    window.show()?;
                }
                setup_state.update(|state| state.setup_completed = true);
                Ok(())
            })();
            if let Err(error) = setup_result {
                setup_state.fail(StartupFailure::from_error(error.as_ref()));
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            desktop_health_check,
            desktop_health_export,
            desktop_health_test_start,
            desktop_health_test_status,
            desktop_host_focus,
            desktop_notification_hold_status,
            desktop_notification_hold_set,
            desktop_notification_hold_resume,
            desktop_activity_page,
            desktop_activity_detail,
            desktop_attention_ack,
            desktop_result_mark_seen,
            desktop_status,
            desktop_hook_health,
            desktop_hook_verify,
            desktop_hook_self_test,
            desktop_autostart_status,
            desktop_autostart_set,
            desktop_diagnostics,
            desktop_save_policy,
            desktop_hook_plan,
            desktop_install_hook,
            desktop_uninstall_hook,
            desktop_launcher_status,
            desktop_launcher_refresh,
            desktop_launcher_save,
            desktop_launch,
            desktop_relay_status,
            desktop_relay_configure,
            desktop_relay_probe,
            desktop_relay_test,
            desktop_deliveries,
            desktop_delivery_metadata,
            desktop_delivery_detail,
            desktop_export_delivery,
            desktop_result_link,
            desktop_result_open,
            desktop_result_revoke,
            desktop_result_resend
        ])
        .build(context);
    let exit_code = match result {
        Ok(app) => {
            let event_state = startup.clone();
            let is_probe = probe.is_some();
            let watcher = Arc::new(Mutex::new(None::<std::thread::JoinHandle<()>>));
            let event_watcher = watcher.clone();
            let exit_code = app.run_return(move |app, event| {
                if matches!(event, tauri::RunEvent::Ready) {
                    let state = event_state.snapshot();
                    if state.failure.is_some() {
                        if !is_probe {
                            show_failure(&state);
                        }
                        app.exit(1);
                    } else if is_probe {
                        let state = event_state.clone();
                        let app = app.clone();
                        let handle = std::thread::spawn(move || {
                            let deadline =
                                std::time::Instant::now() + std::time::Duration::from_secs(15);
                            loop {
                                let snapshot = state.snapshot();
                                if snapshot.runtime_stopped {
                                    break;
                                }
                                if snapshot.failure.is_some() {
                                    app.exit(1);
                                    break;
                                }
                                if snapshot.page_loaded && snapshot.frontend_ready {
                                    // Worker panics are also reflected in the
                                    // shutdown result before the final receipt.
                                    std::thread::sleep(std::time::Duration::from_secs(1));
                                    app.exit(0);
                                    break;
                                }
                                if std::time::Instant::now() >= deadline {
                                    state.fail(StartupFailure {
                                        code: "DESKTOP_READY_TIMEOUT",
                                        message: "原生页面未在期限内完成初始化".into(),
                                    });
                                    app.exit(1);
                                    break;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(50));
                            }
                        });
                        *event_watcher.lock().unwrap_or_else(|p| p.into_inner()) = Some(handle);
                    }
                } else if matches!(event, tauri::RunEvent::Exit) {
                    // DesktopState still owns RuntimeAccessGuard while all
                    // runtime workers are synchronously joined here.
                    if let Some(runtime) =
                        app.try_state::<Arc<crate::desktop_runtime::DesktopRuntime>>()
                    {
                        if !runtime.shutdown() {
                            event_state.fail(StartupFailure {
                                code: "DESKTOP_RUNTIME_SHUTDOWN_FAILED",
                                message: "桌面后台已停止，但部分工作线程未正常完成".into(),
                            });
                        }
                    }
                    event_state.update(|state| state.runtime_stopped = true);
                }
            });
            if let Some(handle) = watcher.lock().unwrap_or_else(|p| p.into_inner()).take() {
                if handle.join().is_err() {
                    startup.fail(StartupFailure {
                        code: "STARTUP_PROBE_WORKER_FAILED",
                        message: "启动检查工作线程未正常完成".into(),
                    });
                }
            }
            exit_code
        }
        Err(error) => {
            startup.fail(StartupFailure::from_error(&error));
            startup.update(|state| state.runtime_stopped = true);
            if probe.is_none() {
                show_failure(&startup.snapshot());
            }
            1
        }
    };
    let mut final_state = startup.snapshot();
    // A page racing a failed setup cannot serve as successful readiness evidence.
    final_state.page_loaded &= final_state.setup_completed;
    final_state.frontend_ready &= final_state.setup_completed;
    let success = exit_code == 0 && final_state.failure.is_none();
    if let Some(probe) = probe {
        // The main entry point returns SUCCESS/FAILURE from this bool, so the
        // receipt uses the same code even if the event loop returned zero after
        // a controlled setup or shutdown failure.
        probe.report(&final_state, i32::from(!success));
    }
    success
}
