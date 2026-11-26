//! Community lifecycle, channels, roles, the roster, moderation, invites, pictures
//! (`features/servers/useServerEvents.ts`, plus `useAuthEvents`' `server_list_received`).
//!
//! The message listeners of that hook (`channel_message_*`) belong to the message model
//! (`todo::m2`), and `voice_force_notify` to voice (`todo::m3`).

use decibell_core::events::{
    BanListReceivedPayload, ChannelActionRespondedPayload, ChannelListUpdatedPayload,
    ChannelOverwritesReceivedPayload, ChannelUpdatedPayload, CommunityAuthRespondedPayload, ConnectionEventPayload,
    InviteCreateRespondedPayload, InviteListReceivedPayload, InviteRevokeRespondedPayload, MemberListReceivedPayload,
    MemberRemovePayload, MemberUpsertPayload, MembershipRevokedPayload, MembershipsReceivedPayload,
    ModActionRespondedPayload, RoleActionRespondedPayload, RoleListReceivedPayload, ServerInfo,
    ServerListReceivedPayload, ServerMetaUpdatedPayload, ServerPictureChangedPayload, ServerPictureReceivedPayload,
    ServerPictureUpdateRespondedPayload,
};
use gpui::{App, SharedString};

use super::todo;
use crate::Model;
use crate::actions;
use crate::ids::ServerId;
use crate::stores::community::{
    Ban, Channel, ChannelKind, Community, Invite, Member, MemberPage, Overwrite, Role, RosterCounts,
};
use crate::stores::navigation::ActiveView;
use crate::stores::overlays::{AuthErrorNotice, MembershipRevocationNotice};
use crate::stores::servers::Server;

fn community(model: &Model, server: ServerId, cx: &mut App) -> Community {
    model.communities.update(cx, |c, cx| c.ensure(server, cx))
}

/// `message || fallback` as a toast title.
fn or_fallback(message: &str, fallback: &'static str) -> SharedString {
    if message.is_empty() { SharedString::new_static(fallback) } else { message.to_string().into() }
}

// ── Lists from central ──────────────────────────────────────────────────────

/// Central's public directory. Discovery results live in their own list; the server bar
/// reads the memberships, so this never touches the user's own tiles.
pub fn server_list_received(model: &Model, p: &ServerListReceivedPayload, cx: &mut App) {
    model.servers.update(cx, |s, cx| {
        s.set_discoverable(p.servers.iter().map(Server::from).collect(), cx);
        for server in &p.servers {
            s.set_picture_version(ServerId::from_central(server.id), &server.picture_version, cx);
        }
    });
}

/// Auto-rejoin: the sign-in reply's memberships become tiles marked "connecting…" (the
/// core connects to each by itself) until each community answers.
pub fn memberships_received(model: &Model, p: &MembershipsReceivedPayload, cx: &mut App) {
    model.servers.update(cx, |s, cx| {
        s.merge(p.memberships.iter().map(Server::from), cx);
        for m in &p.memberships {
            s.set_picture_version(ServerId::from_central(m.id), &m.picture_version, cx);
        }
    });
    model
        .connections
        .update(cx, |c, cx| c.set_pending(p.memberships.iter().map(|m| ServerId::from_central(m.id)), cx));
}

/// Another device of ours joined a community: the core is connecting to it; its tile
/// shows now.
pub fn membership_added(model: &Model, p: &ServerInfo, cx: &mut App) {
    model.servers.update(cx, |s, cx| {
        s.merge([Server::from(p)], cx);
        s.set_picture_version(ServerId::from_central(p.id), &p.picture_version, cx);
    });
}

// ── A community answered our authentication ─────────────────────────────────

