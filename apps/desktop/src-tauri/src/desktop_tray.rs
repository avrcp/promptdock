//! Tray ownership for the desktop host.  This module deliberately reads only
//! the local activity projection; it never starts a worker or contacts Relay
//! while updating the shell UI.

use std::{
    error::Error,
    sync::Mutex,
    time::{Duration, Instant},
};

use rusqlite::OptionalExtension;
use tauri::{Emitter, Manager};

use crate::{
    activity::ActivityPageRequest,
    db::{now_ms, Db},
    error::AppError,
    notification::{
        hold::{HoldDuration, UserHoldState},
        runtime::NotificationRuntimeState,
    },
    relay::state::RelayRuntimeState,
    storage::activity_repository::ActivityRepository,
};

const TRAY_ID: &str = "promptdock-desktop";
const SCAN_THROTTLE: Duration = Duration::from_secs(1);
const EXPIRY_RECHECK: Duration = Duration::from_secs(10);

const MENU_SHOW: &str = "tray-show";
const MENU_ATTENTION: &str = "tray-attention";
const MENU_LATEST: &str = "tray-latest-result";
const MENU_HOST: &str = "tray-focus-host";
const MENU_HOLD_15: &str = "tray-hold-15";
const MENU_HOLD_60: &str = "tray-hold-60";
const MENU_RESUME: &str = "tray-hold-resume";
const MENU_QUIT: &str = "tray-quit";

#[derive(Clone)]
struct TrayHandles {
    icon: tauri::tray::TrayIcon,
    attention: tauri::menu::MenuItem<tauri::Wry>,
    latest: tauri::menu::MenuItem<tauri::Wry>,
    hold_15: tauri::menu::MenuItem<tauri::Wry>,
    hold_60: tauri::menu::MenuItem<tauri::Wry>,
    resume: tauri::menu::MenuItem<tauri::Wry>,
}

