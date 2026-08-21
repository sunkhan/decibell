//! Community server client. Each entry in `AppState.communities` owns
//! one of these. Auth handshake is part of `connect`; subsequent
//! requests (channel ops, voice, streaming, attachments) go through
//! the protocol-helper methods (send_channel_message, etc.) and are
//! wired up to user-facing commands as their feature PRs land.
//!
//! PR3 scope: route_packets handles only CommunityAuthRes — that's
//! the response that establishes a community session and ships the
//! channel list. Future PRs add handlers for messages, channel
//! lifecycle, voice presence, streaming, invites, members.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use super::connection::{backoff_duration, build_packet, Connection};
use super::proto::*;
use crate::events;
use crate::state::AppState;

/// Map a wire-format Attachment to the bus-side AttachmentPayload.
/// Handles the protobuf enum → string conversion for `kind`.
fn channel_info_payload(c: ChannelInfo) -> events::ChannelInfoPayload {
    events::ChannelInfoPayload {
        id: c.id,
        name: c.name,
        r#type: match channel_info::Type::try_from(c.r#type) {
            Ok(channel_info::Type::Text) => "text",
            Ok(channel_info::Type::Voice) => "voice",
            Ok(channel_info::Type::Category) => "category",
            Err(_) => "unknown",
        }
        .to_string(),
        voice_bitrate_kbps: c.voice_bitrate_kbps,
        retention_days_text: c.retention_days_text,
        retention_days_image: c.retention_days_image,
        retention_days_video: c.retention_days_video,
        retention_days_document: c.retention_days_document,
        retention_days_audio: c.retention_days_audio,
        my_permissions: c.my_permissions,
        slowmode_seconds: c.slowmode_seconds,
    }
}

fn member_info_payload(m: MemberInfo) -> events::MemberInfoPayload {
    events::MemberInfoPayload {
        username: m.username,
        joined_at: m.joined_at,
        nickname: m.nickname,
        is_owner: m.is_owner,
        is_online: m.is_online,
        role_ids: m.role_ids,
        timed_out_until: m.timed_out_until,
    }
}

fn map_attachment(a: Attachment) -> events::AttachmentPayload {
    events::AttachmentPayload {
        id: a.id,
        message_id: a.message_id,
        kind: match attachment::Kind::try_from(a.kind) {
            Ok(attachment::Kind::Image) => "image",
            Ok(attachment::Kind::Video) => "video",
            Ok(attachment::Kind::Document) => "document",
            Ok(attachment::Kind::Audio) => "audio",
            Err(_) => "unknown",
        }
        .to_string(),
        filename: a.filename,
        mime: a.mime,
        size_bytes: a.size_bytes,
        url: a.url,
        position: a.position,
        created_at: a.created_at,
        purged_at: a.purged_at,
        width: a.width,
        height: a.height,
        thumbnail_size_bytes: a.thumbnail_size_bytes,
        thumbnail_sizes_mask: a.thumbnail_sizes_mask,
        duration_ms: a.duration_ms,
        placeholder: a.placeholder,
    }
}

pub struct CommunityClient {
    connection: Option<Connection>,
    router_task: Option<JoinHandle<()>>,
    reconnect_task: Option<JoinHandle<()>>,
    ping_task: Option<JoinHandle<()>>,
    pub server_id: String,
    pub host: String,
    pub port: u16,
    pub jwt: String,
    /// Populated from CommunityAuthResponse. 0 means the server didn't
    /// report an attachment endpoint (old server or HTTP disabled).
    pub attachment_port: u16,
    /// Per-file upload cap reported by the server. 0 = unlimited.
    pub max_attachment_bytes: i64,
    /// Set once the server has told us this connection is over for good:
    /// a failed COMMUNITY_AUTH_RES (the server closes the socket after
    /// every rejection, and the same JWT with no invite can't do better
    /// next time), a MEMBERSHIP_REVOKED (kick/ban), or a successful leave.
    /// The read loop consults it when the socket drops: without it the
    /// reconnect loop ran forever after a kick — re-auth, get `not_member`,
    /// toast "Couldn't join server", back off, repeat every 30 s.
    terminated: Arc<AtomicBool>,
}

