//! Read-only, local evidence. Persisted user trust is never effective host authorization.
use crate::hook_installer::{HookFeatures, HookManager};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TrustEvidence {
    NotApplicable,
    Unknown,
    NoMatchingRecord,
    MatchingRecord,
    DefinitionChanged,
    ExplicitlyDisabled,
}

pub(crate) struct HookTrustResolver {
    pub user_config: Option<PathBuf>,
}
impl HookTrustResolver {
    pub fn from_environment() -> Self {
        let home = std::env::var_os("CODEX_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(|s| PathBuf::from(s).join(".codex")));
        Self {
            user_config: home
                .filter(|p| p.is_absolute())
                .map(|p| p.join("config.toml")),
        }
    }
    #[cfg(test)]
    fn resolve(&self, _source: &Path, key: &str, hash: &str) -> TrustEvidence {
        self.user_config
            .as_deref()
            .and_then(|p| bounded_read(p).ok())
            .map(|b| trust(&b, key, hash))
            .unwrap_or(TrustEvidence::Unknown)
    }
}

pub(crate) fn fingerprint(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
pub(crate) fn bounded_read(path: &Path) -> Result<Vec<u8>, String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("HOOK_SOURCE_UNREADABLE".into()),
    };
    let mut bytes = Vec::new();
    file.take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|_| "HOOK_SOURCE_UNREADABLE")?;
    if bytes.len() > 1_048_576 {
        return Err("HOOK_SOURCE_TOO_LARGE".into());
    }
    Ok(bytes)
}
fn trust(bytes: &[u8], key: &str, hash: &str) -> TrustEvidence {
    let Some(root) = std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| toml::from_str::<toml::Value>(s).ok())
    else {
        return TrustEvidence::Unknown;
    };
    let state = root
        .get("hooks")
        .and_then(|v| v.get("state"))
        .and_then(|v| v.get(key));
    if state.is_some_and(|s| {
        !s.is_table()
            || s.get("enabled").is_some_and(|v| !v.is_bool())
            || s.get("trusted_hash").is_some_and(|v| !v.is_str())
    }) {
        return TrustEvidence::Unknown;
    }
    if state
        .and_then(|v| v.get("enabled"))
        .and_then(toml::Value::as_bool)
        == Some(false)
    {
        return TrustEvidence::ExplicitlyDisabled;
    }
    match state
        .and_then(|v| v.get("trusted_hash"))
        .and_then(toml::Value::as_str)
    {
        Some(value) if value == hash => TrustEvidence::MatchingRecord,
        Some(_) => TrustEvidence::DefinitionChanged,
        None => TrustEvidence::NoMatchingRecord,
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HandlerEvidence {
    pub event: String,
    pub required: bool,
    pub configured: bool,
    pub trust: TrustEvidence,
    pub evidence_source: &'static str,
    pub last_error_code: Option<String>,
    pub last_observed_at: Option<i64>,
    pub observed_registration_id: Option<String>,
    pub observation_current: bool,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HookHealthSnapshot {
    pub runtime_epoch: String,
    pub revision: u64,
    pub observed_at: i64,
    pub fresh: bool,
    pub observation_enabled: bool,
    pub hook_source_path: Option<String>,
    pub user_state_source_path: Option<String>,
    pub source_resolution: &'static str,
    pub compatibility: &'static str,
    pub host_build: Option<String>,
    pub installation: &'static str,
    pub registration_id: Option<String>,
    pub definition_fingerprint: Option<String>,
    pub handlers: Vec<HandlerEvidence>,
    pub diagnostic_code: Option<String>,
    pub verification: crate::hook_verification::VerificationView,
}

pub(crate) fn inspect(
    source: Option<&Path>,
    manager: Option<&HookManager>,
    resolver: &HookTrustResolver,
    enabled: bool,
    attention: bool,
) -> HookHealthSnapshot {
    let mut result = HookHealthSnapshot {
        runtime_epoch: String::new(),
        revision: 0,
        observed_at: crate::db::now_ms().unwrap_or(0),
        fresh: true,
        observation_enabled: enabled,
        hook_source_path: source.map(|p| p.to_string_lossy().into_owned()),
        user_state_source_path: resolver
            .user_config
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        source_resolution: if resolver.user_config.is_some() {
            "candidate"
        } else {
            "unknown"
        },
        compatibility: "unverified",
        host_build: None,
        installation: "absent",
        registration_id: None,
        definition_fingerprint: None,
        handlers: vec![],
        diagnostic_code: None,
        verification: Default::default(),
    };
    let (Some(source), Some(manager)) = (source, manager) else {
        return result;
    };
    let read_pair = || -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
        Ok((
            bounded_read(source)?,
            resolver
                .user_config
                .as_deref()
                .map(bounded_read)
                .transpose()?,
        ))
    };
    let mut stable = None;
    for _ in 0..3 {
        match read_pair().and_then(|before| read_pair().map(|after| (before, after))) {
            Ok((before, after)) if before == after => {
                stable = Some(before);
                break;
            }
            Ok(_) => result.diagnostic_code = Some("HOOK_SOURCE_CHANGING".into()),
            Err(code) => result.diagnostic_code = Some(code),
        }
    }
    let Some((bytes, user)) = stable else {
        result.fresh = false;
        result.installation = "checking";
        return result;
    };
    result.diagnostic_code = None;
    if bytes.is_empty() {
        return result;
    }
    let Ok(root) = serde_json::from_slice::<Value>(&bytes) else {
        result.installation = "error";
        result.diagnostic_code = Some("HOOK_JSON_INVALID".into());
        return result;
    };
    let Ok(specs) = manager.desired_specs(HookFeatures {
        prompt_capture: true,
        run_lifecycle: true,
        attention: true,
        capture_agent_outputs: false,
    }) else {
        result.installation = "error";
        return result;
    };
    let mut semantic_definition = Vec::new();
    for spec in specs {
        let needed = spec.event_name != "PermissionRequest" || attention;
        let locations = root
            .get("hooks")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|events| events.iter())
            .flat_map(|(event, groups)| {
                groups
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .flat_map(move |(gi, group)| {
                        group
                            .get("hooks")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .enumerate()
                            .filter_map(move |(hi, handler)| {
                                let owned = ["command", "commandWindows"].iter().any(|key| {
                                    handler
                                        .get(key)
                                        .and_then(Value::as_str)
                                        .is_some_and(|s| s.contains(spec.marker))
                                });
                                owned.then_some((event.as_str(), gi, hi, group, handler))
                            })
                    })
            })
            .collect::<Vec<_>>();
        let mut item = HandlerEvidence {
            event: spec.event_name.into(),
            // PermissionRequest becomes required exactly when the user enables
            // attention. It is otherwise explicitly not applicable.
            required: needed,
            configured: false,
            trust: if enabled && needed {
                TrustEvidence::Unknown
            } else {
                TrustEvidence::NotApplicable
            },
            evidence_source: "none",
            last_error_code: None,
            last_observed_at: None,
            observed_registration_id: None,
            observation_current: false,
        };
        if let [(event, gi, hi, group, handler)] = locations.as_slice() {
            let supported = handler.as_object().is_some_and(|h| {
                h.keys().all(|k| {
                    [
                        "type",
                        "command",
                        "commandWindows",
                        "timeout",
                        "async",
                        "statusMessage",
                        "additionalContextLimit",
                    ]
                    .contains(&k.as_str())
                })
            }) && group
                .as_object()
                .is_some_and(|g| g.keys().all(|k| ["matcher", "hooks"].contains(&k.as_str())));
            item.configured = *event == spec.event_name
                && handler["type"] == "command"
                && handler["command"] == spec.command
                && handler["commandWindows"] == spec.command_windows
                && handler["timeout"] == spec.timeout_seconds
                && handler["async"] == false
                && handler.get("statusMessage").is_none()
                && handler.get("additionalContextLimit").is_none()
                && (spec.event_name != "PermissionRequest" || group.get("matcher").is_none());
            if enabled
                && needed
                && supported
                && handler.get("additionalContextLimit").is_none()
                && *event == spec.event_name
            {
                if let Ok(hash) = crate::hook_installer::hook_hash(spec.event_name, group, handler)
                {
                    semantic_definition.extend_from_slice(spec.event_name.as_bytes());
                    semantic_definition.push(0);
                    semantic_definition.extend_from_slice(hash.as_bytes());
                    semantic_definition.push(0);
                    let label = match spec.event_name {
                        "UserPromptSubmit" => "user_prompt_submit",
                        "Stop" => "stop",
                        _ => "permission_request",
                    };
                    let key = format!("{}:{label}:{gi}:{hi}", source.display());
                    item.trust = user
                        .as_ref()
                        .map(|b| trust(b, &key, &hash))
                        .unwrap_or(TrustEvidence::Unknown);
                    item.evidence_source = if user.is_some() {
                        "persisted_user_config"
                    } else {
                        "none"
                    };
                }
            }
            if !supported {
                item.last_error_code = Some("HOOK_FORMAT_UNSUPPORTED".into());
                item.configured = false;
            }
        } else if locations.len() > 1 {
            item.last_error_code = Some("HOOK_DUPLICATE".into());
        }
        result.handlers.push(item);
    }
    // Full hooks.json remains the installer's CAS input. Health/verification
    // use only the managed handlers' normalized semantic definition so an
    // unrelated Hook or JSON whitespace does not invalidate this product.
    result.definition_fingerprint =
        (!semantic_definition.is_empty()).then(|| fingerprint(&semantic_definition));
    result.installation = if result
        .handlers
        .iter()
        .filter(|h| h.required)
        .all(|h| h.configured)
    {
        "current"
    } else {
        "needs_repair"
    };
    if result
        .handlers
        .iter()
        .any(|h| h.last_error_code.as_deref() == Some("HOOK_FORMAT_UNSUPPORTED"))
    {
        result.installation = "error";
        result.compatibility = "unsupported";
        result.diagnostic_code = Some("HOOK_FORMAT_UNSUPPORTED".into());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn independent_upstream_golden_hashes_match_supported_normalization() {
        let cases: Value = serde_json::from_str(include_str!(
            "../../fixtures/upstream/openai-codex-hook-trust-9688359/cases.json"
        ))
        .unwrap();
        let expected: Value = serde_json::from_str(include_str!(
            "../../fixtures/upstream/openai-codex-hook-trust-9688359/expected.json"
        ))
        .unwrap();
        for (input, expected) in cases["cases"]
            .as_array()
            .unwrap()
            .iter()
            .zip(expected["cases"].as_array().unwrap())
            .take(3)
        {
            let group =
                serde_json::json!({"matcher":input["matcher"],"hooks":[input["handler"].clone()]});
            let actual = crate::hook_installer::hook_hash(
                input["eventName"].as_str().unwrap(),
                &group,
                &input["handler"],
            )
            .unwrap();
            assert_eq!(
                actual,
                expected["trustedHash"].as_str().unwrap(),
                "{}",
                input["id"]
            );
        }
        // Non-default additionalContextLimit is outside this product's fixed handlers.
        // The health reader rejects that field instead of using a partial hash as trust.
    }
    #[test]
    fn paused_and_unknown_formats_never_claim_trusted_and_content_changes_are_seen() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("hooks.json");
        let manager = HookManager::new(
            source.clone(),
            d.path().join("inbox"),
            d.path().join("app.exe"),
        )
        .unwrap();
        let resolver = HookTrustResolver {
            user_config: Some(d.path().join("user.toml")),
        };
        let missing = inspect(Some(&source), Some(&manager), &resolver, false, false);
        assert_eq!(missing.installation, "absent");
        assert!(!missing.observation_enabled);
        manager
            .install_features(HookFeatures {
                prompt_capture: true,
                run_lifecycle: true,
                attention: false,
                capture_agent_outputs: false,
            })
            .unwrap();
        let paused = inspect(Some(&source), Some(&manager), &resolver, false, false);
        assert!(paused
            .handlers
            .iter()
            .all(|h| h.trust == TrustEvidence::NotApplicable));
        let current = inspect(Some(&source), Some(&manager), &resolver, true, false);
        assert_eq!(current.installation, "current");
        let mut root: Value = serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
        root["hooks"]["Stop"][0]["hooks"][0]["additionalContextLimit"] = 42.into();
        std::fs::write(&source, serde_json::to_vec(&root).unwrap()).unwrap();
        let changed = inspect(Some(&source), Some(&manager), &resolver, true, false);
        assert_ne!(
            changed.definition_fingerprint,
            current.definition_fingerprint
        );
        assert_eq!(changed.installation, "needs_repair");
        assert_eq!(
            changed
                .handlers
                .iter()
                .find(|h| h.event == "Stop")
                .unwrap()
                .trust,
            TrustEvidence::Unknown
        );
    }
    #[test]
    fn user_source_is_independent_of_project_and_disabled_wins() {
        let d = tempfile::tempdir().unwrap();
        let project = d.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let user = d.path().join("user.toml");
        std::fs::write(
            project.join("config.toml"),
            "[hooks.state.test]\ntrusted_hash='match'\n",
        )
        .unwrap();
        std::fs::write(
            &user,
            "[hooks.state.test]\ntrusted_hash='match'\nenabled=false\n",
        )
        .unwrap();
        let resolver = HookTrustResolver {
            user_config: Some(user.clone()),
        };
        assert_eq!(
            resolver.resolve(&project.join("hooks.json"), "test", "match"),
            TrustEvidence::ExplicitlyDisabled
        );
        std::fs::write(&user, "[hooks.state.test]\ntrusted_hash='match'\n").unwrap();
        assert_eq!(
            resolver.resolve(&project.join("hooks.json"), "test", "match"),
            TrustEvidence::MatchingRecord
        );
        std::fs::write(user, "").unwrap();
        assert_eq!(
            resolver.resolve(&project.join("hooks.json"), "test", "match"),
            TrustEvidence::NoMatchingRecord
        );
    }
}