/// Managed before `DesktopRuntime` starts.  Menu events obtain runtime state
/// lazily so a fallible tray setup can never leave background workers alive.
pub(crate) struct TrayState {
    handles: TrayHandles,
    summary: Mutex<Option<TraySummary>>,
    scan: Mutex<ScanState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TraySummary {
    attention_count: u32,
    started_count: u32,
    latest_result_id: Option<String>,
    hold: UserHoldState,
    hold_until: Option<i64>,
    relay_problem: bool,
}

#[derive(Default)]
struct ScanState {
    last_db_revision: Option<u64>,
    last_scan: Option<Instant>,
}

pub(crate) fn install(app: &tauri::App) -> Result<(), Box<dyn Error>> {
    let show = tauri::menu::MenuItem::with_id(
        app,
        MENU_SHOW,
        "打开 PromptDock Desktop",
        true,
        None::<&str>,
    )?;
    let attention =
        tauri::menu::MenuItem::with_id(app, MENU_ATTENTION, "待查看 0", false, None::<&str>)?;
    let latest =
        tauri::menu::MenuItem::with_id(app, MENU_LATEST, "打开最新结果", false, None::<&str>)?;
    let focus_host =
        tauri::menu::MenuItem::with_id(app, MENU_HOST, "切回桌面应用", true, None::<&str>)?;
    let hold_15 =
        tauri::menu::MenuItem::with_id(app, MENU_HOLD_15, "暂缓推送 15 分钟", true, None::<&str>)?;
    let hold_60 =
        tauri::menu::MenuItem::with_id(app, MENU_HOLD_60, "暂缓推送 60 分钟", true, None::<&str>)?;
    let resume = tauri::menu::MenuItem::with_id(app, MENU_RESUME, "恢复推送", false, None::<&str>)?;
    let activity_separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let host_separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let hold_separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let quit = tauri::menu::MenuItem::with_id(app, MENU_QUIT, "退出", true, None::<&str>)?;
    let menu = tauri::menu::Menu::with_items(
        app,
        &[
            &show,
            &attention,
            &latest,
            &activity_separator,
            &focus_host,
            &host_separator,
            &hold_15,
            &hold_60,
            &resume,
            &hold_separator,
            &quit,
        ],
    )?;
    let icon = tauri::tray::TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("PromptDock Desktop：正常")
        .icon(symbol_icon(TrayTone::Normal))
        .menu(&menu)
        .on_menu_event(handle_menu_event)
        .build(app)?;
    app.manage(TrayState {
        handles: TrayHandles {
            icon,
            attention,
            latest,
            hold_15,
            hold_60,
            resume,
        },
        summary: Mutex::new(None),
        scan: Mutex::new(ScanState::default()),
    });
    Ok(())
}

/// Refreshes shell-only metadata.  The maintenance owner may call this every
/// 500ms: a local DB scan is coalesced to one second and skipped unless the
/// dirty revision changed, with a ten-second expiry backstop.
pub(crate) fn refresh(
    app: &tauri::AppHandle,
    db: &Db,
    delivery: &NotificationRuntimeState,
    relay: &RelayRuntimeState,
) -> Result<(), AppError> {
    let Some(state) = app.try_state::<TrayState>() else {
        return Ok(());
    };
    let now_instant = Instant::now();
    let revision = db.revision();
    {
        let mut scan = state.scan.lock().map_err(|_| unavailable())?;
        let elapsed = scan
            .last_scan
            .map(|at| now_instant.saturating_duration_since(at));
        let due_for_expiry = elapsed.is_none_or(|value| value >= EXPIRY_RECHECK);
        let dirty = scan.last_db_revision != Some(revision);
        if elapsed.is_some_and(|value| value < SCAN_THROTTLE) || (!dirty && !due_for_expiry) {
            return Ok(());
        }
        scan.last_scan = Some(now_instant);
        scan.last_db_revision = Some(revision);
    }

    let now = now_ms()?;
    let page = db.with_connection(|conn| {
        ActivityRepository::page(
            conn,
            &ActivityPageRequest {
                limit: Some(1),
                ..Default::default()
            },
        )
    })?;
    let latest_result_id = db.with_connection(|conn| latest_open_result_id(conn, now))?;
    let hold_status = delivery.user_hold_status(now)?;
    let hold = hold_status.state;
    let relay_status = relay.status();
    let relay_problem = relay_status.configured && !relay.notification_delivery_ready();
    let summary = TraySummary {
        attention_count: page.counts.attention,
        started_count: page.counts.started,
        latest_result_id,
        hold,
        hold_until: (hold == UserHoldState::Active)
            .then_some(hold_status.until)
            .flatten()
            .filter(|until| *until > now),
        relay_problem,
    };

    let mut previous = state.summary.lock().map_err(|_| unavailable())?;
    if previous.as_ref() == Some(&summary) {
        return Ok(());
    }
    apply_summary(&state.handles, &summary)?;
    *previous = Some(summary);
    Ok(())
}

fn latest_open_result_id(
    conn: &rusqlite::Connection,
    now: i64,
) -> Result<Option<String>, AppError> {
    conn.query_row(
        "SELECT outbox.id
           FROM notification_outbox AS outbox
           INNER JOIN relay_result_publications AS pages ON pages.outbox_id = outbox.id
           INNER JOIN agent_runs AS runs ON runs.run_key = outbox.agent_run_key
          WHERE runs.parent_run_key IS NULL
            AND pages.page_state = 'available'
            AND pages.page_expires_at > ?
          ORDER BY runs.last_event_at DESC,
                   pages.result_revision DESC,
                   outbox.id DESC
          LIMIT 1",
        [now],
        |row| row.get(0),
    )
    .optional()
    .map_err(AppError::from)
}

fn apply_summary(handles: &TrayHandles, summary: &TraySummary) -> Result<(), AppError> {
    let tone = summary.tone();
    handles
        .attention
        .set_text(format!("待查看 {}", summary.attention_count))
        .map_err(tray_error)?;
    handles
        .attention
        .set_enabled(summary.attention_count > 0)
        .map_err(tray_error)?;
    handles
        .latest
        .set_enabled(summary.latest_result_id.is_some())
        .map_err(tray_error)?;
    let held = summary.hold != UserHoldState::Inactive;
    handles.hold_15.set_enabled(!held).map_err(tray_error)?;
    handles.hold_60.set_enabled(!held).map_err(tray_error)?;
    handles.resume.set_enabled(held).map_err(tray_error)?;
    handles
        .icon
        .set_tooltip(Some(summary.tooltip()))
        .map_err(tray_error)?;
    handles
        .icon
        .set_icon(Some(symbol_icon(tone)))
        .map_err(tray_error)
}

impl TraySummary {
    fn tone(&self) -> TrayTone {
        if self.hold != UserHoldState::Inactive {
            TrayTone::Hold
        } else if self.relay_problem {
            TrayTone::ConnectionProblem
        } else if self.attention_count > 0 {
            TrayTone::Attention
        } else {
            TrayTone::Normal
        }
    }