pub fn community_auth_responded(model: &Model, p: &CommunityAuthRespondedPayload, cx: &mut App) {
    let server = ServerId::from_wire(&p.server_id);
    if !p.success {
        // An auto-rejoin that was refused: the user was kicked or banned while offline.
        // Drop the stale membership at central and say so once.
        if model.connections.read(cx).is_pending(server) {
            model.connections.update(cx, |c, cx| c.remove_pending(server, cx));
            actions::servers::request_drop_membership(server, cx);
            let name = model.servers.read(cx).get(server).map(|s| s.name.to_string());
            let name = name.as_deref().unwrap_or("a community server");
            model.toasts.update(cx, |t, cx| {
                t.error("Membership revoked", Some(format!("You're no longer a member of {name}.").into()), cx)
            });
            return;
        }
        // The invite-redeem flow shows this inline…
        model.overlays.update(cx, |o, cx| {
            o.set_auth_error(
                Some(AuthErrorNotice {
                    server,
                    message: p.message.clone().into(),
                    error_code: p.error_code.clone().into(),
                }),
                cx,
            )
        });
        // …and a plain browse-view connect has nothing mounted to show it, so toast too.
        model.toasts.update(cx, |t, cx| {
            t.error("Couldn't join server", Some(or_fallback(&p.message, "Connection rejected.")), cx)
        });
        return;
    }
    // Invite joins open the connection under a provisional "host:port" id; the core
    // re-keys it onto central's id once the community reports it. Follow the re-key so
    // everything below lands under the real id.
    let requested = ServerId::from_wire(&p.requested_server_id);
    if !requested.is_none() && requested != server {
        if model.nav.read(cx).active_server == Some(requested) {
            model.nav.update(cx, |nav, cx| nav.set_active_server(Some(server), cx));
        }
        model.connections.update(cx, |c, cx| c.remove_connected(requested, cx));
    }
    // The community's message bucket is per session and starts full.
    todo::m2::reset_send_pacing(model, server, cx);
    // The voice / stream presence snapshot follows this answer and lists only non-empty
    // channels: drop what we had, or a channel that emptied while we were away keeps
    // ghost members.
    todo::m3::clear_server_presence(model, server, None, cx);
    // Make sure a tile exists: central's list may not name an unlisted community.
    model.servers.update(cx, |s, cx| {
        s.merge(
            [Server {
                id: server,
                name: p.server_name.clone().into(),
                description: p.server_description.clone().into(),
                host: p.host.clone().into(),
                port: p.port as i32,
                member_count: 0,
            }],
            cx,
        )
    });
    model.connections.update(cx, |c, cx| c.remove_pending(server, cx));
    // Nothing posted, edited or deleted while we were away reached the cached channels
    // (review B4): requests made on the old connection are never answered, cached
    // slices refetch or go windowed.
    let channels: Vec<Channel> = p.channels.iter().map(Channel::from).collect();
    todo::m2::community_authenticated(model, server, &channels, cx);

    let community = community(model, server, cx);
    let first_text = channels.iter().find(|c| c.kind == ChannelKind::Text).map(|c| c.id.clone());
    let has_channel = |id: &str| channels.iter().any(|c| &*c.id == id);
    // Auto-select the first text channel so the user lands somewhere after joining, but
    // only when this answer is for the community on screen and its channel isn't valid.
    // Auto-rejoin answers arrive one per membership: without the guard each would yank
    // the active channel.
    let select = {
        let nav = model.nav.read(cx);
        (nav.active_server == Some(server) && !nav.active_channel.as_deref().is_some_and(has_channel))
            .then_some(first_text)
            .flatten()
    };
    community.channels.update(cx, |c, cx| c.set_all(channels, cx));
    community.meta.update(cx, |m, cx| {
        m.set_identity(&p.server_name, &p.server_description, p.owner_uid, p.public_listing, cx);
        m.set_attachment_config(p.attachment_port, p.max_attachment_bytes, cx);
    });
    model.connections.update(cx, |c, cx| c.add_connected(server, cx));
    // The attachment endpoint and its token, for whoever transfers files.
    if p.attachment_port > 0 {
        todo::m2::attachment_target_changed(model, server, cx);
    }
    if let Some(channel) = select {
        model.nav.update(cx, |nav, cx| nav.set_active_channel(Some(channel), cx));
    }
    // The members sidebar: page 1 = the first stretch of the online-then-offline
    // sequence, our own row and the counts; deltas keep it live from here on. Always
    // fetched on (re)authentication.
    community.roster.update(cx, |r, cx| r.refetch(true, cx));
}

// ── Connection state of a community ─────────────────────────────────────────

pub fn connection_lost(model: &Model, p: &ConnectionEventPayload, cx: &mut App) {
    let Some(server_id) = p.server_id.as_deref().filter(|id| !id.is_empty()) else { return };
    if p.server_type != "community" {
        return;
    }
    let server = ServerId::from_wire(server_id);
    model.connections.update(cx, |c, cx| c.remove_connected(server, cx));
    // Their replies went down with the connection; the attachment token too.
    todo::m2::community_connection_lost(model, server, cx);
    // No updates arrive while we're cut off; show nothing rather than a roster that
    // silently goes stale.
    todo::m3::clear_server_presence(model, server, None, cx);
}

