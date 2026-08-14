fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc");
    let mut config = prost_build::Config::new();
    config.protoc_executable(protoc);
    config
        .compile_protos(
            &[
                "../../protocol/kide/worker.proto",
                "../../protocol/kide/artifact.proto",
            ],
            &["../../protocol"],
        )
        .expect("generates KIDE protobuf protocols");
    println!("cargo:rerun-if-changed=../../protocol/kide/worker.proto");
    println!("cargo:rerun-if-changed=../../protocol/kide/artifact.proto");
}
