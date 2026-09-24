use std::path::PathBuf;

use crate::auth::DeviceScope;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "promptdock-relay",
    version,
    about = "PromptDock notification relay"
)]
pub struct Cli {
    /// Optional TOML configuration file.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the HTTP relay server.
    Serve,
    /// Initialize and migrate the configured SQLite database.
    Init,
    /// Print a secret-free operational status snapshot as JSON.
    Status,
    /// Run deployment and storage diagnostics as safe JSON.
    Doctor,
    /// Print immutable release build identity as JSON without loading runtime configuration.
    BuildInfo,
    /// Create a consistent database and encrypted-connection backup.
    Backup {
        /// Empty or not-yet-created destination directory.
        #[arg(long, value_name = "DIR")]
        output: PathBuf,
    },
    /// Manage durable device credentials.
    Device {
        #[command(subcommand)]
        command: DeviceCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    /// Create a device and print its bearer token exactly once.
    Create {
        #[arg(long, value_name = "NAME")]
        name: String,
        #[arg(long = "scope", value_name = "SCOPE", required = true, num_args = 1..)]
        scopes: Vec<DeviceScope>,
    },
    /// Rotate a live device credential and replace its scopes atomically.
    Rotate {
        #[arg(value_name = "DEVICE_ID")]
        id: String,
        #[arg(long = "scope", value_name = "SCOPE", required = true, num_args = 1..)]
        scopes: Vec<DeviceScope>,
    },
    /// List safe device metadata without credential hashes.
    List,
    /// Revoke a device credential. Repeating this command is safe.
    Revoke {
        #[arg(value_name = "DEVICE_ID")]
        id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_serve_with_config() {
        let cli = Cli::try_parse_from(["promptdock-relay", "serve", "--config", "relay.toml"])
            .expect("valid CLI");
        assert!(matches!(cli.command, Command::Serve));
        assert_eq!(cli.config, Some(PathBuf::from("relay.toml")));
    }

    #[test]
    fn parses_device_lifecycle_commands() {
        for (arguments, expected) in [
            (
                vec![
                    "promptdock-relay",
                    "device",
                    "create",
                    "--name",
                    "OFFICE-PC",
                    "--scope",
                    "notify:write",
                ],
                "create",
            ),
            (vec!["promptdock-relay", "device", "list"], "list"),
            (
                vec![
                    "promptdock-relay",
                    "device",
                    "revoke",
                    "00000000-0000-4000-8000-000000000001",
                ],
                "revoke",
            ),
        ] {
            let cli = Cli::try_parse_from(arguments).expect("valid device command");
            let Command::Device { command } = cli.command else {
                panic!("expected device command")
            };
            assert_eq!(
                match command {
                    DeviceCommand::Create { .. } => "create",
                    DeviceCommand::Rotate { .. } => "rotate",
                    DeviceCommand::List => "list",
                    DeviceCommand::Revoke { .. } => "revoke",
                },
                expected
            );
        }
    }

    #[test]
    fn parses_operations_commands() {
        for (arguments, expected) in [
            (vec!["promptdock-relay", "init"], "init"),
            (vec!["promptdock-relay", "status"], "status"),
            (vec!["promptdock-relay", "doctor"], "doctor"),
            (vec!["promptdock-relay", "build-info"], "build-info"),
            (
                vec!["promptdock-relay", "backup", "--output", "backup-dir"],
                "backup",
            ),
        ] {
            let cli = Cli::try_parse_from(arguments).expect("valid operations command");
            assert_eq!(
                match cli.command {
                    Command::Init => "init",
                    Command::Status => "status",
                    Command::Doctor => "doctor",
                    Command::BuildInfo => "build-info",
                    Command::Backup { output } => {
                        assert_eq!(output, PathBuf::from("backup-dir"));
                        "backup"
                    }
                    Command::Serve | Command::Device { .. } => "other",
                },
                expected
            );
        }
    }

    #[test]
    fn requires_a_subcommand() {
        assert!(Cli::try_parse_from(["promptdock-relay"]).is_err());
    }
}
