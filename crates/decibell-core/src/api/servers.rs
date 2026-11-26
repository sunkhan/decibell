//! Connecting to a community (the body of the addon's `connect_to_community`
//! / `join_with_invite` commands; central's router also calls it).

use crate::net::community::CommunityClient;
use crate::state;

/// Connect (or reconnect) to community `server_id` at host:port with our
/// session token, presenting `invite_code` when joining by invite. Replaces
/// any existing connection to it.
pub async fn connect_with_invite(
    server_id: String,
    host: String,
    port: u16,
    invite_code: Option<String>,
) -> Result<(), String> {
    let state_arc = state::shared();

    let jwt = {
        let s = state_arc.lock().await;
        s.token
            .clone()
            .ok_or_else(|| "Not authenticated".to_string())?
    };

    // Tear down any existing client for this server_id first — a fresh
    // connect should replace it, otherwise we'd leak its tasks.
    {
        let mut s = state_arc.lock().await;
        crate::e2ee::channel_keys::forget_server(&mut s, &server_id);
        if let Some(mut existing) = s.communities.remove(&server_id) {
            existing.disconnect();
        }
    }

    let client = CommunityClient::connect(
        server_id.clone(),
        host,
        port,
        jwt,
        invite_code,
        state_arc.clone(),
    )
    .await?;

    let mut s = state_arc.lock().await;
    s.communities.insert(server_id, client);

    Ok(())
}
