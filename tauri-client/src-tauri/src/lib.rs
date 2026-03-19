mod commands;
mod events;
mod media;
mod net;
mod state;

use std::sync::Arc;
use tokio::sync::Mutex;
use state::{AppState, SharedState};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Arc::new(Mutex::new(AppState::default())) as SharedState)
        .invoke_handler(tauri::generate_handler![commands::ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
