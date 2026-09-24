use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const MANIFEST_VERSION: u32 = 1;
const CONTRACT_NAME: &str = "promptdock-relay-api-v1";
const JSON_MEDIA_TYPE: &str = "application/json";
const API_SCHEMA_VERSION: u32 = 1;

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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/server-http/v1")
}

fn read_manifest() -> ContractManifest {
    let path = fixtures_directory().join("manifest.json");
    let bytes = fs::read(&path).expect("read API contract manifest");
    serde_json::from_slice(&bytes).expect("parse strict API contract manifest")
}

fn public_fixture_bytes() -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(fixtures_directory())
        .expect("read API fixture directory")
        .filter_map(|entry| {
            let entry = entry.expect("fixture directory entry");
            let file_type = entry.file_type().expect("fixture entry type");
            if !file_type.is_file() {
                return None;
            }
            let name = entry.file_name().into_string().expect("UTF-8 fixture name");
            if !name.ends_with("-v1.json") {
                return None;
            }
            Some((
                name,
                fs::read(entry.path()).expect("read raw fixture bytes"),
            ))
        })
        .collect()
}

fn validate_manifest(
    manifest: &ContractManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    if manifest.manifest_version != MANIFEST_VERSION {
        return Err("unsupported manifestVersion".to_owned());
    }
    if manifest.contract != CONTRACT_NAME {
        return Err("unexpected contract name".to_owned());
    }

    let mut seen = BTreeSet::new();
    let mut previous: Option<&str> = None;
    for entry in &manifest.fixtures {
        let mut components = Path::new(&entry.path).components();
        if !matches!(components.next(), Some(Component::Normal(_)))
            || components.next().is_some()
            || !entry.path.ends_with("-v1.json")
        {
            return Err(format!("unsafe or non-v1 fixture path: {}", entry.path));
        }
        if !seen.insert(entry.path.clone()) {
            return Err(format!("duplicate fixture path: {}", entry.path));
        }
        if previous.is_some_and(|previous| previous >= entry.path.as_str()) {
            return Err("fixture paths are not strictly sorted".to_owned());
        }
        previous = Some(&entry.path);
        if entry.media_type != JSON_MEDIA_TYPE {
            return Err(format!("unexpected media type for {}", entry.path));
        }
        if entry.schema_version != API_SCHEMA_VERSION {
            return Err(format!("unexpected schema version for {}", entry.path));
        }
        if entry.sha256.len() != 64
            || !entry
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!("invalid lowercase SHA-256 for {}", entry.path));
        }
        let bytes = files
            .get(&entry.path)
            .ok_or_else(|| format!("manifest contains extra fixture: {}", entry.path))?;
        let actual = format!("{:x}", Sha256::digest(bytes));
        if actual != entry.sha256 {
            return Err(format!("raw-byte SHA-256 mismatch for {}", entry.path));
        }
    }

    let actual_paths = files.keys().cloned().collect::<BTreeSet<_>>();
    if seen != actual_paths {
        let missing = actual_paths.difference(&seen).cloned().collect::<Vec<_>>();
        return Err(format!("manifest omits public fixtures: {missing:?}"));
    }
    Ok(())
}

#[test]
fn checked_in_manifest_is_a_complete_raw_byte_inventory() {
    validate_manifest(&read_manifest(), &public_fixture_bytes())
        .expect("checked-in API contract manifest");
}

#[test]
fn validator_rejects_omissions_extras_duplicates_bad_hashes_and_byte_drift() {
    let manifest = read_manifest();
    let files = public_fixture_bytes();

    let mut missing = manifest.clone();
    missing.fixtures.pop();
    assert!(validate_manifest(&missing, &files).is_err());

    let mut extra = manifest.clone();
    extra.fixtures.push(FixtureEntry {
        path: "zz-extra-v1.json".to_owned(),
        sha256: "0".repeat(64),
        media_type: JSON_MEDIA_TYPE.to_owned(),
        schema_version: API_SCHEMA_VERSION,
    });
    assert!(validate_manifest(&extra, &files).is_err());

    let mut duplicate = manifest.clone();
    duplicate.fixtures.insert(1, duplicate.fixtures[0].clone());
    assert!(validate_manifest(&duplicate, &files).is_err());

    let mut uppercase = manifest.clone();
    uppercase.fixtures[0].sha256 = uppercase.fixtures[0].sha256.to_ascii_uppercase();
    assert!(validate_manifest(&uppercase, &files).is_err());

    let mut short = manifest.clone();
    short.fixtures[0].sha256.pop();
    assert!(validate_manifest(&short, &files).is_err());

    let mut changed_bytes = files.clone();
    changed_bytes
        .get_mut(&manifest.fixtures[0].path)
        .expect("fixture bytes")
        .push(b' ');
    assert!(validate_manifest(&manifest, &changed_bytes).is_err());
}

#[test]
fn manifest_and_entries_reject_unknown_fields() {
    let mut root = serde_json::to_value(serde_json::json!({
        "manifestVersion": 1,
        "contract": CONTRACT_NAME,
        "fixtures": [],
    }))
    .expect("manifest JSON");
    root.as_object_mut()
        .expect("manifest object")
        .insert("unknown".to_owned(), serde_json::Value::Bool(true));
    assert!(serde_json::from_value::<ContractManifest>(root).is_err());

    let mut entry = serde_json::json!({
        "path": "error-v1.json",
        "sha256": "0".repeat(64),
        "mediaType": JSON_MEDIA_TYPE,
        "schemaVersion": 1,
    });
    entry
        .as_object_mut()
        .expect("entry object")
        .insert("unknown".to_owned(), serde_json::Value::Bool(true));
    assert!(serde_json::from_value::<FixtureEntry>(entry).is_err());
}
