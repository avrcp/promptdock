use std::{fs, path::Path};

use promptdock_server::gateway::protocol::{
    GatewayClientFrameV5, GatewayProtocolError, GatewayServerFrameV5, MAX_GATEWAY_FRAME_BYTES,
    MAX_GATEWAY_RESPONSE_BYTES, decode_client_frame, decode_server_frame, encode_client_frame,
    encode_server_frame,
};
use serde_json::{Value, json};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/node-link/v5")
        .join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read Gateway fixture {}: {error}", path.display()))
}

fn semantic_json(text: &str) -> Value {
    serde_json::from_str(text).expect("fixture JSON")
}

#[test]
fn canonical_wire_inventory_is_metadata_only() {
    const FORBIDDEN_KEYS: &[&str] = &[
        "prompt",
        "output",
        "objective",
        "path",
        "branch",
        "baseCommit",
        "reviewText",
        "testNames",
        "diff",
        "log",
        "tool",
        "threadId",
        "turnId",
        "cwd",
        "runKey",
        "rawRunKey",
        "providerId",
        "modelName",
        "agentLabel",
        "agentRole",
        "taskPreview",
        "latestTaskPreview",
        "resultPreview",
    ];
    for name in [
        "error-v5.json",
        "hello-v5.json",
        "ping-v5.json",
        "policy-revoked-v5.json",
        "pong-v5.json",
        "request-ack-v5.json",
        "request-cancel-run-v5.json",
        "request-get-run-detail-v5.json",
        "request-get-device-info-v5.json",
        "request-get-run-tree-v5.json",
        "request-list-harness-profiles-v5.json",
        "request-list-runs-v5.json",
        "request-list-runtimes-v5.json",
        "request-list-task-presets-v5.json",
        "request-list-workspaces-v5.json",
        "request-start-run-v5.json",
        "response-cancel-run-v5.json",
        "response-get-run-detail-v5.json",
        "response-get-device-info-v5.json",
        "response-get-run-tree-v5.json",
        "response-list-harness-profiles-v5.json",
        "response-list-runs-v5.json",
        "response-list-runtimes-v5.json",
        "response-list-task-presets-v5.json",
        "response-list-workspaces-v5.json",
        "response-start-run-v5.json",
        "superseded-v5.json",
        "welcome-v5.json",
    ] {
        let value = semantic_json(&fixture(name));
        assert_no_forbidden_keys(&value, FORBIDDEN_KEYS, name);
    }
}

fn assert_no_forbidden_keys(value: &Value, forbidden: &[&str], fixture_name: &str) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                assert!(
                    !forbidden.contains(&key.as_str()),
                    "forbidden {key} in {fixture_name}"
                );
                assert_no_forbidden_keys(child, forbidden, fixture_name);
            }
        }
        Value::Array(values) => {
            for child in values {
                assert_no_forbidden_keys(child, forbidden, fixture_name);
            }
        }
        _ => {}
    }
}

#[test]
fn every_fixture_uses_the_production_closed_codec_and_round_trips() {
    for name in [
        "welcome-v5.json",
        "ping-v5.json",
        "request-get-device-info-v5.json",
        "request-get-run-detail-v5.json",
        "request-get-run-tree-v5.json",
        "request-list-harness-profiles-v5.json",
        "request-list-task-presets-v5.json",
        "request-list-runtimes-v5.json",
        "request-list-workspaces-v5.json",
        "request-list-runs-v5.json",
        "request-start-run-v5.json",
        "request-cancel-run-v5.json",
        "superseded-v5.json",
        "policy-revoked-v5.json",
    ] {
        let source = fixture(name);
        let frame = decode_server_frame(&source).expect("valid server fixture");
        let encoded = encode_server_frame(&frame).expect("encode server fixture");
        assert_eq!(semantic_json(&encoded), semantic_json(&source), "{name}");
    }

    for name in [
        "hello-v5.json",
        "pong-v5.json",
        "request-ack-v5.json",
        "response-get-device-info-v5.json",
        "response-get-run-detail-v5.json",
        "response-get-run-tree-v5.json",
        "response-list-harness-profiles-v5.json",
        "response-list-task-presets-v5.json",
        "response-list-runtimes-v5.json",
        "response-list-workspaces-v5.json",
        "response-list-runs-v5.json",
        "response-start-run-v5.json",
        "response-cancel-run-v5.json",
        "error-v5.json",
    ] {
        let source = fixture(name);
        let frame = decode_client_frame(&source).expect("valid client fixture");
        let encoded = encode_client_frame(&frame).expect("encode client fixture");
        assert_eq!(semantic_json(&encoded), semantic_json(&source), "{name}");
    }
}

