//! Voice-session work the community read loop does itself (the join / leave
//! commands are the addon's, `commands/voice.rs`).

use crate::state;

/// A moderator moved us (VOICE_FORCE_NOTIFY{MOVED}): the server already
/// switched our session, so the engine (UDP sockets, audio) keeps running
/// and only the MLS group follows — a driver left on the old channel never
/// joins the new one's group, and nobody there can open our media. Called
/// from the community read loop, before any packet for the new channel.
/// Returns the new group's command channel so the caller can seed the roster.
pub fn rebind_voice_group(
    s: &mut state::AppState,
    state_arc: std::sync::Arc<tokio::sync::Mutex<state::AppState>>,
    server_id: &str,
    channel_id: &str,
) -> Result<Option<tokio::sync::mpsc::UnboundedSender<crate::e2ee::group::GroupCmd>>, String> {
    if s.voice_engine.is_none() || s.connected_voice_server.as_deref() != Some(server_id) {
        return Ok(None);
    }
    if s.connected_voice_channel.as_deref() == Some(channel_id) {
        return Ok(None);
    }
    // The engine's pipelines hold this ring; keep the instance, drop the
    // old group's keys so nothing is sealed until the new epoch lands.
    let Some(old) = s.voice_mls.take() else {
        return Err("no voice encryption group to move".into());
    };
    let ring = old.ring.clone();
    drop(old);
    s.connected_voice_channel = Some(channel_id.to_string());
    // The server dropped our watches with the old channel.
    crate::media::watched_streams_clear();
    if s.uid <= 0 {
        return Err("Not signed in".into());
    }
    ring.store(std::sync::Arc::new(crate::media::frame_crypto::KeyRing::empty(&s.uid.to_string())));
    let identity = s
        .e2ee
        .store
        .as_ref()
        .and_then(|st| st.current())
        .cloned()
        .ok_or("Encryption keys not loaded")?;
    let client = s
        .communities
        .get(server_id)
        .ok_or_else(|| format!("Not connected to community {server_id}"))?;
    let write_tx = client.connection_write_tx().ok_or("Community connection lost")?;
    let handle = crate::e2ee::group::start(
        state_arc,
        server_id.to_string(),
        channel_id.to_string(),
        s.uid,
        &identity,
        write_tx,
        ring,
        false,
    )?;
    let tx = handle.tx.clone();
    s.voice_mls = Some(handle);
    Ok(Some(tx))
}
