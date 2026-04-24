mod commands;
mod config;
mod events;
mod media;
mod net;
mod state;
#[cfg(target_os = "linux")]
mod audio_routing;

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
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            use tauri::Manager;
            use tauri_plugin_deep_link::DeepLinkExt;
            let handle = app.handle().clone();
            // Runtime registration is required for Linux AppImage builds (the
            // bundle's .desktop file registers at install time, but running
            // from a dev build or an unpacked AppImage needs this).
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            let _ = app.deep_link().register_all();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    events::emit_deep_link_received(&handle, url.to_string());
                }
            });
            Ok(())
        })
        .manage(Arc::new(Mutex::new(AppState::default())) as SharedState)
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::auth::login,
            commands::auth::register,
            commands::auth::logout,
            commands::servers::request_server_list,
            commands::servers::connect_to_community,
            commands::servers::disconnect_from_community,
            commands::servers::redeem_invite,
            commands::community::create_invite,
            commands::community::list_invites,
            commands::community::revoke_invite,
            commands::community::list_members,
            commands::community::kick_member,
            commands::community::ban_member,
            commands::community::leave_server,
            commands::community::parse_invite_link,
            commands::community::resolve_invite_code,
            commands::channels::send_channel_message,
            commands::channels::request_channel_history,
            commands::channels::update_channel_retention,
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
            commands::streaming::watch_self_stream,
            commands::streaming::stop_watching,
            commands::streaming::request_keyframe,
            commands::voice::set_stream_volume,
            commands::voice::set_stream_stereo,
            commands::voice::set_user_volume,
            commands::voice::set_aec_enabled,
            commands::voice::set_noise_suppression_level,
            commands::voice::set_agc_enabled,
            commands::settings::load_config,
            commands::settings::save_settings,
            commands::settings::list_audio_devices,
            commands::settings::set_input_device,
            commands::settings::set_output_device,
            commands::settings::set_separate_stream_output,
            commands::settings::set_stream_output_device,
            commands::settings::start_mic_test,
            commands::settings::stop_mic_test,
            commands::sounds::play_sound,
            commands::attachments::upload_attachment,
            commands::attachments::cancel_attachment_upload,
            commands::attachments::download_attachment,
            commands::attachments::fetch_attachment_bytes,
            commands::attachments::set_transfer_limits,
            commands::attachments::stat_attachment_file,
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
