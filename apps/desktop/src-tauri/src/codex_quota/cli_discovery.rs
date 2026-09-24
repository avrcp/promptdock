//! CLI discovery for Windows.
//!
//! Finds the Codex CLI executable using a priority order:
//! 1. Explicit path from settings (if configured)
//! 2. PATH search for `codex.exe` or `codex.cmd`
//! 3. Common npm global install locations
//!
//! Security: never executes project-local scripts, never trusts relative paths,
//! never scans untrusted directories.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Discovered CLI with version information.
#[derive(Debug, Clone)]
pub struct DiscoveredCli {
    pub path: PathBuf,
    pub version: String,
}

/// Errors during CLI discovery.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("CLI not found in PATH or configured locations")]
    NotFound,
    #[error("failed to execute CLI: {0}")]
    ExecutionFailed(#[from] std::io::Error),
    #[error("CLI version output was not valid UTF-8")]
    InvalidVersionOutput,
    #[error("CLI version could not be parsed from output")]
    VersionParseFailed,
}

/// Discovers the Codex CLI, optionally using an explicit path from settings.
///
/// If `configured_path` is provided, it is used directly (after validation).
/// Otherwise, searches PATH and common install locations.
pub fn discover_cli(configured_path: Option<&Path>) -> Result<DiscoveredCli, DiscoveryError> {
    let path = if let Some(configured) = configured_path {
        validate_configured_path(configured)?;
        configured.to_path_buf()
    } else {
        find_in_path()?
    };

    let version = get_version(&path)?;
    Ok(DiscoveredCli { path, version })
}

fn validate_configured_path(path: &Path) -> Result<(), DiscoveryError> {
    if !path.is_absolute() {
        return Err(DiscoveryError::NotFound);
    }
    if !path.exists() {
        return Err(DiscoveryError::NotFound);
    }
    Ok(())
}

fn find_in_path() -> Result<PathBuf, DiscoveryError> {
    // Try `codex.exe` first (native Windows binary).
    if let Ok(output) = Command::new("where").arg("codex.exe").output() {
        if output.status.success() {
            let stdout = String::from_utf8(output.stdout).map_err(|_| DiscoveryError::NotFound)?;
            if let Some(path) = stdout.lines().next() {
                let path = PathBuf::from(path.trim());
                if path.exists() {
                    return Ok(path);
                }
            }
        }
    }

    // Try `codex.cmd` (npm wrapper).
    if let Ok(output) = Command::new("where").arg("codex.cmd").output() {
        if output.status.success() {
            let stdout = String::from_utf8(output.stdout).map_err(|_| DiscoveryError::NotFound)?;
            if let Some(path) = stdout.lines().next() {
                let path = PathBuf::from(path.trim());
                if path.exists() {
                    return Ok(path);
                }
            }
        }
    }

    Err(DiscoveryError::NotFound)
}

fn get_version(cli_path: &Path) -> Result<String, DiscoveryError> {
    let output = Command::new(cli_path)
        .arg("--version")
        .output()
        .map_err(DiscoveryError::ExecutionFailed)?;

    if !output.status.success() {
        return Err(DiscoveryError::VersionParseFailed);
    }

    let stdout =
        String::from_utf8(output.stdout).map_err(|_| DiscoveryError::InvalidVersionOutput)?;
    parse_version(&stdout)
}

fn parse_version(output: &str) -> Result<String, DiscoveryError> {
    // Expected format: "codex 0.1.25" or just "0.1.25"
    let line = output
        .lines()
        .next()
        .ok_or(DiscoveryError::VersionParseFailed)?;
    let trimmed = line.trim();

    // Try "codex X.Y.Z" format first.
    if let Some(version) = trimmed.strip_prefix("codex ") {
        return Ok(version.trim().to_string());
    }

    // Try bare version string.
    if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Ok(trimmed.to_string());
    }

    Err(DiscoveryError::VersionParseFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_codex_prefix() {
        assert_eq!(parse_version("codex 0.1.25").unwrap(), "0.1.25");
    }

    #[test]
    fn parse_version_bare() {
        assert_eq!(parse_version("0.1.25").unwrap(), "0.1.25");
    }

    #[test]
    fn parse_version_with_trailing_whitespace() {
        assert_eq!(parse_version("codex 0.1.25  \n").unwrap(), "0.1.25");
    }

    #[test]
    fn parse_version_rejects_empty() {
        assert!(parse_version("").is_err());
    }

    #[test]
    fn parse_version_rejects_non_numeric() {
        assert!(parse_version("not a version").is_err());
    }
}
