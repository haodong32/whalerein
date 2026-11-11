fn main() {
    let proto = "../../whalerein.proto";
    println!("cargo:rerun-if-changed={proto}");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let descriptor_path = format!("{out_dir}/whalerein_descriptor.bin");

    let mut config = prost_build::Config::new();
    config.enable_type_names();
    config.file_descriptor_set_path(&descriptor_path);
    config
        .compile_protos(&[proto], &["../.."])
        .expect("compile whalerein.proto");

    let descriptor = std::fs::read(&descriptor_path).expect("read descriptor");
    pbjson_build::Builder::new()
        .register_descriptors(&descriptor)
        .expect("register descriptors")
        .build(&[".whalerein.v1"])
        .expect("generate serde impls");
}