    fn tooltip(&self) -> String {
        let state = match self.tone() {
            TrayTone::Normal => "正常",
            TrayTone::Attention => "有待查看提醒",
            TrayTone::ConnectionProblem => "Relay 连接异常",
            TrayTone::Hold => "通知暂缓中",
        };
        let hold_until = self
            .hold_until
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|until| {
                format!(
                    "；暂缓至 {}",
                    until.with_timezone(&chrono::Local).format("%H:%M")
                )
            })
            .unwrap_or_default();
        format!(
            "PromptDock Desktop：{state}；待查看 {}；已开始 {}{hold_until}",
            self.attention_count, self.started_count
        )
    }
}

#[derive(Clone, Copy)]
enum TrayTone {
    Normal,
    Attention,
    ConnectionProblem,
    Hold,
}

fn symbol_icon(tone: TrayTone) -> tauri::image::Image<'static> {
    let mut rgba = vec![0_u8; 32 * 32 * 4];
    match tone {
        // Green circle and check mark: ready for the normal state.
        TrayTone::Normal => {
            fill_circle(&mut rgba, 16, 16, 13, [50, 185, 120, 255]);
            draw_line(&mut rgba, 8, 16, 13, 21, [255, 255, 255, 255], 2);
            draw_line(&mut rgba, 13, 21, 24, 10, [255, 255, 255, 255], 2);
        }
        // Amber triangle and exclamation mark: pending attention.
        TrayTone::Attention => {
            for y in 4..28 {
                let half_width = (y - 4) / 2 + 1;
                for x in (16 - half_width)..=(16 + half_width) {
                    set_pixel(&mut rgba, x, y, [239, 168, 45, 255]);
                }
            }
            draw_line(&mut rgba, 16, 11, 16, 19, [255, 255, 255, 255], 2);
            fill_circle(&mut rgba, 16, 23, 2, [255, 255, 255, 255]);
        }
        // Red X: Relay is configured but no longer delivery-ready.
        TrayTone::ConnectionProblem => {
            draw_line(&mut rgba, 7, 7, 24, 24, [208, 74, 74, 255], 3);
            draw_line(&mut rgba, 24, 7, 7, 24, [208, 74, 74, 255], 3);
        }
        // Blue pause bars: outbound delivery is temporarily held.
        TrayTone::Hold => {
            fill_rect(&mut rgba, 8, 6, 13, 26, [102, 126, 165, 255]);
            fill_rect(&mut rgba, 19, 6, 24, 26, [102, 126, 165, 255]);
        }
    }
    tauri::image::Image::new_owned(rgba, 32, 32)
}

fn set_pixel(rgba: &mut [u8], x: i32, y: i32, color: [u8; 4]) {
    if !(0..32).contains(&x) || !(0..32).contains(&y) {
        return;
    }
    let index = ((y as usize * 32) + x as usize) * 4;
    rgba[index..index + 4].copy_from_slice(&color);
}

fn fill_circle(rgba: &mut [u8], center_x: i32, center_y: i32, radius: i32, color: [u8; 4]) {
    for y in center_y - radius..=center_y + radius {
        for x in center_x - radius..=center_x + radius {
            if (x - center_x).pow(2) + (y - center_y).pow(2) <= radius.pow(2) {
                set_pixel(rgba, x, y, color);
            }
        }
    }
}

fn fill_rect(rgba: &mut [u8], left: i32, top: i32, right: i32, bottom: i32, color: [u8; 4]) {
    for y in top..bottom {
        for x in left..right {
            set_pixel(rgba, x, y, color);
        }
    }
}

