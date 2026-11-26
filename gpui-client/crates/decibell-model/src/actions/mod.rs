//! What the UI asks for: store updates plus the core call, as the renderer's handlers
//! do them (`handleLogin`, `openServer`, `refetchRoster`, …). Plain functions over
//! `&mut App`; each names the renderer code it ports.

pub mod auth;
pub mod servers;
pub mod settings;
