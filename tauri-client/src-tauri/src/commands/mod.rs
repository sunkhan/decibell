pub mod auth;
pub mod channels;
pub mod community;
pub mod friends;
pub mod messaging;
pub mod servers;
pub mod settings;
pub mod streaming;
pub mod sounds;
pub mod voice;

#[tauri::command]
pub fn ping() -> String {
    "pong".to_string()
}
