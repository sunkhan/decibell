fn main() {
    tauri_build::build();
    // prost-build is supposed to emit cargo:rerun-if-changed for its inputs,
    // but that's been unreliable in practice: editing messages.proto without
    // also touching build.rs has let cargo reuse a stale OUT_DIR/chatproj.rs
    // on release builds, producing "variant not found" errors. State it
    // ourselves so incremental rebuilds are deterministic when the proto
    // changes.
    println!("cargo:rerun-if-changed=../../proto/messages.proto");
    println!("cargo:rerun-if-changed=build.rs");
    prost_build::compile_protos(
        &["../../proto/messages.proto"],
        &["../../proto/"],
    )
    .expect("Failed to compile protobuf");
}
