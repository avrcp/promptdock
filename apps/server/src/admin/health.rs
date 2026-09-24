use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

#[derive(Clone, Debug, Default)]
pub struct RuntimeHealthRegistry {
    workers: Arc<RwLock<BTreeMap<WorkerKey, WorkerHealth>>>,
}

impl RuntimeHealthRegistry {
    pub fn set_state(&self, key: WorkerKey, state: WorkerState) {
        let now = unix_timestamp_ms();
        if let Ok(mut workers) = self.workers.write() {
            let health = workers.entry(key).or_insert_with(|| WorkerHealth {
                state,
                started_at: Some(now),
                last_tick_at: None,
                last_error_code: None,
            });
            health.state = state;
            if health.started_at.is_none() && state != WorkerState::Disabled {
                health.started_at = Some(now);
            }
        }
    }

    pub fn fail(&self, key: WorkerKey, error_code: &'static str) {
        if let Ok(mut workers) = self.workers.write() {
            let now = unix_timestamp_ms();
            let health = workers.entry(key).or_insert_with(|| WorkerHealth {
                state: WorkerState::Failed,
                started_at: Some(now),
                last_tick_at: None,
                last_error_code: None,
            });
            health.state = WorkerState::Failed;
            health.last_error_code = Some(error_code);
        }
    }

    pub fn tick(&self, key: WorkerKey) {
        if let Ok(mut workers) = self.workers.write() {
            let now = unix_timestamp_ms();
            let health = workers.entry(key).or_insert_with(|| WorkerHealth {
                state: WorkerState::Running,
                started_at: Some(now),
                last_tick_at: None,
                last_error_code: None,
            });
            health.last_tick_at = Some(now);
        }
    }

    pub fn snapshot(&self) -> BTreeMap<WorkerKey, WorkerHealth> {
        self.workers
            .read()
            .map(|workers| workers.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerKey {
    Outbox,
    Inbound,
    Retention,
    WechatMonitor,
    Gateway,
    AdminHttp,
    PublicHttp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Starting,
    Running,
    Idle,
    Degraded,
    Failed,
    Stopping,
    Stopped,
    Disabled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerHealth {
    pub state: WorkerState,
    pub started_at: Option<i64>,
    pub last_tick_at: Option<i64>,
    pub last_error_code: Option<&'static str>,
}

fn unix_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_uses_closed_worker_keys_and_safe_error_codes() {
        let registry = RuntimeHealthRegistry::default();
        registry.set_state(WorkerKey::AdminHttp, WorkerState::Starting);
        registry.tick(WorkerKey::AdminHttp);
        registry.fail(WorkerKey::AdminHttp, "ADMIN_HTTP_STOPPED");
        let snapshot = registry.snapshot();
        let health = snapshot.get(&WorkerKey::AdminHttp).expect("admin health");
        assert_eq!(health.state, WorkerState::Failed);
        assert!(health.started_at.is_some());
        assert!(health.last_tick_at.is_some());
        assert_eq!(health.last_error_code, Some("ADMIN_HTTP_STOPPED"));
    }
}
