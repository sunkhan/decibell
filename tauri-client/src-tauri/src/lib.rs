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
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(Arc::new(Mutex::new(AppState::default())) as SharedState)
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::auth::login,
            commands::auth::register,
            commands::auth::logout,
            commands::servers::request_server_list,
            commands::servers::connect_to_community,
            commands::servers::disconnect_from_community,
            commands::channels::join_channel,
            commands::channels::send_channel_message,
            commands::friends::request_friend_list,
            commands::friends::send_friend_action,
            commands::messaging::send_private_message,
            commands::messaging::set_dm_privacy,
            commands::voice::join_voice_channel,
            commands::voice::leave_voice_channel,
            commands::voice::set_voice_mute,
            commands::voice::set_voice_deafen,
            commands::voice::set_voice_threshold,
            commands::streaming::list_capture_sources,
            commands::streaming::start_screen_share,
            commands::streaming::stop_screen_share,
            commands::streaming::watch_stream,
            commands::streaming::stop_watching,
            commands::streaming::request_keyframe,
            commands::voice::set_stream_volume,
            commands::voice::set_user_volume,
        ])
        .on_window_event(|_window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Hard-exit: the OS reclaims all threads, sockets, and memory.
                #[cfg(target_os = "linux")]
                unsafe { libc::_exit(0); }
                #[cfg(not(target_os = "linux"))]
                std::process::exit(0);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
