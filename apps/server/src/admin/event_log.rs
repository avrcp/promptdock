use std::{collections::VecDeque, sync::Arc};

use tokio::sync::Mutex;

const EVENT_CAPACITY: usize = 256;

#[derive(Clone, Debug, Default)]
pub struct OperationalEventLog {
    events: Arc<Mutex<VecDeque<OperationalEvent>>>,
}

impl OperationalEventLog {
    pub async fn record(&self, event: OperationalEvent) {
        let mut events = self.events.lock().await;
        if events.len() == EVENT_CAPACITY {
            events.pop_front();
        }
        events.push_back(event);
    }

    pub async fn snapshot(&self) -> Vec<OperationalEvent> {
        self.events.lock().await.iter().cloned().collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationalEvent {
    pub id: String,
    pub kind: OperationalEventKind,
    pub message: &'static str,
    pub occurred_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationalEventKind {
    ServerStarted,
    DeviceCreated,
    DeviceRotated,
    DeviceEnabled,
    DeviceDisabled,
    DeviceRevoked,
    WechatLoginStarted,
    WechatLoginCancelled,
    WechatVerifySubmitted,
    WechatDisconnected,
    WechatTestEnqueued,
    RetentionCompleted,
}
