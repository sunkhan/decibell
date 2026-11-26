//! The Decibell desktop client as a library: `main.rs` only parses the command line and calls
//! [`boot::run`]. `decibell-capture` (feature `capture`) builds the same [`root::Root`] in a
//! headless window.
//!
//! - [`args`]: the command line.
//! - [`boot`]: GPUI app start-up, the main window, the self-test run.
//! - [`logging`]: `log` records to stderr (`DECIBELL_LOG`).
//! - [`window`]: the main window's options.
//! - [`root`]: the root view: window frame, title bar, content.
//! - [`hotkeys`]: the focused hotkey fallback the root feeds, and its self-test.
//! - [`registry`]: every screen and self-test of the workspace.
//! - [`selftests`]: the app's own self-tests; [`joins`]: those of what the app joins between
//!   crates (`registry::join_features`).
//! - [`render_cost`]: the self-test that measures what a change costs in views rendered.
//! - [`software_codecs`]: the self-test that the software encoder and decoders are in the binary.
//! - [`video`]: the video crate wired to the model and the core.
//! - [`copy_image`]: the self-test of "Copy image" through the system clipboards.

pub mod app;
pub mod args;
pub mod audio;
pub mod boot;
pub mod joins;
pub mod copy_image;
#[cfg(feature = "dev-local")]
pub mod dev;
pub mod hotkeys;
pub mod logging;
pub mod registry;
pub mod render_cost;
pub mod root;
pub mod selftests;
pub mod software_codecs;
pub mod video;
pub mod window;
