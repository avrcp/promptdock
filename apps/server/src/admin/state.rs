use std::{
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    config::{AdminMode, Config, ConfigError, ServerExposure},
    retention::RetentionService,
    state::AppState,
};

use super::{
    cursor::AdminCursorCodec, event_log::OperationalEventLog, health::RuntimeHealthRegistry,
};

#[derive(Clone)]
pub struct AdminState {
    pub app: AppState,
    pub config: Arc<SafeAdminRuntimeConfig>,
    pub started_at: Instant,
    pub started_at_wall_ms: i64,
    pub retention: RetentionService,
    pub health: RuntimeHealthRegistry,
    pub events: OperationalEventLog,
    pub cursors: AdminCursorCodec,
}

impl AdminState {
    pub fn new(
        app: AppState,
        config: &Config,
        retention: RetentionService,
        health: RuntimeHealthRegistry,
    ) -> Result<Self, ConfigError> {
        Ok(Self {
            retention,
            app,
            config: Arc::new(SafeAdminRuntimeConfig::try_from(config)?),
            started_at: Instant::now(),
            started_at_wall_ms: unix_timestamp_ms(),
            health,
            events: OperationalEventLog::default(),
            cursors: AdminCursorCodec::new(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeAdminRuntimeConfig {
    pub allowed_origin: String,
    pub mode: AdminMode,
    pub route_timeout_seconds: u64,
    pub max_page_size: usize,
    pub wechat_enabled: bool,
    pub public_bind_class: SafePublicBindClass,
    pub retention: SafeRetentionConfig,
}

impl TryFrom<&Config> for SafeAdminRuntimeConfig {
    type Error = ConfigError;

    fn try_from(config: &Config) -> Result<Self, Self::Error> {
        Ok(Self {
            allowed_origin: config.admin.normalized_allowed_origin()?,
            mode: config.admin.mode,
            route_timeout_seconds: config.admin.route_timeout_seconds,
            max_page_size: config.admin.max_page_size,
            wechat_enabled: config.wechat.enabled,
            public_bind_class: match config.server.exposure {
                ServerExposure::Loopback => SafePublicBindClass::Loopback,
                ServerExposure::Container => SafePublicBindClass::Container,
            },
            retention: SafeRetentionConfig {
                accepted_days: config.retention.accepted_days,
                dead_letter_days: config.retention.dead_letter_days,
                inbound_terminal_days: config.retention.inbound_terminal_days,
                inbound_expired_days: config.retention.inbound_expired_days,
            },
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafePublicBindClass {
    Loopback,
    Container,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SafeRetentionConfig {
    pub accepted_days: u32,
    pub dead_letter_days: u32,
    pub inbound_terminal_days: u32,
    pub inbound_expired_days: u32,
}

fn unix_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
