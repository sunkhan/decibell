//! What every crate of the GPUI client registers into, and the instrumentation behind it.
//!
//! - [`Screen`]: a named view built from fixture state. `decibell-capture` renders screens to PNG
//!   files; `decibell --screen <name>` shows one in the real window.
//! - [`SelfTest`]: a scripted check that `decibell --selftest <name>|all` runs against the real
//!   app in its real window, driven through a [`TestCx`].
//! - [`perf`]: process start, the first presented frame, frame counters.
//! - [`probes`]: where interactive elements were laid out, for the self-tests that click them
//!   (off unless `DECIBELL_TEST_PROBES=1`).
//! - [`focus`]: whether the window counts as focused, and the self-tests' hold of it.
//!
//! A crate exposes `pub fn screens() -> Vec<Screen>` and `pub fn selftests() -> Vec<SelfTest>`;
//! `decibell-app`'s `registry` module lists the crates. No linker tricks: a screen or test that
//! isn't listed there doesn't exist. How to add one: `gpui-client/CONVENTIONS.md`.

pub mod focus;
pub mod perf;
pub mod probes;
mod screens;
mod selftest;

pub use screens::Screen;
pub use selftest::{Frames, SelfTest, TestCx, run_selftests};
