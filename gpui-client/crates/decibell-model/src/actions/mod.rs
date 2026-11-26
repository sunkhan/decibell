//! What the UI asks for: store updates plus the core call, as the renderer's handlers
//! do them (`handleLogin`, `openServer`, `refetchRoster`, …). Plain functions over
//! `&mut App`; each names the renderer code it ports.

pub mod auth;
pub mod calls;
pub mod devices;
pub mod e2ee;
pub mod friends;
pub mod hotkeys;
pub mod presence;
pub mod servers;
pub mod settings;
pub mod transfers;
pub mod voice;
