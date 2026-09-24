use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const MANIFEST_VERSION: u32 = 1;
const CONTRACT_NAME: &str = "promptdock-relay-gateway-v5";
const JSON_MEDIA_TYPE: &str = "application/json";
const SCHEMA_VERSION: u32 = 5;
const MAX_FRAME_BYTES: usize = 65_536;
const MAX_RESPONSE_BYTES: usize = 16_384;
const EXPECTED_FIXTURES: [&str; 28] = [
    "error-v5.json",
    "hello-v5.json",
    "ping-v5.json",
    "policy-revoked-v5.json",
    "pong-v5.json",
    "request-ack-v5.json",
    "request-cancel-run-v5.json",
    "request-get-device-info-v5.json",
    "request-get-run-detail-v5.json",
    "request-get-run-tree-v5.json",
    "request-list-harness-profiles-v5.json",
    "request-list-runs-v5.json",
    "request-list-runtimes-v5.json",
    "request-list-task-presets-v5.json",
    "request-list-workspaces-v5.json",
    "request-start-run-v5.json",
    "response-cancel-run-v5.json",
    "response-get-device-info-v5.json",
    "response-get-run-detail-v5.json",
    "response-get-run-tree-v5.json",
    "response-list-harness-profiles-v5.json",
    "response-list-runs-v5.json",
    "response-list-runtimes-v5.json",
    "response-list-task-presets-v5.json",
    "response-list-workspaces-v5.json",
    "response-start-run-v5.json",
    "superseded-v5.json",
    "welcome-v5.json",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContractManifest {
    manifest_version: u32,
    contract: String,
    fixtures: Vec<FixtureEntry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureEntry {
    path: String,
    sha256: String,
    media_type: String,
    schema_version: u32,
}

fn fixtures_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/node-link/v5")
}

fn read_manifest() -> ContractManifest {
    let path = fixtures_directory().join("manifest.json");
    let bytes = fs::read(&path).expect("read Gateway contract manifest");
    serde_json::from_slice(&bytes).expect("parse strict Gateway contract manifest")
}

fn public_fixture_bytes() -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(fixtures_directory())
        .expect("read Gateway fixture directory")
        .filter_map(|entry| {
            let entry = entry.expect("fixture directory entry");
            if !entry.file_type().expect("fixture entry type").is_file() {
                return None;
            }
            let name = entry.file_name().into_string().expect("UTF-8 fixture name");
            if name == "manifest.json" {
                return None;
            }
            Some((name, fs::read(entry.path()).expect("read fixture bytes")))
        })
        .collect()
}

fn validate_manifest(
    manifest: &ContractManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    if manifest.manifest_version != MANIFEST_VERSION || manifest.contract != CONTRACT_NAME {
        return Err("unsupported Gateway manifest identity".to_owned());
    }
    let actual_names = files.keys().map(String::as_str).collect::<Vec<_>>();
    if actual_names != EXPECTED_FIXTURES {
        return Err("Gateway fixture inventory changed".to_owned());
    }

    let mut seen = BTreeSet::new();
    let mut previous: Option<&str> = None;
    for entry in &manifest.fixtures {
        let mut components = Path::new(&entry.path).components();
        if !matches!(components.next(), Some(Component::Normal(_)))
            || components.next().is_some()
            || !is_canonical_fixture_name(&entry.path)
        {
            return Err(format!("unsafe fixture path: {}", entry.path));
        }
        if !seen.insert(entry.path.clone()) {
            return Err(format!("duplicate fixture path: {}", entry.path));
        }
        if previous.is_some_and(|previous| previous >= entry.path.as_str()) {
            return Err("fixture paths are not strictly sorted".to_owned());
        }
        previous = Some(&entry.path);
        if entry.media_type != JSON_MEDIA_TYPE || entry.schema_version != SCHEMA_VERSION {
            return Err(format!("invalid fixture metadata: {}", entry.path));
        }
        if entry.sha256.len() != 64
            || !entry
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!("invalid lowercase SHA-256: {}", entry.path));
        }
        let bytes = files
            .get(&entry.path)
            .ok_or_else(|| format!("manifest contains an extra fixture: {}", entry.path))?;
        validate_canonical_json_bytes(&entry.path, bytes)?;
        let actual = format!("{:x}", Sha256::digest(bytes));
        if actual != entry.sha256 {
            return Err(format!("raw-byte SHA-256 mismatch: {}", entry.path));
        }
    }

    let actual_paths = files.keys().cloned().collect::<BTreeSet<_>>();
    if seen == actual_paths {
        Ok(())
    } else {
        Err("manifest and fixture inventory differ".to_owned())
    }
}

fn is_canonical_fixture_name(value: &str) -> bool {
    value.ends_with("-v5.json")
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
}

fn validate_canonical_json_bytes(path: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty()
        || bytes.len() > MAX_FRAME_BYTES
        || bytes.starts_with(&[0xef, 0xbb, 0xbf])
        || bytes.contains(&b'\r')
        || !bytes.ends_with(b"\n")
    {
        return Err(format!("non-canonical fixture bytes: {path}"));
    }
    if path.starts_with("response-") && bytes.len() > MAX_RESPONSE_BYTES {
        return Err("response fixture exceeds 16 KiB".to_owned());
    }
    serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|_| format!("invalid fixture JSON: {path}"))?;
    Ok(())
}

#[test]
fn checked_in_gateway_manifest_is_a_complete_raw_byte_inventory() {
    validate_manifest(&read_manifest(), &public_fixture_bytes())
        .expect("checked-in Gateway contract manifest");
}

#[test]
fn validator_rejects_inventory_hash_order_metadata_and_byte_drift() {
    let manifest = read_manifest();
    let files = public_fixture_bytes();

    let mut missing = manifest.clone();
    missing.fixtures.pop();
    assert!(validate_manifest(&missing, &files).is_err());

    let mut extra_entry = manifest.clone();
    extra_entry.fixtures.push(FixtureEntry {
        path: "zz-extra-v5.json".to_owned(),
        sha256: "0".repeat(64),
        media_type: JSON_MEDIA_TYPE.to_owned(),
        schema_version: SCHEMA_VERSION,
    });
    assert!(validate_manifest(&extra_entry, &files).is_err());

    let mut duplicate = manifest.clone();
    duplicate.fixtures.insert(1, duplicate.fixtures[0].clone());
    assert!(validate_manifest(&duplicate, &files).is_err());

    let mut reversed = manifest.clone();
    reversed.fixtures.swap(0, 1);
    assert!(validate_manifest(&reversed, &files).is_err());

    let mut uppercase_hash = manifest.clone();
    uppercase_hash.fixtures[0].sha256 = uppercase_hash.fixtures[0].sha256.to_ascii_uppercase();
    assert!(validate_manifest(&uppercase_hash, &files).is_err());

    let mut wrong_contract = manifest.clone();
    wrong_contract.contract = "other-contract".to_owned();
    assert!(validate_manifest(&wrong_contract, &files).is_err());

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
fn manifest_and_entries_reject_unknown_fields() {
    let root = serde_json::json!({
        "manifestVersion": 1,
        "contract": CONTRACT_NAME,
        "fixtures": [],
        "unknown": true
    });
    assert!(serde_json::from_value::<ContractManifest>(root).is_err());

    let entry = serde_json::json!({
        "path": "hello-v5.json",
        "sha256": "0".repeat(64),
        "mediaType": JSON_MEDIA_TYPE,
        "schemaVersion": 2,
        "unknown": true
    });
    assert!(serde_json::from_value::<FixtureEntry>(entry).is_err());
}
