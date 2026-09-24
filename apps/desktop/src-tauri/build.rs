fn main() {
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=PROMPTDOCK_BUILD_COMMIT={commit}");
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");
    const FRONTEND_COMMANDS: &[&str] = &[
        "desktop_health_check",
        "desktop_health_export",
        "desktop_health_test_start",
        "desktop_health_test_status",
        "desktop_host_focus",
        "desktop_notification_hold_status",
        "desktop_notification_hold_set",
        "desktop_notification_hold_resume",
        "desktop_activity_page",
        "desktop_activity_detail",
        "desktop_attention_ack",
        "desktop_result_mark_seen",
        "desktop_launcher_status",
        "desktop_launcher_refresh",
        "desktop_launcher_save",
        "desktop_launch",
        "desktop_relay_status",
        "desktop_relay_configure",
        "desktop_relay_probe",
        "desktop_relay_test",
        "desktop_deliveries",
        "desktop_delivery_detail",
        "desktop_export_delivery",
        "desktop_result_link",
        "desktop_result_open",
        "desktop_result_revoke",
        "desktop_result_resend",
        "desktop_status",
        "desktop_hook_health",
        "desktop_hook_plan",
        "desktop_hook_verify",
        "desktop_hook_self_test",
        "desktop_delivery_metadata",
        "desktop_autostart_status",
        "desktop_autostart_set",
        "desktop_diagnostics",
        "desktop_save_policy",
        "desktop_install_hook",
        "desktop_uninstall_hook",
    ];

    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(FRONTEND_COMMANDS)),
    )
    .expect("failed to build the PromptDock Tauri manifest")
}