pub fn connection_restored(model: &Model, p: &ConnectionEventPayload, cx: &mut App) {
    let Some(server_id) = p.server_id.as_deref().filter(|id| !id.is_empty()) else { return };
    if p.server_type == "community" {
        let server = ServerId::from_wire(server_id);
        model.connections.update(cx, |c, cx| c.add_connected(server, cx));
    }
}

// ── Roster ──────────────────────────────────────────────────────────────────

/// Roster protocol: a paged snapshot plus live deltas with a revision. A delta that
/// doesn't continue the last applied revision means we missed one (a reconnect window):
/// page 1 is fetched again, once; deltas wait for it.
pub fn member_list_received(model: &Model, p: &MemberListReceivedPayload, cx: &mut App) {
    let roster = community(model, ServerId::from_wire(&p.server_id), cx).roster;
    if !p.lookup_uids.is_empty() {
        roster.update(cx, |r, cx| {
            if p.success {
                r.apply_lookup(p.extra.iter().map(Member::from).collect(), p.revision, cx);
            } else {
                r.lookup_refused(&p.lookup_uids, cx);
            }
        });
        return;
    }
    roster.update(cx, |r, cx| {
        if !p.success {
            // Refused (rate limited).
            r.page_refused(p.first_page, cx);
            return;
        }
        let consistent = r.apply_page(
            MemberPage {
                members: p.members.iter().map(Member::from).collect(),
                extra: p.extra.iter().map(Member::from).collect(),
                revision: p.revision,
                counts: RosterCounts { online_count: p.online_count, total_members: p.total_members },
                online_complete: p.online_complete,
                has_more: p.has_more,
                next_after: p.next_after.clone().into(),
                first_page: p.first_page,
            },
            cx,
        );
        if !consistent {
            r.refetch(false, cx);
        }
    });
}

pub fn member_upsert(model: &Model, p: &MemberUpsertPayload, cx: &mut App) {
    let roster = community(model, ServerId::from_wire(&p.server_id), cx).roster;
    let counts = RosterCounts { online_count: p.online_count, total_members: p.total_members };
    roster.update(cx, |r, cx| {
        if !r.upsert(Member::from(&p.member), p.revision, counts, cx) {
            r.refetch(false, cx);
        }
    });
}

pub fn member_remove(model: &Model, p: &MemberRemovePayload, cx: &mut App) {
    let roster = community(model, ServerId::from_wire(&p.server_id), cx).roster;
    let counts = RosterCounts { online_count: p.online_count, total_members: p.total_members };
    roster.update(cx, |r, cx| {
        if !r.remove(p.uid, p.revision, counts, cx) {
            r.refetch(false, cx);
        }
    });
}

// ── Channels, roles, overwrites ─────────────────────────────────────────────

/// The community's full ordered channel list, after any channel create / rename /
/// delete / reorder, or a permission change that moves what we can see.
pub fn channel_list_updated(model: &Model, p: &ChannelListUpdatedPayload, cx: &mut App) {
    let server = ServerId::from_wire(&p.server_id);
    let community = community(model, server, cx);
    let channels: Vec<Channel> = p.channels.iter().map(Channel::from).collect();
    let has = |id: &str| channels.iter().any(|c| &*c.id == id);
    // Drop cached state for channels that no longer exist: a future channel could reuse
    // the same slug and must refetch instead of inheriting stale history.
    let gone: Vec<_> =
        community.channels.read(cx).list().iter().filter(|c| !has(&c.id)).map(|c| c.id.clone()).collect();
    for channel in &gone {
        todo::m2::purge_channel_state(model, server, channel, cx);
    }
    // A channel we can no longer see (or that was deleted) gets no more presence
    // updates; drop its voice roster so it can't reappear stale.
    todo::m3::clear_server_presence(model, server, Some(&channels), cx);
    // If the active channel vanished, land on the first text channel.
    let reselect = {
        let nav = model.nav.read(cx);
        (nav.active_server == Some(server) && nav.active_channel.as_deref().is_some_and(|id| !has(id)))
            .then(|| channels.iter().find(|c| c.kind == ChannelKind::Text).map(|c| c.id.clone()))
    };
    community.channels.update(cx, |c, cx| c.set_all(channels, cx));
    if let Some(channel) = reselect {
        model.nav.update(cx, |nav, cx| nav.set_active_channel(channel, cx));
    }
}

