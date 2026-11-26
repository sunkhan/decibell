#![deny(clippy::all)]

//! The shared core of the Decibell clients (docs/superpowers/specs/2026-11-07-
//! qt-native-client-design.md §6): every server connection, the protocol state
//! machines, voice / UDP media, native capture + encode, all E2EE, global
//! hotkeys, settings. It holds no UI binding: a client installs an
//! [`events::EventSink`] once ([`events::install_sink`]) and calls in.
//!
//! A client starts it with [`api::lifecycle::init`] (its event sink and the
//! tokio runtime the core runs on) and then calls the command API, [`api`]:
//! plain Rust functions, async ones awaitable on any executor ([`rt`]). The
//! Electron addon (`electron-client/native`, napi) is the first client: its
//! `init` builds a sink over three ThreadsafeFunctions and its `#[napi]`
//! commands are one-line wrappers over `api`. State is process-global
//! ([`state::init`] once, then [`state::set_boot`]); the crate never builds a
//! tokio runtime: it runs on the host's.

pub mod api;
pub mod config;
pub mod e2ee;
pub mod events;
pub mod hotkeys;
pub mod media;
pub mod net;
pub mod presence;
pub mod rt;
pub mod state;
pub mod system_idle;
