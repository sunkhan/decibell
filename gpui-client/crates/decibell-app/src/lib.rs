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
//! - [`updater`]: the updater (`decibell-release`) driven from the app: Settings → About, the
//!   one-shot check timer, Restart; [`crash`]: crash reports started and fed the names to scrub.
//! - [`video`]: the video crate wired to the model and the core.
//!
//! Development builds only (feature `dev`, the default; never the `ship` build):
//! - [`selftests`]: the app's own self-tests; [`joins`]: those of what the app joins between
//!   crates (`registry::join_features`).
//! - [`render_cost`]: the self-test that measures what a change costs in views rendered.
//! - [`software_codecs`]: the self-test that the software encoder and decoders are in the binary.
//! - [`copy_image`]: the self-test of "Copy image" through the system clipboards.

pub mod app;
pub mod args;
pub mod audio;
pub mod boot;
#[cfg(feature = "dev")]
pub mod copy_image;
pub mod crash;
#[cfg(feature = "dev-local")]
pub mod dev;
pub mod hotkeys;
#[cfg(feature = "dev")]
pub mod joins;
pub mod logging;
pub mod registry;
#[cfg(feature = "dev")]
pub mod render_cost;
pub mod root;
#[cfg(feature = "dev")]
pub mod selftests;
#[cfg(feature = "dev")]
pub mod software_codecs;
pub mod updater;
pub mod video;
pub mod window;

// The shipped build has none of the development features, is optimised, and never honours
// `DECIBELL_CENTRAL` (the core's `local-central`, which every development path turns on through
// the model's feature of that name; `gpui-client/packaging/check-ship.sh` also looks for the
// variable's name in the binary).
#[cfg(all(feature = "ship", any(feature = "dev", feature = "dev-local", feature = "capture")))]
compile_error!("decibell-app: `ship` excludes `dev`, `dev-local` and `capture` (build it with --no-default-features --features ship)");
#[cfg(all(feature = "ship", debug_assertions))]
compile_error!("decibell-app: the ship build is a release build (--release)");
#[cfg(feature = "ship")]
const _: () = assert!(
    !decibell_model::core::DEVELOPMENT_BUILD,
    "decibell-app: the ship build must not honour DECIBELL_CENTRAL (decibell-model's local-central is on)"
);

/// The client's version: the repository's one version site (`electron-client/package.json`).
pub const VERSION: &str = decibell_release::VERSION;