pub fn channel_action_responded(model: &Model, p: &ChannelActionRespondedPayload, cx: &mut App) {
    if !p.success {
        model.toasts.update(cx, |t, cx| t.error(or_fallback(&p.message, "Channel action failed"), None, cx));
        return;
    }
    // Jump straight into a channel you just created (the list push that follows fills
    // in the sidebar entry).
    if p.action == "create"
        && let Some(channel) = &p.channel
        && channel.r#type == "text"
        && model.nav.read(cx).active_server == Some(ServerId::from_wire(&p.server_id))
    {
        let id = channel.id.clone().into();
        model.nav.update(cx, |nav, cx| nav.set_active_channel(Some(id), cx));
    }
}

/// A channel's settings changed (retention, bitrate, slowmode, topic, a rename push).
pub fn channel_updated(model: &Model, p: &ChannelUpdatedPayload, cx: &mut App) {
    let Some(channel) = p.channel.as_ref().filter(|_| p.success) else { return };
    let server = ServerId::from_wire(&p.server_id);
    let community = community(model, server, cx);
    let previous_bitrate = community.channels.read(cx).get(&channel.id).map(|c| c.voice_bitrate_kbps);
    let channel = Channel::from(channel);
    // Live bitrate: the voice channel we're in is retuned in place, no rejoin.
    todo::m3::channel_updated(model, server, &channel, previous_bitrate, cx);
    community.channels.update(cx, |c, cx| c.upsert(channel, cx));
}

pub fn role_list_received(model: &Model, p: &RoleListReceivedPayload, cx: &mut App) {
    if !p.success {
        return;
    }
    let community = community(model, ServerId::from_wire(&p.server_id), cx);
    let roles: Vec<Role> = p.roles.iter().map(Role::from).collect();
    // A deleted role is announced only by this list: its holders get no member delta.
    community.roster.update(cx, |r, cx| r.prune_roles(&roles, cx));
    community.roles.update(cx, |r, cx| r.set(roles, cx));
}

pub fn channel_overwrites_received(model: &Model, p: &ChannelOverwritesReceivedPayload, cx: &mut App) {
    if !p.success {
        return;
    }
    let community = community(model, ServerId::from_wire(&p.server_id), cx);
    let overwrites = p.overwrites.iter().map(Overwrite::from).collect();
    community
        .channels
        .update(cx, |c, cx| c.set_overwrites(p.channel_id.clone().into(), overwrites, cx));
}

/// Successful role actions confirm themselves through the pushed role list and member
/// deltas: only denials need showing.
pub fn role_action_responded(model: &Model, p: &RoleActionRespondedPayload, cx: &mut App) {
    if !p.success {
        model.toasts.update(cx, |t, cx| t.error(or_fallback(&p.message, "Role action failed"), None, cx));
    }
}

// ── Moderation ──────────────────────────────────────────────────────────────

pub fn mod_action_responded(model: &Model, p: &ModActionRespondedPayload, cx: &mut App) {
    if !p.success {
        // A refused message already showed through the typed CHANNEL_MSG_REJECTED; this
        // legacy echo of it is for older clients.
        if p.action == "message" && todo::m2::legacy_rejection_suppressed(model, cx) {
            return;
        }
        // The community denied the kick / ban / leave: say so instead of leaving the
        // member silently in the list.
        model.toasts.update(cx, |t, cx| t.error(or_fallback(&p.message, "Action failed"), None, cx));
        return;
    }
    if p.action == "leave" {
        let server = ServerId::from_wire(&p.server_id);
        model.connections.update(cx, |c, cx| c.remove_connected(server, cx));
        model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Browse, cx));
    }
    // Kick / ban / nickname / unban confirm themselves through the pushed deltas.
}