impl CommunityClient {
    /// Connect to a community server and authenticate with JWT.
    /// If `invite_code` is supplied, the server will consume it to grant
    /// membership as part of the same handshake.
    pub async fn connect(
        server_id: String,
        host: String,
        port: u16,
        jwt: String,
        invite_code: Option<String>,
        state: Arc<Mutex<AppState>>,
    ) -> Result<Self, String> {
        let (connection, read_rx) = Connection::connect(&host, port).await?;

        // Send CommunityAuthRequest
        let auth_data = build_packet(
            packet::Type::CommunityAuthReq,
            packet::Payload::CommunityAuthReq(CommunityAuthRequest {
                jwt_token: jwt.clone(),
                invite_code: invite_code.unwrap_or_default(),
            }),
            Some(&jwt),
        );
        connection.send(auth_data).await?;

        let sid = server_id.clone();
        let terminated = Arc::new(AtomicBool::new(false));
        let router_task = tokio::spawn(Self::route_packets(
            read_rx,
            state.clone(),
            sid,
            terminated.clone(),
        ));

        // Keepalive ping every 30s. Same lock-free pattern as central.
        let ping_write_tx = connection.clone_write_tx();
        let ping_jwt = jwt.clone();
        let ping_task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let data = build_packet(
                    packet::Type::ClientPing,
                    packet::Payload::Handshake(Handshake {
                        protocol_version: 0,
                        client_id: String::new(),
                    }),
                    Some(&ping_jwt),
                );
                let send_result = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    ping_write_tx.send(data),
                )
                .await;
                match send_result {
                    Ok(Ok(())) => {}
                    _ => break,
                }
            }
        });

        Ok(CommunityClient {
            connection: Some(connection),
            router_task: Some(router_task),
            reconnect_task: None,
            ping_task: Some(ping_task),
            server_id,
            host,
            port,
            jwt,
            attachment_port: 0,
            max_attachment_bytes: 0,
            terminated,
        })
    }

    /// Clone the underlying write channel for direct sends that bypass AppState lock.
    pub fn connection_write_tx(&self) -> Option<mpsc::Sender<Vec<u8>>> {
        self.connection.as_ref().map(|c| c.clone_write_tx())
    }

    /// Send a raw packet.
    pub async fn send(&self, data: Vec<u8>) -> Result<(), String> {
        match &self.connection {
            Some(conn) => conn.send(data).await,
            None => Err("Not connected to community server".to_string()),
        }
    }

    /// Start screen sharing in a voice channel. Server creates a stream
    /// registry entry and broadcasts StreamPresenceUpdate to peers.
    pub async fn start_stream(
        &self,
        channel_id: &str,
        target_fps: i32,
        target_bitrate_kbps: i32,
        has_audio: bool,
        resolution_width: u32,
        resolution_height: u32,
        chosen_codec: i32,
        enforced_codec: i32,
    ) -> Result<(), String> {
        let data = build_packet(
            packet::Type::StartStreamReq,
            packet::Payload::StartStreamReq(StartStreamRequest {
                channel_id: channel_id.into(),
                target_fps,
                target_bitrate_kbps,
                has_audio,
                resolution_width,
                resolution_height,
                chosen_codec,
                enforced_codec,
            }),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    pub async fn stop_stream(&self, channel_id: &str) -> Result<(), String> {
        let data = build_packet(
            packet::Type::StopStreamReq,
            packet::Payload::StopStreamReq(StopStreamRequest {
                channel_id: channel_id.into(),
            }),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    pub async fn watch_stream(&self, channel_id: &str, target_username: &str) -> Result<(), String> {
        let data = build_packet(
            packet::Type::WatchStreamReq,
            packet::Payload::WatchStreamReq(WatchStreamRequest {
                channel_id: channel_id.into(),
                target_username: target_username.into(),
            }),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    pub async fn stop_watching(
        &self,
        channel_id: &str,
        target_username: &str,
    ) -> Result<(), String> {
        let data = build_packet(
            packet::Type::StopWatchingReq,
            packet::Payload::StopWatchingReq(StopWatchingRequest {
                channel_id: channel_id.into(),
                target_username: target_username.into(),
            }),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    /// Build the StreamThumbnailUpdate packet and clone the write channel,
    /// without awaiting. This lets a caller holding the AppState lock
    /// release it *before* awaiting the send — holding the global mutex
    /// across a network send (thumbnails fire every ~3s) stalls every other
    /// command. Returns None if the connection is down. Server enforces
    /// identity from the session, so `owner_username` is left blank.
    pub fn thumbnail_send_parts(
        &self,
        channel_id: &str,
        jpeg_data: &[u8],
    ) -> Option<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)> {
        let tx = self.connection_write_tx()?;
        let data = build_packet(
            packet::Type::StreamThumbnailUpdate,
            packet::Payload::StreamThumbnailUpdate(StreamThumbnailUpdate {
                channel_id: channel_id.into(),
                owner_username: String::new(),
                thumbnail_data: jpeg_data.to_vec(),
            }),
            Some(&self.jwt),
        );
        Some((tx, data))
    }

    /// Disconnect from the community server. Stops reconnection and keepalive.
    pub fn disconnect(&mut self) {
        if let Some(task) = self.ping_task.take() {
            task.abort();
        }
        if let Some(task) = self.reconnect_task.take() {
            task.abort();
        }
        if let Some(task) = self.router_task.take() {
            task.abort();
        }
        if let Some(conn) = self.connection.take() {
            conn.shutdown();
        }
    }

    /// Start reconnection loop. Called when the read loop ends unexpectedly.
    fn start_reconnect(
        server_id: String,
        host: String,
        port: u16,
        jwt: String,
        state: Arc<Mutex<AppState>>,
        terminated: Arc<AtomicBool>,
    ) {
        let sid_for_task = server_id.clone();
        let state_for_store = state.clone();
        let task = tokio::spawn(async move {
            events::emit_connection_lost("community", Some(sid_for_task.clone()));

            let mut attempt = 0u32;
            loop {
                let delay = backoff_duration(attempt);
                log::info!(
                    "Community {} reconnect attempt {} in {:?}",
                    sid_for_task,
                    attempt + 1,
                    delay
                );
                tokio::time::sleep(delay).await;

                match Connection::connect(&host, port).await {
                    Ok((connection, read_rx)) => {
                        log::info!("Reconnected to community server {}", sid_for_task);

                        // Re-authenticate. No invite code on reconnect — the
                        // user is already a member at this point. Text channels
                        // are implicit subscriptions, so no per-channel rejoin
                        // is needed; auth success alone restores delivery.
                        let auth_data = build_packet(
                            packet::Type::CommunityAuthReq,
                            packet::Payload::CommunityAuthReq(CommunityAuthRequest {
                                jwt_token: jwt.clone(),
                                invite_code: String::new(),
                            }),
                            Some(&jwt),
                        );
                        let _ = connection.send(auth_data).await;

                        let sid = sid_for_task.clone();
                        let router = tokio::spawn(Self::route_packets(
                            read_rx,
                            state.clone(),
                            sid,
                            terminated.clone(),
                        ));

                        // Restart keepalive ping task
                        let ping_write_tx = connection.clone_write_tx();
                        let ping_jwt = jwt.clone();
                        let ping_task = tokio::spawn(async move {
                            loop {
                                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                                let data = build_packet(
                                    packet::Type::ClientPing,
                                    packet::Payload::Handshake(Handshake {
                                        protocol_version: 0,
                                        client_id: String::new(),
                                    }),
                                    Some(&ping_jwt),
                                );
                                let send_result = tokio::time::timeout(
                                    std::time::Duration::from_secs(5),
                                    ping_write_tx.send(data),
                                )
                                .await;
                                match send_result {
                                    Ok(Ok(())) => {}
                                    _ => break,
                                }
                            }
                        });

                        let mut s = state.lock().await;
                        if let Some(client) = s.communities.get_mut(&sid_for_task) {
                            if let Some(old_ping) = client.ping_task.take() {
                                old_ping.abort();
                            }
                            client.connection = Some(connection);
                            client.router_task = Some(router);
                            client.reconnect_task = None;
                            client.ping_task = Some(ping_task);
                        }
                        drop(s);

                        events::emit_connection_restored("community", Some(sid_for_task.clone()));
                        return;
                    }
                    Err(e) => {
                        log::warn!("Community {} reconnect failed: {}", sid_for_task, e);
                        attempt += 1;
                    }
                }
            }
        });

        // Store the reconnect task handle
        tokio::spawn(async move {
            let mut s = state_for_store.lock().await;
            if let Some(client) = s.communities.get_mut(&server_id) {
                client.reconnect_task = Some(task);
            }
        });
    }

    /// Route incoming packets to bus events.
    ///
    /// PR3 surface: CommunityAuthRes. Other handlers (channel
    /// messages, history, voice presence, streaming, invites,
    /// members) port in their feature PRs.
    async fn route_packets(
        mut read_rx: mpsc::Receiver<Packet>,
        state: Arc<Mutex<AppState>>,
        mut server_id: String,
        terminated: Arc<AtomicBool>,
    ) {
        while let Some(packet) = read_rx.recv().await {
            match packet.payload {
                Some(packet::Payload::CommunityAuthRes(resp)) => {
                    let channels: Vec<events::ChannelInfoPayload> = resp
                        .channels
                        .into_iter()
                        .map(channel_info_payload)
                        .collect();
                    if !resp.success {
                        // The server closes the socket after any rejection
                        // (auth / not_member / banned / invalid_invite) and
                        // none of those change by retrying the same JWT
                        // without an invite — don't reconnect.
                        terminated.store(true, Ordering::SeqCst);
                    }
                    let requested_server_id = server_id.clone();
                    let (mut host, mut port) = (String::new(), 0u16);
                    if resp.success {
                        let mut s = state.lock().await;
                        // Re-key onto central's id. An invite join only
                        // knows host:port, so the renderer opened this
                        // connection under a synthetic "host:port" id; the
                        // server now tells us the id central's directory
                        // and membership list use. Without this the same
                        // server showed up twice (and the invite-joined
                        // one had no ServerBar tile until re-login).
                        if resp.server_id > 0 {
                            let real = resp.server_id.to_string();
                            if real != server_id && s.communities.contains_key(&server_id) {
                                // A stale client under the real id (e.g. an
                                // earlier attempt) would shadow this live one.
                                if let Some(mut old) = s.communities.remove(&real) {
                                    old.disconnect();
                                }
                                if let Some(mut client) = s.communities.remove(&server_id) {
                                    client.server_id = real.clone();
                                    s.communities.insert(real.clone(), client);
                                }
                                log::info!(
                                    "Community {} re-keyed to central id {}",
                                    server_id,
                                    real
                                );
                                server_id = real;
                            }
                        }
                        // Cache attachment endpoint on the client so
                        // upload commands don't have to re-resolve.
                        if let Some(client) = s.communities.get_mut(&server_id) {
                            if resp.attachment_port > 0 {
                                client.attachment_port = resp.attachment_port as u16;
                            }
                            client.max_attachment_bytes = resp.max_attachment_bytes;
                            host = client.host.clone();
                            port = client.port;
                        }
                    }
                    events::emit_community_auth_responded(events::CommunityAuthRespondedPayload {
                        server_id: server_id.clone(),
                        requested_server_id,
                        host,
                        port,
                        success: resp.success,
                        message: resp.message,
                        channels,
                        error_code: resp.error_code,
                        server_name: resp.server_name,
                        server_description: resp.server_description,
                        owner_username: resp.owner_username,
                        attachment_port: resp.attachment_port,
                        max_attachment_bytes: resp.max_attachment_bytes,
                    });
                }
                Some(packet::Payload::ChannelMsg(msg)) => {
                    let context = msg.channel_id.clone();
                    let id = msg.id;
                    let nonce = msg.nonce.clone();
                    let attachments = msg.attachments.into_iter().map(map_attachment).collect();
                    events::emit_message_received(events::MessageReceivedPayload {
                        context,
                        server_id: server_id.clone(),
                        sender: msg.sender,
                        recipient: String::new(),
                        content: msg.content,
                        timestamp: msg.timestamp.to_string(),
                        id,
                        attachments,
                        nonce,
                    });
                }
                Some(packet::Payload::ChannelHistoryRes(resp)) => {
                    let messages = resp
                        .messages
                        .into_iter()
                        .map(|m| events::ChannelMessagePayload {
                            id: m.id,
                            sender: m.sender,
                            channel_id: m.channel_id,
                            content: m.content,
                            timestamp: m.timestamp,
                            attachments: m.attachments.into_iter().map(map_attachment).collect(),
                            nonce: m.nonce,
                        })
                        .collect();
                    events::emit_channel_history_received(events::ChannelHistoryReceivedPayload {
                        server_id: server_id.clone(),
                        channel_id: resp.channel_id,
                        messages,
                        has_more: resp.has_more,
                    });
                }
                Some(packet::Payload::ChannelPruned(msg)) => {
                    let purged_attachments = msg
                        .purged_attachments
                        .into_iter()
                        .map(|t| events::AttachmentTombstonePayload {
                            attachment_id: t.attachment_id,
                            purged_at: t.purged_at,
                        })
                        .collect();
                    events::emit_channel_pruned(events::ChannelPrunedPayload {
                        server_id: server_id.clone(),
                        channel_id: msg.channel_id,
                        deleted_message_ids: msg.deleted_message_ids,
                        purged_attachments,
                    });
                }
                Some(packet::Payload::ChannelWipeRes(resp)) => {
                    events::emit_channel_wipe_responded(events::ChannelWipeRespondedPayload {
                        server_id: server_id.clone(),
                        channel_id: resp.channel_id,
                        success: resp.success,
                        message: resp.message,
                        deleted_message_count: resp.deleted_message_count,
                        deleted_attachment_count: resp.deleted_attachment_count,
                    });
                }
                Some(packet::Payload::ChannelWiped(msg)) => {
                    events::emit_channel_wiped(events::ChannelWipedPayload {
                        server_id: server_id.clone(),
                        channel_id: msg.channel_id,
                        wiped_at: msg.wiped_at,
                        wiped_by: msg.wiped_by,
                    });
                }
                Some(packet::Payload::MessageDeleteRes(resp)) => {
                    events::emit_channel_message_delete_responded(
                        events::ChannelMessageDeleteRespondedPayload {
                            success: resp.success,
                            message: resp.message,
                            server_id: server_id.clone(),
                            channel_id: resp.channel_id,
                            message_id: resp.message_id,
                        },
                    );
                }
                Some(packet::Payload::ChannelMessageDeleted(b)) => {
                    events::emit_channel_message_deleted(
                        events::ChannelMessageDeletedPayload {
                            server_id: server_id.clone(),
                            channel_id: b.channel_id,
                            message_id: b.message_id,
                            deleted_at: b.deleted_at,
                            deleted_by: b.deleted_by,
                        },
                    );
                }
                Some(packet::Payload::UpdateServerPictureRes(resp)) => {
                    events::emit_server_picture_update_responded(
                        events::ServerPictureUpdateRespondedPayload {
                            success: resp.success,
                            message: resp.message,
                            server_id: server_id.clone(),
                            version: resp.version,
                        },
                    );
                }
                Some(packet::Payload::InviteCreateRes(resp)) => {
                    let invite = resp.invite.map(|i| events::InviteInfoPayload {
                        code: i.code,
                        created_by: i.created_by,
                        created_at: i.created_at,
                        expires_at: i.expires_at,
                        max_uses: i.max_uses,
                        uses: i.uses,
                    });
                    events::emit_invite_create_responded(events::InviteCreateRespondedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        invite,
                    });
                }
                Some(packet::Payload::InviteListRes(resp)) => {
                    let invites: Vec<events::InviteInfoPayload> = resp
                        .invites
                        .into_iter()
                        .map(|i| events::InviteInfoPayload {
                            code: i.code,
                            created_by: i.created_by,
                            created_at: i.created_at,
                            expires_at: i.expires_at,
                            max_uses: i.max_uses,
                            uses: i.uses,
                        })
                        .collect();
                    events::emit_invite_list_received(events::InviteListReceivedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        invites,
                    });
                }
                Some(packet::Payload::InviteRevokeRes(resp)) => {
                    events::emit_invite_revoke_responded(events::InviteRevokeRespondedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        code: resp.code,
                    });
                }
                Some(packet::Payload::MemberListRes(resp)) => {
                    let members: Vec<events::MemberInfoPayload> =
                        resp.members.into_iter().map(member_info_payload).collect();
                    events::emit_member_list_received(events::MemberListReceivedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        members,
                        revision: resp.revision,
                        total_members: resp.total_members,
                        has_more: resp.has_more,
                        next_after: resp.next_after,
                        first_page: resp.first_page,
                    });
                }
                Some(packet::Payload::MemberUpsert(up)) => {
                    if let Some(m) = up.member {
                        events::emit_member_upsert(events::MemberUpsertPayload {
                            server_id: server_id.clone(),
                            member: member_info_payload(m),
                            revision: up.revision,
                        });
                    }
                }
                Some(packet::Payload::MemberRemove(rm)) => {
                    events::emit_member_remove(events::MemberRemovePayload {
                        server_id: server_id.clone(),
                        username: rm.username,
                        revision: rm.revision,
                    });
                }
                Some(packet::Payload::BanListRes(resp)) => {
                    let entries = resp
                        .entries
                        .into_iter()
                        .map(|b| events::BanInfoPayload {
                            username: b.username,
                            banned_by: b.banned_by,
                            reason: b.reason,
                            banned_at: b.banned_at,
                            expires_at: b.expires_at,
                        })
                        .collect();
                    events::emit_ban_list_received(events::BanListReceivedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        entries,
                        revision: resp.revision,
                    });
                }
                Some(packet::Payload::ServerMetaUpdate(m)) => {
                    events::emit_server_meta_updated(events::ServerMetaUpdatedPayload {
                        server_id: server_id.clone(),
                        server_name: m.server_name,
                        server_description: m.server_description,
                        owner_username: m.owner_username,
                    });
                }
                Some(packet::Payload::ServerUpdateRes(resp)) => {
                    events::emit_server_update_responded(events::ServerUpdateRespondedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                    });
                }
                Some(packet::Payload::AuditLogRes(resp)) => {
                    let entries = resp
                        .entries
                        .into_iter()
                        .map(|e| events::AuditEntryPayload {
                            id: e.id,
                            timestamp: e.timestamp,
                            actor: e.actor,
                            action: e.action,
                            target: e.target,
                            channel_id: e.channel_id,
                            details: e.details,
                        })
                        .collect();
                    events::emit_audit_log_received(events::AuditLogReceivedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        entries,
                        has_more: resp.has_more,
                    });
                }
                Some(packet::Payload::VoiceForceNotify(n)) => {
                    events::emit_voice_force_notify(events::VoiceForceNotifyPayload {
                        server_id: server_id.clone(),
                        action: match voice_force_notify::Action::try_from(n.action) {
                            Ok(voice_force_notify::Action::Disconnected) => "disconnected",
                            _ => "moved",
                        }
                        .to_string(),
                        channel_id: n.channel_id,
                        actor: n.actor,
                    });
                }
                Some(packet::Payload::ModActionRes(resp)) => {
                    if resp.success && resp.action == "leave" {
                        terminated.store(true, Ordering::SeqCst);
                    }
                    events::emit_mod_action_responded(events::ModActionRespondedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        username: resp.username,
                        action: resp.action,
                    });
                }
                Some(packet::Payload::ChannelOverwritesRes(resp)) => {
                    let overwrites = resp
                        .overwrites
                        .into_iter()
                        .map(|o| events::ChannelOverwritePayload {
                            channel_id: o.channel_id,
                            target_type: match channel_overwrite::TargetType::try_from(o.target_type) {
                                Ok(channel_overwrite::TargetType::Member) => "member",
                                _ => "role",
                            }
                            .to_string(),
                            target_id: o.target_id,
                            allow: o.allow,
                            deny: o.deny,
                        })
                        .collect();
                    events::emit_channel_overwrites_received(events::ChannelOverwritesReceivedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        channel_id: resp.channel_id,
                        overwrites,
                    });
                }
                Some(packet::Payload::RoleListRes(resp)) => {
                    let roles = resp
                        .roles
                        .into_iter()
                        .map(|r| events::RoleInfoPayload {
                            id: r.id,
                            name: r.name,
                            color: r.color,
                            position: r.position,
                            permissions: r.permissions,
                            is_default: r.is_default,
                        })
                        .collect();
                    events::emit_role_list_received(events::RoleListReceivedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        roles,
                    });
                }
                Some(packet::Payload::RoleActionRes(resp)) => {
                    let role = resp.role.map(|r| events::RoleInfoPayload {
                        id: r.id,
                        name: r.name,
                        color: r.color,
                        position: r.position,
                        permissions: r.permissions,
                        is_default: r.is_default,
                    });
                    events::emit_role_action_responded(events::RoleActionRespondedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        action: resp.action,
                        role,
                    });
                }
                Some(packet::Payload::MembershipRevoked(rev)) => {
                    terminated.store(true, Ordering::SeqCst);
                    events::emit_membership_revoked(events::MembershipRevokedPayload {
                        server_id: server_id.clone(),
                        action: rev.action,
                        reason: rev.reason,
                        actor: rev.actor,
                        expires_at: rev.expires_at,
                    });
                }
                Some(packet::Payload::VoicePresenceUpdate(update)) => {
                    let user_states = update
                        .user_states
                        .into_iter()
                        .map(|s| events::VoiceUserStatePayload {
                            username: s.username,
                            is_muted: s.is_muted,
                            is_deafened: s.is_deafened,
                            is_server_muted: s.is_server_muted,
                            is_server_deafened: s.is_server_deafened,
                        })
                        .collect();

                    // Mirror per-user capabilities into voice_caps_cache so the
                    // streamer-side CodecSelector can read watcher decode caps
                    // without bouncing back through JS. Bus payload below
                    // carries the same data so the renderer's voiceStore can
                    // drive watch-button gating + badges.
                    let participants = update.active_users.clone();
                    let user_capabilities: Vec<events::ClientCapabilitiesPayload> = update
                        .user_capabilities
                        .iter()
                        .map(|caps| events::ClientCapabilitiesPayload {
                            encode: caps
                                .encode
                                .iter()
                                .map(|c| events::CodecCapabilityPayload {
                                    codec: c.codec,
                                    max_width: c.max_width,
                                    max_height: c.max_height,
                                    max_fps: c.max_fps,
                                })
                                .collect(),
                            decode: caps
                                .decode
                                .iter()
                                .map(|c| events::CodecCapabilityPayload {
                                    codec: c.codec,
                                    max_width: c.max_width,
                                    max_height: c.max_height,
                                    max_fps: c.max_fps,
                                })
                                .collect(),
                        })
                        .collect();

                    let cache_arc = {
                        let s = state.lock().await;
                        s.voice_caps_cache.clone()
                    };
                    if let Ok(mut cache) = cache_arc.write() {
                        cache.clear();
                        for (idx, username) in participants.iter().enumerate() {
                            let raw = match update.user_capabilities.get(idx) {
                                Some(c) => c,
                                None => continue,
                            };
                            let to_cap = |c: &CodecCapability| crate::media::caps::CodecCap {
                                codec: match c.codec {
                                    1 => crate::media::caps::CodecKind::H264Hw,
                                    2 => crate::media::caps::CodecKind::H264Sw,
                                    3 => crate::media::caps::CodecKind::H265,
                                    4 => crate::media::caps::CodecKind::Av1,
                                    _ => crate::media::caps::CodecKind::Unknown,
                                },
                                max_width: c.max_width,
                                max_height: c.max_height,
                                max_fps: c.max_fps,
                            };
                            cache.insert(
                                username.clone(),
                                crate::media::caps::PeerCaps {
                                    encode: raw.encode.iter().map(to_cap).collect(),
                                    decode: raw.decode.iter().map(to_cap).collect(),
                                },
                            );
                        }
                    }
                    drop(cache_arc);

                    events::emit_voice_presence_updated(events::VoicePresenceUpdatedPayload {
                        server_id: server_id.clone(),
                        channel_id: update.channel_id,
                        participants,
                        user_states,
                        user_capabilities,
                    });
                }
                Some(packet::Payload::StreamPresenceUpdate(update)) => {
                    let streams = update
                        .active_streams
                        .into_iter()
                        .map(|s| events::StreamInfoPayload {
                            stream_id: s.stream_id,
                            owner_username: s.owner_username,
                            has_audio: s.has_audio,
                            resolution_width: s.resolution_width,
                            resolution_height: s.resolution_height,
                            fps: s.fps,
                            current_codec: s.current_codec,
                            enforced_codec: s.enforced_codec,
                            watcher_count: s.watcher_count,
                        })
                        .collect();
                    events::emit_stream_presence_updated(
                        server_id.clone(),
                        update.channel_id,
                        streams,
                    );
                }
                Some(packet::Payload::StreamThumbnailUpdate(thumb)) => {
                    events::emit_stream_thumbnail_updated(
                        thumb.owner_username,
                        thumb.thumbnail_data,
                    );
                }
                Some(packet::Payload::FetchStreamThumbnailRes(res)) => {
                    // Resolve the matching oneshot waiter. If no waiter
                    // is registered (response arrived after timeout),
                    // drop silently — the caller already returned an
                    // error.
                    let mut s = state.lock().await;
                    if let Some(tx) = s.pending_thumbnail_fetches.remove(&res.owner_username) {
                        let _ = tx.send(res);
                    }
                }
                Some(packet::Payload::StreamCodecChangedNotify(notify)) => {
                    events::emit_stream_codec_changed(events::StreamCodecChangedPayload {
                        channel_id: notify.channel_id,
                        streamer_username: notify.streamer_username,
                        new_codec: notify.new_codec,
                        new_width: notify.new_width,
                        new_height: notify.new_height,
                        new_fps: notify.new_fps,
                        reason: notify.reason,
                    });
                }
                Some(packet::Payload::StreamWatcherNotify(notify)) => {
                    let evt = crate::state::WatcherEvent {
                        channel_id: notify.channel_id,
                        streamer_username: notify.streamer_username,
                        watcher_username: notify.watcher_username,
                        action: notify.action,
                    };
                    let s = state.lock().await;
                    let _ = s.watcher_event_tx.send(evt);
                }
                Some(packet::Payload::ChannelUpdateRes(resp)) => {
                    let channel = resp.channel.map(channel_info_payload);
                    events::emit_channel_updated(events::ChannelUpdatedPayload {
                        server_id: server_id.clone(),
                        success: resp.success,
                        message: resp.message,
                        channel,
                    });
                }
                Some(packet::Payload::ChannelActionRes(resp)) => {
                    let channel = resp.channel.map(channel_info_payload);
                    events::emit_channel_action_responded(
                        events::ChannelActionRespondedPayload {
                            server_id: server_id.clone(),
                            success: resp.success,
                            message: resp.message,
                            action: resp.action,
                            channel,
                        },
                    );
                }
                Some(packet::Payload::ChannelListUpdate(update)) => {
                    let channels = update
                        .channels
                        .into_iter()
                        .map(channel_info_payload)
                        .collect();
                    events::emit_channel_list_updated(events::ChannelListUpdatedPayload {
                        server_id: server_id.clone(),
                        channels,
                    });
                }
                _ => {
                    log::debug!("Unhandled community packet type: {}", packet.r#type);
                }
            }
        }

        // Read loop ended. If the server told us this session is over
        // for good (rejected auth, kicked/banned, left), retire the
        // client instead of reconnecting: the renderer has already
        // processed the corresponding event, and connection_lost lets
        // it clear connectedServers + the attachment registry.
        if terminated.load(Ordering::SeqCst) {
            log::info!(
                "Community {} session ended by server; not reconnecting",
                server_id
            );
            let removed = {
                let mut s = state.lock().await;
                s.pending_thumbnail_fetches.clear();
                s.communities.remove(&server_id)
            };
            events::emit_connection_lost("community", Some(server_id.clone()));
            // Dropping the client aborts its tasks — including this one,
            // which takes effect at the next await; we return right away.
            drop(removed);
            return;
        }

        // Connection lost unexpectedly. Pull host/port/jwt from
        // current AppState so a moved client doesn't lose its config.
        let (host, port, jwt) = {
            let mut s = state.lock().await;
            // Drop in-flight thumbnail waiters so their callers fail
            // fast (RecvError) instead of riding out the full timeout.
            s.pending_thumbnail_fetches.clear();
            match s.communities.get(&server_id) {
                Some(c) => (c.host.clone(), c.port, c.jwt.clone()),
                None => {
                    log::warn!(
                        "Community {} read loop ended but client missing from state",
                        server_id
                    );
                    return;
                }
            }
        };
        log::warn!(
            "Community server {} read loop ended, starting reconnect",
            server_id
        );
        Self::start_reconnect(server_id, host, port, jwt, state, terminated);
    }
}

/// Abort every spawned tokio task when the client drops. Same reason
/// as CentralClient: without this, dropping a CommunityClient leaks
/// the router / ping / reconnect tasks and the tokio runtime can't
/// terminate, so closing the Electron window leaves decibell.exe
/// hanging in the background. (Connection's own Drop handles the
/// read/write tasks underneath.)
impl Drop for CommunityClient {
    fn drop(&mut self) {
        if let Some(t) = self.router_task.take() {
            t.abort();
        }
        if let Some(t) = self.reconnect_task.take() {
            t.abort();
        }
        if let Some(t) = self.ping_task.take() {
            t.abort();
        }
    }
}
