use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tauri::AppHandle;

use super::connection::{backoff_duration, build_packet, Connection};
use super::proto::*;
use crate::events;
use crate::state::AppState;

const CENTRAL_HOST: &str = "178.104.131.247";
const CENTRAL_PORT: u16 = 8080;

pub struct CentralClient {
    connection: Option<Connection>,
    router_task: Option<JoinHandle<()>>,
    reconnect_task: Option<JoinHandle<()>>,
    ping_task: Option<JoinHandle<()>>,
}

impl CentralClient {
    /// Connect to the central server and start the packet routing loop.
    pub async fn connect(
        app: AppHandle,
        state: Arc<Mutex<AppState>>,
    ) -> Result<Self, String> {
        let (connection, read_rx) = Connection::connect(CENTRAL_HOST, CENTRAL_PORT).await?;

        let router_task = tokio::spawn(Self::route_packets(read_rx, app.clone(), state.clone()));

        // Keepalive ping every 30s so the central server knows we're alive.
        // Without this, a dead connection can go undetected for minutes.
        let ping_state = state.clone();
        let ping_task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let s = ping_state.lock().await;
                let sent = if let Some(ref central) = s.central {
                    let data = build_packet(
                        packet::Type::ClientPing,
                        // Reuse Handshake as a lightweight no-op payload
                        packet::Payload::Handshake(Handshake {
                            protocol_version: 0,
                            client_id: String::new(),
                        }),
                        None,
                    );
                    central.send(data).await.is_ok()
                } else {
                    false
                };
                drop(s);
                if !sent {
                    break;
                }
            }
        });

        Ok(CentralClient {
            connection: Some(connection),
            router_task: Some(router_task),
            reconnect_task: None,
            ping_task: Some(ping_task),
        })
    }

    /// Send a raw packet over the connection.
    pub async fn send(&self, data: Vec<u8>) -> Result<(), String> {
        match &self.connection {
            Some(conn) => conn.send(data).await,
            None => Err("Not connected to central server".to_string()),
        }
    }

    /// Send a LoginRequest.
    pub async fn login(&self, username: &str, password: &str) -> Result<(), String> {
        let data = build_packet(
            packet::Type::LoginReq,
            packet::Payload::LoginReq(LoginRequest {
                username: username.into(),
                password: password.into(),
            }),
            None,
        );
        self.send(data).await
    }

    /// Send a RegisterRequest.
    pub async fn register(
        &self,
        username: &str,
        email: &str,
        password: &str,
    ) -> Result<(), String> {
        let data = build_packet(
            packet::Type::RegisterReq,
            packet::Payload::RegisterReq(RegisterRequest {
                username: username.into(),
                password: password.into(),
                email: email.into(),
            }),
            None,
        );
        self.send(data).await
    }

    /// Send a ServerListRequest.
    pub async fn request_server_list(&self, token: Option<&str>) -> Result<(), String> {
        let data = build_packet(
            packet::Type::ServerListReq,
            packet::Payload::ServerListReq(ServerListRequest {}),
            token,
        );
        self.send(data).await
    }

    /// Send a DirectMessage.
    pub async fn send_private_message(
        &self,
        sender: &str,
        recipient: &str,
        content: &str,
        token: Option<&str>,
    ) -> Result<(), String> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let data = build_packet(
            packet::Type::DirectMsg,
            packet::Payload::DirectMsg(DirectMessage {
                sender: sender.into(),
                recipient: recipient.into(),
                content: content.into(),
                timestamp,
            }),
            token,
        );
        self.send(data).await
    }

    /// Send a FriendListReq.
    pub async fn request_friend_list(&self, token: Option<&str>) -> Result<(), String> {
        let data = build_packet(
            packet::Type::FriendListReq,
            packet::Payload::FriendListReq(FriendListReq {}),
            token,
        );
        self.send(data).await
    }

    /// Send a FriendActionReq.
    pub async fn send_friend_action(
        &self,
        action: i32,
        target_username: &str,
        token: Option<&str>,
    ) -> Result<(), String> {
        let data = build_packet(
            packet::Type::FriendActionReq,
            packet::Payload::FriendActionReq(FriendActionReq {
                action,
                target_username: target_username.into(),
            }),
            token,
        );
        self.send(data).await
    }

    /// Send a DmPrivacySetting to the central server.
    pub async fn send_dm_privacy(&self, friends_only: bool, token: Option<&str>) -> Result<(), String> {
        let data = build_packet(
            packet::Type::DmPrivacy,
            packet::Payload::DmPrivacy(DmPrivacySetting {
                friends_only,
            }),
            token,
        );
        self.send(data).await
    }

    /// Disconnect from the central server. Stops reconnection and keepalive.
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
    pub fn start_reconnect(
        &mut self,
        app: AppHandle,
        state: Arc<Mutex<AppState>>,
    ) {
        let task = tokio::spawn(async move {
            events::emit_connection_lost(&app, "central", None);

            let mut attempt = 0u32;
            loop {
                let delay = backoff_duration(attempt);
                log::info!(
                    "Central reconnect attempt {} in {:?}",
                    attempt + 1,
                    delay
                );
                tokio::time::sleep(delay).await;

                match Connection::connect(CENTRAL_HOST, CENTRAL_PORT).await {
                    Ok((connection, read_rx)) => {
                        log::info!("Reconnected to central server");

                        // Re-authenticate with stored credentials
                        let mut s = state.lock().await;
                        if let Some((ref user, ref pass)) = s.credentials {
                            let login_data = build_packet(
                                packet::Type::LoginReq,
                                packet::Payload::LoginReq(LoginRequest {
                                    username: user.clone(),
                                    password: pass.clone(),
                                }),
                                None,
                            );
                            let _ = connection.send(login_data).await;
                        }

                        let router =
                            tokio::spawn(Self::route_packets(read_rx, app.clone(), state.clone()));

                        // Restart keepalive ping task
                        let ping_state = state.clone();
                        let ping_task = tokio::spawn(async move {
                            loop {
                                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                                let s = ping_state.lock().await;
                                let sent = if let Some(ref central) = s.central {
                                    let data = build_packet(
                                        packet::Type::ClientPing,
                                        packet::Payload::Handshake(Handshake {
                                            protocol_version: 0,
                                            client_id: String::new(),
                                        }),
                                        None,
                                    );
                                    central.send(data).await.is_ok()
                                } else {
                                    false
                                };
                                drop(s);
                                if !sent { break; }
                            }
                        });

                        if let Some(ref mut central) = s.central {
                            if let Some(old_ping) = central.ping_task.take() {
                                old_ping.abort();
                            }
                            central.connection = Some(connection);
                            central.router_task = Some(router);
                            central.reconnect_task = None;
                            central.ping_task = Some(ping_task);
                        }
                        drop(s);

                        events::emit_connection_restored(&app, "central", None);
                        return;
                    }
                    Err(e) => {
                        log::warn!("Central reconnect failed: {}", e);
                        attempt += 1;
                    }
                }
            }
        });
        self.reconnect_task = Some(task);
    }

    /// Route incoming packets to Tauri events.
    async fn route_packets(
        mut read_rx: mpsc::Receiver<Packet>,
        app: AppHandle,
        state: Arc<Mutex<AppState>>,
    ) {
        while let Some(packet) = read_rx.recv().await {
            match packet.payload {
                Some(packet::Payload::LoginRes(resp)) => {
                    if resp.success {
                        let mut s = state.lock().await;
                        s.token = Some(resp.jwt_token.clone());
                        let username = s.credentials
                            .as_ref()
                            .map(|(user, _)| user.clone())
                            .unwrap_or_else(|| "unknown".to_string());
                        s.username = Some(username.clone());
                        drop(s);
                        events::emit_login_succeeded(&app, username);
                    } else {
                        events::emit_login_failed(&app, resp.message);
                    }
                }
                Some(packet::Payload::RegisterRes(resp)) => {
                    events::emit_register_responded(&app, resp.success, resp.message);
                }
                Some(packet::Payload::ServerListRes(resp)) => {
                    let servers: Vec<events::ServerInfo> = resp
                        .servers
                        .into_iter()
                        .map(|s| events::ServerInfo {
                            id: s.id,
                            name: s.name,
                            description: s.description,
                            host_ip: s.host_ip,
                            port: s.port,
                            member_count: s.member_count,
                        })
                        .collect();
                    events::emit_server_list_received(&app, servers);
                }
                Some(packet::Payload::DirectMsg(msg)) => {
                    events::emit_message_received(
                        &app,
                        "dm".to_string(),
                        msg.sender,
                        msg.recipient,  // NEW — pass recipient through
                        msg.content,
                        msg.timestamp.to_string(),
                    );
                }
                Some(packet::Payload::PresenceUpdate(update)) => {
                    events::emit_user_list_updated(&app, update.online_users);
                }
                Some(packet::Payload::FriendListRes(resp)) => {
                    let friends: Vec<events::FriendInfoPayload> = resp
                        .friends
                        .into_iter()
                        .map(|f| events::FriendInfoPayload {
                            username: f.username,
                            status: match friend_info::Status::try_from(f.status) {
                                Ok(friend_info::Status::Online) => "online",
                                Ok(friend_info::Status::Offline) => "offline",
                                Ok(friend_info::Status::PendingIncoming) => "pending_incoming",
                                Ok(friend_info::Status::PendingOutgoing) => "pending_outgoing",
                                Ok(friend_info::Status::Blocked) => "blocked",
                                Err(_) => "unknown",
                            }
                            .to_string(),
                        })
                        .collect();
                    events::emit_friend_list_received(&app, friends);
                }
                Some(packet::Payload::FriendActionRes(resp)) => {
                    events::emit_friend_action_responded(&app, resp.success, resp.message);
                }
                _ => {
                    log::debug!("Unhandled central packet type: {}", packet.r#type);
                }
            }
        }

        // Read loop ended — connection lost, start reconnect
        log::warn!("Central server read loop ended, starting reconnect");
        let mut s = state.lock().await;
        if let Some(ref mut central) = s.central {
            central.start_reconnect(app, state.clone());
        }
    }
}
