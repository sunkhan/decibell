//! Server discovery + community-server connection lifecycle.
//!
//! PR3 surface: ask central for the server list, connect to a
//! community server (optionally redeeming an invite as part of the
//! handshake), disconnect from a community server.

use tokio::sync::oneshot;

use crate::api::CoreError;
use crate::net::community::CommunityClient;
use crate::net::connection::build_packet;
use crate::net::proto::{
    packet, FetchServerPictureReq, InviteResolveRequest, InviteResolveResponse,
    MembershipRevokeReq, ServerListRequest, UpdateServerPictureReq,
};
use crate::state;

/// Ask the central server for the user's server list. The response
/// arrives asynchronously as a `server_list_received` event.
pub async fn request_server_list() -> Result<(), CoreError> {
    crate::rt::run(async move {
        let state_arc = state::shared();

        let (write_tx, data) = {
            let s = state_arc.lock().await;
            let token = s.token.clone();
            let central = s
                .central
                .as_ref()
                .ok_or_else(|| CoreError::other("Not connected to central server"))?;
            let tx = central
                .connection_write_tx()
                .ok_or_else(|| CoreError::other("Central connection lost"))?;
            let pkt = build_packet(
                packet::Type::ServerListReq,
                packet::Payload::ServerListReq(ServerListRequest {}),
                token.as_deref(),
            );
            (tx, pkt)
        };

        match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(CoreError::other("Connection closed")),
            Err(_) => Err(CoreError::other("Send timed out")),
        }
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct ConnectToCommunityArgs {
    pub server_id: String,
    pub host: String,
    pub port: i32,
}

pub async fn connect_to_community(args: ConnectToCommunityArgs) -> Result<(), CoreError> {
    crate::rt::run(async move {
        let port = u16::try_from(args.port).map_err(|_| CoreError::invalid("Invalid port"))?;
        connect_with_invite(args.server_id, args.host, port, None)
            .await
            .map_err(CoreError::other)
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct RedeemInviteArgs {
    pub server_id: String,
    pub host: String,
    pub port: i32,
    pub invite_code: String,
}

/// Connect to a community server while consuming an invite code so the
/// user joins (becomes a persistent member) as part of the handshake.
pub async fn redeem_invite(args: RedeemInviteArgs) -> Result<(), CoreError> {
    crate::rt::run(async move {
        let code = args.invite_code.trim().to_uppercase();
        if code.is_empty() {
            return Err(CoreError::invalid("Invite code is empty"));
        }
        let port = u16::try_from(args.port).map_err(|_| CoreError::invalid("Invalid port"))?;
        connect_with_invite(args.server_id, args.host, port, Some(code))
            .await
            .map_err(CoreError::other)
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct DisconnectFromCommunityArgs {
    pub server_id: String,
}

pub async fn disconnect_from_community(
    args: DisconnectFromCommunityArgs,
) -> Result<(), CoreError> {
    crate::rt::run(async move {
        let state_arc = state::shared();

        let removed = {
            let mut s = state_arc.lock().await;
            crate::e2ee::channel_keys::forget_server(&mut s, &args.server_id);
            s.communities.remove(&args.server_id)
        };

        match removed {
            Some(mut client) => {
                client.disconnect();
                Ok(())
            }
            None => Err(CoreError::other(format!(
                "Not connected to community {}",
                args.server_id
            ))),
        }
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct ParseInviteLinkArgs {
    pub url: String,
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct ParsedInviteLink {
    pub host: String,
    pub port: u16,
    pub code: String,
}

/// Parse a `decibell://` invite URL into its components. Accepted shapes:
///   decibell://invite/<code>                 (host "" / port 0: resolve
///                                             the code via central)
///   decibell://invite/<host>:<port>/<code>
///   decibell://invite/<host>/<port>/<code>
///   decibell:invite/<host>:<port>/<code>
/// The renderer parses links itself (features/servers/inviteLink.ts);
/// this stays for API completeness and older callers.
pub fn parse_invite_link(args: ParseInviteLinkArgs) -> Result<ParsedInviteLink, CoreError> {
    let trimmed = args.url.trim();
    let after_scheme = trimmed
        .strip_prefix("decibell://")
        .or_else(|| trimmed.strip_prefix("decibell:"))
        .ok_or_else(|| CoreError::invalid("Not a decibell:// URL"))?;
    let body = after_scheme
        .strip_prefix("invite/")
        .or_else(|| after_scheme.strip_prefix("/invite/"))
        .ok_or_else(|| CoreError::invalid("URL is not an invite link"))?;

    let body = body.trim_end_matches('/');
    let parts: Vec<&str> = body.split('/').collect();
    if parts.is_empty() {
        return Err(CoreError::invalid("Invite link missing components"));
    }

    let code = parts.last().unwrap().to_string();
    if code.is_empty() {
        return Err(CoreError::invalid("Invite code missing"));
    }

    let (host, port) = if parts.len() == 1 {
        (String::new(), 0u16)
    } else if parts.len() == 2 {
        let hp = parts[0];
        match hp.rsplit_once(':') {
            Some((h, p)) => (
                percent_decode(h),
                p.parse::<u16>()
                    .map_err(|_| CoreError::invalid("Invalid port in invite link"))?,
            ),
            None => return Err(CoreError::invalid("Invite link missing port")),
        }
    } else {
        let host = percent_decode(parts[0]);
        let port = parts[1]
            .parse::<u16>()
            .map_err(|_| CoreError::invalid("Invalid port in invite link"))?;
        (host, port)
    };

    if parts.len() > 1 && host.is_empty() {
        return Err(CoreError::invalid("Host missing in invite link"));
    }

    Ok(ParsedInviteLink {
        host,
        port,
        code: code.to_uppercase(),
    })
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct ResolveInviteCodeArgs {
    pub code: String,
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct ResolvedInvite {
    pub host: String,
    pub port: u16,
    pub code: String,
    /// Pre-join preview (invite cards in chat). 0 / empty when central
    /// has no directory row for the community, or is an older build.
    pub server_id: i64,
    pub server_name: String,
    pub server_description: String,
    pub member_count: i32,
    pub picture_version: String,
}

/// Ask the central server to resolve a raw invite code to a community
/// host:port, plus the community's directory row for the pre-join
/// preview. Returns an error describing why the code couldn't be
/// resolved (unknown, expired, central not reachable). Times out after
/// 5 seconds.
pub async fn resolve_invite_code(args: ResolveInviteCodeArgs) -> Result<ResolvedInvite, CoreError> {
    crate::rt::run(async move {
        let code = args.code.trim().to_uppercase();
        if code.is_empty() {
            return Err(CoreError::invalid("Invite code is empty"));
        }
        let state_arc = state::shared();

        // Build packet + register waiter under a single lock acquisition.
        let (write_tx, data, rx) = {
            let mut s = state_arc.lock().await;
            let central = s
                .central
                .as_ref()
                .ok_or_else(|| CoreError::other("Not connected to central server"))?;
            let tx = central
                .connection_write_tx()
                .ok_or_else(|| CoreError::other("Central connection lost"))?;
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
            return Err(CoreError::other("Failed to send resolve request"));
        }

        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(resp)) => {
                if resp.success {
                    let port = u16::try_from(resp.port)
                        .map_err(|_| CoreError::other("Central returned invalid port"))?;
                    Ok(ResolvedInvite {
                        host: resp.host,
                        port,
                        code: resp.code,
                        server_id: resp.server_id,
                        server_name: resp.server_name,
                        server_description: resp.server_description,
                        member_count: resp.member_count,
                        picture_version: resp.picture_version,
                    })
                } else {
                    let msg = if resp.message.is_empty() {
                        "Invite not found".to_string()
                    } else {
                        resp.message
                    };
                    Err(CoreError::refused(msg))
                }
            }
            Ok(Err(_)) => Err(CoreError::other(
                "Central connection closed before response",
            )),
            Err(_) => {
                state_arc.lock().await.pending_invite_resolves.remove(&code);
                Err(CoreError::other("Timed out waiting for central server"))
            }
        }
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct GetAttachmentTargetArgs {
    pub server_id: String,
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct AttachmentTarget {
    pub host: String,
    pub port: i32,
    pub jwt: String,
    /// Per-file upload cap reported by the server. 0 = unlimited.
    pub max_attachment_bytes: i64,
    /// sha256-hex of the certificate this community presented on its chat
    /// connection; the attachment listener uses the same certificate, so
    /// Electron pins the HTTPS host to it.
    pub cert_fingerprint: String,
}

/// Resolve the HTTPS endpoint for attachment uploads/downloads against a
/// connected community server. Returns null when the server didn't
/// advertise an attachment port (older builds, or HTTP disabled).
/// Renderer drives the actual transfer — this command just hands over
/// the host/port/jwt so `fetch()` can run with native HTTP/2 +
/// connection reuse + Chromium's TLS stack.
pub async fn get_attachment_target(
    args: GetAttachmentTargetArgs,
) -> Result<Option<AttachmentTarget>, CoreError> {
    crate::rt::run(async move {
        let state_arc = state::shared();
        let s = state_arc.lock().await;
        let client = s.communities.get(&args.server_id).ok_or_else(|| {
            CoreError::other(format!("Not connected to community {}", args.server_id))
        })?;
        if client.attachment_port == 0 {
            return Ok(None);
        }
        Ok(Some(AttachmentTarget {
            host: client.host.clone(),
            port: client.attachment_port as i32,
            jwt: client.jwt.clone(),
            max_attachment_bytes: client.max_attachment_bytes,
            cert_fingerprint: client.cert_fingerprint.clone(),
        }))
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct TrustCertificateArgs {
    pub host: String,
    pub port: u16,
}

/// User-initiated re-trust after a `CERT_MISMATCH:` failure: forget the
/// pin (and central's expectation) for host:port so the next connect
/// accepts — and pins — whatever certificate is presented. Only do this
/// when the operator confirms the server was legitimately re-keyed.
pub async fn trust_certificate(args: TrustCertificateArgs) -> Result<(), CoreError> {
    crate::rt::run(async move {
        crate::net::pins::retrust(&args.host, args.port);
        Ok(())
    })
    .await
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

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
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
pub async fn request_drop_membership(args: DropMembershipArgs) -> Result<(), CoreError> {
    crate::rt::run(async move {
        let server_id: i64 = args.server_id.parse().map_err(|_| {
            CoreError::invalid(format!("Invalid server_id: {}", args.server_id))
        })?;

        let state_arc = state::shared();
        let (write_tx, data) = {
            let s = state_arc.lock().await;
            let token = s.token.clone();
            let central = s
                .central
                .as_ref()
                .ok_or_else(|| CoreError::other("Not connected to central server"))?;
            let tx = central
                .connection_write_tx()
                .ok_or_else(|| CoreError::other("Central connection lost"))?;
            let pkt = build_packet(
                packet::Type::MembershipRevokeReq,
                packet::Payload::MembershipRevokeReq(MembershipRevokeReq {
                    username: String::new(),
                    server_id,
                    uid: 0,
                }),
                token.as_deref(),
            );
            (tx, pkt)
        };

        match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(CoreError::other("Connection closed")),
            Err(_) => Err(CoreError::other("Send timed out")),
        }
    })
    .await
}

/// Sends UPDATE_SERVER_PICTURE_REQ over the community session for
/// server_id. The ack arrives as the `server_picture_update_responded`
/// event; on success, central then broadcasts SERVER_PICTURE_CHANGED
/// to all online members (including the requester). `picture`: the image
/// bytes in any owned container (the addon passes its JS Buffer as is).
pub async fn update_server_picture<B>(server_id: String, picture: B) -> Result<(), CoreError>
where
    B: AsRef<[u8]> + Send + 'static,
{
    crate::rt::run(async move {
        let state_arc = state::shared();
        let (write_tx, data) = {
            let s = state_arc.lock().await;
            let community = s.communities.get(&server_id).ok_or_else(|| {
                CoreError::other(format!(
                    "Not connected to community server {}",
                    server_id
                ))
            })?;
            let tx = community.connection_write_tx().ok_or_else(|| {
                CoreError::other("Community connection lost")
            })?;
            let pkt = build_packet(
                packet::Type::UpdateServerPictureReq,
                packet::Payload::UpdateServerPictureReq(UpdateServerPictureReq {
                    data: picture.as_ref().to_vec(),
                }),
                None,
            );
            (tx, pkt)
        };

        match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(CoreError::other("Connection closed")),
            Err(_) => Err(CoreError::other("Send timed out")),
        }
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct FetchServerPictureArgs {
    pub server_id: i32,
}

/// Sends FETCH_SERVER_PICTURE_REQ over the JWT-authed central
/// session. Response lands as the `server_picture_received` event,
/// with `data` already encoded as a data URL ready for <img src>.
pub async fn fetch_server_picture(args: FetchServerPictureArgs) -> Result<(), CoreError> {
    crate::rt::run(async move {
        let state_arc = state::shared();
        let (write_tx, data) = {
            let s = state_arc.lock().await;
            let central = s.central.as_ref().ok_or_else(|| {
                CoreError::other("Not connected to central server")
            })?;
            let tx = central.connection_write_tx().ok_or_else(|| {
                CoreError::other("Central connection lost")
            })?;
            let token = s.token.clone();
            let pkt = build_packet(
                packet::Type::FetchServerPictureReq,
                packet::Payload::FetchServerPictureReq(FetchServerPictureReq {
                    server_id: args.server_id,
                }),
                token.as_deref(),
            );
            (tx, pkt)
        };

        match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(CoreError::other("Connection closed")),
            Err(_) => Err(CoreError::other("Send timed out")),
        }
    })
    .await
}

// ─── Connecting (the commands above; central's router too) ─────────────

/// Connect (or reconnect) to community `server_id` at host:port with our
/// session token, presenting `invite_code` when joining by invite. Replaces
/// any existing connection to it.
pub(crate) async fn connect_with_invite(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(url: &str) -> Result<(String, u16, String), String> {
        parse_invite_link(ParseInviteLinkArgs { url: url.into() })
            .map(|p| (p.host, p.port, p.code))
            .map_err(|e| e.into_message())
    }

    #[test]
    fn invite_links_parse_in_every_accepted_shape() {
        assert_eq!(parse("decibell://invite/abc12"), Ok((String::new(), 0, "ABC12".into())));
        assert_eq!(parse(" decibell://invite/example.org:8082/abc/ "), Ok(("example.org".into(), 8082, "ABC".into())));
        assert_eq!(parse("decibell://invite/example.org/8082/abc"), Ok(("example.org".into(), 8082, "ABC".into())));
        assert_eq!(parse("decibell:invite/10.0.0.1:9/x"), Ok(("10.0.0.1".into(), 9, "X".into())));
        assert_eq!(parse("decibell:///invite/h%41st:1/x"), Ok(("hAst".into(), 1, "X".into())));
    }

    #[test]
    fn bad_invite_links_answer_as_before() {
        for (url, err) in [
            ("https://invite/abc", "Not a decibell:// URL"),
            ("decibell://join/abc", "URL is not an invite link"),
            ("decibell://invite/", "Invite code missing"),
            ("decibell://invite/host/abc", "Invite link missing port"),
            ("decibell://invite/host:99999/abc", "Invalid port in invite link"),
            ("decibell://invite/host/port/abc", "Invalid port in invite link"),
            ("decibell://invite/:8082/abc", "Host missing in invite link"),
        ] {
            assert_eq!(parse(url), Err(err.to_string()), "{url}");
        }
        let e = parse_invite_link(ParseInviteLinkArgs { url: "x".into() }).err().expect("refused");
        assert_eq!(e.kind(), crate::api::ErrorKind::InvalidArgument);
    }

    #[test]
    fn percent_decoding_leaves_bad_escapes_alone() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
