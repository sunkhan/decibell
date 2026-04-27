use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tauri::AppHandle;

use super::connection::{backoff_duration, build_packet, Connection};
use super::proto::*;
use crate::events;
use crate::state::AppState;

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
        app: AppHandle,
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
        let router_task = tokio::spawn(Self::route_packets(
            read_rx,
            app.clone(),
            state.clone(),
            sid,
        ));

        // Keepalive ping every 30s — detects dead connections at the
        // application layer (complements TCP keepalive). Uses cloned
        // write_tx so it never touches AppState.
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
                ).await;
                match send_result {
                    Ok(Ok(())) => {}
                    _ => break, // connection dead
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

    /// Send a channel message.
    pub async fn send_channel_message(
        &self,
        sender: &str,
        channel_id: &str,
        content: &str,
    ) -> Result<(), String> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let data = build_packet(
            packet::Type::ChannelMsg,
            packet::Payload::ChannelMsg(ChannelMessage {
                sender: sender.into(),
                channel_id: channel_id.into(),
                content: content.into(),
                timestamp,
                id: 0,
                attachments: Vec::new(),
                nonce: String::new(),
            }),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    /// Join a voice channel.
    pub async fn join_voice_channel(&self, channel_id: &str) -> Result<(), String> {
        let data = build_packet(
            packet::Type::JoinVoiceReq,
            packet::Payload::JoinVoiceReq(JoinVoiceRequest {
                channel_id: channel_id.into(),
                capabilities: None, // populated in Plan A Group 6 Task 12
            }),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    /// Notify server of mute/deafen state change.
    pub async fn send_voice_state_notify(&self, is_muted: bool, is_deafened: bool) -> Result<(), String> {
        let data = build_packet(
            packet::Type::VoiceStateNotify,
            packet::Payload::VoiceStateNotify(VoiceStateNotify {
                is_muted,
                is_deafened,
            }),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    /// Leave the current voice channel.
    pub async fn leave_voice_channel(&self) -> Result<(), String> {
        let data = build_packet(
            packet::Type::LeaveVoiceReq,
            packet::Payload::LeaveVoiceReq(LeaveVoiceRequest {}),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    /// Start screen sharing in a voice channel.
    pub async fn start_stream(
        &self,
        channel_id: &str,
        target_fps: i32,
        target_bitrate_kbps: i32,
        has_audio: bool,
        resolution_width: u32,
        resolution_height: u32,
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
                // See commands/streaming.rs comment — populated in later plan tasks.
                chosen_codec: 0,
                enforced_codec: 0,
            }),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    /// Stop screen sharing.
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

    /// Request to watch a user's stream.
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

    /// Send a stream thumbnail to all voice channel participants.
    pub async fn send_stream_thumbnail(&self, channel_id: &str, jpeg_data: &[u8]) -> Result<(), String> {
        let data = build_packet(
            packet::Type::StreamThumbnailUpdate,
            packet::Payload::StreamThumbnailUpdate(StreamThumbnailUpdate {
                channel_id: channel_id.into(),
                owner_username: String::new(), // Server enforces identity
                thumbnail_data: jpeg_data.to_vec(),
            }),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    /// Stop watching a user's stream.
    pub async fn stop_watching(&self, channel_id: &str, target_username: &str) -> Result<(), String> {
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

    /// Start reconnection loop.
    fn start_reconnect(
        server_id: String,
        host: String,
        port: u16,
        jwt: String,
        app: AppHandle,
        state: Arc<Mutex<AppState>>,
    ) {
        let sid_for_task = server_id.clone();
        let state_for_store = state.clone();
        let task = tokio::spawn(async move {
            events::emit_connection_lost(&app, "community", Some(sid_for_task.clone()));

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
                            app.clone(),
                            state.clone(),
                            sid,
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
                                ).await;
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

                        events::emit_connection_restored(
                            &app,
                            "community",
                            Some(sid_for_task.clone()),
                        );
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

    /// Route incoming packets to Tauri events.
    async fn route_packets(
        mut read_rx: mpsc::Receiver<Packet>,
        app: AppHandle,
        state: Arc<Mutex<AppState>>,
        server_id: String,
    ) {
        while let Some(packet) = read_rx.recv().await {
            match packet.payload {
                Some(packet::Payload::CommunityAuthRes(resp)) => {
                    let channels: Vec<events::ChannelInfoPayload> = resp
                        .channels
                        .into_iter()
                        .map(|c| events::ChannelInfoPayload {
                            id: c.id,
                            name: c.name,
                            r#type: match channel_info::Type::try_from(c.r#type) {
                                Ok(channel_info::Type::Text) => "text",
                                Ok(channel_info::Type::Voice) => "voice",
                                Err(_) => "unknown",
                            }
                            .to_string(),
                            voice_bitrate_kbps: c.voice_bitrate_kbps,
                            retention_days_text: c.retention_days_text,
                            retention_days_image: c.retention_days_image,
                            retention_days_video: c.retention_days_video,
                            retention_days_document: c.retention_days_document,
                            retention_days_audio: c.retention_days_audio,
                        })
                        .collect();
                    // Cache attachment endpoint on the client so upload
                    // commands don't have to re-resolve on every call.
                    if resp.success {
                        let mut s = state.lock().await;
                        if let Some(client) = s.communities.get_mut(&server_id) {
                            if resp.attachment_port > 0 {
                                client.attachment_port = resp.attachment_port as u16;
                            }
                            client.max_attachment_bytes = resp.max_attachment_bytes;
                        }
                    }
                    events::emit_community_auth_responded(
                        &app,
                        server_id.clone(),
                        resp.success,
                        resp.message,
                        channels,
                        resp.error_code,
                        resp.server_name,
                        resp.server_description,
                        resp.owner_username,
                        resp.attachment_port,
                        resp.max_attachment_bytes,
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
                    events::emit_invite_create_responded(
                        &app,
                        server_id.clone(),
                        resp.success,
                        resp.message,
                        invite,
                    );
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
                    events::emit_invite_list_received(
                        &app,
                        server_id.clone(),
                        resp.success,
                        resp.message,
                        invites,
                    );
                }
                Some(packet::Payload::InviteRevokeRes(resp)) => {
                    events::emit_invite_revoke_responded(
                        &app,
                        server_id.clone(),
                        resp.success,
                        resp.message,
                        resp.code,
                    );
                }
                Some(packet::Payload::MemberListRes(resp)) => {
                    let members: Vec<events::MemberInfoPayload> = resp
                        .members
                        .into_iter()
                        .map(|m| events::MemberInfoPayload {
                            username: m.username,
                            joined_at: m.joined_at,
                            nickname: m.nickname,
                            is_owner: m.is_owner,
                            is_online: m.is_online,
                        })
                        .collect();
                    events::emit_member_list_received(
                        &app,
                        server_id.clone(),
                        resp.success,
                        resp.message,
                        members,
                        resp.bans,
                    );
                }
                Some(packet::Payload::ModActionRes(resp)) => {
                    events::emit_mod_action_responded(
                        &app,
                        server_id.clone(),
                        resp.success,
                        resp.message,
                        resp.username,
                        resp.action,
                    );
                }
                Some(packet::Payload::MembershipRevoked(rev)) => {
                    events::emit_membership_revoked(
                        &app,
                        server_id.clone(),
                        rev.action,
                        rev.reason,
                        rev.actor,
                    );
                }
                Some(packet::Payload::ChannelMsg(msg)) => {
                    let context = msg.channel_id.clone();
                    let id = msg.id;
                    let nonce = msg.nonce.clone();
                    let attachments = msg.attachments.into_iter()
                        .map(map_attachment).collect();
                    events::emit_message_received(
                        &app,
                        context,
                        msg.sender,
                        String::new(),  // No recipient for channel messages
                        msg.content,
                        msg.timestamp.to_string(),
                        id,
                        attachments,
                        nonce,
                    );
                }
                Some(packet::Payload::ChannelHistoryRes(resp)) => {
                    let messages = resp.messages.into_iter()
                        .map(|m| events::ChannelMessagePayload {
                            id: m.id,
                            sender: m.sender,
                            channel_id: m.channel_id,
                            content: m.content,
                            timestamp: m.timestamp,
                            attachments: m.attachments.into_iter()
                                .map(map_attachment).collect(),
                            nonce: m.nonce,
                        })
                        .collect();
                    events::emit_channel_history_received(
                        &app,
                        server_id.clone(),
                        resp.channel_id,
                        messages,
                        resp.has_more,
                    );
                }
                Some(packet::Payload::ChannelPruned(msg)) => {
                    let tombstones = msg.purged_attachments.into_iter()
                        .map(|t| events::AttachmentTombstonePayload {
                            attachment_id: t.attachment_id,
                            purged_at: t.purged_at,
                        })
                        .collect();
                    events::emit_channel_pruned(
                        &app,
                        server_id.clone(),
                        msg.channel_id,
                        msg.deleted_message_ids,
                        tombstones,
                    );
                }
                Some(packet::Payload::ChannelWipeRes(resp)) => {
                    events::emit_channel_wipe_responded(
                        &app,
                        server_id.clone(),
                        resp.channel_id,
                        resp.success,
                        resp.message,
                        resp.deleted_message_count,
                        resp.deleted_attachment_count,
                    );
                }
                Some(packet::Payload::ChannelWiped(msg)) => {
                    events::emit_channel_wiped(
                        &app,
                        server_id.clone(),
                        msg.channel_id,
                        msg.wiped_at,
                        msg.wiped_by,
                    );
                }
                Some(packet::Payload::ChannelUpdateRes(resp)) => {
                    let channel = resp.channel.map(|c| events::ChannelInfoPayload {
                        id: c.id,
                        name: c.name,
                        r#type: match channel_info::Type::try_from(c.r#type) {
                            Ok(channel_info::Type::Text) => "text",
                            Ok(channel_info::Type::Voice) => "voice",
                            Err(_) => "unknown",
                        }.to_string(),
                        voice_bitrate_kbps: c.voice_bitrate_kbps,
                        retention_days_text: c.retention_days_text,
                        retention_days_image: c.retention_days_image,
                        retention_days_video: c.retention_days_video,
                        retention_days_document: c.retention_days_document,
                        retention_days_audio: c.retention_days_audio,
                    });
                    events::emit_channel_updated(
                        &app,
                        server_id.clone(),
                        resp.success,
                        resp.message,
                        channel,
                    );
                }
                Some(packet::Payload::VoicePresenceUpdate(update)) => {
                    let user_states: Vec<events::VoiceUserStatePayload> = update
                        .user_states
                        .into_iter()
                        .map(|s| events::VoiceUserStatePayload {
                            username: s.username,
                            is_muted: s.is_muted,
                            is_deafened: s.is_deafened,
                        })
                        .collect();
                    events::emit_voice_presence_updated(
                        &app,
                        server_id.clone(),
                        update.channel_id,
                        update.active_users,
                        user_states,
                    );
                }
                Some(packet::Payload::StreamPresenceUpdate(update)) => {
                    events::emit_stream_presence_updated(
                        &app,
                        server_id.clone(),
                        update.channel_id,
                        update.active_streams.into_iter().map(|s| events::StreamInfoPayload {
                            stream_id: s.stream_id,
                            owner_username: s.owner_username,
                            has_audio: s.has_audio,
                            resolution_width: s.resolution_width,
                            resolution_height: s.resolution_height,
                            fps: s.fps,
                        }).collect(),
                    );
                }
                Some(packet::Payload::StreamThumbnailUpdate(update)) => {
                    events::emit_stream_thumbnail_updated(
                        &app,
                        update.owner_username,
                        update.thumbnail_data,
                    );
                }
                _ => {
                    log::debug!(
                        "Unhandled community {} packet type: {}",
                        server_id,
                        packet.r#type
                    );
                }
            }
        }

        // Read loop ended — start reconnect
        log::warn!("Community {} read loop ended, starting reconnect", server_id);
        let s = state.lock().await;
        if let Some(client) = s.communities.get(&server_id) {
            let host = client.host.clone();
            let port = client.port;
            let jwt = client.jwt.clone();
            let sid = server_id.clone();
            drop(s);
            Self::start_reconnect(sid, host, port, jwt, app, state);
        }
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
    }
}
