//! `#[napi]` wrappers over `decibell_core::api::servers`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::servers::{
    AttachmentTarget, ConnectToCommunityArgs, DisconnectFromCommunityArgs, DropMembershipArgs,
    FetchServerPictureArgs, GetAttachmentTargetArgs, ParseInviteLinkArgs, ParsedInviteLink,
    RedeemInviteArgs, ResolveInviteCodeArgs, ResolvedInvite, TrustCertificateArgs,
};

// The struct with a JS Buffer stays here (the core takes plain bytes); every
// other argument and result struct is the core's.

#[napi(object)]
pub struct UpdateServerPictureArgs {
    pub server_id: String,
    pub data: napi::bindgen_prelude::Buffer,
}

/// Ask the central server for the user's server list. The response
/// arrives asynchronously as a `server_list_received` event.
#[napi]
pub async fn request_server_list() -> napi::Result<()> {
    api::servers::request_server_list()
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn connect_to_community(args: ConnectToCommunityArgs) -> napi::Result<()> {
    api::servers::connect_to_community(args)
        .await
        .map_err(napi::Error::from)
}

/// Connect to a community server while consuming an invite code so the
/// user joins (becomes a persistent member) as part of the handshake.
#[napi]
pub async fn redeem_invite(args: RedeemInviteArgs) -> napi::Result<()> {
    api::servers::redeem_invite(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn disconnect_from_community(args: DisconnectFromCommunityArgs) -> napi::Result<()> {
    api::servers::disconnect_from_community(args)
        .await
        .map_err(napi::Error::from)
}

/// Parse a `decibell://` invite URL into its components. Accepted shapes:
///   decibell://invite/<code>                 (host "" / port 0: resolve
///                                             the code via central)
///   decibell://invite/<host>:<port>/<code>
///   decibell://invite/<host>/<port>/<code>
///   decibell:invite/<host>:<port>/<code>
/// The renderer parses links itself (features/servers/inviteLink.ts);
/// this stays for API completeness and older callers.
#[napi]
pub fn parse_invite_link(args: ParseInviteLinkArgs) -> napi::Result<ParsedInviteLink> {
    api::servers::parse_invite_link(args).map_err(napi::Error::from)
}

/// Ask the central server to resolve a raw invite code to a community
/// host:port, plus the community's directory row for the pre-join
/// preview. Returns an error describing why the code couldn't be
/// resolved (unknown, expired, central not reachable). Times out after
/// 5 seconds.
#[napi]
pub async fn resolve_invite_code(args: ResolveInviteCodeArgs) -> napi::Result<ResolvedInvite> {
    api::servers::resolve_invite_code(args)
        .await
        .map_err(napi::Error::from)
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
    api::servers::get_attachment_target(args)
        .await
        .map_err(napi::Error::from)
}

/// User-initiated re-trust after a `CERT_MISMATCH:` failure: forget the
/// pin (and central's expectation) for host:port so the next connect
/// accepts — and pins — whatever certificate is presented. Only do this
/// when the operator confirms the server was legitimately re-keyed.
#[napi]
pub async fn trust_certificate(args: TrustCertificateArgs) -> napi::Result<()> {
    api::servers::trust_certificate(args)
        .await
        .map_err(napi::Error::from)
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
    api::servers::request_drop_membership(args)
        .await
        .map_err(napi::Error::from)
}

/// Sends UPDATE_SERVER_PICTURE_REQ over the community session for
/// server_id. The ack arrives as the `server_picture_update_responded`
/// event; on success, central then broadcasts SERVER_PICTURE_CHANGED
/// to all online members (including the requester).
#[napi]
pub async fn update_server_picture(args: UpdateServerPictureArgs) -> napi::Result<()> {
    api::servers::update_server_picture(args.server_id, args.data)
        .await
        .map_err(napi::Error::from)
}

/// Sends FETCH_SERVER_PICTURE_REQ over the JWT-authed central
/// session. Response lands as the `server_picture_received` event,
/// with `data` already encoded as a data URL ready for <img src>.
#[napi]
pub async fn fetch_server_picture(args: FetchServerPictureArgs) -> napi::Result<()> {
    api::servers::fetch_server_picture(args)
        .await
        .map_err(napi::Error::from)
}
