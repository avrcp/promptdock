use std::{fs, process::Command};

use promptdock_server::{
    auth::{AuthenticationError, DeviceAuthService, DeviceScope},
    config::DatabaseConfig,
    db,
};
use serde_json::Value;

fn relay(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_promptdock-relay"))
        .args(arguments)
        .output()
        .expect("run relay CLI")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("UTF-8 output")
}

#[test]
fn device_cli_creates_lists_and_idempotently_revokes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = directory.path().join("relay.db");
    let config = directory.path().join("relay.toml");
    fs::write(
        &config,
        format!("[database]\npath = '{}'\n", database.display()),
    )
    .expect("write configuration");
    let config = config.to_str().expect("configuration path");

    let create = relay(&[
        "device",
        "create",
        "--name",
        "  OFFICE-PC  ",
        "--scope",
        "notify:write",
        "--scope",
        "channel:read",
        "--config",
        config,
    ]);
    assert!(create.status.success(), "{}", text(&create.stderr));
    let stdout = text(&create.stdout);
    let stderr = text(&create.stderr);
    let device_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("device_id="))
        .expect("device ID")
        .to_owned();
    let token = stdout
        .lines()
        .find_map(|line| line.strip_prefix("device_token="))
        .expect("device token")
        .to_owned();
    assert_eq!(stdout.matches(&token).count(), 1);
    assert!(!stderr.contains(&token));
    assert!(token.starts_with(&format!("pdv2.{device_id}.")));

    let list = relay(&["device", "list", "--config", config]);
    assert!(list.status.success(), "{}", text(&list.stderr));
    let list_stdout = text(&list.stdout);
    assert!(!list_stdout.contains(&token));
    assert!(!list_stdout.contains("token_hash"));
    let devices: Value = serde_json::from_str(&list_stdout).expect("device JSON");
    assert_eq!(devices[0]["id"], device_id);
    assert_eq!(devices[0]["name"], "OFFICE-PC");
    assert_eq!(devices[0]["enabled"], true);
    assert_eq!(devices[0]["credentialVersion"], 2);
    assert_eq!(
        devices[0]["scopes"],
        serde_json::json!(["notify:write", "channel:read"])
    );

    let revoke = relay(&["device", "revoke", &device_id, "--config", config]);
    assert!(revoke.status.success(), "{}", text(&revoke.stderr));
    let first_list = relay(&["device", "list", "--config", config]);
    let first: Value = serde_json::from_slice(&first_list.stdout).expect("first revoked JSON");
    let revoked_at = first[0]["revokedAt"].clone();
    assert!(!revoked_at.is_null());
    assert_eq!(first[0]["enabled"], false);

    let repeated = relay(&["device", "revoke", &device_id, "--config", config]);
    assert!(repeated.status.success(), "{}", text(&repeated.stderr));
    let second_list = relay(&["device", "list", "--config", config]);
    let second: Value = serde_json::from_slice(&second_list.stdout).expect("second revoked JSON");
    assert_eq!(second[0]["revokedAt"], revoked_at);
}

#[test]
fn invalid_device_input_is_safe_and_does_not_mutate_the_database() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = directory.path().join("relay.db");
    let config = directory.path().join("relay.toml");
    fs::write(
        &config,
        format!("[database]\npath = '{}'\n", database.display()),
    )
    .expect("write configuration");
    let config = config.to_str().expect("configuration path");

    let invalid = relay(&[
        "device",
        "create",
        "--name",
        "   ",
        "--scope",
        "notify:write",
        "--config",
        config,
    ]);
    assert!(!invalid.status.success());
    let error = text(&invalid.stderr);
    assert!(error.contains("device name must contain between 1 and 80 characters"));
    assert!(!error.contains(&database.display().to_string()));
    assert!(!error.to_ascii_lowercase().contains("insert into"));

    let list = relay(&["device", "list", "--config", config]);
    assert!(list.status.success(), "{}", text(&list.stderr));
    let devices: Value = serde_json::from_slice(&list.stdout).expect("device JSON");
    assert_eq!(devices, Value::Array(Vec::new()));
}

