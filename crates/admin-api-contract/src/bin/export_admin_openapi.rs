use relay_admin_api::AdminApiDoc;
use utoipa::OpenApi as _;

fn main() {
    let mut document = AdminApiDoc::openapi();
    document.openapi = utoipa::openapi::OpenApiVersion::Version31;
    let mut value = serde_json::to_value(document).expect("Admin API v2 OpenAPI must serialize");
    normalize_json_integer_formats(&mut value);
    let json = serde_json::to_string_pretty(&value).expect("Admin API v2 OpenAPI must serialize");
    println!("{json}");
}

/// JavaScript represents JSON integers as numbers. Removing the 64-bit
/// storage hint prevents generators from coercing millisecond timestamps into
/// bigint values that do not match `JSON.parse` or the live Relay response.
fn normalize_json_integer_formats(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            let integer = object.get("type").is_some_and(|schema_type| {
                schema_type.as_str() == Some("integer")
                    || schema_type.as_array().is_some_and(|types| {
                        types.iter().any(|value| value.as_str() == Some("integer"))
                    })
            });
            if integer
                && object
                    .get("format")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|format| matches!(format, "int64" | "uint64"))
            {
                object.remove("format");
            }
            for child in object.values_mut() {
                normalize_json_integer_formats(child);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                normalize_json_integer_formats(child);
            }
        }
        _ => {}
    }
}
