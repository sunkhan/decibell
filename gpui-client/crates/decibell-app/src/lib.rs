//! The Decibell desktop client as a library: `main.rs` only parses the command line and calls
//! [`boot::run`]. `decibell-capture` (feature `capture`) builds the same [`root::Root`] in a
//! headless window.
//!
//! - [`args`]: the command line.
//! - [`boot`]: GPUI app start-up, the main window, the self-test run.
//! - [`logging`]: `log` records to stderr (`DECIBELL_LOG`).
//! - [`window`]: the main window's options.
//! - [`root`]: the root view: window frame, title bar, content.
//! - [`registry`]: every screen and self-test of the workspace.
//! - [`selftests`]: the app's own self-tests.

pub mod args;
pub mod boot;
pub mod logging;
pub mod registry;
pub mod root;
pub mod selftests;
pub mod window;
