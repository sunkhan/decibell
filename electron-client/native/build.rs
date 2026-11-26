// napi-build emits the linker flags Node native addons need. They are
// cdylib link args, so they must come from this crate's build script (the
// one that links the .node), not from crates/decibell-core's.
//
// The wire-protocol proto (prost-build) is compiled by decibell-core's
// build.rs now; this crate only re-exports its modules.
fn main() {
    napi_build::setup();
}
