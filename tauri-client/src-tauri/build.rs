fn main() {
    tauri_build::build();
    prost_build::compile_protos(
        &["../../proto/messages.proto"],
        &["../../proto/"],
    )
    .expect("Failed to compile protobuf");
}
