//! Server discovery + community-server connection lifecycle.
//!
//! PR3 surface: ask central for the server list, connect to a
//! community server (optionally redeeming an invite as part of the
//! handshake), disconnect from a community server.

use tokio::sync::oneshot;

use crate::net::community::CommunityClient;
use crate::net::connection::build_packet;
use crate::net::proto::{
    packet, InviteResolveRequest, InviteResolveResponse, MembershipRevokeReq, ServerListRequest,
};
use crate::state;

/// Ask the central server for the user's server list. The response
/// arrives asynchronously as a `server_list_received` event.
#[napi]
pub async fn request_server_list() -> napi::Result<()> {
    let state_arc = state::shared();

    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let token = s.token.clone();
        let central = s
            .central
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not connected to central server"))?;
        let tx = central
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Central connection lost"))?;
        let pkt = build_packet(
            packet::Type::ServerListReq,
            packet::Payload::ServerListReq(ServerListRequest {}),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct ConnectToCommunityArgs {
    pub server_id: String,
    pub host: String,
    pub port: i32,
}

#[napi]
pub async fn connect_to_community(args: ConnectToCommunityArgs) -> napi::Result<()> {
    let port = u16::try_from(args.port).map_err(|_| napi::Error::from_reason("Invalid port"))?;
    connect_with_invite(args.server_id, args.host, port, None).await
}

#[napi(object)]
pub struct RedeemInviteArgs {
    pub server_id: String,
    pub host: String,
    pub port: i32,
    pub invite_code: String,
}

/// Connect to a community server while consuming an invite code so the
/// user joins (becomes a persistent member) as part of the handshake.
#[napi]
pub async fn redeem_invite(args: RedeemInviteArgs) -> napi::Result<()> {
    let code = args.invite_code.trim().to_uppercase();
    if code.is_empty() {
        return Err(napi::Error::from_reason("Invite code is empty"));
    }
    let port = u16::try_from(args.port).map_err(|_| napi::Error::from_reason("Invalid port"))?;
    connect_with_invite(args.server_id, args.host, port, Some(code)).await
}

#[napi(object)]
pub struct DisconnectFromCommunityArgs {
    pub server_id: String,
}

#[napi]
pub async fn disconnect_from_community(
    args: DisconnectFromCommunityArgs,
) -> napi::Result<()> {
    let state_arc = state::shared();

    let removed = {
        let mut s = state_arc.lock().await;
        s.communities.remove(&args.server_id)
    };

    match removed {
        Some(mut client) => {
            client.disconnect();
            Ok(())
        }
        None => Err(napi::Error::from_reason(format!(
            "Not connected to community {}",
            args.server_id
        ))),
    }
}

#[napi(object)]
pub struct ParseInviteLinkArgs {
    pub url: String,
}

#[napi(object)]
pub struct ParsedInviteLink {
    pub host: String,
    pub port: u16,
    pub code: String,
}

/// Parse a `decibell://` invite URL into its components. Accepted shapes:
///   decibell://invite/<host>:<port>/<code>
///   decibell://invite/<host>/<port>/<code>
///   decibell:invite/<host>:<port>/<code>
#[napi]
pub fn parse_invite_link(args: ParseInviteLinkArgs) -> napi::Result<ParsedInviteLink> {
    let trimmed = args.url.trim();
    let after_scheme = trimmed
        .strip_prefix("decibell://")
        .or_else(|| trimmed.strip_prefix("decibell:"))
        .ok_or_else(|| napi::Error::from_reason("Not a decibell:// URL"))?;
    let body = after_scheme
        .strip_prefix("invite/")
        .or_else(|| after_scheme.strip_prefix("/invite/"))
        .ok_or_else(|| napi::Error::from_reason("URL is not an invite link"))?;

    let body = body.trim_end_matches('/');
    let parts: Vec<&str> = body.split('/').collect();
    if parts.len() < 2 {
        return Err(napi::Error::from_reason("Invite link missing components"));
    }

    let code = parts.last().unwrap().to_string();
    if code.is_empty() {
        return Err(napi::Error::from_reason("Invite code missing"));
    }

    let (host, port) = if parts.len() == 2 {
        let hp = parts[0];
        match hp.rsplit_once(':') {
            Some((h, p)) => (
                percent_decode(h),
                p.parse::<u16>()
                    .map_err(|_| napi::Error::from_reason("Invalid port in invite link"))?,
            ),
            None => return Err(napi::Error::from_reason("Invite link missing port")),
        }
    } else {
        let host = percent_decode(parts[0]);
        let port = parts[1]
            .parse::<u16>()
            .map_err(|_| napi::Error::from_reason("Invalid port in invite link"))?;
        (host, port)
    };

    if host.is_empty() {
        return Err(napi::Error::from_reason("Host missing in invite link"));
    }

    Ok(ParsedInviteLink {
        host,
        port,
        code: code.to_uppercase(),
    })
}

#[napi(object)]
pub struct ResolveInviteCodeArgs {
    pub code: String,
}

#[napi(object)]
pub struct ResolvedInvite {
    pub host: String,
    pub port: u16,
    pub code: String,
}

/// Ask the central server to resolve a raw invite code to a community
/// host:port. Returns the resolved endpoint or an error describing why
/// the code couldn't be resolved (unknown, expired, central not
/// reachable). Times out after 5 seconds.
#[napi]
pub async fn resolve_invite_code(args: ResolveInviteCodeArgs) -> napi::Result<ResolvedInvite> {
    let code = args.code.trim().to_uppercase();
    if code.is_empty() {
        return Err(napi::Error::from_reason("Invite code is empty"));
    }
    let state_arc = state::shared();

    // Build packet + register waiter under a single lock acquisition.
    let (write_tx, data, rx) = {
        let mut s = state_arc.lock().await;
        let central = s
            .central
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not connected to central server"))?;
        let tx = central
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Central connection lost"))?;
        let token = s.token.clone();
        let data = build_packet(
            packet::Type::InviteResolveReq,
            packet::Payload::InviteResolveReq(InviteResolveRequest { code: code.clone() }),
            token.as_deref(),
        );
        let (otx, orx) = oneshot::channel::<InviteResolveResponse>();
        // Last-request-wins: replace any earlier waiter for this code.
        s.pending_invite_resolves.insert(code.clone(), otx);
        (tx, data, orx)
    };

    if tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data))
        .await
        .is_err()
    {
        state_arc.lock().await.pending_invite_resolves.remove(&code);
        return Err(napi::Error::from_reason("Failed to send resolve request"));
    }

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(resp)) => {
            if resp.success {
                let port = u16::try_from(resp.port)
                    .map_err(|_| napi::Error::from_reason("Central returned invalid port"))?;
                Ok(ResolvedInvite {
                    host: resp.host,
                    port,
                    code: resp.code,
                })
            } else {
                let msg = if resp.message.is_empty() {
                    "Invite not found".to_string()
                } else {
                    resp.message
                };
                Err(napi::Error::from_reason(msg))
            }
        }
        Ok(Err(_)) => Err(napi::Error::from_reason(
            "Central connection closed before response",
        )),
        Err(_) => {
            state_arc.lock().await.pending_invite_resolves.remove(&code);
            Err(napi::Error::from_reason("Timed out waiting for central server"))
        }
    }
}

