//! The command API: every operation a client asks of the core, as plain Rust
//! (step 3 of the core split, docs/superpowers/specs/2026-11-07-qt-native-
//! client-design.md §6). One module per area; the Electron addon's `#[napi]`
//! commands (electron-client/native/src/commands/) are one-line wrappers over
//! the functions here, with the same names and argument structs.
//!
//! Start the core first ([`lifecycle::init`]); then:
//!
//! - Every `pub async fn` returns `Result<T, CoreError>` and runs its body
//!   through [`crate::rt::run`], so it can be awaited on any executor, inside a
//!   tokio runtime or not.
//! - Every `pub fn` (sync) neither blocks nor needs a runtime context.
//! - Most calls only queue a request: they return once it is sent, and the
//!   answer arrives later as an event through the client's
//!   [`crate::events::EventSink`]. Each function's doc says which event.
//! - Argument and result structs carry `#[napi(object)]` only with the `napi`
//!   feature (the addon's); their field names are the addon's JS names in
//!   snake_case.
//!
//! The `pub(crate)` functions are the core's own call-outs (central's and the
//! community's routers run them on the runtime).

pub mod auth;
pub mod call;
pub mod channels;
pub mod community;
pub mod devices;
pub mod dm;
pub mod e2ee;
mod error;
pub mod friends;
pub mod hotkeys;
pub mod lifecycle;
pub mod messaging;
pub mod presence;
pub mod servers;
pub mod settings;
pub mod stream_audio;
pub mod streaming;
pub mod voice;

pub use error::{CoreError, ErrorKind};

#[cfg(test)]
mod tests;
