use std::net::IpAddr;

use crate::error::AppError;

use super::{config::validate_proxy_config, model::LauncherProxyConfig};

const PROXY_VARIABLES: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
    "ALL_PROXY",
    "all_proxy",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyEnvironment {
    pub proxy_url: String,
    pub no_proxy: String,
}

impl LauncherProxyConfig {
    /// `Ok(None)` means explicitly disabled. Invalid enabled configurations are
    /// errors, so callers cannot silently turn them into a direct connection.
    pub fn environment(&self) -> Result<Option<ProxyEnvironment>, AppError> {
        if !self.enabled {
            return Ok(None);
        }
        validate_proxy_config(self)?;
        let host = match self.host.parse::<IpAddr>() {
            Ok(IpAddr::V6(address)) => format!("[{address}]"),
            _ => self.host.clone(),
        };
        Ok(Some(ProxyEnvironment {
            proxy_url: format!("http://{host}:{}", self.port),
            no_proxy: self.no_proxy.join(","),
        }))
    }
}

pub fn apply_config_to_std_command(
    command: &mut std::process::Command,
    config: &LauncherProxyConfig,
) -> Result<Option<String>, AppError> {
    let environment = config.environment()?;
    apply_to_std_command(command, environment.as_ref());
    Ok(environment.map(|value| value.proxy_url))
}

pub fn apply_to_std_command(
    command: &mut std::process::Command,
    environment: Option<&ProxyEnvironment>,
) {
    for variable in PROXY_VARIABLES {
        command.env_remove(variable);
    }
    if let Some(environment) = environment {
        command
            .env("HTTP_PROXY", &environment.proxy_url)
            .env("HTTPS_PROXY", &environment.proxy_url)
            .env("http_proxy", &environment.proxy_url)
            .env("https_proxy", &environment.proxy_url)
            .env("NO_PROXY", &environment.no_proxy)
            .env("no_proxy", &environment.no_proxy);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        ffi::{OsStr, OsString},
    };

    use super::*;

    fn configured_environment(
        command: &std::process::Command,
    ) -> HashMap<OsString, Option<OsString>> {
        command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned)))
            .collect()
    }

    fn configured_value<'a>(
        values: &'a HashMap<OsString, Option<OsString>>,
        name: &str,
    ) -> Option<&'a Option<OsString>> {
        #[cfg(windows)]
        {
            values.iter().find_map(|(candidate, value)| {
                candidate
                    .to_string_lossy()
                    .eq_ignore_ascii_case(name)
                    .then_some(value)
            })
        }
        #[cfg(not(windows))]
        {
            values.get(OsStr::new(name))
        }
    }

    #[test]
    fn invalid_enabled_proxy_is_an_error_instead_of_direct_mode() {
        let invalid = LauncherProxyConfig {
            host: "proxy.example.com".to_owned(),
            ..LauncherProxyConfig::default()
        };
        let mut command = std::process::Command::new("fixture");
        command.env("HTTP_PROXY", "http://parent:1");
        let before = configured_environment(&command);

        assert!(apply_config_to_std_command(&mut command, &invalid).is_err());
        assert_eq!(configured_environment(&command), before);
    }

    #[test]
    fn ipv6_proxy_urls_are_bracketed() {
        let config = LauncherProxyConfig {
            host: "::1".to_owned(),
            ..LauncherProxyConfig::default()
        };
        assert_eq!(
            config.environment().unwrap().unwrap().proxy_url,
            "http://[::1]:10808"
        );
    }

    #[test]
    fn enabled_policy_replaces_inherited_values_and_clears_all_proxy() {
        let config = LauncherProxyConfig::default();
        let mut command = std::process::Command::new("fixture");
        for name in PROXY_VARIABLES {
            command.env(name, "parent-value");
        }
        apply_config_to_std_command(&mut command, &config).unwrap();
        let values = configured_environment(&command);

        assert_eq!(
            configured_value(&values, "HTTP_PROXY").and_then(Option::as_deref),
            Some(OsStr::new("http://127.0.0.1:10808"))
        );
        assert_eq!(configured_value(&values, "ALL_PROXY"), Some(&None));
    }

    #[test]
    fn disabled_policy_clears_every_inherited_proxy_variable() {
        let config = LauncherProxyConfig {
            enabled: false,
            host: "ignored.invalid".to_owned(),
            port: 0,
            no_proxy: Vec::new(),
        };
        let mut command = std::process::Command::new("fixture");
        for name in PROXY_VARIABLES {
            command.env(name, "parent-value");
        }
        assert_eq!(
            apply_config_to_std_command(&mut command, &config).unwrap(),
            None
        );
        let values = configured_environment(&command);
        for name in PROXY_VARIABLES {
            assert_eq!(configured_value(&values, name), Some(&None), "{name}");
        }
    }
}
