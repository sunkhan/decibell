//! Communities: opening, joining, leaving, the roster's requests, invite lookups
//! (`features/servers/openServer.ts`, `ServerBrowseView.tsx`, `ServerBar.tsx`,
//! `useServerEvents.ts`' `refetchRoster`, `features/members/memberLookup.ts`).

use decibell_core::api;
use gpui::{App, SharedString};

use crate::Model;
use crate::call::{call, offer_retrust};
use crate::error::{CertMismatch, UiError};
use crate::ids::ServerId;
use crate::invite_link::parse_invite_link;
use crate::stores::community::MemberRequest;
use crate::stores::invites::ResolvedInvite;
use crate::stores::navigation::ActiveView;
use crate::stores::overlays::Retry;
use crate::stores::roster_order::{ROSTER_FIRST_PAGE, ROSTER_PAGE};

/// Ask central for the public directory; the answer is `server_list_received`.
pub fn request_server_list(cx: &mut App) {
    call(cx, api::servers::request_server_list(), |result, _| {
        if let Err(e) = result {
            log::debug!("request_server_list: {e}");
        }
    });
}

/// Show a community (`openServer`): make it the active one, switch to the server view,
/// and land on a channel. The current channel stays if it belongs to this community;
/// otherwise the first text channel. With no channels yet (a join in flight) there is
/// nothing to pick: the authentication answer's handler selects the first text channel
/// once it lands, because the community is active by then.
pub fn open_server(server: ServerId, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let current = model.nav.read(cx).active_channel.clone();
    model.nav.update(cx, |nav, cx| {
        nav.set_active_server(Some(server), cx);
        nav.set_active_view(ActiveView::Server, cx);
    });
    let (holds_current, first_text) = match model.community(server, cx) {
        Some(community) => {
            let channels = community.channels.read(cx);
            (
                current.as_ref().is_some_and(|id| channels.contains(id)),
                channels.first_text().map(|c| c.id.clone()),
            )
        }
        None => (false, None),
    };
    if holds_current {
        return;
    }
    // Clear first: a stale channel from another community must never sit under this one.
    model.nav.update(cx, |nav, cx| {
        nav.set_active_channel(None, cx);
        if let Some(first_text) = first_text {
            nav.set_active_channel(Some(first_text), cx);
        }
    });
}

/// The server bar's Home tile.
pub fn go_home(cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    model.nav.update(cx, |nav, cx| {
        nav.set_active_server(None, cx);
        nav.set_active_channel(None, cx);
        nav.set_active_view(ActiveView::Home, cx);
    });
}

/// Join a community from the directory (`handleConnect`): connect, then show it. A
/// certificate that no longer matches its pin opens the re-trust prompt instead of an
/// error (`done` then gets `Ok`). `done`: the browse view clears its "connecting" state
/// and shows a failure inline (as `String(err)`: [`UiError::js_string`]).
pub fn connect(
    server: ServerId,
    host: String,
    port: i32,
    cx: &mut App,
    done: impl FnOnce(Result<(), UiError>, &mut App) + 'static,
) {
    let args = api::servers::ConnectToCommunityArgs { server_id: server.wire(), host, port };
    call(cx, api::servers::connect_to_community(args), move |result, cx| match result {
        Ok(()) => {
            open_server(server, cx);
            done(Ok(()), cx);
        }
        Err(e) if offer_retrust(&e, None, cx) => done(Ok(()), cx),
        Err(e) => done(Err(e), cx),
    });
}

/// Join with an invite at a known endpoint (`handleRedeemInvite`'s last step, the
/// deep-link prompt, an invite card's Join). The connection opens under the provisional
/// id `"host:port"`; the community's answer re-keys it onto central's id
/// (`events/servers.rs`), and the active community follows.
pub fn redeem_invite(
    host: String,
    port: u16,
    code: String,
    cx: &mut App,
    done: impl FnOnce(Result<(), UiError>, &mut App) + 'static,
) {
    let server = ServerId::from_wire(&format!("{host}:{port}"));
    let args = api::servers::RedeemInviteArgs {
        server_id: server.wire(),
        host,
        port: port as i32,
        invite_code: code,
    };
    call(cx, api::servers::redeem_invite(args), move |result, cx| match result {
        Ok(()) => {
            open_server(server, cx);
            done(Ok(()), cx);
        }
        Err(e) if offer_retrust(&e, None, cx) => done(Ok(()), cx),
        Err(e) => done(Err(e), cx),
    });
}

/// Leave a community. The answer is `mod_action_responded` with action `leave`, which
/// marks it disconnected and shows the browse view.
pub fn leave(server: ServerId, cx: &mut App) {
    let args = api::community::LeaveServerArgs { server_id: server.wire() };
    call(cx, api::community::leave_server(args), |result, _| {
        if let Err(e) = result {
            log::error!("leave_server: {e}");
        }
    });
}

