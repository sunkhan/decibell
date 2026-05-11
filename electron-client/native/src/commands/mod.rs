//! `#[napi]` command surface — what the JS side reaches via
//! `invoke('method_name', args)`. Each module here mirrors the layout
//! of `tauri-client/src-tauri/src/commands/`; module bodies port
//! verbatim, only `#[tauri::command]` → `#[napi]` and
//! `tauri::State<'_, SharedState>` → `state::shared()` extractions
//! change.

pub mod auth;
pub mod channels;
pub mod community;
pub mod friends;
pub mod messaging;
pub mod servers;
pub mod settings;
pub mod streaming;
pub mod voice;
