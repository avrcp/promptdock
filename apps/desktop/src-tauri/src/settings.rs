//! Recoverable in-memory projection of capture-policy.json. Never another file authority.
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{error::AppError, model::SettingsSnapshot};

#[derive(Debug, Default)]
struct Projection {
    snapshot: SettingsSnapshot,
    pending: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SettingsStore(Arc<Mutex<Projection>>);

impl SettingsStore {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn open(_path: std::path::PathBuf) -> Result<Self, AppError> {
        Ok(Self::new())
    }

    pub fn get(&self) -> Result<SettingsSnapshot, AppError> {
        let projection = self.guard()?;
        if projection.pending {
            return Err(AppError::new(
                "POLICY_PENDING_APPLY",
                "内容策略已保存，正在应用；相关投递已暂停",
            ));
        }
        Ok(projection.snapshot.clone())
    }

    pub(crate) fn begin_apply(&self) -> Result<(), AppError> {
        self.guard()?.pending = true;
        Ok(())
    }

    pub(crate) fn pending(&self) -> bool {
        self.guard().map(|p| p.pending).unwrap_or(true)
    }

    pub fn replace(&self, snapshot: SettingsSnapshot) -> Result<SettingsSnapshot, AppError> {
        if !snapshot.notifications.validate() {
            return Err(AppError::new(
                "INVALID_NOTIFICATION_SETTINGS",
                "通知策略无效",
            ));
        }
        let mut projection = self.guard()?;
        projection.snapshot = snapshot.clone();
        projection.pending = false;
        Ok(snapshot)
    }

    fn guard(&self) -> Result<MutexGuard<'_, Projection>, AppError> {
        self.0
            .lock()
            .map_err(|_| AppError::new("SETTINGS_UNAVAILABLE", "策略投影暂时不可用"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_projection_prevents_policy_consumption_until_applied() {
        let store = SettingsStore::new();
        let mut snapshot = store.get().unwrap();
        store.begin_apply().unwrap();
        assert_eq!(store.get().unwrap_err().code, "POLICY_PENDING_APPLY");
        snapshot.notifications.policy_revision = 7;
        store.replace(snapshot.clone()).unwrap();
        assert!(!store.pending());
        assert_eq!(store.get().unwrap(), snapshot);
    }

    #[test]
    fn projection_has_no_persistent_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notification-settings.json");
        let store = SettingsStore::open(path.clone()).unwrap();
        store.replace(store.get().unwrap()).unwrap();
        assert!(!path.exists());
    }
}
