use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const FRONTEND_COMMANDS: &[&str] = &[
    "desktop_health_check",
    "desktop_health_export",
    "desktop_health_test_start",
    "desktop_health_test_status",
    "desktop_activity_page",
    "desktop_activity_detail",
    "desktop_attention_ack",
    "desktop_result_mark_seen",
    "desktop_host_focus",
    "desktop_notification_hold_status",
    "desktop_notification_hold_set",
    "desktop_notification_hold_resume",
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

// Exact APIs used by WindowTitleBar and the read-only runtime subscriptions.
const CORE_PERMISSIONS: &[&str] = &[
    "core:event:allow-listen",
    "core:event:allow-unlisten",
    "core:window:allow-is-maximized",
    "core:window:allow-internal-toggle-maximize",
    "core:window:allow-close",
    "core:window:allow-minimize",
    "core:window:allow-start-dragging",
    "core:window:allow-toggle-maximize",
];

fn tauri_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repository_root() -> PathBuf {
    tauri_root()
        .parent()
        .expect("src-tauri must have a repository parent")
        .to_path_buf()
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).expect("fixture source must be readable UTF-8")
}

fn json(path: impl AsRef<Path>) -> Value {
    serde_json::from_str(&read(path)).expect("configuration must be valid JSON")
}

fn expected_permissions() -> BTreeSet<String> {
    FRONTEND_COMMANDS
        .iter()
        .map(|command| format!("allow-{}", command.replace('_', "-")))
        .chain(
            CORE_PERMISSIONS
                .iter()
                .map(|permission| (*permission).to_owned()),
        )
        .collect()
}

fn extract_manifest_commands(build_script: &str) -> BTreeSet<String> {
    let body = build_script
        .split_once("const FRONTEND_COMMANDS: &[&str] = &[")
        .expect("build script must declare the frontend ACL command list")
        .1
        .split_once("];")
        .expect("frontend ACL command list must be closed")
        .0;
    body.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix('"')?
                .strip_suffix("\",")
                .map(str::to_owned)
        })
        .collect()
}

fn extract_handler_commands(lib: &str) -> BTreeSet<String> {
    let body = lib
        .split_once(".invoke_handler(tauri::generate_handler![")
        .expect("application must have a generated invoke handler")
        .1
        .split_once("])")
        .expect("generated invoke handler must be closed")
        .0;
    body.lines()
        .filter_map(|line| {
            let entry = line.trim().trim_end_matches(',');
            entry
                .rsplit("::")
                .next()
                .filter(|command| !command.is_empty())
                .map(str::to_owned)
        })
        .collect()
}

fn visit_frontend_sources(root: &Path, output: &mut String) {
    for entry in fs::read_dir(root).expect("frontend source directory must be readable") {
        let entry = entry.expect("frontend directory entry must be readable");
        let path = entry.path();
        if path.is_dir() {
            visit_frontend_sources(&path, output);
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.contains(".test.") || name.contains(".spec.") {
            continue;
        }
        if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("ts" | "vue")
        ) {
            output.push_str(&read(path));
            output.push('\n');
        }
    }
}

fn extract_literal_invokes(source: &str) -> BTreeSet<String> {
    let mut commands = BTreeSet::new();
    let mut remaining = source;
    while let Some(offset) = remaining.find("invoke") {
        remaining = &remaining[offset + "invoke".len()..];
        let Some(open) = remaining.find('(') else {
            break;
        };
        let argument = remaining[open + 1..].trim_start();
        let Some(quote) = argument
            .chars()
            .next()
            .filter(|value| matches!(value, '\'' | '"'))
        else {
            remaining = &remaining[open + 1..];
            continue;
        };
        if let Some(end) = argument[1..].find(quote) {
            commands.insert(argument[1..end + 1].to_owned());
        }
        remaining = &remaining[open + 1..];
    }
    commands
}

