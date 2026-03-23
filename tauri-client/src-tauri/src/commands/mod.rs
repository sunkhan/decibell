pub mod auth;
pub mod channels;
pub mod friends;
pub mod messaging;
pub mod servers;
pub mod streaming;
pub mod voice;

#[tauri::command]
pub fn ping() -> String {
    "pong".to_string()
}
