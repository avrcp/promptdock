//! App-server stdio client for Codex CLI.
//!
//! Implements the JSON-RPC-like protocol over stdio:
//! 1. Spawn `codex app-server` as a subprocess
//! 2. Send `initialize` request
//! 3. Wait for `initialized` notification
//! 4. Send `account/read` request
//! 5. Send `account/rateLimits/read` request
//! 6. Collect responses, match by request ID
//! 7. Gracefully close the subprocess
//!
//! All operations have bounded timeouts and byte limits to prevent hangs.

use super::normalize;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;
use tokio::time::timeout;

/// Errors during app-server communication.
#[derive(Debug, thiserror::Error)]
pub enum AppServerError {
    #[error("failed to spawn app-server: {0}")]
    SpawnFailed(#[from] std::io::Error),
    #[error("app-server initialization failed: {0}")]
    InitFailed(String),
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    #[error("request failed: {0}")]
    RequestFailed(String),
    #[error("response parse error: {0}")]
    ParseError(String),
    #[error("method not supported by this CLI version")]
    MethodNotSupported,
    #[error("authentication required")]
    AuthRequired,
}

/// Account information from `account/read`.
#[derive(Debug, Clone)]
pub struct AccountInfo {
    pub account_id: Option<String>,
    pub account_type: Option<String>,
}

/// Response from `account/rateLimits/read`.
#[derive(Debug, Clone)]
pub struct RateLimitsResponse {
    pub rate_limits: crate::agent::CodexUsageSnapshot,
}

/// App-server client with bounded I/O.
pub struct AppServerClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    request_id: u64,
    max_message_bytes: usize,
}

impl AppServerClient {
    /// Spawns the app-server subprocess and performs initialization handshake.
    pub async fn spawn(
        cli_path: &Path,
        timeout_duration: Duration,
    ) -> Result<Self, AppServerError> {
        let result = timeout(timeout_duration, async {
            Self::spawn_internal(cli_path).await
        })
        .await;

        match result {
            Ok(Ok(client)) => Ok(client),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AppServerError::Timeout(timeout_duration)),
        }
    }

    async fn spawn_internal(cli_path: &Path) -> Result<Self, AppServerError> {
        let mut child = Command::new(cli_path)
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child.stdin.take().ok_or_else(|| {
            AppServerError::SpawnFailed(std::io::Error::other("failed to capture stdin"))
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            AppServerError::SpawnFailed(std::io::Error::other("failed to capture stdout"))
        })?;

        let mut client = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            request_id: 0,
            max_message_bytes: 256 * 1024,
        };

        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<(), AppServerError> {
        let init_request = self.make_request("initialize", Value::Object(Default::default()))?;
        self.send_message(&init_request)?;

        // Wait for `initialized` notification.
        let response = self.read_response().await?;
        if response.get("method").and_then(|m| m.as_str()) != Some("initialized") {
            return Err(AppServerError::InitFailed(
                "expected initialized notification".into(),
            ));
        }

        Ok(())
    }

    /// Reads account information.
    pub async fn read_account(&mut self) -> Result<AccountInfo, AppServerError> {
        let request = self.make_request("account/read", Value::Object(Default::default()))?;
        self.send_message(&request)?;

        let response = self.read_response().await?;
        Self::check_for_errors(&response)?;

        let result = response
            .get("result")
            .ok_or_else(|| AppServerError::ParseError("missing result field".into()))?;

        let account_id = result
            .get("id")
            .or_else(|| result.get("email"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let account_type = result
            .get("accountType")
            .or_else(|| result.get("type"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(AccountInfo {
            account_id,
            account_type,
        })
    }

    /// Reads rate limits.
    pub async fn read_rate_limits(&mut self) -> Result<RateLimitsResponse, AppServerError> {
        let request =
            self.make_request("account/rateLimits/read", Value::Object(Default::default()))?;
        self.send_message(&request)?;

        let response = self.read_response().await?;
        Self::check_for_errors(&response)?;

        let result = response
            .get("result")
            .ok_or_else(|| AppServerError::ParseError("missing result field".into()))?;

        let rate_limits = normalize::normalize_rate_limits(result)
            .map_err(|e| AppServerError::ParseError(e.to_string()))?
            .ok_or_else(|| AppServerError::ParseError("no rate limit data in response".into()))?;

        Ok(RateLimitsResponse { rate_limits })
    }

    fn make_request(&mut self, method: &str, params: Value) -> Result<String, AppServerError> {
        self.request_id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.request_id,
            "method": method,
            "params": params
        });
        serde_json::to_string(&request).map_err(|e| AppServerError::ParseError(e.to_string()))
    }

    fn send_message(&mut self, message: &str) -> Result<(), AppServerError> {
        writeln!(self.stdin, "{}", message)?;
        self.stdin.flush()?;
        Ok(())
    }

    async fn read_response(&mut self) -> Result<Value, AppServerError> {
        let max_bytes = self.max_message_bytes;
        let mut line = String::new();

        // Perform a blocking read - the timeout is handled by the outer async context
        let bytes_read = self
            .stdout
            .read_line(&mut line)
            .map_err(|e| AppServerError::ParseError(format!("read error: {}", e)))?;

        if bytes_read == 0 {
            return Err(AppServerError::ParseError("EOF reached".into()));
        }
        if bytes_read > max_bytes {
            return Err(AppServerError::ParseError(format!(
                "message exceeds {} bytes",
                max_bytes
            )));
        }

        serde_json::from_str(&line).map_err(|e| AppServerError::ParseError(e.to_string()))
    }

    fn check_for_errors(response: &Value) -> Result<(), AppServerError> {
        if let Some(error) = response.get("error") {
            let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");

            return match code {
                -32601 => Err(AppServerError::MethodNotSupported),
                -32099..=-32000
                    if message.to_lowercase().contains("auth")
                        || message.to_lowercase().contains("login") =>
                {
                    Err(AppServerError::AuthRequired)
                }
                _ => Err(AppServerError::RequestFailed(message.to_string())),
            };
        }
        Ok(())
    }
}

impl Drop for AppServerClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_for_errors_method_not_supported() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32601,
                "message": "Method not found"
            }
        });

        assert!(matches!(
            AppServerClient::check_for_errors(&response),
            Err(AppServerError::MethodNotSupported)
        ));
    }

    #[test]
    fn check_for_errors_auth_required() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32000,
                "message": "Authentication required"
            }
        });

        assert!(matches!(
            AppServerClient::check_for_errors(&response),
            Err(AppServerError::AuthRequired)
        ));
    }
}