/// A clicked or forwarded `decibell://invite/…` link (`openInviteLink`, the
/// `deep_link_received` listener): the confirm-then-join prompt reads
/// `ServersStore::pending_invite`. False when the link doesn't parse.
pub fn open_invite_link(url: &str, cx: &mut App) -> bool {
    let Some(parsed) = parse_invite_link(url) else {
        log::warn!("[deep-link] could not parse: {url}");
        return false;
    };
    let Some(model) = cx.try_global::<Model>().cloned() else { return false };
    model.servers.update(cx, |s, cx| s.set_pending_invite(Some(parsed), cx));
    true
}

/// A tile saw a picture version without bytes (`ServerBar`'s fetch effect): ask central
/// for the picture; it arrives as `server_picture_received`.
pub fn fetch_server_picture(server: ServerId, cx: &mut App) {
    let Some(server_id) = server.central() else { return };
    let args = api::servers::FetchServerPictureArgs { server_id };
    call(cx, api::servers::fetch_server_picture(args), |result, _| {
        if let Err(e) = result {
            log::debug!("fetch_server_picture: {e}");
        }
    });
}

/// Trust the certificate a server now presents (the re-trust prompt's confirm), then run
/// the notice's retry.
pub fn trust_certificate(cert: CertMismatch, retry: Option<Retry>, cx: &mut App) {
    let args = api::servers::TrustCertificateArgs {
        host: cert.host.to_string(),
        port: cert.port,
    };
    call(cx, api::servers::trust_certificate(args), move |result, cx| match result {
        Ok(()) => {
            if let Some(model) = cx.try_global::<Model>().cloned() {
                model.overlays.update(cx, |o, cx| o.set_cert_mismatch(None, cx));
            }
            if let Some(retry) = retry {
                retry(cx);
            }
        }
        Err(e) => crate::call::show_error(&e, "Couldn't trust the certificate", cx),
    });
}

/// The roster's requests (`Communities` hands this to every `Roster`).
pub(crate) fn send_member_request(server: ServerId, request: MemberRequest, cx: &mut App) {
    let server_id = server.wire();
    let first_page = request == MemberRequest::FirstPage;
    let args = match request {
        MemberRequest::FirstPage => api::community::ListMembersArgs {
            server_id,
            after: Some(String::new()),
            limit: Some(ROSTER_FIRST_PAGE),
            uids: None,
        },
        MemberRequest::NextPage { after } => api::community::ListMembersArgs {
            server_id,
            after: Some(after.to_string()),
            limit: Some(ROSTER_PAGE),
            uids: None,
        },
        MemberRequest::Lookup(uids) => api::community::ListMembersArgs {
            server_id,
            after: None,
            limit: None,
            uids: Some(uids),
        },
    };
    call(cx, api::community::list_members(args), move |result: Result<(), UiError>, cx| {
        let Err(e) = result else { return };
        log::error!("list_members: {e}");
        // Not sent (the connection is gone): the reconnect's first page starts over.
        if let Some(community) = cx.try_global::<Model>().and_then(|m| m.community(server, cx)) {
            community.roster.update(cx, |roster, cx| {
                if first_page {
                    roster.set_refetching(false, cx);
                } else {
                    roster.set_loading_more(false, cx);
                }
            });
        }
    });
}

/// The invite cards' lookup (`InviteResolve`'s resolver).
pub(crate) fn resolve_invite(code: SharedString, cx: &mut App) {
    let args = api::servers::ResolveInviteCodeArgs { code: code.to_string() };
    call(cx, api::servers::resolve_invite_code(args), move |result, cx| {
        let result = result.map(|r| ResolvedInvite {
            host: r.host.into(),
            port: r.port,
            code: r.code.into(),
            server_id: r.server_id,
            server_name: r.server_name.into(),
            server_description: r.server_description.into(),
            member_count: r.member_count,
            picture_version: r.picture_version.into(),
        });
        if let Some(model) = cx.try_global::<Model>().cloned() {
            model.invites.update(cx, |invites, cx| invites.finish(code, result, cx));
        }
    });
}

/// Tell central to drop a membership a community no longer honours (auto-rejoin refused:
/// kicked or banned while offline).
pub(crate) fn request_drop_membership(server: ServerId, cx: &mut App) {
    let args = api::servers::DropMembershipArgs { server_id: server.wire() };
    call(cx, api::servers::request_drop_membership(args), |result: Result<(), UiError>, _| {
        if let Err(e) = result {
            log::error!("[auto-rejoin] request_drop_membership: {e}");
        }
    });
}

