use std::{fs, path::Path, process::Command};

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

fn write_config(path: &Path, database: &Path, connection: &Path, wechat_enabled: bool) {
    fs::write(
        path,
        format!(
            "[database]\npath = '{}'\n[wechat]\nenabled = {wechat_enabled}\nconnection_file = '{}'\n",
            database.display(),
            connection.display()
        ),
    )
    .expect("write configuration");
}

#[test]
fn init_status_and_doctor_emit_safe_json_and_correct_exit_status() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = directory.path().join("relay.db");
    let connection = directory.path().join("wechat-connection.enc");
    let config = directory.path().join("relay.toml");
    write_config(&config, &database, &connection, false);
    let config_text = config.to_str().expect("config path");

    let init = relay(&["init", "--config", config_text]);
    assert!(init.status.success(), "{}", text(&init.stderr));
    let initialized: Value = serde_json::from_slice(&init.stdout).expect("init JSON");
    assert_eq!(initialized["status"], "initialized");
    assert_eq!(initialized["schemaIdentity"], "promptdock-relay-v4");
    assert_eq!(initialized["schemaRevision"], 3);

    let created = relay(&[
        "device",
        "create",
        "--name",
        "OPERATIONS-PC",
        "--scope",
        "notify:write",
        "--config",
        config_text,
    ]);
    assert!(created.status.success(), "{}", text(&created.stderr));
    let token = text(&created.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("device_token="))
        .expect("device token")
        .to_owned();

    let status = relay(&["status", "--config", config_text]);
    assert!(status.status.success(), "{}", text(&status.stderr));
    let status_text = text(&status.stdout);
    let status_json: Value = serde_json::from_str(&status_text).expect("status JSON");
    assert_eq!(status_json["devices"]["total"], 1);
    assert_eq!(status_json["devices"]["enabled"], 1);
    assert_eq!(status_json["wechat"]["enabled"], false);
    assert_eq!(status_json["wechat"]["encryptedConnectionPresent"], false);
    assert!(!status_text.contains(&token));
    assert!(!status_text.contains(&directory.path().display().to_string()));

    let doctor = relay(&["doctor", "--config", config_text]);
    assert!(doctor.status.success(), "{}", text(&doctor.stderr));
    let doctor_json: Value = serde_json::from_slice(&doctor.stdout).expect("doctor JSON");
    assert_eq!(doctor_json["status"], "ok");
    assert!(!text(&doctor.stdout).contains(&directory.path().display().to_string()));
}

#[test]
fn doctor_failure_is_json_and_exits_nonzero_without_internal_details() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("invalid.toml");
    fs::write(&config, "[database]\nmax_connections = 0\n").expect("invalid config");

    let output = relay(&["doctor", "--config", config.to_str().expect("config path")]);
    assert!(!output.status.success());
    let json: Value = serde_json::from_slice(&output.stdout).expect("doctor failure JSON");
    assert_eq!(json["status"], "failed");
    let combined = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(!combined.contains(&directory.path().display().to_string()));
    assert!(!combined.to_ascii_lowercase().contains("sqlite"));
}

#[test]
fn backup_cli_creates_consistent_safe_artifacts_and_refuses_overwrite() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = directory.path().join("relay.db");
    let connection = directory.path().join("wechat-connection.enc");
    let config = directory.path().join("relay.toml");
    write_config(&config, &database, &connection, false);
    let config_text = config.to_str().expect("config path");
    let init = relay(&["init", "--config", config_text]);
    assert!(init.status.success(), "{}", text(&init.stderr));
    let created = relay(&[
        "device",
        "create",
        "--name",
        "BACKUP-PC",
        "--scope",
        "notify:write",
        "--config",
        config_text,
    ]);
    assert!(created.status.success(), "{}", text(&created.stderr));
    fs::write(&connection, b"encrypted-connection-only").expect("encrypted connection");
    let master_key_sentinel = b"MASTER-KEY-MUST-NOT-BE-BACKED-UP";
    fs::write(directory.path().join("master.key"), master_key_sentinel)
        .expect("master key sentinel");

    let output_directory = directory.path().join("backup");
    let backup = relay(&[
        "backup",
        "--output",
        output_directory.to_str().expect("output path"),
        "--config",
        config_text,
    ]);
    assert!(backup.status.success(), "{}", text(&backup.stderr));
    let report: Value = serde_json::from_slice(&backup.stdout).expect("backup JSON");
    assert_eq!(report["status"], "created");
    assert_eq!(report["encryptedConnectionIncluded"], true);

    let manifest = fs::read(output_directory.join("manifest.json")).expect("manifest");
    let manifest_json: Value = serde_json::from_slice(&manifest).expect("manifest JSON");
    assert_eq!(manifest_json["masterCredentialIncluded"], false);
    let database_bytes = fs::read(output_directory.join("relay.db")).expect("database backup");
    let connection_bytes =
        fs::read(output_directory.join("wechat-connection.enc")).expect("connection backup");
    assert_eq!(
        manifest_json["database"]["blake3"],
        blake3::hash(&database_bytes).to_hex().to_string()
    );
    assert_eq!(
        manifest_json["encryptedConnection"]["blake3"],
        blake3::hash(&connection_bytes).to_hex().to_string()
    );
    assert_eq!(manifest_json["database"]["file"], "relay.db");
    assert!(!text(&manifest).contains(&directory.path().display().to_string()));
    for entry in fs::read_dir(&output_directory).expect("backup entries") {
        let bytes = fs::read(entry.expect("entry").path()).expect("backup file");
        assert!(
            !bytes
                .windows(master_key_sentinel.len())
                .any(|window| window == master_key_sentinel)
        );
    }

    let backup_config = directory.path().join("backup.toml");
    write_config(
        &backup_config,
        &output_directory.join("relay.db"),
        &output_directory.join("wechat-connection.enc"),
        false,
    );
    let backed_status = relay(&[
        "status",
        "--config",
        backup_config.to_str().expect("backup config path"),
    ]);
    assert!(
        backed_status.status.success(),
        "{}",
        text(&backed_status.stderr)
    );
    let backed_json: Value = serde_json::from_slice(&backed_status.stdout).expect("status JSON");
    assert_eq!(backed_json["devices"]["total"], 1);

    let repeated = relay(&[
        "backup",
        "--output",
        output_directory.to_str().expect("output path"),
        "--config",
        config_text,
    ]);
    assert!(!repeated.status.success());
    assert!(text(&repeated.stderr).contains("new or empty directory"));
}