fn draw_line(rgba: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32, color: [u8; 4], width: i32) {
    let (mut x, mut y) = (x0, y0);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let step_x = if x0 < x1 { 1 } else { -1 };
    let step_y = if y0 < y1 { 1 } else { -1 };
    let mut error = dx + dy;
    loop {
        fill_circle(rgba, x, y, width, color);
        if x == x1 && y == y1 {
            break;
        }
        let doubled = 2 * error;
        if doubled >= dy {
            error += dy;
            x += step_x;
        }
        if doubled <= dx {
            error += dx;
            y += step_y;
        }
    }
}

fn handle_menu_event(app: &tauri::AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        MENU_SHOW => show_main_window(app),
        MENU_ATTENTION => {
            let _ = app.emit("tray-activity-attention", ());
            show_main_window(app);
        }
        MENU_LATEST => open_latest_result(app),
        MENU_HOST => focus_host(app),
        MENU_HOLD_15 => set_hold(app, HoldDuration::Minutes15),
        MENU_HOLD_60 => set_hold(app, HoldDuration::Minutes60),
        MENU_RESUME => resume_hold(app),
        MENU_QUIT => app.exit(0),
        _ => {}
    }
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn open_latest_result(app: &tauri::AppHandle) {
    let Some(runtime) = app.try_state::<std::sync::Arc<crate::desktop_runtime::DesktopRuntime>>()
    else {
        feedback(app, "桌面运行时尚未就绪", "danger");
        return;
    };
    let Some(tray) = app.try_state::<TrayState>() else {
        return;
    };
    let id = tray
        .summary
        .lock()
        .ok()
        .and_then(|summary| summary.as_ref()?.latest_result_id.clone());
    let Some(id) = id else {
        feedback(app, "没有可打开的最新结果", "danger");
        return;
    };
    let relay = runtime.relay.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match relay.result_link(&id).await.and_then(|link| {
            crate::desktop_app::open_result_url(&link.url)
                .map_err(|message| AppError::new("RESULT_OPEN_FAILED", message))
        }) {
            Ok(()) => feedback(&app, "已打开最新结果", "success"),
            Err(error) => {
                feedback(&app, &error.message, "danger");
                show_main_window(&app);
            }
        }
    });
}

fn focus_host(app: &tauri::AppHandle) {
    let Some(launcher) = app.try_state::<crate::launcher::LauncherState>() else {
        feedback(app, "宿主启动器尚未就绪", "danger");
        return;
    };
    let launcher = launcher.inner().clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match tauri::async_runtime::spawn_blocking(move || launcher.focus_host()).await {
            Ok(Ok(result)) => {
                use crate::launcher::HostFocusStatus;
                let (message, tone) = match result.status {
                    HostFocusStatus::Focused => ("已切回桌面应用", "success"),
                    HostFocusStatus::HostNotRunning => ("桌面应用未运行，可从首页启动", "danger"),
                    HostFocusStatus::WindowNotFound => {
                        ("桌面应用进程存在，但没有可激活窗口", "danger")
                    }
                    HostFocusStatus::ForegroundDenied => {
                        ("Windows 未允许切换前台，请手动选择桌面应用", "danger")
                    }
                    HostFocusStatus::MultipleCandidates => {
                        ("检测到多个桌面应用窗口，请手动选择", "danger")
                    }
                };
                feedback(&app, message, tone);
                if result.status != HostFocusStatus::Focused {
                    show_main_window(&app);
                }
            }
            Ok(Err(error)) => {
                feedback(&app, &error.message, "danger");
                show_main_window(&app);
            }
            Err(_) => {
                feedback(&app, "宿主窗口操作失败", "danger");
                show_main_window(&app);
            }
        }
    });
}

