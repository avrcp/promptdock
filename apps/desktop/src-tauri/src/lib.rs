mod activity;
pub mod adapters;
pub mod agent;
pub mod agent_event_capture;
mod atomic_file;
mod capture_policy;
pub mod codex_quota;
mod content_crypto;
mod db;
mod diagnostics;
pub mod error;
mod event_runtime;
pub mod hook_installer;
mod inbox;
pub mod model;
mod notification;
mod platform;
mod relay;
pub mod runtime_access;
mod settings;
pub mod source;
mod stable_data_root;
mod storage;

mod desktop_app;
pub mod desktop_capture;
mod desktop_health;
mod desktop_policy;
mod desktop_results;
mod desktop_runtime;
mod desktop_startup;
mod desktop_tray;
mod hook_health;
pub mod hook_self_test;
mod hook_verification;
pub mod launcher;

pub fn run() -> bool {
    desktop_app::run()
}

pub fn self_test_desktop_startup(scenario: &str, cleanup_token: &str) -> bool {
    desktop_app::self_test_startup(scenario, cleanup_token)
}
