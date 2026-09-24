//! Epoch-isolated in-memory quota cache.
//!
//! The cache is keyed by source epoch; when CLI/config changes, the epoch
//! increments and old snapshots are rejected. This prevents stale data from
//! a different account or CLI version from polluting the current view.

use super::{QuotaSnapshot, QuotaStatus};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

/// Default cache TTL: 2 minutes.
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(120);

/// Thread-safe quota cache with epoch isolation.
#[derive(Clone)]
pub struct QuotaCache {
    inner: Arc<RwLock<QuotaCacheInner>>,
}

struct QuotaCacheInner {
    snapshot: Option<QuotaSnapshot>,
    current_epoch: u64,
    ttl: Duration,
}

impl QuotaCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(QuotaCacheInner {
                snapshot: None,
                current_epoch: 0,
                ttl: DEFAULT_CACHE_TTL,
            })),
        }
    }

    /// Updates the current epoch, invalidating any cached snapshot.
    pub fn set_epoch(&self, epoch: u64) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if inner.current_epoch != epoch {
            inner.current_epoch = epoch;
            inner.snapshot = None;
        }
    }

    /// Stores a snapshot if its epoch matches the current epoch.
    /// Returns true if stored, false if rejected (epoch mismatch).
    pub fn store(&self, snapshot: QuotaSnapshot) -> bool {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if snapshot.source_epoch != inner.current_epoch {
            return false;
        }
        inner.snapshot = Some(snapshot);
        true
    }

    /// Returns a clone of the cached snapshot if:
    /// - A snapshot exists
    /// - Its epoch matches the current epoch
    /// - It is not older than the TTL
    ///
    /// Returns `None` if any condition fails.
    pub fn load(&self) -> Option<QuotaSnapshot> {
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let snapshot = inner.snapshot.as_ref()?;

        if snapshot.source_epoch != inner.current_epoch {
            return None;
        }

        let age = SystemTime::now()
            .duration_since(snapshot.sampled_at)
            .unwrap_or(Duration::from_secs(0));

        if age > inner.ttl {
            return None;
        }

        Some(snapshot.clone())
    }

    /// Returns the cached snapshot regardless of TTL, for diagnostic purposes.
    /// Still respects epoch isolation.
    #[cfg(test)]
    pub fn load_stale(&self) -> Option<QuotaSnapshot> {
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let snapshot = inner.snapshot.as_ref()?;

        if snapshot.source_epoch != inner.current_epoch {
            return None;
        }

        Some(snapshot.clone())
    }

    /// Returns the status of the cached snapshot.
    pub fn status(&self) -> QuotaStatus {
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let Some(snapshot) = inner.snapshot.as_ref() else {
            return QuotaStatus::Checking;
        };

        if snapshot.source_epoch != inner.current_epoch {
            return QuotaStatus::Checking;
        }

        let age = SystemTime::now()
            .duration_since(snapshot.sampled_at)
            .unwrap_or(Duration::from_secs(0));

        if age > inner.ttl {
            QuotaStatus::Stale
        } else {
            snapshot.status
        }
    }

    /// Clears the cached snapshot without changing the epoch.
    #[cfg(test)]
    pub fn clear(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        inner.snapshot = None;
    }

    /// Sets a custom TTL (for testing).
    #[cfg(test)]
    pub fn set_ttl(&self, ttl: Duration) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        inner.ttl = ttl;
    }
}

impl Default for QuotaCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn make_snapshot(epoch: u64, _used_percent: u8) -> QuotaSnapshot {
        QuotaSnapshot {
            source_epoch: epoch,
            cli_path: "/usr/bin/codex".into(),
            cli_version: "0.1.25".into(),
            account_id: Some("user@example.com".into()),
            account_type: Some("chatgpt".into()),
            primary: None,
            secondary: None,
            sampled_at: SystemTime::now(),
            status: QuotaStatus::Ready,
        }
    }

    #[test]
    fn store_and_load_within_epoch() {
        let cache = QuotaCache::new();
        cache.set_epoch(1);
        let snapshot = make_snapshot(1, 50);
        assert!(cache.store(snapshot.clone()));
        let loaded = cache.load().unwrap();
        assert_eq!(loaded.source_epoch, 1);
    }

    #[test]
    fn epoch_change_invalidates_cache() {
        let cache = QuotaCache::new();
        cache.set_epoch(1);
        cache.store(make_snapshot(1, 50));
        assert!(cache.load().is_some());

        cache.set_epoch(2);
        assert!(cache.load().is_none());
    }

    #[test]
    fn store_rejects_wrong_epoch() {
        let cache = QuotaCache::new();
        cache.set_epoch(2);
        let snapshot = make_snapshot(1, 50);
        assert!(!cache.store(snapshot));
        assert!(cache.load().is_none());
    }

    #[test]
    fn ttl_expiry() {
        let cache = QuotaCache::new();
        cache.set_ttl(Duration::from_millis(50));
        cache.set_epoch(1);
        cache.store(make_snapshot(1, 50));

        assert!(cache.load().is_some());
        thread::sleep(Duration::from_millis(60));
        assert!(cache.load().is_none());
    }

    #[test]
    fn load_stale_ignores_ttl_but_respects_epoch() {
        let cache = QuotaCache::new();
        cache.set_ttl(Duration::from_millis(50));
        cache.set_epoch(1);
        cache.store(make_snapshot(1, 50));

        thread::sleep(Duration::from_millis(60));
        assert!(cache.load().is_none());
        assert!(cache.load_stale().is_some());

        cache.set_epoch(2);
        assert!(cache.load_stale().is_none());
    }

    #[test]
    fn clear_removes_snapshot() {
        let cache = QuotaCache::new();
        cache.set_epoch(1);
        cache.store(make_snapshot(1, 50));
        assert!(cache.load().is_some());

        cache.clear();
        assert!(cache.load().is_none());
    }

    #[test]
    fn status_reflects_cache_state() {
        let cache = QuotaCache::new();
        cache.set_epoch(1);
        assert_eq!(cache.status(), QuotaStatus::Checking);

        cache.store(make_snapshot(1, 50));
        assert_eq!(cache.status(), QuotaStatus::Ready);

        cache.set_ttl(Duration::from_millis(50));
        thread::sleep(Duration::from_millis(60));
        assert_eq!(cache.status(), QuotaStatus::Stale);
    }
}
