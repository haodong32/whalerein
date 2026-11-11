//! Generated Protobuf and proto3 JSON types for the WhaleRein v1 contract.

include!(concat!(env!("OUT_DIR"), "/whalerein.v1.rs"));
include!(concat!(env!("OUT_DIR"), "/whalerein.v1.serde.rs"));

/// Protocol version carried on every request and result envelope.
pub const PROTOCOL_VERSION: u32 = 1;

/// Version of the pairing and channel prologue strings.
pub const PROLOGUE_VERSION: &str = "whalerein/v1";
