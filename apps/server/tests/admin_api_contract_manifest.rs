use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use relay_admin_api::{
    ADMIN_SCHEMA_VERSION as SCHEMA_VERSION, AdminCapability, CONTRACT_NAME, ContractManifest,
    FixtureEntry, InboundCommand, JSON_MEDIA_TYPE, parse_manifest, routes,
    validate_canonical_json_bytes, validate_fixture_manifest,
};

const EXPECTED_FIXTURES: [&str; 24] = [
    "deliveries-page-v2.json",
    "device-action-receipt-v2.json",
    "device-create-receipt-v2.json",
    "device-detail-v2.json",
    "device-rotate-receipt-v2.json",
    "devices-page-v2.json",
    "error-v2.json",
    "inbound-command-actions-v2.json",
    "inbound-commands-page-v2.json",
    "interactive-replies-page-v2.json",
    "meta-v2.json",
    "overview-v2.json",
    "result-detail-v2.json",
    "result-receipt-v2.json",
    "results-page-v2.json",
    "retention-action-receipt-v2.json",
    "system-v2.json",
    "wechat-action-receipt-v2.json",
    "wechat-events-page-v2.json",
    "wechat-login-cancelled-v2.json",
    "wechat-login-verify-required-v2.json",
    "wechat-login-waiting-scan-v2.json",
    "wechat-status-v2.json",
    "wechat-test-receipt-v2.json",
];

fn contract_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/admin-api/v2")
}

fn fixtures_directory() -> PathBuf {
    contract_directory().join("fixtures")
}

fn read_manifest() -> ContractManifest {
    let path = contract_directory().join("manifest.json");
    let bytes = fs::read(&path).expect("read Admin API contract manifest");
    parse_manifest(&bytes).expect("parse strict Admin API contract manifest")
}

fn fixture_bytes() -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(fixtures_directory())
        .expect("read Admin API fixture directory")
        .filter_map(|entry| {
            let entry = entry.expect("fixture directory entry");
            if !entry.file_type().expect("fixture entry type").is_file() {
                return None;
            }
            let name = entry.file_name().into_string().expect("UTF-8 fixture name");
            Some((
                name,
                fs::read(entry.path()).expect("read raw fixture bytes"),
            ))
        })
        .collect()
}

fn inventory(name: &str) -> serde_json::Value {
    let path = contract_directory().join("inventories").join(name);
    let bytes = fs::read(&path).expect("read Admin API inventory");
    validate_canonical_json_bytes(name, &bytes).expect("canonical inventory JSON");
    serde_json::from_slice(&bytes).expect("parse Admin API inventory")
}

fn validate_manifest(
    manifest: &ContractManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    validate_fixture_manifest(manifest, files, &EXPECTED_FIXTURES)
}

#[test]
fn checked_in_manifest_is_a_complete_raw_byte_inventory() {
    validate_manifest(&read_manifest(), &fixture_bytes())
        .expect("checked-in Admin API contract manifest");
}

