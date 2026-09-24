use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use thiserror::Error;
use url::{Host, Url};

pub const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
pub const MAX_RESULT_BODY_BYTES: usize = 262_144;
pub const ADMIN_REQUEST_BODY_BYTES: usize = 16 * 1024;
pub const ADMIN_RESPONSE_BODY_BYTES: usize = 256 * 1024;
pub const ADMIN_QR_RESPONSE_BODY_BYTES: usize = 512 * 1024;
pub const MAX_ADMIN_PAGE_SIZE: usize = 100;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub admin: AdminConfig,
    pub database: DatabaseConfig,
    pub wechat: WechatConfig,
    pub retention: RetentionConfig,
    pub results: ResultsConfig,
    pub logging: LoggingConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ResultsConfig {
    pub enabled: bool,
    pub public_origin: Option<String>,
    pub share_ttl_days: u32,
    pub max_body_bytes: usize,
    pub max_retained_body_bytes: usize,
}

impl Default for ResultsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            public_origin: None,
            share_ttl_days: 7,
            max_body_bytes: MAX_RESULT_BODY_BYTES,
            max_retained_body_bytes: 268_435_456,
        }
    }
}

impl ResultsConfig {
    pub fn normalized_public_origin(&self) -> Result<String, ConfigError> {
        let value = self
            .public_origin
            .as_deref()
            .ok_or(ConfigError::ResultsOrigin)?;
        let parsed = Url::parse(value).map_err(|_| ConfigError::ResultsOrigin)?;
        if parsed.scheme() != "https"
            || parsed.host().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != "/"
        {
            return Err(ConfigError::ResultsOrigin);
        }
        Ok(parsed.origin().ascii_serialization())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AdminConfig {
    pub enabled: bool,
    pub bind: SocketAddr,
    pub allowed_origin: String,
    pub mode: AdminMode,
    pub route_timeout_seconds: u64,
    pub max_page_size: usize,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: SocketAddr::from(([127, 0, 0, 1], 8081)),
            allowed_origin: "http://127.0.0.1:5173".to_owned(),
            mode: AdminMode::ReadOnly,
            route_timeout_seconds: 5,
            max_page_size: MAX_ADMIN_PAGE_SIZE,
        }
    }
}

impl AdminConfig {
    pub fn route_timeout(&self) -> Duration {
        Duration::from_secs(self.route_timeout_seconds)
    }

    pub fn normalized_allowed_origin(&self) -> Result<String, ConfigError> {
        let parsed = Url::parse(&self.allowed_origin).map_err(|_| ConfigError::AdminOrigin)?;
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != "/"
        {
            return Err(ConfigError::AdminOrigin);
        }
        let secure = parsed.scheme() == "https";
        let loopback_http = parsed.scheme() == "http"
            && match parsed.host() {
                Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
                Some(Host::Ipv4(address)) => address.is_loopback(),
                Some(Host::Ipv6(address)) => address.is_loopback(),
                None => false,
            };
        if !secure && !loopback_http {
            return Err(ConfigError::AdminOrigin);
        }
        Ok(parsed.origin().ascii_serialization())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdminMode {
    #[default]
    ReadOnly,
    Operator,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub exposure: ServerExposure,
    pub notification_only: bool,
    pub request_body_limit_bytes: usize,
    pub route_timeout_seconds: u64,
    pub shutdown_timeout_seconds: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 8080)),
            exposure: ServerExposure::Loopback,
            notification_only: false,
            request_body_limit_bytes: MAX_REQUEST_BODY_BYTES,
            route_timeout_seconds: 10,
            shutdown_timeout_seconds: 15,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServerExposure {
    #[default]
    Loopback,
    Container,
}

impl ServerConfig {
    pub fn route_timeout(&self) -> Duration {
        Duration::from_secs(self.route_timeout_seconds)
    }

    pub fn shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.shutdown_timeout_seconds)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    pub path: PathBuf,
    pub max_connections: u32,
    pub busy_timeout_ms: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("promptdock-relay.db"),
            max_connections: 4,
            busy_timeout_ms: 5_000,
        }
    }
}

