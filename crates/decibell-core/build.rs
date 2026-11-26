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
    napi_type_defs();
    prost_build::compile_protos(&["../../proto/messages.proto"], &["../../proto/"])
        .expect("Failed to compile protobuf");
}

/// With the `napi` feature, `api`'s structs carry `#[napi(object)]`, and
/// napi-derive writes their TypeScript definitions while THIS crate compiles,
/// into the file the napi CLI names in `TYPE_DEF_TMP_PATH` (a temp file the
/// CLI keeps between builds; each crate replaces only its own lines). A build
/// that reuses this crate's compiled output writes nothing, so:
///   - rebuild when that variable changes (a plain `cargo build` / `cargo test`
///     against `npx napi build`), as napi-build does for the addon itself;
///   - rebuild when the temp file was wiped (a reboot clears /tmp): a stamp
///     next to it, written once with an old mtime, goes missing with it.
/// Without these, an addon-only change after either would regenerate
/// index.d.ts without the core's interfaces.
fn napi_type_defs() {
    if std::env::var_os("CARGO_FEATURE_NAPI").is_none() {
        return;
    }
    println!("cargo:rerun-if-env-changed=TYPE_DEF_TMP_PATH");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_NAPI_RS_CLI_VERSION");
    let Some(type_defs) = std::env::var_os("TYPE_DEF_TMP_PATH") else {
        return;
    };
    let mut stamp = type_defs;
    stamp.push(".decibell-core.stamp");
    let stamp = std::path::PathBuf::from(stamp);
    if !stamp.exists() {
        // An mtime older than any build, so the stamp itself never reads as
        // changed: only its absence reruns this script.
        let written = std::fs::File::create(&stamp).and_then(|f| {
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
        });
        if let Err(e) = written {
            println!("cargo:warning=napi type-def stamp {}: {e}", stamp.display());
            return;
        }
    }
    println!("cargo:rerun-if-changed={}", stamp.display());
}
