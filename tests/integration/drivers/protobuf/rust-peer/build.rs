use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest
        .join("../../../../..")
        .canonicalize()
        .expect("repository root");
    let proto_dir = root.join("proto/v1");
    let proto = proto_dir.join("whalerein.proto");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let descriptor = out.join("whalerein_descriptor.bin");

    println!("cargo:rerun-if-changed={}", proto.display());
    let mut config = prost_build::Config::new();
    config.enable_type_names();
    // Golden vectors use deterministic map order; protobuf map semantics are unordered.
    config.btree_map(["."]);
    config.file_descriptor_set_path(&descriptor);
    config
        .compile_protos(&[proto.to_str().expect("utf-8 proto path")], &[proto_dir.to_str().expect("utf-8 proto dir")])
        .expect("compile protobuf");

    let bytes = std::fs::read(&descriptor).expect("read descriptor");
    pbjson_build::Builder::new()
        .btree_map(["."])
        .register_descriptors(&bytes)
        .expect("register descriptors")
        .build(&[".whalerein.v1"])
        .expect("generate serde implementations");
}