#[napi(object)]
pub struct GetAttachmentTargetArgs {
    pub server_id: String,
}

#[napi(object)]
pub struct AttachmentTarget {
    pub host: String,
    pub port: i32,
    pub jwt: String,
    /// Per-file upload cap reported by the server. 0 = unlimited.
    pub max_attachment_bytes: i64,
}

/// Resolve the HTTPS endpoint for attachment uploads/downloads against a
/// connected community server. Returns null when the server didn't
/// advertise an attachment port (older builds, or HTTP disabled).
/// Renderer drives the actual transfer — this command just hands over
/// the host/port/jwt so `fetch()` can run with native HTTP/2 +
/// connection reuse + Chromium's TLS stack.
#[napi]
pub async fn get_attachment_target(
    args: GetAttachmentTargetArgs,
) -> napi::Result<Option<AttachmentTarget>> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    let client = s.communities.get(&args.server_id).ok_or_else(|| {
        napi::Error::from_reason(format!("Not connected to community {}", args.server_id))
    })?;
    if client.attachment_port == 0 {
        return Ok(None);
    }
    Ok(Some(AttachmentTarget {
        host: client.host.clone(),
        port: client.attachment_port as i32,
        jwt: client.jwt.clone(),
        max_attachment_bytes: client.max_attachment_bytes,
    }))
}

/// Best-effort URL-decoding for the host segment of an invite link.
/// We don't pull in the urlencoding crate just for this — invite URLs
/// rarely contain encoded chars, and a manual %XX scan is sufficient.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) async fn connect_with_invite(
    server_id: String,
    host: String,
    port: u16,
    invite_code: Option<String>,
) -> napi::Result<()> {
    let state_arc = state::shared();

    let jwt = {
        let s = state_arc.lock().await;
        s.token
            .clone()
            .ok_or_else(|| napi::Error::from_reason("Not authenticated"))?
    };

    // Tear down any existing client for this server_id first — a fresh
    // connect should replace it, otherwise we'd leak its tasks.
    {
        let mut s = state_arc.lock().await;
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
    .await
    .map_err(napi::Error::from_reason)?;

    let mut s = state_arc.lock().await;
    s.communities.insert(server_id, client);

    Ok(())
}

#[napi(object)]
pub struct DropMembershipArgs {
    pub server_id: String,
}

/// Auto-rejoin stale-membership cleanup: tells central to drop the
/// user's user_communities row for this server. Used when an
/// auto-rejoin auth comes back with success=false (kicked/banned while
/// offline). Authenticated via the user's JWT on the central session
/// — central's MEMBERSHIP_REVOKE_REQ handler enforces self-revoke when
/// the auth_token doesn't match the shared secret, ignoring the
/// username field on the packet.
#[napi]
pub async fn request_drop_membership(args: DropMembershipArgs) -> napi::Result<()> {
    let server_id: i64 = args.server_id.parse().map_err(|_| {
        napi::Error::from_reason(format!("Invalid server_id: {}", args.server_id))
    })?;

    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let token = s.token.clone();
        let central = s
            .central
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not connected to central server"))?;
        let tx = central
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Central connection lost"))?;
        let pkt = build_packet(
            packet::Type::MembershipRevokeReq,
            packet::Payload::MembershipRevokeReq(MembershipRevokeReq {
                username: String::new(),
                server_id,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}
