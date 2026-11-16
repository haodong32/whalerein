//! Cross-language compatibility and checked-in contract artifacts.
//!
//! These tests are the enforcement point for `proto/v1/WIRE.md`,
//! `proto/v1/error_codes.json`, `proto/v1/compat/baseline.json`,
//! `proto/v1/compat/vectors.json` and `proto/v1/compat/surface.json`. They run
//! with `./repo test --project proto`, so a schema or document edit that is not
//! reflected in the golden artifacts fails before `./repo proto` can refresh a
//! generated binding.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use prost::Message;
use serde_json::{json, Value};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .expect("repository root")
}

fn read(relative: &str) -> String {
    std::fs::read_to_string(repository_root().join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn read_json(relative: &str) -> Value {
    serde_json::from_str(&read(relative))
        .unwrap_or_else(|error| panic!("parse {relative}: {error}"))
}

fn vectors() -> Vec<Value> {
    let data = read_json("proto/v1/compat/vectors.json");
    assert_eq!(
        data["schema"].as_str(),
        Some("whalerein/proto-compat-vectors/v1")
    );
    data["vectors"].as_array().expect("vectors").clone()
}

fn vector(id: &str) -> Value {
    vectors()
        .into_iter()
        .find(|entry| entry["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("missing vector {id}"))
}

fn decodes_in_language(entry: &Value, language: &str) -> bool {
    entry["languages"]
        .as_array()
        .expect("languages")
        .iter()
        .any(|value| value.as_str() == Some(language))
}

#[test]
fn new_writer_and_old_reader_ignore_unknown_json_fields() {
    let entry = vector("json-unknown-field-is-ignored");
    assert!(decodes_in_language(&entry, "rust"));
    let request: whalerein_proto::HostRequest =
        serde_json::from_value(entry["json"].clone()).expect("unknown fields are additive");
    assert_eq!(request.request_id, "req-1");
    assert_eq!(request.kind, whalerein_proto::RequestKind::Query as i32);
    assert_eq!(request.protocol_version, 1);
}

#[test]
fn old_writer_and_new_reader_fill_defaults_for_absent_fields() {
    let entry = vector("json-old-reader-accepts-missing-new-fields");
    let result: whalerein_proto::HostResult =
        serde_json::from_value(entry["json"].clone()).expect("absent fields mean defaults");
    assert_eq!(result.request_id, "req-2");
    assert_eq!(
        result.status,
        whalerein_proto::OnlineStatus::Succeeded as i32
    );
    assert_eq!(result.protocol_version, 0);
    assert!(result.result_body.is_empty());
}

#[test]
fn binary_readers_skip_unknown_tags() {
    let entry = vector("binary-unknown-tag-is-ignored");
    let bytes = hex_decode(entry["hex"].as_str().expect("hex"));
    let request =
        whalerein_proto::HostRequest::decode(bytes.as_slice()).expect("unknown tags are additive");
    assert_eq!(request.request_id, "req-1");
    assert_eq!(request.protocol_version, 0);
}

#[test]
fn unknown_enum_names_and_numbers_are_explicit_errors() {
    for id in [
        "json-unknown-enum-name-is-explicit",
        "json-unknown-enum-number-needs-validation",
    ] {
        let entry = vector(id);
        let error = serde_json::from_value::<whalerein_proto::HostRequest>(entry["json"].clone())
            .expect_err("an unknown enum must never fall back to the zero value");
        let message = error.to_string();
        assert!(
            message.contains("unknown variant") || message.contains("invalid value"),
            "{id}: {message}"
        );
    }
    let entry = vector("json-unknown-enum-number-needs-validation");
    assert_eq!(
        entry["errorCode"].as_str(),
        Some("protocol_version_incompatible")
    );
}

#[test]
fn missing_or_unknown_oneof_variant_is_never_an_empty_success() {
    for id in [
        "request-payload-missing-oneof",
        "request-payload-unknown-oneof-variant",
    ] {
        let entry = vector(id);
        let payload: whalerein_proto::RequestPayload =
            serde_json::from_value(entry["json"].clone()).expect("unknown field is skipped");
        let error = whalerein_proto::require_request_payload(payload)
            .expect_err("a missing oneof must be rejected");
        assert_eq!(error.code(), whalerein_proto::ErrorCode::InvalidArgument);
        assert!(error.message().contains("RequestPayload"));
        assert!(error.message().contains("protocol version"));
    }
}

#[test]
fn int64_json_strings_stay_exact_near_two_to_the_53() {
    let entry = vector("int64-near-two-to-the-53");
    let request: whalerein_proto::HostRequest =
        serde_json::from_value(entry["json"].clone()).expect("decode");
    assert_eq!(request.expires_at_unix_ms, 9_007_199_254_740_993);
    let encoded = serde_json::to_value(&request).expect("encode");
    assert_eq!(
        encoded["expiresAtUnixMs"].as_str(),
        Some("9007199254740993")
    );
}

#[test]
fn zero_valued_fields_are_omitted_from_json() {
    let entry = vector("zero-values-are-omitted");
    let result: whalerein_proto::HostResult =
        serde_json::from_value(entry["json"].clone()).expect("decode");
    let encoded = serde_json::to_value(&result).expect("encode");
    assert_eq!(encoded, json!({"requestId": "req-6"}));
    assert!(encoded.get("status").is_none());
    assert!(encoded.get("protocolVersion").is_none());
}

#[test]
fn error_code_golden_table_matches_schema_core_names_and_wire_document() {
    let table = read_json("proto/v1/error_codes.json");
    assert_eq!(table["schema"].as_str(), Some("whalerein/error-codes/v1"));
    let codes = table["codes"].as_array().expect("codes");
    assert_eq!(codes.len(), whalerein_proto::ALL_ERROR_CODES.len());

    let documented = wire_error_table();
    let actions = [
        "none",
        "fix_input",
        "reauth",
        "refresh",
        "retry",
        "upgrade",
        "resync",
        "restart_pairing",
        "review_pairing",
    ];
    let mut exits = BTreeMap::new();
    for (position, code) in whalerein_proto::ALL_ERROR_CODES.iter().enumerate() {
        let entry = &codes[position];
        let name = whalerein_proto::error_code_name(*code);
        assert_eq!(entry["name"].as_str(), Some(name));
        assert_eq!(entry["number"].as_i64(), Some(*code as i64));
        let exit = entry["exitCode"].as_i64().expect("exitCode");
        assert!((1..=9).contains(&exit), "{name} exit {exit}");
        let action = entry["action"].as_str().expect("action");
        assert!(actions.contains(&action), "{name} action {action}");
        assert!(
            entry["meaning"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "{name} needs a meaning"
        );
        exits.insert(name, exit as u8);
        let documented = documented
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from the WIRE.md table"));
        assert_eq!(documented.0, exit as u8, "{name} exit differs from WIRE.md");
        assert_eq!(documented.1, action, "{name} action differs from WIRE.md");
    }
    assert_eq!(
        exits.get("protocol_version_incompatible"),
        Some(&9),
        "iOS must map a version mismatch to an upgrade prompt, not retry"
    );
    assert_eq!(exits.get("unavailable"), Some(&5));
}

/// Parse the `ErrorCode -> exit -> action` table from WIRE.md.
fn wire_error_table() -> BTreeMap<String, (u8, String)> {
    let wire = read("proto/v1/WIRE.md");
    let mut rows = BTreeMap::new();
    let mut in_section = false;
    for line in wire.lines() {
        if line.starts_with("## CLI exit codes") {
            in_section = true;
            continue;
        }
        if in_section && line.starts_with("## ") {
            break;
        }
        if !in_section || !line.starts_with("| `") {
            continue;
        }
        let columns: Vec<&str> = line.split('|').map(str::trim).collect();
        if columns.len() < 4 {
            continue;
        }
        let name = columns[1].trim_matches('`').to_owned();
        if name == "ErrorCode" {
            continue;
        }
        let exit: u8 = columns[2]
            .parse()
            .unwrap_or_else(|_| panic!("bad exit for {name}: {}", columns[2]));
        let action = columns[3].trim_matches('`').to_owned();
        rows.insert(name, (exit, action));
    }
    assert!(!rows.is_empty(), "WIRE.md error table not found");
    rows
}

#[test]
fn swift_surface_classifies_every_wire_error_code() {
    let surface = read_json("proto/v1/bindings/swift/ios-surface.json");
    assert_eq!(surface["schema"].as_str(), Some("whalerein/ios-surface/v1"));
    let entries = surface["errorCodes"].as_object().expect("errorCodes");
    for code in whalerein_proto::ALL_ERROR_CODES {
        let name = whalerein_proto::error_code_name(code);
        let entry = entries
            .get(name)
            .unwrap_or_else(|| panic!("{name} has no Swift classification"));
        assert!(
            entry["swiftCase"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "{name} needs a Swift case"
        );
        let status = entry["status"].as_str().expect("status");
        assert!(
            status == "mapped" || status == "follow-up",
            "{name} has status {status}"
        );
    }
    assert_eq!(entries.len(), whalerein_proto::ALL_ERROR_CODES.len());
    // A version mismatch must be an upgrade action in the wire contract, and
    // iOS now has distinct non-retryable upgrade cases for both.
    assert_eq!(
        entries["protocol_version_incompatible"]["status"].as_str(),
        Some("mapped")
    );
    assert_eq!(
        entries["protocol_version_incompatible"]["swiftCase"].as_str(),
        Some("ProtocolVersionIncompatible")
    );
    assert_eq!(
        entries["unsupported_capability"]["status"].as_str(),
        Some("mapped")
    );
    assert_eq!(
        entries["unsupported_capability"]["swiftCase"].as_str(),
        Some("UnsupportedCapability")
    );
}

#[test]
fn schema_baseline_rejects_removals_reuse_and_type_changes() {
    let baseline = read_json("proto/v1/compat/baseline.json");
    let current = descriptor_schema();
    let baseline = Baseline::from_value(&baseline);
    let violations = baseline.violations(&current);
    assert!(
        violations.is_empty(),
        "schema changed incompatibly: {violations:#?}\n\
         Additive changes need ./repo proto && --write; a breaking change needs \
         --update-baseline --rationale and a protocol version bump."
    );
    // The baseline must also be complete enough to catch the next removal.
    assert_eq!(baseline.messages.len(), current.messages.len());
    assert_eq!(baseline.enums.len(), current.enums.len());
}

#[test]
fn contract_surface_snapshot_matches_schema_wire_and_harness_contract() {
    let surface = read_json("proto/v1/compat/surface.json");
    assert_eq!(
        surface["schema"].as_str(),
        Some("whalerein/proto-surface/v1")
    );
    let schema = descriptor_schema();
    let message_names: Vec<String> = schema
        .messages
        .keys()
        .map(|name| format!("whalerein.v1.{name}"))
        .collect();
    let enum_names: Vec<String> = schema
        .enums
        .keys()
        .map(|name| format!("whalerein.v1.{name}"))
        .collect();
    assert_eq!(
        surface["messages"],
        serde_json::to_value(&message_names).unwrap()
    );
    assert_eq!(surface["enums"], serde_json::to_value(&enum_names).unwrap());

    let names: Vec<String> = whalerein_proto::ALL_ERROR_CODES
        .iter()
        .map(|code| whalerein_proto::error_code_name(*code).to_owned())
        .collect();
    assert_eq!(surface["errorCodes"], serde_json::to_value(&names).unwrap());
    assert_eq!(surface["zeroValuePolicy"].as_str(), Some("omitted"));
    assert_eq!(
        surface["int64JsonEncoding"].as_str(),
        Some("decimal-string")
    );
    assert_eq!(
        surface["timeUnit"].as_str(),
        Some("unix-epoch-milliseconds")
    );
    assert_eq!(surface["protocolWindow"], json!({"min": 1, "max": 1}));

    let wire = read("proto/v1/WIRE.md");
    assert_eq!(wire_routes(&wire), string_list(&surface["httpRoutes"]));
    assert_eq!(wire_topics(&wire), string_list(&surface["mqttTopics"]));
    assert!(
        wire.contains("/v1/requests/{request_id}/heartbeat"),
        "WIRE.md must document the CLI heartbeat route"
    );

    let harness = read("docs/architecture/harness-contract.md");
    let aliases = surface["harnessTypes"].as_object().expect("harnessTypes");
    for (alias, canonical) in aliases {
        let canonical = canonical.as_str().expect("canonical type");
        assert!(
            schema.messages.contains_key(canonical),
            "harness alias {alias} points at an unknown type {canonical}"
        );
        assert!(
            harness.contains(&format!("`{alias}`")),
            "harness contract no longer mentions `{alias}`; update the snapshot or the doc"
        );
    }
    assert!(harness.contains("RuntimeState"));
    assert!(harness.contains("ApprovalState"));
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item.as_str().expect("string").to_owned())
        .collect()
}

fn wire_routes(wire: &str) -> Vec<String> {
    let block = code_block(wire, "## HTTP surface", "http");
    block
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut parts = line.split_whitespace();
            format!(
                "{} {}",
                parts.next().expect("method"),
                parts.next().expect("path")
            )
        })
        .collect()
}

fn wire_topics(wire: &str) -> Vec<String> {
    let block = code_block(wire, "## Topics", "text");
    block
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn code_block(document: &str, heading: &str, language: &str) -> String {
    let start = document
        .find(heading)
        .unwrap_or_else(|| panic!("missing heading {heading}"));
    let fence = format!("```{language}");
    let start = document[start..]
        .find(&fence)
        .map(|offset| start + offset + fence.len())
        .unwrap_or_else(|| panic!("missing {language} block under {heading}"));
    let end = document[start..]
        .find("```")
        .map(|offset| start + offset)
        .expect("unterminated code block");
    document[start..end].trim_start_matches('\n').to_owned()
}

fn hex_decode(text: &str) -> Vec<u8> {
    assert!(text.len() % 2 == 0, "hex length");
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
        .collect()
}

#[derive(Debug)]
struct FieldSchema {
    name: String,
    number: i32,
    type_name: String,
    json_name: String,
    oneof: Option<String>,
    repeated: bool,
    optional: bool,
}

#[derive(Debug, Default)]
struct Schema {
    messages: BTreeMap<String, Vec<FieldSchema>>,
    enums: BTreeMap<String, Vec<(String, i32)>>,
}

struct Baseline {
    messages: BTreeMap<String, Vec<FieldSchema>>,
    enums: BTreeMap<String, Vec<(String, i32)>>,
}

impl Baseline {
    fn from_value(value: &Value) -> Self {
        Self {
            messages: parse_messages(&value["messages"]),
            enums: parse_enums(&value["enums"]),
        }
    }

    fn violations(&self, current: &Schema) -> Vec<String> {
        let mut violations = Vec::new();
        for (message, fields) in &self.messages {
            let Some(current_fields) = current.messages.get(message) else {
                violations.push(format!("removed message {message}"));
                continue;
            };
            let by_number: BTreeMap<i32, &FieldSchema> = current_fields
                .iter()
                .map(|field| (field.number, field))
                .collect();
            let by_name: BTreeMap<&str, &FieldSchema> = current_fields
                .iter()
                .map(|field| (field.name.as_str(), field))
                .collect();
            for field in fields {
                let label = format!("{message}.{}", field.name);
                let Some(current_field) = by_number.get(&field.number) else {
                    if let Some(moved) = by_name.get(field.name.as_str()) {
                        violations.push(format!(
                            "renumbered field {label} from {} to {}",
                            field.number, moved.number
                        ));
                    } else {
                        violations.push(format!("removed field {label} (tag {})", field.number));
                    }
                    continue;
                };
                if current_field.name != field.name {
                    violations.push(format!(
                        "tag {} of {message} was reused from {} to {}",
                        field.number, field.name, current_field.name
                    ));
                }
                if current_field.type_name != field.type_name {
                    violations.push(format!(
                        "changed type of {label}: {} -> {}",
                        field.type_name, current_field.type_name
                    ));
                }
                if current_field.json_name != field.json_name {
                    violations.push(format!(
                        "changed JSON name of {label}: {} -> {}",
                        field.json_name, current_field.json_name
                    ));
                }
                if current_field.oneof != field.oneof {
                    violations.push(format!(
                        "changed oneof membership of {label}: {:?} -> {:?}",
                        field.oneof, current_field.oneof
                    ));
                }
                if current_field.repeated != field.repeated {
                    violations.push(format!("changed repeated label of {label}"));
                }
                if current_field.optional != field.optional {
                    violations.push(format!("changed optional label of {label}"));
                }
            }
        }
        for (enumeration, values) in &self.enums {
            let Some(current_values) = current.enums.get(enumeration) else {
                violations.push(format!("removed enum {enumeration}"));
                continue;
            };
            let by_number: BTreeMap<i32, &(String, i32)> = current_values
                .iter()
                .map(|value| (value.1, value))
                .collect();
            let by_name: BTreeMap<&str, &(String, i32)> = current_values
                .iter()
                .map(|value| (value.0.as_str(), value))
                .collect();
            for value in values {
                let label = format!("{enumeration}.{}", value.0);
                let Some(current_value) = by_number.get(&value.1) else {
                    if let Some(moved) = by_name.get(value.0.as_str()) {
                        violations.push(format!(
                            "renumbered enum value {label} from {} to {}",
                            value.1, moved.1
                        ));
                    } else {
                        violations.push(format!("removed enum value {label} ({})", value.1));
                    }
                    continue;
                };
                if current_value.0 != value.0 {
                    violations.push(format!(
                        "enum number {} of {enumeration} was reused from {} to {}",
                        value.1, value.0, current_value.0
                    ));
                }
            }
        }
        violations
    }
}

fn parse_messages(value: &Value) -> BTreeMap<String, Vec<FieldSchema>> {
    let mut messages = BTreeMap::new();
    for (name, message) in value.as_object().expect("messages") {
        let fields = message["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .map(|field| FieldSchema {
                name: field["name"].as_str().expect("name").to_owned(),
                number: field["number"].as_i64().expect("number") as i32,
                type_name: field["type_name"].as_str().unwrap_or("").to_owned(),
                json_name: field["json_name"].as_str().expect("json_name").to_owned(),
                oneof: field["oneof"].as_str().map(str::to_owned),
                repeated: field["repeated"].as_bool().unwrap_or(false),
                optional: field["optional"].as_bool().unwrap_or(false),
            })
            .collect();
        messages.insert(name.clone(), fields);
    }
    messages
}

fn parse_enums(value: &Value) -> BTreeMap<String, Vec<(String, i32)>> {
    let mut enums = BTreeMap::new();
    for (name, values) in value.as_object().expect("enums") {
        let values = values
            .as_array()
            .expect("values")
            .iter()
            .map(|value| {
                (
                    value["name"].as_str().expect("name").to_owned(),
                    value["number"].as_i64().expect("number") as i32,
                )
            })
            .collect();
        enums.insert(name.clone(), values);
    }
    enums
}

fn descriptor_schema() -> Schema {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/whalerein_descriptor.bin"));
    let set = prost_types::FileDescriptorSet::decode(bytes.as_slice()).expect("descriptor");
    let mut schema = Schema::default();
    for file in &set.file {
        for message in &file.message_type {
            let name = message.name.clone().expect("message name");
            let oneofs: Vec<String> = message
                .oneof_decl
                .iter()
                .map(|oneof| oneof.name.clone().unwrap_or_default())
                .collect();
            let fields = message
                .field
                .iter()
                .map(|field| {
                    let optional = field.proto3_optional.unwrap_or(false);
                    let oneof = if optional {
                        None
                    } else {
                        field
                            .oneof_index
                            .and_then(|index| oneofs.get(index as usize))
                            .cloned()
                    };
                    FieldSchema {
                        name: field.name.clone().unwrap_or_default(),
                        number: field.number.unwrap_or(0),
                        type_name: field
                            .type_name
                            .as_deref()
                            .unwrap_or("")
                            .trim_start_matches('.')
                            .to_owned(),
                        json_name: field.json_name.clone().unwrap_or_default(),
                        oneof,
                        repeated: field.label
                            == Some(prost_types::field_descriptor_proto::Label::Repeated as i32),
                        optional,
                    }
                })
                .collect();
            schema.messages.insert(name, fields);
        }
        for enumeration in &file.enum_type {
            let name = enumeration.name.clone().expect("enum name");
            let values = enumeration
                .value
                .iter()
                .map(|value| {
                    (
                        value.name.clone().unwrap_or_default(),
                        value.number.unwrap_or(0),
                    )
                })
                .collect();
            schema.enums.insert(name, values);
        }
    }
    assert!(!schema.messages.is_empty(), "descriptor has no messages");
    schema
}
