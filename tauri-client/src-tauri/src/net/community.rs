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
    pub server_id: String,
    pub host: String,
    pub port: u16,
    pub jwt: String,
    pub joined_channels: Vec<String>,
}

impl CommunityClient {
    /// Connect to a community server and authenticate with JWT.
    pub async fn connect(
        server_id: String,
        host: String,
        port: u16,
        jwt: String,
        app: AppHandle,
        state: Arc<Mutex<AppState>>,
    ) -> Result<Self, String> {
        let (connection, read_rx) = Connection::connect(&host, port).await?;

        // Send CommunityAuthRequest
        let auth_data = build_packet(
            packet::Type::CommunityAuthReq,
            packet::Payload::CommunityAuthReq(CommunityAuthRequest {
                jwt_token: jwt.clone(),
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

        Ok(CommunityClient {
            connection: Some(connection),
            router_task: Some(router_task),
            reconnect_task: None,
            server_id,
            host,
            port,
            jwt,
            joined_channels: Vec::new(),
        })
    }

    /// Send a raw packet.
    pub async fn send(&self, data: Vec<u8>) -> Result<(), String> {
        match &self.connection {
            Some(conn) => conn.send(data).await,
            None => Err("Not connected to community server".to_string()),
        }
    }

    /// Join a channel.
    pub async fn join_channel(&mut self, channel_id: &str) -> Result<(), String> {
        let data = build_packet(
            packet::Type::JoinChannelReq,
            packet::Payload::JoinChannelReq(JoinChannelRequest {
                channel_id: channel_id.into(),
            }),
            Some(&self.jwt),
        );
        self.send(data).await?;

        if !self.joined_channels.contains(&channel_id.to_string()) {
            self.joined_channels.push(channel_id.to_string());
        }
        Ok(())
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

    /// Disconnect from the community server. Stops reconnection.
    pub fn disconnect(&mut self) {
        if let Some(task) = self.reconnect_task.take() {
            task.abort();
        }
        if let Some(task) = self.router_task.take() {
            task.abort();
        }
        if let Some(conn) = self.connection.take() {
            conn.shutdown();
        }
        self.joined_channels.clear();
    }

    /// Start reconnection loop.
    fn start_reconnect(
        server_id: String,
        host: String,
        port: u16,
        jwt: String,
        joined_channels: Vec<String>,
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

                        // Re-authenticate
                        let auth_data = build_packet(
                            packet::Type::CommunityAuthReq,
                            packet::Payload::CommunityAuthReq(CommunityAuthRequest {
                                jwt_token: jwt.clone(),
                            }),
                            Some(&jwt),
                        );
                        let _ = connection.send(auth_data).await;

                        // Re-join channels
                        for channel_id in &joined_channels {
                            let join_data = build_packet(
                                packet::Type::JoinChannelReq,
                                packet::Payload::JoinChannelReq(JoinChannelRequest {
                                    channel_id: channel_id.clone(),
                                }),
                                Some(&jwt),
                            );
                            let _ = connection.send(join_data).await;
                        }

                        let sid = sid_for_task.clone();
                        let router = tokio::spawn(Self::route_packets(
                            read_rx,
                            app.clone(),
                            state.clone(),
                            sid,
                        ));

                        let mut s = state.lock().await;
                        if let Some(client) = s.communities.get_mut(&sid_for_task) {
                            client.connection = Some(connection);
                            client.router_task = Some(router);
                            client.reconnect_task = None;
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
                        })
                        .collect();
                    events::emit_community_auth_responded(
                        &app,
                        server_id.clone(),
                        resp.success,
                        resp.message,
                        channels,
                    );
                }
                Some(packet::Payload::ChannelMsg(msg)) => {
                    let context = msg.channel_id.clone();
                    events::emit_message_received(
                        &app,
                        context,
                        msg.sender,
                        String::new(),  // No recipient for channel messages
                        msg.content,
                        msg.timestamp.to_string(),
                    );
                }
                Some(packet::Payload::JoinChannelRes(resp)) => {
                    events::emit_join_channel_responded(
                        &app,
                        server_id.clone(),
                        resp.success,
                        resp.channel_id,
                        resp.active_users,
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
            let joined = client.joined_channels.clone();
            let sid = server_id.clone();
            drop(s);
            Self::start_reconnect(sid, host, port, jwt, joined, app, state);
        }
    }
}
