use tauri::State;

use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state::SharedState;

// --- helpers ---

async fn send_for_server(
    server_id: &str,
    state: &State<'_, SharedState>,
    pkt_type: packet::Type,
    payload: packet::Payload,
) -> Result<(), String> {
    let (write_tx, data) = {
        let s = state.lock().await;
        let client = s.communities
            .get(server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        let tx = client.connection_write_tx()
            .ok_or("Community connection lost")?;
        let pkt = build_packet(pkt_type, payload, Some(&client.jwt));
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
}

// --- invite commands (owner only, enforced server-side) ---

#[tauri::command]
pub async fn create_invite(
    server_id: String,
    expires_at: i64,   // 0 = never
    max_uses: i32,     // 0 = unlimited
    state: State<'_, SharedState>,
) -> Result<(), String> {
    send_for_server(
        &server_id,
        &state,
        packet::Type::InviteCreateReq,
        packet::Payload::InviteCreateReq(InviteCreateRequest { expires_at, max_uses }),
    )
    .await
}

#[tauri::command]
pub async fn list_invites(
    server_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    send_for_server(
        &server_id,
        &state,
        packet::Type::InviteListReq,
        packet::Payload::InviteListReq(InviteListRequest {}),
    )
    .await
}

#[tauri::command]
pub async fn revoke_invite(
    server_id: String,
    code: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    send_for_server(
        &server_id,
        &state,
        packet::Type::InviteRevokeReq,
        packet::Payload::InviteRevokeReq(InviteRevokeRequest { code }),
    )
    .await
}

// --- members + moderation ---

#[tauri::command]
pub async fn list_members(
    server_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    send_for_server(
        &server_id,
        &state,
        packet::Type::MemberListReq,
        packet::Payload::MemberListReq(MemberListRequest {}),
    )
    .await
}

#[tauri::command]
pub async fn kick_member(
    server_id: String,
    username: String,
    reason: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    send_for_server(
        &server_id,
        &state,
        packet::Type::KickMemberReq,
        packet::Payload::KickMemberReq(KickMemberRequest { username, reason }),
    )
    .await
}

#[tauri::command]
pub async fn ban_member(
    server_id: String,
    username: String,
    reason: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    send_for_server(
        &server_id,
        &state,
        packet::Type::BanMemberReq,
        packet::Payload::BanMemberReq(BanMemberRequest { username, reason }),
    )
    .await
}

#[tauri::command]
pub async fn leave_server(
    server_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    send_for_server(
        &server_id,
        &state,
        packet::Type::LeaveServerReq,
        packet::Payload::LeaveServerReq(LeaveServerRequest {}),
    )
    .await
}

// --- deep-link parsing ---

/// Parse a decibell:// invite URL into its components.
/// Accepted shapes (any leading scheme is tolerated):
///   decibell://invite/<host>:<port>/<code>
///   decibell://invite/<host>/<port>/<code>
///   decibell:invite/<host>:<port>/<code>
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedInviteLink {
    pub host: String,
    pub port: u16,
    pub code: String,
}

#[tauri::command]
pub fn parse_invite_link(url: String) -> Result<ParsedInviteLink, String> {
    let trimmed = url.trim();
    // Strip leading "decibell:" or "decibell://" and a trailing "invite/" prefix.
    let after_scheme = trimmed
        .strip_prefix("decibell://")
        .or_else(|| trimmed.strip_prefix("decibell:"))
        .ok_or("Not a decibell:// URL")?;
    let body = after_scheme
        .strip_prefix("invite/")
        .or_else(|| after_scheme.strip_prefix("/invite/"))
        .ok_or("URL is not an invite link")?;

    let body = body.trim_end_matches('/');
    let parts: Vec<&str> = body.split('/').collect();
    if parts.len() < 2 {
        return Err("Invite link missing components".into());
    }

    // Last segment is always the code.
    let code = parts.last().unwrap().to_string();
    if code.is_empty() {
        return Err("Invite code missing".into());
    }

    // The remaining prefix is host[:port] — possibly split across two segments
    // if the producer used the alternative `host/port/code` form.
    let (host, port) = if parts.len() == 2 {
        let hp = parts[0];
        match hp.rsplit_once(':') {
            Some((h, p)) => (
                urlencoding::decode(h).map_err(|e| e.to_string())?.to_string(),
                p.parse::<u16>().map_err(|_| "Invalid port in invite link")?,
            ),
            None => return Err("Invite link missing port".into()),
        }
    } else {
        // host / port / code form
        let host = urlencoding::decode(parts[0]).map_err(|e| e.to_string())?.to_string();
        let port = parts[1]
            .parse::<u16>()
            .map_err(|_| "Invalid port in invite link")?;
        (host, port)
    };

    if host.is_empty() {
        return Err("Host missing in invite link".into());
    }

    Ok(ParsedInviteLink {
        host,
        port,
        code: code.to_uppercase(),
    })
}
