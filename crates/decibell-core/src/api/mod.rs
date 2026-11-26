//! Client actions as plain Rust, for the bodies the core itself also runs:
//! the network routers (`net/central.rs`, `net/community.rs`) call these, so
//! they can't live in a UI binding's command layer. The Electron addon's
//! `#[napi]` commands wrap them.
//!
//! Step 3 of the core split (docs/superpowers/specs/2026-11-07-qt-native-
//! client-design.md §6) moves the remaining command bodies here, one module
//! per area, next to these.

pub mod auth;
pub mod presence;
pub mod servers;
pub mod voice;