fn set_hold(app: &tauri::AppHandle, duration: HoldDuration) {
    let Some(runtime) = app.try_state::<std::sync::Arc<crate::desktop_runtime::DesktopRuntime>>()
    else {
        feedback(app, "桌面运行时尚未就绪", "danger");
        return;
    };
    let delivery = runtime.delivery.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = tauri::async_runtime::spawn_blocking(move || {
            let now = now_ms()?;
            let status = delivery.user_hold_status(now)?;
            delivery.set_user_hold(status.revision, duration, now)
        })
        .await;
        match result {
            Ok(Ok(receipt)) if receipt.status == "applied" => {
                let _ = app.emit("notification-hold-changed", ());
                feedback(&app, "通知已暂缓", "success");
            }
            Ok(Ok(_)) => feedback(&app, "通知暂缓状态已变化，请重试", "danger"),
            Ok(Err(error)) => {
                feedback(&app, &error.message, "danger");
                show_main_window(&app);
            }
            Err(_) => {
                feedback(&app, "通知暂缓操作失败", "danger");
                show_main_window(&app);
            }
        }
    });
}

fn resume_hold(app: &tauri::AppHandle) {
    let Some(runtime) = app.try_state::<std::sync::Arc<crate::desktop_runtime::DesktopRuntime>>()
    else {
        feedback(app, "桌面运行时尚未就绪", "danger");
        return;
    };
    let delivery = runtime.delivery.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = tauri::async_runtime::spawn_blocking(move || {
            let now = now_ms()?;
            let status = delivery.user_hold_status(now)?;
            delivery.resume_user_hold(status.revision, now)
        })
        .await;
        match result {
            Ok(Ok(receipt)) if receipt.status == "applied" => {
                let _ = app.emit("notification-hold-changed", ());
                feedback(&app, "通知已恢复", "success");
            }
            Ok(Ok(_)) => feedback(&app, "通知暂缓状态已变化，请重试", "danger"),
            Ok(Err(error)) => {
                feedback(&app, &error.message, "danger");
                show_main_window(&app);
            }
            Err(_) => {
                feedback(&app, "恢复通知失败", "danger");
                show_main_window(&app);
            }
        }
    });
}

fn feedback(app: &tauri::AppHandle, message: &str, tone: &str) {
    let _ = app.emit(
        "desktop-action-feedback",
        serde_json::json!({ "message": message, "tone": tone }),
    );
}

fn unavailable() -> AppError {
    AppError::new("TRAY_STATE_UNAVAILABLE", "托盘状态暂时不可用")
}
fn tray_error(error: tauri::Error) -> AppError {
    AppError::internal("TRAY_UPDATE_FAILED", "无法更新托盘状态", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_has_a_distinct_summary_and_tooltip() {
        let summary = TraySummary {
            attention_count: 2,
            started_count: 0,
            latest_result_id: None,
            hold: UserHoldState::Inactive,
            hold_until: None,
            relay_problem: false,
        };
        assert!(matches!(summary.tone(), TrayTone::Attention));
        assert_eq!(
            summary.tooltip(),
            "PromptDock Desktop：有待查看提醒；待查看 2；已开始 0"
        );
    }

    #[test]
    fn no_result_remains_normal_when_relay_is_healthy() {
        let summary = TraySummary {
            attention_count: 0,
            started_count: 3,
            latest_result_id: None,
            hold: UserHoldState::Inactive,
            hold_until: None,
            relay_problem: false,
        };
        assert!(matches!(summary.tone(), TrayTone::Normal));
    }

    #[test]
    fn hold_precedes_connection_and_attention_status() {
        let summary = TraySummary {
            attention_count: 1,
            started_count: 0,
            latest_result_id: Some("result".into()),
            hold: UserHoldState::Active,
            hold_until: Some(123_456),
            relay_problem: true,
        };
        assert!(matches!(summary.tone(), TrayTone::Hold));
        assert!(summary.tooltip().contains("通知暂缓中"));
        assert!(summary.tooltip().contains("暂缓至 "));
    }

    #[test]
    fn four_tones_render_different_symbol_shapes() {
        let normal = symbol_icon(TrayTone::Normal);
        let attention = symbol_icon(TrayTone::Attention);
        let connection = symbol_icon(TrayTone::ConnectionProblem);
        let hold = symbol_icon(TrayTone::Hold);
        assert_ne!(normal.rgba(), attention.rgba());
        assert_ne!(attention.rgba(), connection.rgba());
        assert_ne!(connection.rgba(), hold.rgba());
    }
}
