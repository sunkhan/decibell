use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::Framed;

use super::framing::LengthPrefixCodec;
use super::proto::Packet;
use super::tls::create_tls_connector;

/// A managed TCP/TLS connection with read/write channels.
pub struct Connection {
    write_tx: mpsc::Sender<Vec<u8>>,
    read_task: JoinHandle<()>,
    write_task: JoinHandle<()>,
}

impl Connection {
    /// Establish a TCP/TLS connection and start read/write loops.
    /// Returns the Connection and an mpsc::Receiver that yields decoded Packets.
    pub async fn connect(
        host: &str,
        port: u16,
    ) -> Result<(Self, mpsc::Receiver<Packet>), String> {
        let addr = format!("{}:{}", host, port);
        let tcp = TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("TCP connect to {} failed: {}", addr, e))?;

        let connector = create_tls_connector();
        let domain = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|e| format!("Invalid server name '{}': {}", host, e))?;

        let tls_stream = connector
            .connect(domain, tcp)
            .await
            .map_err(|e| format!("TLS handshake with {} failed: {}", addr, e))?;

        let framed = Framed::new(tls_stream, LengthPrefixCodec);
        let (mut sink, mut stream) = framed.split();

        // Write channel: commands send packets here, write loop sends them over the wire
        let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(64);

        // Read channel: read loop sends decoded packets here
        let (read_tx, read_rx) = mpsc::channel::<Packet>(64);

        let write_task = tokio::spawn(async move {
            while let Some(data) = write_rx.recv().await {
                if sink.send(data).await.is_err() {
                    break;
                }
            }
        });

        let read_task = tokio::spawn(async move {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(data) => {
                        match Packet::decode(&data[..]) {
                            Ok(packet) => {
                                if read_tx.send(packet).await.is_err() {
                                    break; // receiver dropped
                                }
                            }
                            Err(e) => {
                                log::warn!("Failed to decode packet: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("Read error: {}", e);
                        break;
                    }
                }
            }
        });

        Ok((
            Connection {
                write_tx,
                read_task,
                write_task,
            },
            read_rx,
        ))
    }

    /// Send a serialized packet over the connection.
    pub async fn send(&self, data: Vec<u8>) -> Result<(), String> {
        self.write_tx
            .send(data)
            .await
            .map_err(|_| "Connection closed".to_string())
    }

    /// Check if the connection is still alive.
    pub fn is_alive(&self) -> bool {
        !self.read_task.is_finished()
    }

    /// Shut down the connection.
    pub fn shutdown(self) {
        self.read_task.abort();
        self.write_task.abort();
    }
}

/// Build a serialized Packet ready to send over the wire.
pub fn build_packet(
    msg_type: super::proto::packet::Type,
    payload: super::proto::packet::Payload,
    auth_token: Option<&str>,
) -> Vec<u8> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let packet = Packet {
        r#type: msg_type as i32,
        timestamp,
        auth_token: auth_token.unwrap_or_default().to_string(),
        payload: Some(payload),
    };
    packet.encode_to_vec()
}

/// Exponential backoff delays for reconnection: 1, 2, 4, 8, 16, 30 (capped).
pub fn backoff_duration(attempt: u32) -> Duration {
    let secs = match attempt {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 8,
        4 => 16,
        _ => 30,
    };
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_duration() {
        assert_eq!(backoff_duration(0), Duration::from_secs(1));
        assert_eq!(backoff_duration(1), Duration::from_secs(2));
        assert_eq!(backoff_duration(2), Duration::from_secs(4));
        assert_eq!(backoff_duration(3), Duration::from_secs(8));
        assert_eq!(backoff_duration(4), Duration::from_secs(16));
        assert_eq!(backoff_duration(5), Duration::from_secs(30));
        assert_eq!(backoff_duration(100), Duration::from_secs(30));
    }

    #[test]
    fn test_build_packet_sets_fields() {
        use crate::net::proto::{LoginRequest, packet};

        let data = build_packet(
            packet::Type::LoginReq,
            packet::Payload::LoginReq(LoginRequest {
                username: "test".into(),
                password: "pass".into(),
            }),
            Some("my_token"),
        );

        let decoded = Packet::decode(&data[..]).unwrap();
        assert_eq!(decoded.r#type, packet::Type::LoginReq as i32);
        assert_eq!(decoded.auth_token, "my_token");
        assert!(decoded.timestamp > 0);
        match decoded.payload {
            Some(packet::Payload::LoginReq(req)) => {
                assert_eq!(req.username, "test");
            }
            _ => panic!("Wrong payload type"),
        }
    }

    #[test]
    fn test_build_packet_no_token() {
        use crate::net::proto::{ServerListRequest, packet};

        let data = build_packet(
            packet::Type::ServerListReq,
            packet::Payload::ServerListReq(ServerListRequest {}),
            None,
        );

        let decoded = Packet::decode(&data[..]).unwrap();
        assert_eq!(decoded.auth_token, "");
    }
}
