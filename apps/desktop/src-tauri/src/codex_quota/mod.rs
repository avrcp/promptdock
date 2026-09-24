//! Independent Codex account quota collector.
//!
//! This module provides a background quota snapshot service that queries
//! account rate limits via the Codex app-server protocol. It is fully
//! independent from the Hook/ingestor/finalizer pipeline and must never
//! block notification delivery.

mod app_server;
mod cache;
mod cli_discovery;
mod normalize;
mod runtime;

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

pub use runtime::CodexQuotaRuntime;

/// Quota collection status for UI display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    /// Quota collection is disabled in settings.
    Disabled,
    /// CLI not configured or not found.
    CliNotFound,
    /// CLI found but not yet queried.
    Checking,
    /// Valid snapshot available.
    Ready,
    /// Snapshot exists but is older than cache TTL.
    Stale,
    /// CLI requires authentication.
    LoginRequired,
    /// Account type not supported or unverified.
    AccountUnverified,
    /// app-server method not supported by this CLI version.
    UnsupportedMethod,
    /// Response schema changed in incompatible way.
    UnsupportedSchema,
    /// Query timed out.
    Timeout,
    /// CLI returned rate limit error.
    RateLimited,
    /// Generic failure (see diagnostics for details).
    Unavailable,
}

/// A frozen quota snapshot with sampling metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    /// Epoch when CLI/config changed; invalidates old snapshots.
    pub source_epoch: u64,
    /// CLI executable path (for diagnostics, not credentials).
    pub cli_path: String,
    /// CLI version string (e.g., "0.1.25").
    pub cli_version: String,
    /// Account identifier (email or opaque ID, redacted if needed).
    pub account_id: Option<String>,
    /// Account type (e.g., "chatgpt", "api").
    pub account_type: Option<String>,
    /// Primary rate limit window (e.g., 5-hour rolling).
    pub primary: Option<RateLimitWindow>,
    /// Secondary rate limit window (e.g., weekly).
    pub secondary: Option<RateLimitWindow>,
    /// When this snapshot was sampled.
    pub sampled_at: SystemTime,
    /// Status at sampling time.
    pub status: QuotaStatus,
}

/// A single rate limit window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitWindow {
    /// Limit group identifier (e.g., "codex", "gpt-4").
    pub limit_id: String,
    /// Human-readable window name (e.g., "5小时窗口").
    pub display_name: Option<String>,
    /// Percentage of quota used (0-100).
    pub used_percent: u8,
    /// Window duration in minutes.
    pub window_duration_mins: i64,
    /// When this window resets (Unix seconds).
    pub resets_at: i64,
}

impl QuotaSnapshot {
    /// Converts to the legacy `CodexUsageSnapshot` for notification rendering.
    /// Uses primary window if available, else secondary.
    pub fn to_usage_snapshot(&self) -> Option<crate::agent::CodexUsageSnapshot> {
        let window = self.primary.as_ref().or(self.secondary.as_ref())?;
        Some(crate::agent::CodexUsageSnapshot {
            used_percent: Some(window.used_percent),
            window_duration_mins: Some(window.window_duration_mins),
            resets_at: Some(window.resets_at),
        })
    }
}
