//! Generated Protobuf and proto3 JSON types for the WhaleRein v1 contract.

include!(concat!(env!("OUT_DIR"), "/whalerein.v1.rs"));
// pbjson-build 0.9 borrows field lists in its generated formatting calls.
#[allow(clippy::useless_borrows_in_formatting)]
mod serde_impls {
    use super::*;
    include!(concat!(env!("OUT_DIR"), "/whalerein.v1.serde.rs"));
}

/// Protocol version carried on every request and result envelope.
pub const PROTOCOL_VERSION: u32 = 1;

/// Version of the pairing and channel prologue strings.
pub const PROLOGUE_VERSION: &str = "whalerein/v1";
