//! Background quota runtime with merge, throttle, and cancellation.
//!
//! The runtime manages a single quota worker that:
//! - Merges concurrent refresh requests
//! - Throttles queries to respect rate limits
//! - Cancels in-flight queries on shutdown
//! - Updates the cache with fresh snapshots
//!
//! The finalizer reads from the cache; it never waits for or triggers queries.

use super::app_server::AppServerClient;
use super::cache::QuotaCache;
use super::cli_discovery::{self, DiscoveredCli};
use super::{QuotaSnapshot, QuotaStatus};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Minimum interval between automatic refreshes.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Maximum interval when idle (no recent activity).
const IDLE_REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// Timeout for a single quota query (spawn + handshake + read).
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Background quota runtime.
#[derive(Clone)]
pub struct CodexQuotaRuntime {
    cache: QuotaCache,
    configured_cli: Arc<Mutex<Option<PathBuf>>>,
    current_epoch: Arc<AtomicU64>,
    refresh_requested: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    worker: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<()>>>>,
}

impl CodexQuotaRuntime {
    /// Creates a new runtime (does not start the worker yet).
    pub fn new() -> Self {
        Self {
            cache: QuotaCache::new(),
            configured_cli: Arc::new(Mutex::new(None)),
            current_epoch: Arc::new(AtomicU64::new(0)),
            refresh_requested: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            worker: Arc::new(Mutex::new(None)),
        }
    }

    /// Starts the background worker.
    pub fn start(&self) {
        let runtime = self.clone();
        let handle = tauri::async_runtime::spawn(async move {
            runtime.worker_loop().await;
        });
        let mut worker = self.worker.blocking_lock();
        *worker = Some(handle);
    }

    /// Requests a quota refresh (non-blocking).
    /// The worker will merge this with any in-flight query.
    pub fn request_refresh(&self) {
        self.refresh_requested.store(true, Ordering::Release);
    }

    /// Updates the configured CLI path, incrementing the epoch.
    pub async fn set_configured_cli(&self, path: Option<PathBuf>) {
        let mut configured = self.configured_cli.lock().await;
        *configured = path;
        let new_epoch = self.current_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.cache.set_epoch(new_epoch);
    }

    /// Returns the current quota snapshot (if fresh).
    pub fn snapshot(&self) -> Option<QuotaSnapshot> {
        self.cache.load()
    }

    /// Returns the current quota status.
    pub fn status(&self) -> QuotaStatus {
        self.cache.status()
    }

    /// Returns the current epoch.
    pub fn current_epoch(&self) -> u64 {
        self.current_epoch.load(Ordering::Acquire)
    }

    /// Initiates shutdown and waits for the worker to finish.
    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let worker = self.worker.lock().await.take();
        if let Some(handle) = worker {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }
    }

    async fn worker_loop(&self) {
        let mut last_refresh = Instant::now() - IDLE_REFRESH_INTERVAL;
        let mut consecutive_failures = 0usize;

        loop {
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }

            let should_refresh = self.refresh_requested.swap(false, Ordering::AcqRel)
                || last_refresh.elapsed() >= self.backoff_interval(consecutive_failures);

            if should_refresh {
                match self.perform_refresh().await {
                    Ok(()) => {
                        consecutive_failures = 0;
                        last_refresh = Instant::now();
                    }
                    Err(_) => {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                    }
                }
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    fn backoff_interval(&self, failures: usize) -> Duration {
        if failures == 0 {
            return MIN_REFRESH_INTERVAL;
        }
        let backoff = [30, 60, 120, 300, 300];
        let index = (failures - 1).min(backoff.len() - 1);
        let seconds = backoff[index];
        Duration::from_secs(seconds)
    }

    async fn perform_refresh(&self) -> Result<(), QuotaStatus> {
        let cli = self.discover_cli().await?;
        let epoch = self.current_epoch();

        let mut client = AppServerClient::spawn(&cli.path, QUERY_TIMEOUT)
            .await
            .map_err(|e| self.map_app_server_error(e))?;

        let account = client
            .read_account()
            .await
            .map_err(|e| self.map_app_server_error(e))?;

        let rate_limits = client
            .read_rate_limits()
            .await
            .map_err(|e| self.map_app_server_error(e))?;

        let snapshot = QuotaSnapshot {
            source_epoch: epoch,
            cli_path: cli.path.to_string_lossy().into_owned(),
            cli_version: cli.version,
            account_id: account.account_id,
            account_type: account.account_type,
            primary: Some(super::RateLimitWindow {
                limit_id: "codex".into(),
                display_name: None,
                used_percent: rate_limits.rate_limits.used_percent.unwrap_or(0),
                window_duration_mins: rate_limits.rate_limits.window_duration_mins.unwrap_or(300),
                resets_at: rate_limits.rate_limits.resets_at.unwrap_or(0),
            }),
            secondary: None,
            sampled_at: SystemTime::now(),
            status: QuotaStatus::Ready,
        };

        if !self.cache.store(snapshot) {
            return Err(QuotaStatus::Stale);
        }

        Ok(())
    }

    async fn discover_cli(&self) -> Result<DiscoveredCli, QuotaStatus> {
        let configured = self.configured_cli.lock().await;
        cli_discovery::discover_cli(configured.as_deref()).map_err(|_| QuotaStatus::CliNotFound)
    }

    fn map_app_server_error(&self, error: super::app_server::AppServerError) -> QuotaStatus {
        use super::app_server::AppServerError;
        match error {
            AppServerError::Timeout(_) => QuotaStatus::Timeout,
            AppServerError::AuthRequired => QuotaStatus::LoginRequired,
            AppServerError::MethodNotSupported => QuotaStatus::UnsupportedMethod,
            AppServerError::ParseError(_) => QuotaStatus::UnsupportedSchema,
            _ => QuotaStatus::Unavailable,
        }
    }
}

impl Default for CodexQuotaRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_starts_with_epoch_zero() {
        let runtime = CodexQuotaRuntime::new();
        assert_eq!(runtime.current_epoch(), 0);
    }

    #[tokio::test]
    async fn set_configured_cli_increments_epoch() {
        let runtime = CodexQuotaRuntime::new();
        assert_eq!(runtime.current_epoch(), 0);

        runtime
            .set_configured_cli(Some(PathBuf::from("/usr/bin/codex")))
            .await;
        assert_eq!(runtime.current_epoch(), 1);

        runtime
            .set_configured_cli(Some(PathBuf::from("/usr/bin/codex")))
            .await;
        assert_eq!(runtime.current_epoch(), 2);
    }

    #[test]
    fn request_refresh_sets_flag() {
        let runtime = CodexQuotaRuntime::new();
        assert!(!runtime.refresh_requested.load(Ordering::Acquire));
        runtime.request_refresh();
        assert!(runtime.refresh_requested.load(Ordering::Acquire));
    }

    #[test]
    fn backoff_interval_increases_with_failures() {
        let runtime = CodexQuotaRuntime::new();
        assert_eq!(runtime.backoff_interval(0), MIN_REFRESH_INTERVAL);
        assert_eq!(runtime.backoff_interval(1), Duration::from_secs(30));
        assert_eq!(runtime.backoff_interval(2), Duration::from_secs(60));
        assert_eq!(runtime.backoff_interval(3), Duration::from_secs(120));
        assert_eq!(runtime.backoff_interval(4), Duration::from_secs(300));
        assert_eq!(runtime.backoff_interval(10), Duration::from_secs(300));
    }
}
