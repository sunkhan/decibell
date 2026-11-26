//! `#[napi]` command surface — what the JS side reaches via
//! `invoke('method_name', args)`. Each command is a one-line wrapper over
//! the function of the same name in `decibell_core::api::<module>`, which
//! holds the body and the argument / result structs (`#[napi(object)]`
//! through the core's `napi` feature). Only structs with a JS `Buffer` field
//! are defined here, with the conversion. Doc comments stay on the wrappers:
//! napi copies them into index.d.ts.
//!
//! napi resolves a struct's JS name (`E2eeStatus` → `E2EeStatus`: it
//! PascalCases, splitting at digits) only while compiling the crate that
//! defines the struct. A wrapper here that names a core struct whose JS name
//! differs from its Rust name gets `ts_return_type` / `ts_args_type`, or its
//! signature in index.d.ts points at a type that doesn't exist (e2ee.rs).

pub mod auth;
pub mod call;
pub mod channels;
pub mod community;
pub mod devices;
pub mod dm;
pub mod e2ee;
pub mod friends;
pub mod hotkeys;
pub mod messaging;
pub mod presence;
pub mod servers;
pub mod settings;
pub mod stream_audio;
pub mod streaming;
pub mod voice;
