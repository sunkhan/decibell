//! Central server client. Owns the TLS connection to the central
//! authentication / discovery / DM / friends server. Dispatches
//! incoming packets to bus events and supports auto-reconnect with
//! exponential backoff.
//!
//! PR3 scope: route_packets handles login, register, server-list. DM,
//! friends, and invite-resolve handlers land in their feature PRs.
//! Send-side helpers (send_private_message, request_friend_list, etc.)
//! are kept verbatim so subsequent PRs can wire them up to commands
//! without touching this file's connection scaffolding.

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

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
    pub async fn connect(state: Arc<Mutex<AppState>>) -> Result<Self, String> {
        let (connection, read_rx) = Connection::connect(CENTRAL_HOST, CENTRAL_PORT).await?;

        let router_task = tokio::spawn(Self::route_packets(read_rx, state.clone()));

        // Keepalive ping every 30s. Without this, a dead TCP connection
        // can go undetected for minutes.
        //
        // We must NOT hold the AppState lock while calling send(),
        // because send() writes to a bounded channel (cap 64). On a
        // silently-dead TCP connection, the write task blocks, the
        // channel fills, send() blocks, and if we held AppState the
        // entire UI deadlocks. Instead, clone the write_tx sender and
        // send directly without the lock.
        let ping_write_tx = connection.clone_write_tx();
        let ping_task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let data = build_packet(
                    packet::Type::ClientPing,
                    packet::Payload::Handshake(Handshake {
                        protocol_version: 0,
                        client_id: String::new(),
                    }),
                    None,
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

        Ok(CentralClient {
            connection: Some(connection),
            router_task: Some(router_task),
            reconnect_task: None,
            ping_task: Some(ping_task),
        })
    }

    /// Clone the underlying write channel for direct sends that bypass AppState lock.
    pub fn connection_write_tx(&self) -> Option<mpsc::Sender<Vec<u8>>> {
        self.connection.as_ref().map(|c| c.clone_write_tx())
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

    /// Send a ServerListRequest.
    pub async fn request_server_list(&self, token: Option<&str>) -> Result<(), String> {
        let data = build_packet(
            packet::Type::ServerListReq,
            packet::Payload::ServerListReq(ServerListRequest {}),
            token,
        );
        self.send(data).await
    }

    /// Send a DirectMessage to another user via the central server.
    /// The reply path is asynchronous: peer-side delivery (or an
    /// "offline / friends-only" reject) arrives back as a DirectMsg
    /// to *us* with the sender swapped to ourselves and the canonical
    /// error string in `content` — see route_packets.
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
    pub fn start_reconnect(&mut self, state: Arc<Mutex<AppState>>) {
        let task = tokio::spawn(async move {
            events::emit_connection_lost("central", None);

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

                        // Re-authenticate with stored credentials.
                        // Send BEFORE locking AppState — connection.send() writes
                        // to a bounded channel that can block on a dead socket.
                        {
                            let s = state.lock().await;
                            if let Some((ref user, ref pass)) = s.credentials {
                                let login_data = build_packet(
                                    packet::Type::LoginReq,
                                    packet::Payload::LoginReq(LoginRequest {
                                        username: user.clone(),
                                        password: pass.clone(),
                                    }),
                                    None,
                                );
                                drop(s);
                                let _ = connection.send(login_data).await;
                            }
                        }

                        let router = tokio::spawn(Self::route_packets(read_rx, state.clone()));

                        // Restart keepalive ping task (lock-free, see connect())
                        let ping_write_tx = connection.clone_write_tx();
                        let ping_task = tokio::spawn(async move {
                            loop {
                                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                                let data = build_packet(
                                    packet::Type::ClientPing,
                                    packet::Payload::Handshake(Handshake {
                                        protocol_version: 0,
                                        client_id: String::new(),
                                    }),
                                    None,
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

                        events::emit_connection_restored("central", None);
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

    /// Route incoming packets to bus events.
    ///
    /// PR3 surface: login, register, server-list. DM, friends,
    /// invite-resolve handlers land in their feature PRs (each adds a
    /// new arm here and a matching emit_* in events.rs).
    async fn route_packets(mut read_rx: mpsc::Receiver<Packet>, state: Arc<Mutex<AppState>>) {
        while let Some(packet) = read_rx.recv().await {
            match packet.payload {
                Some(packet::Payload::LoginRes(resp)) => {
                    if resp.success {
                        let mut s = state.lock().await;
                        s.token = Some(resp.jwt_token.clone());
                        let username = s
                            .credentials
                            .as_ref()
                            .map(|(user, _)| user.clone())
                            .unwrap_or_else(|| "unknown".to_string());
                        s.username = Some(username.clone());
                        drop(s);
                        events::emit_login_succeeded(username);
                    } else {
                        events::emit_login_failed(resp.message);
                    }
                }
                Some(packet::Payload::RegisterRes(resp)) => {
                    events::emit_register_responded(resp.success, resp.message);
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
                    events::emit_server_list_received(servers);
                }
                Some(packet::Payload::DirectMsg(msg)) => {
                    events::emit_message_received(events::MessageReceivedPayload {
                        context: "dm".to_string(),
                        sender: msg.sender,
                        recipient: msg.recipient,
                        content: msg.content,
                        timestamp: msg.timestamp.to_string(),
                        // DMs aren't persisted server-side and don't carry
                        // attachments or a nonce — keep them zero/empty so
                        // the renderer's MessageReceivedPayload type stays
                        // uniform with the channel-message path.
                        id: 0,
                        attachments: Vec::new(),
                        nonce: String::new(),
                    });
                }
                Some(packet::Payload::PresenceUpdate(update)) => {
                    events::emit_user_list_updated(update.online_users);
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
                    events::emit_friend_list_received(friends);
                }
                Some(packet::Payload::FriendActionRes(resp)) => {
                    events::emit_friend_action_responded(resp.success, resp.message);
                }
                Some(packet::Payload::InviteResolveRes(resp)) => {
                    // Fulfil the `resolve_invite_code` waiter keyed by
                    // the echoed code. Orphan responses (caller already
                    // timed out) are dropped silently.
                    let waiter = {
                        let mut s = state.lock().await;
                        s.pending_invite_resolves.remove(&resp.code)
                    };
                    if let Some(tx) = waiter {
                        let _ = tx.send(resp);
                    } else {
                        log::debug!("Orphan InviteResolveRes for code {}", resp.code);
                    }
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
            central.start_reconnect(state.clone());
        }
    }
}

/// Abort every spawned tokio task when the client drops. Without this,
/// dropping CentralClient (e.g. during app shutdown via `state.central
/// = None`) leaks the router / ping / reconnect tasks — they keep
/// looping forever, the tokio runtime can't terminate, and Electron's
/// main process hangs in the background after the user closes the
/// window. (Connection's own Drop handles the read/write tasks.)
impl Drop for CentralClient {
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
