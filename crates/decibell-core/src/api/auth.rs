//! Signing out (the body of the addon's `logout` command, and what central
//! triggers when another device signs this one out).

use crate::config;
use crate::events;
use crate::state;

/// Central signed this device out: another device did ("signed_out"), or
/// changed the password ("password_changed"). Signs out here as Log out
/// does, saved sign-in and all; the sign-in page says why.
pub async fn signed_out_elsewhere(reason: &'static str) {
    sign_out_locally(Some(reason)).await;
}

/// Tear the session down on this device: connections, media engines, call,
/// keys in memory, saved credentials; then `logged_out` (with `reason` when
/// it wasn't this device's own Log out).
pub async fn sign_out_locally(reason: Option<&'static str>) {
    let state_arc = state::shared();

    // Extract clients + media engines and clear state under lock, then
    // disconnect/stop outside. Taking the engines here is what actually
    // stops the mic + screen capture: without it, logging out while in a
    // voice channel or streaming leaves those threads alive and pumping
    // UDP under the old session (privacy leak), and leaves connected_voice_*
    // pointing at a channel we've left.
    crate::presence::reset();
    let (old_central, old_communities, voice, video, audio) = {
        let mut s = state_arc.lock().await;
        let central = s.central.take();
        let communities: Vec<_> = s.communities.drain().map(|(_, c)| c).collect();
        // A DM call (punching or connected) dies with the session: stop
        // its punch / watchdog and drop any prepared sockets.
        s.pending_call = None;
        if let Some(ac) = s.active_call.take() {
            ac.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(h) = ac.watchdog {
                h.abort();
            }
        }
        crate::media::watched_streams_clear();
        let voice = s.voice_engine.take();
        let video = s.video_engine.take();
        let audio = s.audio_stream_engine.take();
        s.username = None;
        s.uid = 0;
        s.presence_watch.clear();
        s.token = None;
        s.credentials = None;
        s.save_credentials_on_login = false;
        s.resume_token = None;
        s.resume_pending = false;
        s.connected_voice_server = None;
        s.connected_voice_channel = None;
        s.voice_muted = false;
        s.voice_deafened = false;
        s.voice_muted_before_deafen = false;
        s.pending_invite_resolves.clear();
        s.pending_friends_in_community.clear();
        s.pending_avatar_fetches.clear();
        s.pending_thumbnail_fetches.clear();
        s.pending_avatar_update = None;
        // E2EE: keys leave memory with the session; the on-disk store
        // stays (it's per account) and reloads on the next login.
        s.voice_mls = None;
        s.channel_keys.clear();
        s.channel_key_loads.clear();
        s.channel_crypto_tx.clear();
        s.pending_channel_keys.clear();
        s.pending_channel_keys_publish.clear();
        s.e2ee.reset();
        s.pending_key_fetches.clear();
        s.pending_key_publish = None;
        s.pending_backup_fetch = None;
        (central, communities, voice, video, audio)
    };

    if let Some(mut central) = old_central {
        central.disconnect();
    }
    for mut client in old_communities {
        client.disconnect();
    }

    // Stop capture/encode engines off the async runtime — their Drop joins
    // native threads, which must not block a Tokio worker or run under the
    // AppState lock.
    if voice.is_some() || video.is_some() || audio.is_some() {
        tokio::task::spawn_blocking(move || {
            drop(voice);
            drop(video);
            drop(audio);
        });
    }

    events::emit_logged_out(reason);
    events::emit_e2ee_status_changed(events::E2eeStatusPayload {
        supported: false,
        status: "unavailable".to_string(),
        key_id: 0,
        fingerprint: String::new(),
    });

    let _ = config::clear_credentials();
}
