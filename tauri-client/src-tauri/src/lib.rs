mod commands;
mod config;
mod events;
mod local_media_server;
mod media;
mod net;
mod state;
#[cfg(target_os = "linux")]
mod audio_routing;

use std::sync::Arc;
use tokio::sync::Mutex;
use state::{AppState, SharedState};

/// Sweep `decibell-attach-*` files from the media cache dir whose mtime
/// is older than `min_age_secs`. Used at startup to clean up after a
/// previous-session crash; the threshold spares anything an
/// immediately-restarted second instance might still need.
async fn sweep_temp_attachments(min_age_secs: u64) {
    let dir = local_media_server::cache_dir();
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(_) => return,
    };
    let now = std::time::SystemTime::now();
    let threshold = std::time::Duration::from_secs(min_age_secs);
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if !name_str.starts_with("decibell-attach-") {
            continue;
        }
        let Ok(meta) = entry.metadata().await else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        let Ok(age) = now.duration_since(mtime) else { continue };
        if age >= threshold {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

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

            // Initialize the persistent media cache directory
            // (`~/.cache/com.decibell.app/` on Linux, equivalent
            // platform-specific paths elsewhere). Anywhere we write
            // `decibell-attach-*` temp files now lands here instead of
            // `/tmp` — important on Linux distros where `/tmp` is
            // tmpfs (RAM-backed), since cached videos could otherwise
            // accumulate in RAM. Falls back to OS temp dir if the
            // resolver fails.
            match app.path().app_cache_dir() {
                Ok(cache) => {
                    if let Err(e) = local_media_server::init_cache_dir(cache.clone()) {
                        eprintln!("[media] init cache dir at {:?}: {}", cache, e);
                    }
                }
                Err(e) => eprintln!("[media] couldn't resolve app cache dir: {}", e),
            }
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

            // Local HTTP media server for <video>/<audio> playback.
            let state = app.state::<SharedState>().inner().clone();
            tauri::async_runtime::spawn(async move {
                match local_media_server::start().await {
                    Ok(port) => {
                        let mut s = state.lock().await;
                        s.local_media_port = port;
                        eprintln!("[media] local server listening on 127.0.0.1:{}", port);
                    }
                    Err(e) => {
                        eprintln!("[media] failed to start local server: {}", e);
                    }
                }
            });

            // Startup sweep: delete stale `decibell-attach-*` files left
            // behind by a previous session (crash, force-quit, etc.).
            // Threshold of 60 seconds spares anything an immediately-
            // restarted instance might still need.
            tauri::async_runtime::spawn(async {
                sweep_temp_attachments(60).await;
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
            commands::channels::wipe_channel_history,
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
            commands::attachments::save_attachment_to_temp,
            commands::attachments::cleanup_temp_attachment,
            commands::attachments::stage_file_for_media,
            commands::attachments::fetch_attachment_bytes,
            commands::attachments::fetch_attachment_thumbnail,
            commands::attachments::upload_attachment_thumbnail,
            commands::attachments::set_transfer_limits,
            commands::attachments::stat_attachment_file,
            commands::attachments::save_paste_to_temp,
            commands::attachments::read_clipboard_image,
            commands::attachments::read_clipboard_file_paths,
            commands::attachments::copy_image_to_clipboard,
            #[cfg(target_os = "linux")]
            commands::attachments::pick_attachments_xdg,
            #[cfg(target_os = "linux")]
            commands::attachments::pick_save_path_xdg,
        ])
        .on_window_event(|_window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Exit sweep: synchronously delete any temp media files
                // we created this session before the OS reclaims us.
                // Sync stdlib (we're about to die anyway, no async runtime
                // value here).
                if let Ok(entries) = std::fs::read_dir(local_media_server::cache_dir()) {
                    for entry in entries.flatten() {
                        if let Some(name) = entry.file_name().to_str() {
                            if name.starts_with("decibell-attach-") {
                                let _ = std::fs::remove_file(entry.path());
                            }
                        }
                    }
                }
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