#[test]
fn main_capability_is_an_exact_least_privilege_allowlist() {
    let root = tauri_root();
    assert!(!root.join("capabilities/default.json").exists());
    let capability = json(root.join("capabilities/main.json"));
    assert_eq!(capability["identifier"], "main");
    assert_eq!(capability["windows"], serde_json::json!(["main"]));
    assert_eq!(capability["platforms"], serde_json::json!(["windows"]));
    let permissions = capability["permissions"]
        .as_array()
        .expect("permissions must be an array")
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("permission must be a string")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(permissions, expected_permissions());
    assert!(permissions
        .iter()
        .all(|permission| !permission.contains(":default")));
    assert!(permissions
        .iter()
        .all(|permission| !permission.contains("unsafe")));
}

#[test]
fn every_frontend_command_is_registered_manifested_and_authorized() {
    let tauri = tauri_root();
    let expected = FRONTEND_COMMANDS
        .iter()
        .map(|command| (*command).to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        extract_manifest_commands(&read(tauri.join("build.rs"))),
        expected
    );
    assert_eq!(
        extract_handler_commands(&read(tauri.join("src/desktop_app.rs"))),
        expected
    );

    let generated_root = tauri.join("permissions/autogenerated");
    let generated = fs::read_dir(&generated_root)
        .expect("Tauri must generate one ACL permission file per app command")
        .map(|entry| {
            let path = entry
                .expect("generated permission entry must be readable")
                .path();
            let command = path
                .file_stem()
                .and_then(|value| value.to_str())
                .expect("permission filename must be UTF-8")
                .to_owned();
            let permission = read(path);
            assert!(permission.contains(&format!("commands.allow = [\"{command}\"]")));
            assert!(permission.contains(&format!("commands.deny = [\"{command}\"]")));
            command
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(generated, expected);

    let mut frontend = String::new();
    visit_frontend_sources(&repository_root().join("src"), &mut frontend);
    let mut invoked = extract_literal_invokes(&frontend);
    invoked.extend(extract_literal_invokes(
        &frontend.replace("act(", "invoke("),
    ));
    // Resource-scoped mutations pass commands as their second argument; closed
    // action maps also contain literal command values rather than direct invokes.
    invoked.extend(
        frontend
            .split(['\'', '"'])
            .filter(|value| {
                value.starts_with("desktop_")
                    && value.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            })
            .map(str::to_owned),
    );
    assert_eq!(invoked, expected);
}

#[test]
fn csp_and_core_api_surface_are_closed() {
    let config = json(tauri_root().join("tauri.conf.json"));
    assert_eq!(
        config["app"]["security"]["capabilities"],
        serde_json::json!(["main"])
    );
    let csp = config["app"]["security"]["csp"]
        .as_str()
        .expect("CSP must be a string");
    assert!(!csp.contains("unsafe-inline"));
    assert!(!csp.contains("unsafe-eval"));
    for directive in [
        "default-src 'self'",
        "connect-src ipc: http://ipc.localhost",
        "style-src 'self'",
        "object-src 'none'",
        "base-uri 'none'",
        "frame-ancestors 'none'",
        "form-action 'none'",
    ] {
        assert!(csp.contains(directive), "missing CSP directive {directive}");
    }

    let mut frontend = String::new();
    visit_frontend_sources(&repository_root().join("src"), &mut frontend);
    frontend.push_str(&read(repository_root().join("index.html")));
    assert!(
        !frontend.contains("style="),
        "strict style-src requires all inline style attributes to be removed"
    );
    for import in [
        "@tauri-apps/api/app",
        "@tauri-apps/api/image",
        "@tauri-apps/api/menu",
        "@tauri-apps/api/path",
        "@tauri-apps/api/resources",
        "@tauri-apps/api/tray",
        "@tauri-apps/api/webview",
        "@tauri-apps/api/webviewWindow",
    ] {
        assert!(
            !frontend.contains(import),
            "unauthorized core API import {import}"
        );
    }
}
