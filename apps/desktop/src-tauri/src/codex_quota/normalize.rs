//! Normalization of external app-server responses into internal types.
//!
//! This layer is permissive: it ignores unknown fields, handles missing/null
//! gracefully, and never panics on malformed input. It does NOT tolerate
//! missing critical fields or type mismatches.

use crate::agent::CodexUsageSnapshot;
use serde::Deserialize;
use serde_json::Value;

/// Errors during normalization.
#[derive(Debug, thiserror::Error)]
pub enum NormalizeError {
    #[error("field type mismatch: {0}")]
    TypeMismatch(&'static str),
    #[error("value out of range: {0}")]
    OutOfRange(&'static str),
    #[error("unsupported schema: {0}")]
    UnsupportedSchema(String),
}

/// Raw app-server rateLimits response (single-group compatibility view).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRateLimitsCompat {
    used_percent: Option<Value>,
    window_duration_mins: Option<Value>,
    resets_at: Option<Value>,
}

/// Raw app-server rateLimitsByLimitId response (multi-group map).
#[derive(Debug, Deserialize)]
struct RawRateLimitsByLimitId {
    #[serde(flatten)]
    groups: std::collections::HashMap<String, RawRateLimitsGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRateLimitsGroup {
    used_percent: Option<Value>,
    window_duration_mins: Option<Value>,
    resets_at: Option<Value>,
}

/// Normalizes a raw JSON rateLimits response into a `CodexUsageSnapshot`.
///
/// Accepts either:
/// - Single-group compatibility view: `{ usedPercent, windowDurationMins, resetsAt }`
/// - Multi-group map: `{ "limitId": { ... }, ... }`
///
/// Prefers primary group "codex" if present, else first available group.
/// Returns `None` if all fields are null/missing (not an error).
pub fn normalize_rate_limits(value: &Value) -> Result<Option<CodexUsageSnapshot>, NormalizeError> {
    let obj = value
        .as_object()
        .ok_or(NormalizeError::TypeMismatch("expected object"))?;

    // Try multi-group map first (rateLimitsByLimitId).
    if obj.contains_key("rateLimitsByLimitId") {
        let groups_value = &obj["rateLimitsByLimitId"];
        return normalize_rate_limits_by_limit_id(groups_value);
    }

    // Try single-group compatibility view (rateLimits).
    if obj.contains_key("rateLimits") || obj.contains_key("usedPercent") {
        let compat_value = obj.get("rateLimits").unwrap_or(value);
        return normalize_rate_limits_compat(compat_value);
    }

    Err(NormalizeError::UnsupportedSchema(
        "no recognized rate limit structure".into(),
    ))
}

fn normalize_rate_limits_compat(
    value: &Value,
) -> Result<Option<CodexUsageSnapshot>, NormalizeError> {
    let raw: RawRateLimitsCompat = serde_json::from_value(value.clone())
        .map_err(|e| NormalizeError::UnsupportedSchema(e.to_string()))?;

    let used_percent = extract_percent(&raw.used_percent)?;
    let window_duration_mins = extract_i64(&raw.window_duration_mins, "windowDurationMins")?;
    let resets_at = extract_i64(&raw.resets_at, "resetsAt")?;

    // All fields null/missing → no data, not an error.
    if used_percent.is_none() && window_duration_mins.is_none() && resets_at.is_none() {
        return Ok(None);
    }

    Ok(Some(CodexUsageSnapshot {
        used_percent,
        window_duration_mins,
        resets_at,
    }))
}

fn normalize_rate_limits_by_limit_id(
    value: &Value,
) -> Result<Option<CodexUsageSnapshot>, NormalizeError> {
    let groups: RawRateLimitsByLimitId = serde_json::from_value(value.clone())
        .map_err(|e| NormalizeError::UnsupportedSchema(e.to_string()))?;

    // Prefer "codex" group if present.
    let preferred = groups
        .groups
        .get("codex")
        .or_else(|| groups.groups.values().next());

    let Some(group) = preferred else {
        return Ok(None);
    };

    let used_percent = extract_percent(&group.used_percent)?;
    let window_duration_mins = extract_i64(&group.window_duration_mins, "windowDurationMins")?;
    let resets_at = extract_i64(&group.resets_at, "resetsAt")?;

    if used_percent.is_none() && window_duration_mins.is_none() && resets_at.is_none() {
        return Ok(None);
    }

    Ok(Some(CodexUsageSnapshot {
        used_percent,
        window_duration_mins,
        resets_at,
    }))
}

fn extract_percent(value: &Option<Value>) -> Result<Option<u8>, NormalizeError> {
    let Some(v) = value else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    let num = v
        .as_f64()
        .ok_or(NormalizeError::TypeMismatch("usedPercent must be numeric"))?;
    if !(0.0..=100.0).contains(&num) {
        return Err(NormalizeError::OutOfRange("usedPercent must be 0-100"));
    }
    Ok(Some(num as u8))
}

fn extract_i64(value: &Option<Value>, field: &'static str) -> Result<Option<i64>, NormalizeError> {
    let Some(v) = value else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    v.as_i64()
        .ok_or(NormalizeError::TypeMismatch(field))
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn single_group_compat_view_with_all_fields() {
        let input = json!({
            "usedPercent": 47,
            "windowDurationMins": 300,
            "resetsAt": 1800000000
        });
        let result = normalize_rate_limits_compat(&input).unwrap();
        assert_eq!(
            result,
            Some(CodexUsageSnapshot {
                used_percent: Some(47),
                window_duration_mins: Some(300),
                resets_at: Some(1800000000),
            })
        );
    }

    #[test]
    fn single_group_with_null_fields_returns_none() {
        let input = json!({
            "usedPercent": null,
            "windowDurationMins": null,
            "resetsAt": null
        });
        let result = normalize_rate_limits_compat(&input).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn single_group_with_missing_fields_returns_partial() {
        let input = json!({
            "usedPercent": 25
        });
        let result = normalize_rate_limits_compat(&input).unwrap();
        assert_eq!(
            result,
            Some(CodexUsageSnapshot {
                used_percent: Some(25),
                window_duration_mins: None,
                resets_at: None,
            })
        );
    }

    #[test]
    fn single_group_rejects_out_of_range_percent() {
        let input = json!({
            "usedPercent": 150
        });
        let result = normalize_rate_limits_compat(&input);
        assert!(matches!(result, Err(NormalizeError::OutOfRange(_))));
    }

    #[test]
    fn single_group_rejects_negative_percent() {
        let input = json!({
            "usedPercent": -5
        });
        let result = normalize_rate_limits_compat(&input);
        assert!(matches!(result, Err(NormalizeError::OutOfRange(_))));
    }

    #[test]
    fn single_group_rejects_non_numeric_percent() {
        let input = json!({
            "usedPercent": "50"
        });
        let result = normalize_rate_limits_compat(&input);
        assert!(matches!(result, Err(NormalizeError::TypeMismatch(_))));
    }

    #[test]
    fn multi_group_prefers_codex_limit_id() {
        let input = json!({
            "rateLimitsByLimitId": {
                "gpt-4": {
                    "displayName": "GPT-4 Window",
                    "usedPercent": 80,
                    "windowDurationMins": 180,
                    "resetsAt": 1800000100
                },
                "codex": {
                    "displayName": "Codex Window",
                    "usedPercent": 30,
                    "windowDurationMins": 300,
                    "resetsAt": 1800000200
                }
            }
        });
        let result = normalize_rate_limits(&input).unwrap();
        assert_eq!(
            result,
            Some(CodexUsageSnapshot {
                used_percent: Some(30),
                window_duration_mins: Some(300),
                resets_at: Some(1800000200),
            })
        );
    }

    #[test]
    fn multi_group_falls_back_to_first_available() {
        let input = json!({
            "rateLimitsByLimitId": {
                "gpt-4": {
                    "usedPercent": 60,
                    "windowDurationMins": 180,
                    "resetsAt": 1800000300
                }
            }
        });
        let result = normalize_rate_limits(&input).unwrap();
        assert_eq!(
            result,
            Some(CodexUsageSnapshot {
                used_percent: Some(60),
                window_duration_mins: Some(180),
                resets_at: Some(1800000300),
            })
        );
    }

    #[test]
    fn multi_group_with_unknown_fields_ignored() {
        let input = json!({
            "rateLimitsByLimitId": {
                "codex": {
                    "displayName": "Codex",
                    "usedPercent": 40,
                    "windowDurationMins": 300,
                    "resetsAt": 1800000400,
                    "futureField": "ignored",
                    "anotherUnknown": 123
                }
            }
        });
        let result = normalize_rate_limits(&input).unwrap();
        assert_eq!(
            result,
            Some(CodexUsageSnapshot {
                used_percent: Some(40),
                window_duration_mins: Some(300),
                resets_at: Some(1800000400),
            })
        );
    }

    #[test]
    fn wrapped_rate_limits_object() {
        let input = json!({
            "rateLimits": {
                "usedPercent": 55,
                "windowDurationMins": 300,
                "resetsAt": 1800000500
            }
        });
        let result = normalize_rate_limits(&input).unwrap();
        assert_eq!(
            result,
            Some(CodexUsageSnapshot {
                used_percent: Some(55),
                window_duration_mins: Some(300),
                resets_at: Some(1800000500),
            })
        );
    }

    #[test]
    fn rejects_unsupported_schema() {
        let input = json!({
            "unrelatedField": "value"
        });
        let result = normalize_rate_limits(&input);
        assert!(matches!(result, Err(NormalizeError::UnsupportedSchema(_))));
    }

    #[test]
    fn rejects_non_object_input() {
        let input = json!("not an object");
        let result = normalize_rate_limits(&input);
        assert!(matches!(result, Err(NormalizeError::TypeMismatch(_))));
    }

    #[test]
    fn zero_percent_is_valid() {
        let input = json!({
            "usedPercent": 0,
            "windowDurationMins": 300,
            "resetsAt": 1800000600
        });
        let result = normalize_rate_limits_compat(&input).unwrap();
        assert_eq!(
            result,
            Some(CodexUsageSnapshot {
                used_percent: Some(0),
                window_duration_mins: Some(300),
                resets_at: Some(1800000600),
            })
        );
    }

    #[test]
    fn hundred_percent_is_valid() {
        let input = json!({
            "usedPercent": 100,
            "windowDurationMins": 300,
            "resetsAt": 1800000700
        });
        let result = normalize_rate_limits_compat(&input).unwrap();
        assert_eq!(
            result,
            Some(CodexUsageSnapshot {
                used_percent: Some(100),
                window_duration_mins: Some(300),
                resets_at: Some(1800000700),
            })
        );
    }

    #[test]
    fn fractional_percent_truncated_to_u8() {
        let input = json!({
            "usedPercent": 47.9
        });
        let result = normalize_rate_limits_compat(&input).unwrap();
        assert_eq!(
            result,
            Some(CodexUsageSnapshot {
                used_percent: Some(47),
                window_duration_mins: None,
                resets_at: None,
            })
        );
    }
}
