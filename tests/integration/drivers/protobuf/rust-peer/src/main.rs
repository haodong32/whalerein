#![allow(dead_code)]

//! Rust side of the WhaleRein protobuf compatibility fixture.
//!
//! Compiles the same `proto/v1/whalerein.proto` with prost + pbjson and checks
//! that every fixture object serializes to the Node-generated binary and
//! round-trips to the same proto3 JSON.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::ExitCode;

mod proto {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/whalerein.v1.rs"));
    include!(concat!(env!("OUT_DIR"), "/whalerein.v1.serde.rs"));
}

fn check<T>(name: &str, json: &Value, expected_hex: &str) -> Result<(), String>
where
    T: prost::Message + Default + Serialize + DeserializeOwned,
{
    let value: T =
        serde_json::from_value(json.clone()).map_err(|err| format!("{name} JSON decode: {err}"))?;
    let encoded = value.encode_to_vec();
    if hex::encode(&encoded) != expected_hex {
        return Err(format!(
            "{name} binary differs: rust={} fixture={expected_hex}",
            hex::encode(&encoded)
        ));
    }
    let decoded = T::decode(encoded.as_slice()).map_err(|err| format!("{name} binary decode: {err}"))?;
    let roundtrip = serde_json::to_value(&decoded).map_err(|err| format!("{name} JSON encode: {err}"))?;
    if &roundtrip != json {
        return Err(format!(
            "{name} JSON differs:\nrust={}\nfixture={}",
            serde_json::to_string(&roundtrip).unwrap_or_default(),
            serde_json::to_string(json).unwrap_or_default()
        ));
    }
    Ok(())
}

fn run() -> Result<usize, String> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        "../../../fixtures/proto/messages.json".to_string()
    });
    let raw = std::fs::read_to_string(&path).map_err(|err| format!("read {path}: {err}"))?;
    let fixture: Value = serde_json::from_str(&raw).map_err(|err| format!("parse fixture: {err}"))?;
    if fixture.get("schema").and_then(Value::as_str) != Some("whalerein/proto-json/v1") {
        return Err("unexpected fixture schema".into());
    }
    let messages = fixture
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "fixture has no messages array".to_string())?;
    let mut checked = 0;
    for entry in messages {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "message has no name".to_string())?;
        let json = entry
            .get("json")
            .ok_or_else(|| format!("{name} has no json"))?;
        let hex = entry
            .get("hex")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{name} has no hex"))?;
        match name {
            "whalerein.v1.HostRequest" => check::<proto::HostRequest>(name, json, hex)?,
            "whalerein.v1.HostResult" => check::<proto::HostResult>(name, json, hex)?,
            "whalerein.v1.RequestState" => check::<proto::RequestState>(name, json, hex)?,
            "whalerein.v1.PairingClaimRequest" => {
                check::<proto::PairingClaimRequest>(name, json, hex)?
            }
            "whalerein.v1.PairingClaimResponse" => {
                check::<proto::PairingClaimResponse>(name, json, hex)?
            }
            "whalerein.v1.EncryptedFrame" => check::<proto::EncryptedFrame>(name, json, hex)?,
            "whalerein.v1.HostList" => check::<proto::HostList>(name, json, hex)?,
            "whalerein.v1.CapabilitySet" => check::<proto::CapabilitySet>(name, json, hex)?,
            "whalerein.v1.RevocationNotice" => check::<proto::RevocationNotice>(name, json, hex)?,
            "whalerein.v1.Approval" => check::<proto::Approval>(name, json, hex)?,
            "whalerein.v1.EventPayload" => check::<proto::EventPayload>(name,json,hex)?,
            "whalerein.v1.HistoryPage" => check::<proto::HistoryPage>(name,json,hex)?,
            "whalerein.v1.SessionState" => check::<proto::SessionState>(name,json,hex)?,
            "whalerein.v1.RequestPayload" => check::<proto::RequestPayload>(name,json,hex)?,
            "whalerein.v1.ResponsePayload" => check::<proto::ResponsePayload>(name,json,hex)?,
            "whalerein.v1.SourceCapabilities" => check::<proto::SourceCapabilities>(name,json,hex)?,
            other => return Err(format!("unsupported fixture message {other}")),
        }
        checked += 1;
    }
    Ok(checked)
}

fn main() -> ExitCode {
    match run() {
        Ok(checked) => {
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "peer": "rust-prost-pbjson",
                    "description": "Rust protobuf binary and JSON values match all fixtures",
                    "messages": checked,
                })
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("rust protobuf peer failed: {error}");
            ExitCode::FAILURE
        }
    }
}