impl DatabaseConfig {
    pub fn busy_timeout(&self) -> Duration {
        Duration::from_millis(self.busy_timeout_ms)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct WechatConfig {
    pub enabled: bool,
    pub connection_file: PathBuf,
}

impl Default for WechatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            connection_file: PathBuf::from(crate::secret_store::DEFAULT_CONNECTION_FILE),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionConfig {
    pub accepted_days: u32,
    pub dead_letter_days: u32,
    pub inbound_terminal_days: u32,
    pub inbound_expired_days: u32,
    pub bundle_content_days: u32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            accepted_days: 30,
            dead_letter_days: 90,
            inbound_terminal_days: 7,
            inbound_expired_days: 2,
            bundle_content_days: 7,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub filter: String,
    pub format: LoggingFormat,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            filter: "info,promptdock_server=info".to_owned(),
            format: LoggingFormat::Json,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LoggingFormat {
    #[default]
    Json,
}

impl Config {
    pub async fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let config = match path {
            Some(path) => {
                let source = tokio::fs::read_to_string(path)
                    .await
                    .map_err(|_| ConfigError::Read)?;
                toml::from_str(&source).map_err(|_| ConfigError::Parse)?
            }
            None => Self::default(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        match self.server.exposure {
            ServerExposure::Loopback if !self.server.bind.ip().is_loopback() => {
                return Err(ConfigError::LoopbackExposure);
            }
            ServerExposure::Container if !self.server.bind.ip().is_unspecified() => {
                return Err(ConfigError::ContainerExposure);
            }
            ServerExposure::Loopback | ServerExposure::Container => {}
        }
        if !(1..=MAX_REQUEST_BODY_BYTES).contains(&self.server.request_body_limit_bytes) {
            return Err(ConfigError::RequestBodyLimit);
        }
        if !(1..=10).contains(&self.server.route_timeout_seconds) {
            return Err(ConfigError::RouteTimeout);
        }
        if !(1..=15).contains(&self.server.shutdown_timeout_seconds) {
            return Err(ConfigError::ShutdownTimeout);
        }
        if !self.admin.bind.ip().is_loopback() {
            return Err(ConfigError::AdminLoopback);
        }
        self.admin.normalized_allowed_origin()?;
        if !(1..=10).contains(&self.admin.route_timeout_seconds) {
            return Err(ConfigError::AdminRouteTimeout);
        }
        if !(1..=MAX_ADMIN_PAGE_SIZE).contains(&self.admin.max_page_size) {
            return Err(ConfigError::AdminPageSize);
        }
        if self.logging.filter.trim().is_empty() {
            return Err(ConfigError::LoggingFilter);
        }
        if self.database.path.as_os_str().is_empty() {
            return Err(ConfigError::DatabasePath);
        }
        if !(1..=4).contains(&self.database.max_connections) {
            return Err(ConfigError::DatabaseConnections);
        }
        if self.database.busy_timeout_ms != 5_000 {
            return Err(ConfigError::DatabaseBusyTimeout);
        }
        if self.wechat.connection_file.as_os_str().is_empty() {
            return Err(ConfigError::WechatConnectionFile);
        }
        if !(1..=3_650).contains(&self.retention.accepted_days) {
            return Err(ConfigError::AcceptedRetention);
        }
        if !(1..=3_650).contains(&self.retention.dead_letter_days) {
            return Err(ConfigError::DeadLetterRetention);
        }
        if !(1..=3_650).contains(&self.retention.inbound_terminal_days) {
            return Err(ConfigError::InboundTerminalRetention);
        }
        if !(1..=3_650).contains(&self.retention.inbound_expired_days) {
            return Err(ConfigError::InboundExpiredRetention);
        }
        if !(1..=30).contains(&self.retention.bundle_content_days) {
            return Err(ConfigError::BundleContentRetention);
        }
        if !(1..=30).contains(&self.results.share_ttl_days) {
            return Err(ConfigError::ResultsShareTtl);
        }
        if !(1..=MAX_RESULT_BODY_BYTES).contains(&self.results.max_body_bytes) {
            return Err(ConfigError::ResultsBodyLimit);
        }
        if self.results.max_retained_body_bytes < self.results.max_body_bytes
            || self.results.max_retained_body_bytes > 1_073_741_824
        {
            return Err(ConfigError::ResultsRetentionLimit);
        }
        if self.results.enabled {
            self.results.normalized_public_origin()?;
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("configuration file could not be read")]
    Read,
    #[error("configuration file is invalid")]
    Parse,
    #[error("loopback exposure requires a loopback bind address")]
    LoopbackExposure,
    #[error("container exposure requires an unspecified bind address")]
    ContainerExposure,
    #[error("request body limit must be between 1 and 65536 bytes")]
    RequestBodyLimit,
    #[error("route timeout must be between 1 and 10 seconds")]
    RouteTimeout,
    #[error("shutdown timeout must be between 1 and 15 seconds")]
    ShutdownTimeout,
    #[error("Admin listener requires a loopback bind address")]
    AdminLoopback,
    #[error("Admin allowed origin must be HTTPS, or loopback HTTP without extra URL components")]
    AdminOrigin,
    #[error("Admin route timeout must be between 1 and 10 seconds")]
    AdminRouteTimeout,
    #[error("Admin max page size must be between 1 and 100")]
    AdminPageSize,
    #[error("logging filter must not be empty")]
    LoggingFilter,
    #[error("database path must not be empty")]
    DatabasePath,
    #[error("database max connections must be between 1 and 4")]
    DatabaseConnections,
    #[error("database busy timeout must be 5000 milliseconds")]
    DatabaseBusyTimeout,
    #[error("WeChat connection file path must not be empty")]
    WechatConnectionFile,
    #[error("accepted notification retention must be between 1 and 3650 days")]
    AcceptedRetention,
    #[error("dead-letter notification retention must be between 1 and 3650 days")]
    DeadLetterRetention,
    #[error("terminal inbound retention must be between 1 and 3650 days")]
    InboundTerminalRetention,
    #[error("expired inbound retention must be between 1 and 3650 days")]
    InboundExpiredRetention,
    #[error("bundle content retention must be between 1 and 30 days")]
    BundleContentRetention,
    #[error("results public origin must be an HTTPS origin without URL components")]
    ResultsOrigin,
    #[error("results share TTL must be between 1 and 30 days")]
    ResultsShareTtl,
    #[error("results body limit is invalid")]
    ResultsBodyLimit,
    #[error("results retained body limit is invalid")]
    ResultsRetentionLimit,
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[tokio::test]
    async fn defaults_match_phase_one_contract() {
        let config = Config::load(None).await.expect("default config");
        assert_eq!(
            config.server.bind,
            "127.0.0.1:8080".parse().expect("address")
        );
        assert_eq!(config.server.exposure, ServerExposure::Loopback);
        assert!(!config.server.notification_only);
        assert_eq!(config.server.request_body_limit_bytes, 65_536);
        assert_eq!(config.server.route_timeout_seconds, 10);
        assert_eq!(config.server.shutdown_timeout_seconds, 15);
        assert!(!config.admin.enabled);
        assert_eq!(
            config.admin.bind,
            "127.0.0.1:8081".parse().expect("admin address")
        );
        assert_eq!(config.admin.mode, AdminMode::ReadOnly);
        assert_eq!(config.admin.route_timeout_seconds, 5);
        assert_eq!(config.admin.max_page_size, 100);
        assert_eq!(
            config
                .admin
                .normalized_allowed_origin()
                .expect("normalized origin"),
            "http://127.0.0.1:5173"
        );
        assert_eq!(config.database.path, PathBuf::from("promptdock-relay.db"));
        assert_eq!(config.database.max_connections, 4);
        assert_eq!(config.database.busy_timeout_ms, 5_000);
        assert!(!config.wechat.enabled);
        assert_eq!(
            config.wechat.connection_file,
            PathBuf::from(crate::secret_store::DEFAULT_CONNECTION_FILE)
        );
        assert_eq!(config.retention.accepted_days, 30);
        assert_eq!(config.retention.dead_letter_days, 90);
        assert_eq!(config.retention.inbound_terminal_days, 7);
        assert_eq!(config.retention.inbound_expired_days, 2);
        assert_eq!(config.retention.bundle_content_days, 7);
    }

    #[tokio::test]
    async fn partial_toml_uses_typed_defaults() {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        writeln!(file, "[server]\nbind = \"127.0.0.1:9090\"").expect("write config");
        let config = Config::load(Some(file.path())).await.expect("config");
        assert_eq!(config.server.bind.port(), 9090);
        assert_eq!(config.server.request_body_limit_bytes, 65_536);
    }

    #[tokio::test]
    async fn rejects_unknown_or_unsafe_values() {
        for source in [
            "unknown = true",
            "[server]\nrequest_body_limit_bytes = 65537",
            "[server]\nrequest_body_limit_bytes = 0",
            "[server]\nroute_timeout_seconds = 0",
            "[server]\nroute_timeout_seconds = 11",
            "[server]\nshutdown_timeout_seconds = 0",
            "[server]\nshutdown_timeout_seconds = 16",
            "[server]\nbind = \"0.0.0.0:8080\"",
            "[server]\nbind = \"127.0.0.1:8080\"\nexposure = \"container\"",
            "[server]\nexposure = \"public\"",
            "[admin]\nbind = \"0.0.0.0:8081\"",
            "[admin]\nbind = \"192.0.2.1:8081\"",
            "[admin]\nmode = \"read-write\"",
            "[admin]\nroute_timeout_seconds = 0",
            "[admin]\nroute_timeout_seconds = 11",
            "[admin]\nmax_page_size = 0",
            "[admin]\nmax_page_size = 101",
            "[admin]\nallowed_origin = \"http://admin.example.com\"",
            "[admin]\nallowed_origin = \"ftp://127.0.0.1\"",
            "[admin]\nallowed_origin = \"https://user@admin.example.com\"",
            "[admin]\nallowed_origin = \"https://admin.example.com/path\"",
            "[admin]\nallowed_origin = \"https://admin.example.com?query=1\"",
            "[admin]\nallowed_origin = \"https://admin.example.com#fragment\"",
            "[logging]\nfilter = \"  \"",
            "[logging]\nformat = \"text\"",
            "[database]\npath = \"\"",
            "[database]\nmax_connections = 0",
            "[database]\nmax_connections = 5",
            "[database]\nbusy_timeout_ms = 4999",
            "[database]\nbusy_timeout_ms = 5001",
            "[wechat]\nconnection_file = \"\"",
            "[retention]\naccepted_days = 0",
            "[retention]\naccepted_days = 3651",
            "[retention]\ndead_letter_days = 0",
            "[retention]\ndead_letter_days = 3651",
            "[retention]\ninbound_terminal_days = 0",
            "[retention]\ninbound_terminal_days = 3651",
            "[retention]\ninbound_expired_days = 0",
            "[retention]\ninbound_expired_days = 3651",
            "[retention]\nbundle_content_days = 0",
            "[retention]\nbundle_content_days = 31",
        ] {
            let mut file = tempfile::NamedTempFile::new().expect("temp file");
            write!(file, "{source}").expect("write config");
            assert!(
                Config::load(Some(file.path())).await.is_err(),
                "accepted {source}"
            );
        }
    }

    #[tokio::test]
    async fn container_exposure_is_an_explicit_unspecified_bind_exception() {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        writeln!(
            file,
            "[server]\nbind = \"0.0.0.0:8080\"\nexposure = \"container\""
        )
        .expect("write config");
        let config = Config::load(Some(file.path()))
            .await
            .expect("container config");
        assert_eq!(config.server.exposure, ServerExposure::Container);
    }

    #[tokio::test]
    async fn admin_origin_accepts_https_and_literal_loopback_http() {
        for source in [
            "[admin]\nenabled = true\nallowed_origin = \"https://admin.example.com\"",
            "[admin]\nenabled = true\nallowed_origin = \"http://localhost:5173\"",
            "[admin]\nenabled = true\nallowed_origin = \"http://[::1]:5173\"",
        ] {
            let mut file = tempfile::NamedTempFile::new().expect("temp file");
            write!(file, "{source}").expect("write config");
            Config::load(Some(file.path()))
                .await
                .unwrap_or_else(|error| panic!("rejected {source}: {error}"));
        }
    }
}