#[test]
fn outer_and_nested_unknown_fields_arbitrary_actions_and_instance_id_are_rejected() {
    let mut hello = semantic_json(&fixture("hello-v5.json"));
    hello["unknown"] = json!(true);
    assert_eq!(
        decode_client_frame(&hello.to_string()),
        Err(GatewayProtocolError::Malformed)
    );

    let mut hello = semantic_json(&fixture("hello-v5.json"));
    hello["instanceId"] = json!("forbidden");
    assert_eq!(
        decode_client_frame(&hello.to_string()),
        Err(GatewayProtocolError::Malformed)
    );

    let mut welcome = semantic_json(&fixture("welcome-v5.json"));
    welcome["unknown"] = json!(true);
    assert_eq!(
        decode_server_frame(&welcome.to_string()),
        Err(GatewayProtocolError::Malformed)
    );

    let mut request = semantic_json(&fixture("request-get-device-info-v5.json"));
    request["action"] = json!({"kind": "list_runs"});
    assert_eq!(
        decode_server_frame(&request.to_string()),
        Err(GatewayProtocolError::Malformed)
    );

    let mut response = semantic_json(&fixture("response-get-device-info-v5.json"));
    response["result"]["payload"] = json!({"arbitrary": true});
    assert_eq!(
        decode_client_frame(&response.to_string()),
        Err(GatewayProtocolError::Malformed)
    );

    request["action"] = json!({"kind": "get_device_info", "payload": {"sql": "SELECT *"}});
    assert_eq!(
        decode_server_frame(&request.to_string()),
        Err(GatewayProtocolError::Malformed)
    );

    assert_eq!(
        decode_client_frame(r#"{"type":"arbitrary","payload":{}}"#),
        Err(GatewayProtocolError::Malformed)
    );
}

#[test]
fn versions_capabilities_identifiers_and_safe_text_are_strictly_validated() {
    let mut hello = semantic_json(&fixture("hello-v5.json"));
    hello["protocolVersion"] = json!(1);
    assert_eq!(
        decode_client_frame(&hello.to_string()),
        Err(GatewayProtocolError::Validation)
    );

    hello["protocolVersion"] = json!(3);
    hello["capabilities"] = json!(["device_info_v2", "device_info_v2"]);
    assert_eq!(
        decode_client_frame(&hello.to_string()),
        Err(GatewayProtocolError::Malformed)
    );

    hello["capabilities"] = json!(["run_query_v1"]);
    assert_eq!(
        decode_client_frame(&hello.to_string()),
        Err(GatewayProtocolError::Malformed)
    );

    hello["protocolVersion"] = json!(5);
    hello["capabilities"] = json!(["remote_runs_read_v2"]);
    assert!(decode_client_frame(&hello.to_string()).is_ok());

    let mut response = semantic_json(&fixture("response-get-device-info-v5.json"));
    response["requestId"] = json!("00000000-0000-0000-0000-000000000000");
    assert_eq!(
        decode_client_frame(&response.to_string()),
        Err(GatewayProtocolError::Validation)
    );

    response["requestId"] = json!("00000000-0000-4000-8000-00000000000A");
    assert_eq!(
        decode_client_frame(&response.to_string()),
        Err(GatewayProtocolError::Validation)
    );

    response["requestId"] = json!("00000000-0000-4000-8000-000000000003");
    response["result"]["deviceName"] = json!("spoof\u{202e}name");
    assert_eq!(
        decode_client_frame(&response.to_string()),
        Err(GatewayProtocolError::Validation)
    );
}

#[test]
fn job_actions_and_results_enforce_bounds_and_closed_shapes() {
    let mut request = semantic_json(&fixture("request-list-runs-v5.json"));
    request["action"]["pageSize"] = json!(11);
    assert_eq!(
        decode_server_frame(&request.to_string()),
        Err(GatewayProtocolError::Validation)
    );
    request["action"]["pageSize"] = json!(10);
    request["action"]["cursor"] = json!("raw/run/key");
    assert_eq!(
        decode_server_frame(&request.to_string()),
        Err(GatewayProtocolError::Validation)
    );

    let mut tree = semantic_json(&fixture("request-get-run-tree-v5.json"));
    tree["action"]["maxDepth"] = json!(5);
    assert_eq!(
        decode_server_frame(&tree.to_string()),
        Err(GatewayProtocolError::Validation)
    );
    tree["action"]["maxDepth"] = json!(4);
    tree["action"]["maxNodes"] = json!(0);
    assert_eq!(
        decode_server_frame(&tree.to_string()),
        Err(GatewayProtocolError::Validation)
    );

    let mut response = semantic_json(&fixture("response-list-runs-v5.json"));
    response["result"]["items"][0]["title"] = json!("spoof\u{202e}title");
    assert_eq!(
        decode_client_frame(&response.to_string()),
        Err(GatewayProtocolError::Validation)
    );
    response["result"]["items"][0]["title"] = json!("PromptDock");
    response["result"]["items"][0]["privateRunKey"] = json!("forbidden");
    assert_eq!(
        decode_client_frame(&response.to_string()),
        Err(GatewayProtocolError::Malformed)
    );
}

#[test]
fn catalog_and_control_actions_enforce_namespaces_and_async_acceptance() {
    let mut profiles = semantic_json(&fixture("request-list-harness-profiles-v5.json"));
    profiles["action"]["workspaceHandle"] = json!("profile_1234567890abcdef1234567890abcd");
    assert_eq!(
        decode_server_frame(&profiles.to_string()),
        Err(GatewayProtocolError::Validation)
    );

    let mut start = semantic_json(&fixture("request-start-run-v5.json"));
    start["action"]["presetHandle"] = json!("workspace_1234567890abcdef1234567890ab");
    assert_eq!(
        decode_server_frame(&start.to_string()),
        Err(GatewayProtocolError::Validation)
    );

    let mut accepted = semantic_json(&fixture("response-start-run-v5.json"));
    accepted["result"]["phase"] = json!("active");
    assert_eq!(
        decode_client_frame(&accepted.to_string()),
        Err(GatewayProtocolError::Validation)
    );

    let mut runtimes = semantic_json(&fixture("response-list-runtimes-v5.json"));
    runtimes["result"]["items"][0]["capabilityFacets"] = json!(["l3"]);
    assert_eq!(
        decode_client_frame(&runtimes.to_string()),
        Err(GatewayProtocolError::Malformed)
    );

    let mut runs = semantic_json(&fixture("response-list-runs-v5.json"));
    runs["result"]["items"][0]["conditions"] = json!(["running", "running"]);
    assert_eq!(
        decode_client_frame(&runs.to_string()),
        Err(GatewayProtocolError::Validation)
    );

    runs = semantic_json(&fixture("response-list-runs-v5.json"));
    runs["result"]["items"][0]["attentionCount"] = json!(10_001);
    assert_eq!(
        decode_client_frame(&runs.to_string()),
        Err(GatewayProtocolError::Validation)
    );
}

#[test]
fn text_frame_and_response_limits_are_measured_in_utf8_bytes_at_exact_boundaries() {
    let frame_at_limit = padded_hello(MAX_GATEWAY_FRAME_BYTES);
    assert_eq!(frame_at_limit.len(), MAX_GATEWAY_FRAME_BYTES);
    assert_eq!(
        decode_client_frame(&frame_at_limit),
        Err(GatewayProtocolError::Validation)
    );
    let frame_over_limit = padded_hello(MAX_GATEWAY_FRAME_BYTES + 1);
    assert_eq!(
        decode_client_frame(&frame_over_limit),
        Err(GatewayProtocolError::FrameTooLarge)
    );

    let response_at_limit = padded_response(MAX_GATEWAY_RESPONSE_BYTES);
    assert_eq!(response_at_limit.len(), MAX_GATEWAY_RESPONSE_BYTES);
    assert_eq!(
        decode_client_frame(&response_at_limit),
        Err(GatewayProtocolError::Validation)
    );
    let response_over_limit = padded_response(MAX_GATEWAY_RESPONSE_BYTES + 1);
    assert_eq!(
        decode_client_frame(&response_over_limit),
        Err(GatewayProtocolError::ResponseTooLarge)
    );

    let multibyte = "界".repeat(22_000);
    assert!(multibyte.chars().count() < MAX_GATEWAY_FRAME_BYTES);
    assert!(multibyte.len() > MAX_GATEWAY_FRAME_BYTES);
    assert_eq!(
        decode_client_frame(&multibyte),
        Err(GatewayProtocolError::FrameTooLarge)
    );
}

fn padded_hello(target_bytes: usize) -> String {
    let prefix = r#"{"type":"hello","protocolVersion":3,"clientVersion":""#;
    let suffix = r#"","capabilities":[]}"#;
    assert!(prefix.len() + suffix.len() <= target_bytes);
    format!(
        "{prefix}{}{suffix}",
        "a".repeat(target_bytes - prefix.len() - suffix.len())
    )
}

fn padded_response(target_bytes: usize) -> String {
    let prefix = r#"{"type":"response","requestId":"00000000-0000-4000-8000-000000000003","generation":3,"result":{"kind":"get_device_info","schemaVersion":2,"deviceName":""#;
    let suffix = r#"","clientVersion":"1.4.0","capabilities":[]}}"#;
    assert!(prefix.len() + suffix.len() <= target_bytes);
    format!(
        "{prefix}{}{suffix}",
        "a".repeat(target_bytes - prefix.len() - suffix.len())
    )
}

#[test]
fn frame_directions_are_not_interchangeable() {
    assert!(matches!(
        decode_client_frame(&fixture("welcome-v5.json")),
        Err(GatewayProtocolError::Malformed)
    ));
    assert!(matches!(
        decode_server_frame(&fixture("hello-v5.json")),
        Err(GatewayProtocolError::Malformed)
    ));

    let client = decode_client_frame(&fixture("hello-v5.json")).expect("hello");
    assert!(matches!(client, GatewayClientFrameV5::Hello(_)));
    let server = decode_server_frame(&fixture("welcome-v5.json")).expect("welcome");
    assert!(matches!(server, GatewayServerFrameV5::Welcome(_)));
}