#[test]
fn semantic_inventories_are_complete_and_match_shared_closed_values() {
    let names = fs::read_dir(contract_directory().join("inventories"))
        .expect("read Admin API inventory directory")
        .map(|entry| {
            entry
                .expect("inventory entry")
                .file_name()
                .into_string()
                .expect("UTF-8 inventory name")
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "actions-v2.json",
            "capabilities-v2.json",
            "routes-v2.json",
            "states-v2.json",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );

    let capabilities = inventory("capabilities-v2.json");
    assert_eq!(capabilities["schemaVersion"], SCHEMA_VERSION);
    assert_eq!(
        capabilities["capabilities"],
        serde_json::json!(
            AdminCapability::ALL
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
        )
    );

    let actions = inventory("actions-v2.json");
    assert_eq!(actions["schemaVersion"], SCHEMA_VERSION);
    assert_eq!(
        actions["inboundCommands"],
        serde_json::json!(
            InboundCommand::ALL
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
        )
    );
    assert!(!actions.to_string().contains("list_runs"));

    let route_inventory = inventory("routes-v2.json");
    assert_eq!(route_inventory["schemaVersion"], SCHEMA_VERSION);
    assert_eq!(route_inventory["basePath"], relay_admin_api::routes::BASE);
    assert_eq!(
        route_inventory["routes"],
        serde_json::json!([
            { "methods": ["GET", "HEAD"], "path": routes::META },
            { "methods": ["GET", "HEAD"], "path": routes::OVERVIEW },
            { "methods": ["GET", "HEAD"], "path": routes::DEVICES },
            { "methods": ["POST"], "path": routes::DEVICES },
            { "methods": ["GET", "HEAD"], "path": routes::DEVICE },
            { "methods": ["POST"], "path": routes::DEVICE_ROTATE },
            { "methods": ["POST"], "path": routes::DEVICE_ENABLE },
            { "methods": ["POST"], "path": routes::DEVICE_DISABLE },
            { "methods": ["POST"], "path": routes::DEVICE_REVOKE },
            { "methods": ["GET", "HEAD"], "path": routes::WECHAT_STATUS },
            { "methods": ["GET", "HEAD"], "path": routes::WECHAT_EVENTS },
            { "methods": ["POST"], "path": routes::WECHAT_TEST },
            { "methods": ["POST"], "path": routes::WECHAT_DISCONNECT },
            { "methods": ["POST"], "path": routes::WECHAT_LOGIN },
            { "methods": ["GET", "HEAD"], "path": routes::WECHAT_LOGIN_SESSION },
            { "methods": ["DELETE"], "path": routes::WECHAT_LOGIN_SESSION },
            { "methods": ["POST"], "path": routes::WECHAT_LOGIN_VERIFY },
            { "methods": ["GET", "HEAD"], "path": routes::DELIVERIES },
            { "methods": ["GET", "HEAD"], "path": routes::INTERACTIVE_REPLIES },
            { "methods": ["GET", "HEAD"], "path": routes::INBOUND_COMMANDS },
            { "methods": ["GET", "HEAD"], "path": routes::RESULTS },
            { "methods": ["GET", "HEAD"], "path": routes::RESULT },
            { "methods": ["POST"], "path": routes::RESULT_REVOKE },
            { "methods": ["GET", "HEAD"], "path": routes::SYSTEM },
            { "methods": ["POST"], "path": routes::MAINTENANCE_RETENTION }
        ])
    );

    let states = inventory("states-v2.json");
    assert_eq!(states["schemaVersion"], SCHEMA_VERSION);
    assert_eq!(
        states["wechatTestStates"],
        serde_json::json!(["accepted_by_relay"])
    );
}

#[test]
fn validator_rejects_inventory_hash_order_and_metadata_drift() {
    let manifest = read_manifest();
    let files = fixture_bytes();

    let mut missing = manifest.clone();
    missing.fixtures.pop();
    assert!(validate_manifest(&missing, &files).is_err());

    let mut extra = manifest.clone();
    extra.fixtures.push(FixtureEntry {
        path: "zz-extra-v2.json".to_owned(),
        sha256: "0".repeat(64),
        media_type: JSON_MEDIA_TYPE.to_owned(),
        schema_version: SCHEMA_VERSION,
    });
    assert!(validate_manifest(&extra, &files).is_err());

    let mut duplicate = manifest.clone();
    duplicate.fixtures.insert(1, duplicate.fixtures[0].clone());
    assert!(validate_manifest(&duplicate, &files).is_err());

    let mut reversed = manifest.clone();
    reversed.fixtures.swap(0, 1);
    assert!(validate_manifest(&reversed, &files).is_err());

    let mut uppercase_hash = manifest.clone();
    uppercase_hash.fixtures[0].sha256 = uppercase_hash.fixtures[0].sha256.to_ascii_uppercase();
    assert!(validate_manifest(&uppercase_hash, &files).is_err());

    let mut changed_bytes = files.clone();
    changed_bytes
        .get_mut(EXPECTED_FIXTURES[0])
        .expect("fixture")
        .push(b' ');
    assert!(validate_manifest(&manifest, &changed_bytes).is_err());

    let mut rogue_file = files.clone();
    rogue_file.insert("rogue.json".to_owned(), b"{}\n".to_vec());
    assert!(validate_manifest(&manifest, &rogue_file).is_err());
}

#[test]
fn validator_rejects_unsafe_paths_and_invalid_metadata() {
    let manifest = read_manifest();
    let files = fixture_bytes();

    for path in [
        "nested/error-v2.json",
        "Error-v2.json",
        "error.json",
        "_error-v2.json",
    ] {
        let mut candidate = manifest.clone();
        candidate.fixtures[0].path = path.to_owned();
        candidate
            .fixtures
            .sort_by(|left, right| left.path.cmp(&right.path));
        assert!(validate_manifest(&candidate, &files).is_err(), "{path}");
    }

    let mut media_type = manifest.clone();
    media_type.fixtures[0].media_type = "text/plain".to_owned();
    assert!(validate_manifest(&media_type, &files).is_err());

    let mut schema_version = manifest.clone();
    schema_version.fixtures[0].schema_version = 1;
    assert!(validate_manifest(&schema_version, &files).is_err());

    let mut short_hash = manifest.clone();
    short_hash.fixtures[0].sha256.pop();
    assert!(validate_manifest(&short_hash, &files).is_err());
}

#[test]
fn manifest_and_entries_reject_unknown_or_missing_fields() {
    let mut root = serde_json::json!({
        "manifestVersion": 1,
        "contract": CONTRACT_NAME,
        "fixtures": [],
        "unknown": true
    });
    assert!(serde_json::from_value::<ContractManifest>(root.clone()).is_err());
    root.as_object_mut()
        .expect("manifest object")
        .remove("fixtures");
    assert!(serde_json::from_value::<ContractManifest>(root).is_err());

    let root = serde_json::json!({
        "manifestVersion": 1,
        "contract": CONTRACT_NAME,
        "sourceCommit": "not-allowed-here",
        "fixtures": []
    });
    assert!(serde_json::from_value::<ContractManifest>(root).is_err());

    let mut entry = serde_json::json!({
        "path": "error-v2.json",
        "sha256": "0".repeat(64),
        "mediaType": JSON_MEDIA_TYPE,
        "schemaVersion": 2,
        "unknown": true
    });
    assert!(serde_json::from_value::<FixtureEntry>(entry.clone()).is_err());
    entry
        .as_object_mut()
        .expect("fixture object")
        .remove("sha256");
    assert!(serde_json::from_value::<FixtureEntry>(entry).is_err());
}
