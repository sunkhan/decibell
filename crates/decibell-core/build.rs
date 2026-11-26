// prost-build compiles the wire-protocol proto from the repo root —
// the same proto/messages.proto the C++ servers and the tauri-client
// reference both consume. Source of truth for all message types.
// (src/net/proto.rs includes the result from OUT_DIR.)
//
// We declare rerun-if-changed explicitly: prost-build *should* emit
// these itself, but in practice that's been unreliable on incremental
// release builds (stale OUT_DIR/chatproj.rs surfaces as "variant not
// found" errors). State it ourselves and the rebuild is deterministic.
fn main() {
    println!("cargo:rerun-if-changed=../../proto/messages.proto");
    println!("cargo:rerun-if-changed=build.rs");
    prost_build::compile_protos(&["../../proto/messages.proto"], &["../../proto/"])
        .expect("Failed to compile protobuf");
}