/// We were kicked or banned.
pub fn membership_revoked(model: &Model, p: &MembershipRevokedPayload, cx: &mut App) {
    let server = ServerId::from_wire(&p.server_id);
    model.connections.update(cx, |c, cx| c.remove_connected(server, cx));
    todo::m3::clear_server_presence(model, server, None, cx);
    model.overlays.update(cx, |o, cx| {
        o.set_membership_revocation(
            Some(MembershipRevocationNotice {
                server,
                action: p.action.clone().into(),
                reason: p.reason.clone().into(),
                actor: p.actor.clone().into(),
                expires_at: p.expires_at,
            }),
            cx,
        )
    });
    if model.nav.read(cx).active_server == Some(server) {
        model.nav.update(cx, |nav, cx| {
            nav.set_active_server(None, cx);
            nav.set_active_channel(None, cx);
            nav.set_active_view(ActiveView::Home, cx);
        });
    }
}

pub fn ban_list_received(model: &Model, p: &BanListReceivedPayload, cx: &mut App) {
    if !p.success {
        return;
    }
    let moderation = community(model, ServerId::from_wire(&p.server_id), cx).moderation;
    moderation.update(cx, |m, cx| m.set_bans(p.entries.iter().map(Ban::from).collect(), p.truncated, cx));
}

/// In-app rename / ownership transfer: the community pushes the new meta to everyone;
/// the roster deltas carry the owner flag flips.
pub fn server_meta_updated(model: &Model, p: &ServerMetaUpdatedPayload, cx: &mut App) {
    let server = ServerId::from_wire(&p.server_id);
    let meta = community(model, server, cx).meta;
    meta.update(cx, |m, cx| {
        m.set_identity(&p.server_name, &p.server_description, p.owner_uid, p.public_listing, cx)
    });
    // Keep the server bar tile's name in step too.
    model.servers.update(cx, |s, cx| s.rename(server, &p.server_name, &p.server_description, cx));
}

// ── Invites ─────────────────────────────────────────────────────────────────

pub fn invite_list_received(model: &Model, p: &InviteListReceivedPayload, cx: &mut App) {
    if !p.success {
        return;
    }
    let moderation = community(model, ServerId::from_wire(&p.server_id), cx).moderation;
    moderation.update(cx, |m, cx| m.set_invites(p.invites.iter().map(Invite::from).collect(), p.truncated, cx));
}

pub fn invite_create_responded(model: &Model, p: &InviteCreateRespondedPayload, cx: &mut App) {
    if let Some(invite) = p.invite.as_ref().filter(|_| p.success) {
        let moderation = community(model, ServerId::from_wire(&p.server_id), cx).moderation;
        moderation.update(cx, |m, cx| m.upsert_invite(Invite::from(invite), cx));
    } else if !p.success && p.request_id == 0 {
        // A request with an id (a personal invite) reports to its caller instead.
        model.toasts.update(cx, |t, cx| t.error(or_fallback(&p.message, "Failed to create invite"), None, cx));
    }
}

pub fn invite_revoke_responded(model: &Model, p: &InviteRevokeRespondedPayload, cx: &mut App) {
    if p.success {
        let moderation = community(model, ServerId::from_wire(&p.server_id), cx).moderation;
        moderation.update(cx, |m, cx| m.remove_invite(&p.code, cx));
    } else {
        model.toasts.update(cx, |t, cx| t.error(or_fallback(&p.message, "Failed to revoke invite"), None, cx));
    }
}

// ── Pictures ────────────────────────────────────────────────────────────────

pub fn server_picture_update_responded(model: &Model, p: &ServerPictureUpdateRespondedPayload, cx: &mut App) {
    // An open picture editor shows its own refusal. Success: the broadcast
    // (`server_picture_changed`) moves the version, and the editor closes on this answer.
    let server = ServerId::from_wire(&p.server_id);
    if !p.success && !model.servers.read(cx).picture_saves_in_flight.contains(&server) {
        model
            .toasts
            .update(cx, |t, cx| t.error("Couldn't update server picture", Some(p.message.clone().into()), cx));
    }
}

pub fn server_picture_changed(model: &Model, p: &ServerPictureChangedPayload, cx: &mut App) {
    model
        .servers
        .update(cx, |s, cx| s.set_picture_version(ServerId::from_central(p.server_id), &p.version, cx));
}

pub fn server_picture_received(model: &Model, p: ServerPictureReceivedPayload, cx: &mut App) {
    // Empty data means the community has no picture set (or the id is unknown). A fetch
    // for a version that is no longer current is dropped by the store.
    if p.data.is_empty() {
        return;
    }
    model
        .servers
        .update(cx, |s, cx| s.set_picture_data(ServerId::from_central(p.server_id), &p.version, p.data.into(), cx));
}
