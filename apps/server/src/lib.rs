#![forbid(unsafe_code)]

pub mod admin;
pub mod api;
pub mod auth;
pub mod cli;
pub mod config;
pub mod db;
pub mod error;
pub mod gateway;
pub mod inbound;
pub mod operations;
pub mod outbox;
pub mod qr_login;
pub mod rate_limit;
pub mod results;
mod results_viewer;
pub mod retention;
pub mod secret_store;
pub mod server;
pub mod shutdown;
pub mod state;
pub mod telemetry;
pub mod wechat;
pub mod wechat_admin_login;
pub mod wechat_channel;
pub mod wechat_login;
pub mod wechat_monitor;

use auth::DeviceAuthService;
use cli::{Cli, Command, DeviceCommand};
use error::AppError;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildInfo {
    release_version: &'static str,
    source_commit: &'static str,
    admin_api_major: u32,
}

pub async fn run(cli: Cli) -> Result<(), AppError> {
    if matches!(&cli.command, Command::BuildInfo) {
        return print_json(&BuildInfo {
            release_version: env!("CARGO_PKG_VERSION"),
            source_commit: option_env!("PROMPTDOCK_GIT_COMMIT").unwrap_or("unknown"),
            admin_api_major: relay_admin_api::ADMIN_API_MAJOR,
        });
    }
    let doctor_command = matches!(&cli.command, Command::Doctor);
    let config = match config::Config::load(cli.config.as_deref()).await {
        Ok(config) => config,
        Err(_) if doctor_command => {
            print_json(&operations::DoctorReport::config_failure())?;
            return Err(AppError::DoctorFailed);
        }
        Err(error) => return Err(error.into()),
    };
    match cli.command {
        Command::Serve => {
            telemetry::init(&config.logging)?;
            server::serve(config).await.map_err(Into::into)
        }
        Command::Init => print_json(&operations::initialize(&config).await?),
        Command::Status => print_json(&operations::status(&config).await?),
        Command::Doctor => {
            let report = operations::doctor(&config).await;
            let healthy = report.is_healthy();
            print_json(&report)?;
            if healthy {
                Ok(())
            } else {
                Err(AppError::DoctorFailed)
            }
        }
        Command::BuildInfo => unreachable!("build-info returns before configuration loading"),
        Command::Backup { output } => print_json(&operations::backup(&config, &output).await?),
        Command::Device { command } => run_device_command(&config.database, command).await,
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<(), AppError> {
    let output = serde_json::to_string_pretty(value).map_err(|_| AppError::Output)?;
    println!("{output}");
    Ok(())
}

async fn run_device_command(
    config: &config::DatabaseConfig,
    command: DeviceCommand,
) -> Result<(), AppError> {
    let pool = db::open(config).await?;
    let service = DeviceAuthService::new(pool.clone());
    let result: Result<(), AppError> = async {
        match command {
            DeviceCommand::Create { name, scopes } => {
                let created = service.create_device(&name, &scopes).await?;
                println!("device_id={}", created.id);
                println!("device_name={}", created.name);
                println!(
                    "device_scopes={}",
                    created
                        .scopes
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                );
                println!("device_token={}", created.token.expose());
                Ok(())
            }
            DeviceCommand::Rotate { id, scopes } => {
                let rotated = service.rotate_device(&id, &scopes).await?;
                println!("device_id={}", rotated.id);
                println!(
                    "device_scopes={}",
                    rotated
                        .scopes
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                );
                println!("device_token={}", rotated.token.expose());
                Ok(())
            }
            DeviceCommand::List => {
                let devices = service.list_devices().await?;
                let output =
                    serde_json::to_string_pretty(&devices).map_err(|_| AppError::Output)?;
                println!("{output}");
                Ok(())
            }
            DeviceCommand::Revoke { id } => {
                service.revoke_device(&id).await?;
                println!("revoked_device_id={id}");
                Ok(())
            }
        }
    }
    .await;
    drop(service);
    pool.close().await;
    result
}