#[tokio::test]
async fn device_rotate_cli_has_a_secret_safe_atomic_credential_lifecycle() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = directory.path().join("relay.db");
    let config_path = directory.path().join("relay.toml");
    fs::write(
        &config_path,
        format!("[database]\npath = '{}'\n", database.display()),
    )
    .expect("write configuration");
    let config = config_path.to_str().expect("configuration path");

    let create = relay(&[
        "device",
        "create",
        "--name",
        "ROTATE-PC",
        "--scope",
        "notify:write",
        "--config",
        config,
    ]);
    assert!(create.status.success(), "{}", text(&create.stderr));
    let create_stdout = text(&create.stdout);
    let device_id = create_stdout
        .lines()
        .find_map(|line| line.strip_prefix("device_id="))
        .expect("device ID")
        .to_owned();
    let old_token = create_stdout
        .lines()
        .find_map(|line| line.strip_prefix("device_token="))
        .expect("old token")
        .to_owned();
    assert_eq!(create_stdout.matches(&old_token).count(), 1);

    let failed = relay(&[
        "device",
        "rotate",
        &device_id,
        "--scope",
        "admin:all",
        "--config",
        config,
    ]);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty(), "failed rotation emitted stdout");
    assert!(!text(&failed.stderr).contains(&old_token));

    let pool = db::open(&DatabaseConfig {
        path: database.clone(),
        ..DatabaseConfig::default()
    })
    .await
    .expect("database after failed rotation");
    let service = DeviceAuthService::new(pool.clone());
    assert_eq!(
        service
            .authenticate(&old_token)
            .await
            .expect("old credential survives failed rotation")
            .scopes,
        std::collections::BTreeSet::from([DeviceScope::NotifyWrite])
    );
    pool.close().await;

    let rotate = relay(&[
        "device",
        "rotate",
        &device_id,
        "--scope",
        "channel:read",
        "--scope",
        "notify:read_own",
        "--config",
        config,
    ]);
    assert!(rotate.status.success(), "{}", text(&rotate.stderr));
    let rotate_stdout = text(&rotate.stdout);
    let new_token = rotate_stdout
        .lines()
        .find_map(|line| line.strip_prefix("device_token="))
        .expect("new token")
        .to_owned();
    assert_ne!(new_token, old_token);
    assert_eq!(rotate_stdout.matches(&new_token).count(), 1);
    assert!(!text(&rotate.stderr).contains(&new_token));

    let list = relay(&["device", "list", "--config", config]);
    assert!(list.status.success(), "{}", text(&list.stderr));
    let devices: Value = serde_json::from_slice(&list.stdout).expect("device list JSON");
    assert_eq!(devices[0]["id"], device_id);
    assert_eq!(devices[0]["credentialVersion"], 2);
    assert_eq!(
        devices[0]["scopes"],
        serde_json::json!(["notify:read_own", "channel:read"])
    );
    let list_text = text(&list.stdout);
    assert!(!list_text.contains(&old_token));
    assert!(!list_text.contains(&new_token));

    let pool = db::open(&DatabaseConfig {
        path: database.clone(),
        ..DatabaseConfig::default()
    })
    .await
    .expect("database after rotation");
    let service = DeviceAuthService::new(pool.clone());
    assert_eq!(
        service.authenticate(&old_token).await,
        Err(AuthenticationError::Unauthorized)
    );
    assert_eq!(
        service
            .authenticate(&new_token)
            .await
            .expect("new credential")
            .scopes,
        std::collections::BTreeSet::from([DeviceScope::NotifyReadOwn, DeviceScope::ChannelRead,])
    );
    pool.close().await;

    let combined_stderr = [
        text(&create.stderr),
        text(&failed.stderr),
        text(&rotate.stderr),
        text(&list.stderr),
    ]
    .join("\n");
    for token in [&old_token, &new_token] {
        let secret = token.rsplit_once('.').expect("secret segment").1;
        assert!(!combined_stderr.contains(token));
        assert!(!combined_stderr.contains(secret));
        for suffix in ["relay.db", "relay.db-wal", "relay.db-shm"] {
            let path = directory.path().join(suffix);
            if path.exists() {
                let bytes = fs::read(&path).expect("database artifact");
                assert!(
                    !contains_bytes(&bytes, token.as_bytes()),
                    "token leaked to {suffix}"
                );
                assert!(
                    !contains_bytes(&bytes, secret.as_bytes()),
                    "secret segment leaked to {suffix}"
                );
            }
        }
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
