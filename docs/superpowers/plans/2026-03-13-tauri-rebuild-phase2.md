# Phase 2: Rust Networking Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement async Rust networking (TCP/TLS + Protobuf) for the Decibell Tauri client, replacing the C++ ChatBackend's connections to central and community servers.

**Architecture:** Async Rust with tokio for I/O, tokio-rustls for TLS (cert verification skipped), prost for Protobuf codegen from `proto/messages.proto`. Connection manager handles read/write loops and exponential-backoff reconnection. CentralClient and CommunityClient wrap connections with protocol-specific logic. Thin Tauri commands delegate to clients; responses arrive as Tauri events.

**Tech Stack:** Rust, tokio, tokio-rustls, rustls, prost/prost-build, tokio-util (codec), bytes, Tauri v2

**Spec:** `docs/superpowers/specs/2026-03-13-tauri-rebuild-phase2-design.md`

---

## File Structure

```
tauri-client/src-tauri/
├── Cargo.toml                    # MODIFY: add networking dependencies
├── build.rs                      # MODIFY: add prost_build protobuf compilation
└── src/
    ├── lib.rs                    # MODIFY: register new commands, manage AppState
    ├── main.rs                   # NO CHANGE
    ├── state.rs                  # MODIFY: full AppState with auth + connections
    ├── net/
    │   ├── mod.rs                # MODIFY: declare submodules, re-exports
    │   ├── proto.rs              # CREATE: include! generated protobuf code
    │   ├── framing.rs            # CREATE: length-prefix codec (Encoder + Decoder)
    │   ├── tls.rs                # CREATE: TLS connector factory (skip cert verify)
    │   ├── connection.rs         # CREATE: TCP/TLS connection lifecycle + reconnect
    │   ├── central.rs            # CREATE: CentralClient (login, register, friends, DMs)
    │   └── community.rs          # CREATE: CommunityClient (JWT auth, channels, messages)
    ├── commands/
    │   ├── mod.rs                # MODIFY: re-export all command modules
    │   ├── auth.rs               # CREATE: login, register, logout commands
    │   ├── servers.rs            # CREATE: request_server_list, connect/disconnect community
    │   ├── channels.rs           # CREATE: join_channel, send_channel_message
    │   ├── friends.rs            # CREATE: request_friend_list, send_friend_action
    │   └── messaging.rs          # CREATE: send_private_message
    ├── events/
    │   └── mod.rs                # MODIFY: event constants + typed emit helpers
    └── media/
        └── mod.rs                # NO CHANGE (Phase 4-5)
```

---

## Chunk 1: Foundation — Dependencies, Protobuf, Framing, TLS

### Task 1: Add dependencies and protobuf build integration

**Files:**
- Modify: `tauri-client/src-tauri/Cargo.toml`
- Modify: `tauri-client/src-tauri/build.rs`
- Create: `tauri-client/src-tauri/src/net/proto.rs`
- Modify: `tauri-client/src-tauri/src/net/mod.rs`

- [ ] **Step 1: Update Cargo.toml with networking dependencies**

Add these dependencies to the existing `[dependencies]` section:

```toml
tokio = { version = "1", features = ["full"] }
tokio-rustls = "0.26"
rustls = { version = "0.23", features = ["ring"] }
prost = "0.13"
bytes = "1"
tokio-util = { version = "0.7", features = ["codec"] }
futures-util = "0.3"
log = "0.4"
```

Note: `serde` with `derive` feature is already in the existing Cargo.toml from Phase 1.

Add `prost-build` to the existing `[build-dependencies]` section:

```toml
prost-build = "0.13"
```

Also add `protoc` install note: prost-build requires the `protoc` compiler. If not installed, run `choco install protobuf` or download from https://github.com/protocolbuffers/protobuf/releases.

- [ ] **Step 2: Update build.rs to compile protobuf**

Replace contents of `tauri-client/src-tauri/build.rs`:

```rust
fn main() {
    tauri_build::build();
    prost_build::compile_protos(
        &["../../proto/messages.proto"],
        &["../../proto/"],
    )
    .expect("Failed to compile protobuf");
}
```

- [ ] **Step 3: Create net/proto.rs to include generated code**

Create `tauri-client/src-tauri/src/net/proto.rs`:

```rust
include!(concat!(env!("OUT_DIR"), "/chatproj.rs"));
```

- [ ] **Step 4: Update net/mod.rs to declare proto module**

Replace `tauri-client/src-tauri/src/net/mod.rs`:

```rust
pub mod proto;
```

- [ ] **Step 5: Verify protobuf compilation**

Run: `cd tauri-client/src-tauri && cargo check`

Expected: compiles successfully. If `protoc` is missing, install it first.

- [ ] **Step 6: Write protobuf serialization test**

Add test at the bottom of `tauri-client/src-tauri/src/net/proto.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn test_login_request_roundtrip() {
        let req = LoginRequest {
            username: "alice".into(),
            password: "secret123".into(),
        };
        let bytes = req.encode_to_vec();
        let decoded = LoginRequest::decode(&bytes[..]).unwrap();
        assert_eq!(decoded.username, "alice");
        assert_eq!(decoded.password, "secret123");
    }

    #[test]
    fn test_packet_with_oneof_payload() {
        let packet = Packet {
            r#type: packet::Type::LoginReq as i32,
            timestamp: 1234567890,
            auth_token: String::new(),
            payload: Some(packet::Payload::LoginReq(LoginRequest {
                username: "bob".into(),
                password: "pass".into(),
            })),
        };
        let bytes = packet.encode_to_vec();
        let decoded = Packet::decode(&bytes[..]).unwrap();
        assert_eq!(decoded.r#type, packet::Type::LoginReq as i32);
        assert_eq!(decoded.timestamp, 1234567890);
        match decoded.payload {
            Some(packet::Payload::LoginReq(req)) => {
                assert_eq!(req.username, "bob");
                assert_eq!(req.password, "pass");
            }
            other => panic!("Expected LoginReq payload, got {:?}", other),
        }
    }

    #[test]
    fn test_login_response_with_jwt() {
        let resp = LoginResponse {
            success: true,
            message: "Welcome".into(),
            jwt_token: "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.test".into(),
        };
        let bytes = resp.encode_to_vec();
        let decoded = LoginResponse::decode(&bytes[..]).unwrap();
        assert!(decoded.success);
        assert_eq!(decoded.jwt_token, "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.test");
    }

    #[test]
    fn test_packet_auth_token_field() {
        let packet = Packet {
            r#type: packet::Type::ServerListReq as i32,
            timestamp: 0,
            auth_token: "my_jwt_token".into(),
            payload: Some(packet::Payload::ServerListReq(ServerListRequest {})),
        };
        let bytes = packet.encode_to_vec();
        let decoded = Packet::decode(&bytes[..]).unwrap();
        assert_eq!(decoded.auth_token, "my_jwt_token");
    }
}
```

- [ ] **Step 7: Run protobuf tests**

Run: `cd tauri-client/src-tauri && cargo test net::proto::tests -- --nocapture`

Expected: all 4 tests pass.

- [ ] **Step 8: Commit**

```bash
git add tauri-client/src-tauri/Cargo.toml tauri-client/src-tauri/build.rs tauri-client/src-tauri/src/net/proto.rs tauri-client/src-tauri/src/net/mod.rs
git commit -m "feat: add protobuf build integration and networking dependencies"
```

---

### Task 2: Framing codec

**Files:**
- Create: `tauri-client/src-tauri/src/net/framing.rs`
- Modify: `tauri-client/src-tauri/src/net/mod.rs`

- [ ] **Step 1: Write framing codec tests**

Create `tauri-client/src-tauri/src/net/framing.rs` with tests first:

```rust
use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

const MAX_FRAME_SIZE: usize = 2 * 1024 * 1024; // 2MB

#[derive(Debug)]
pub struct LengthPrefixCodec;

impl Decoder for LengthPrefixCodec {
    type Item = Vec<u8>;
    type Error = std::io::Error;

    fn decode(&mut self, _src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        todo!()
    }
}

impl Encoder<Vec<u8>> for LengthPrefixCodec {
    type Error = std::io::Error;

    fn encode(&mut self, _item: Vec<u8>, _dst: &mut BytesMut) -> Result<(), Self::Error> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_prepends_length() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        let data = vec![1, 2, 3, 4, 5];
        codec.encode(data.clone(), &mut buf).unwrap();

        // First 4 bytes should be big-endian length (5)
        assert_eq!(buf.len(), 9); // 4 + 5
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, 5);
        assert_eq!(&buf[4..], &data[..]);
    }

    #[test]
    fn test_decode_complete_frame() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        let data = vec![10, 20, 30];
        buf.put_u32(3); // big-endian length
        buf.put_slice(&data);

        let result = codec.decode(&mut buf).unwrap();
        assert_eq!(result, Some(data));
        assert!(buf.is_empty());
    }

    #[test]
    fn test_decode_incomplete_header() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        buf.put_u8(0);
        buf.put_u8(0); // only 2 of 4 header bytes

        let result = codec.decode(&mut buf).unwrap();
        assert_eq!(result, None); // need more data
        assert_eq!(buf.len(), 2); // data preserved
    }

    #[test]
    fn test_decode_incomplete_body() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        buf.put_u32(10); // says 10 bytes
        buf.put_slice(&[1, 2, 3]); // only 3 bytes

        let result = codec.decode(&mut buf).unwrap();
        assert_eq!(result, None); // need more data
        assert_eq!(buf.len(), 7); // header + partial body preserved
    }

    #[test]
    fn test_decode_rejects_oversized_frame() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        buf.put_u32((MAX_FRAME_SIZE + 1) as u32);

        let result = codec.decode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_roundtrip() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        let original = vec![0xDE, 0xAD, 0xBE, 0xEF];

        codec.encode(original.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_multiple_frames_in_buffer() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();

        // Encode two frames back to back
        codec.encode(vec![1, 2], &mut buf).unwrap();
        codec.encode(vec![3, 4, 5], &mut buf).unwrap();

        let frame1 = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame1, vec![1, 2]);

        let frame2 = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame2, vec![3, 4, 5]);
    }

    #[test]
    fn test_encode_empty_payload() {
        let mut codec = LengthPrefixCodec;
        let mut buf = BytesMut::new();
        codec.encode(vec![], &mut buf).unwrap();

        assert_eq!(buf.len(), 4); // just the length header
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, 0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd tauri-client/src-tauri && cargo test net::framing::tests -- --nocapture`

Expected: all tests FAIL with `not yet implemented` panics.

- [ ] **Step 3: Implement the codec**

Replace the `todo!()` in the `Decoder` impl:

```rust
fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
    if src.len() < 4 {
        src.reserve(4 - src.len());
        return Ok(None);
    }

    let length = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;

    if length > MAX_FRAME_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Frame size {} exceeds maximum {}", length, MAX_FRAME_SIZE),
        ));
    }

    if src.len() < 4 + length {
        src.reserve(4 + length - src.len());
        return Ok(None);
    }

    src.advance(4);
    let body = src.split_to(length).to_vec();
    Ok(Some(body))
}
```

Replace the `todo!()` in the `Encoder` impl:

```rust
fn encode(&mut self, item: Vec<u8>, dst: &mut BytesMut) -> Result<(), Self::Error> {
    dst.put_u32(item.len() as u32);
    dst.put_slice(&item);
    Ok(())
}
```

- [ ] **Step 4: Add framing module to net/mod.rs**

Update `tauri-client/src-tauri/src/net/mod.rs`:

```rust
pub mod framing;
pub mod proto;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd tauri-client/src-tauri && cargo test net::framing::tests -- --nocapture`

Expected: all 8 tests pass.

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/net/framing.rs tauri-client/src-tauri/src/net/mod.rs
git commit -m "feat: implement length-prefix framing codec with tests"
```

---

### Task 3: TLS connector factory

**Files:**
- Create: `tauri-client/src-tauri/src/net/tls.rs`
- Modify: `tauri-client/src-tauri/src/net/mod.rs`

- [ ] **Step 1: Implement TLS connector**

Create `tauri-client/src-tauri/src/net/tls.rs`:

```rust
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use std::sync::Arc;
use tokio_rustls::TlsConnector;

/// Certificate verifier that accepts all certificates.
/// Matches the existing C++ client's `ssl::verify_none` behavior.
#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}

/// Creates a TLS connector that skips certificate verification.
pub fn create_tls_connector() -> TlsConnector {
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();

    TlsConnector::from(Arc::new(config))
}
```

- [ ] **Step 2: Add tls module to net/mod.rs**

Update `tauri-client/src-tauri/src/net/mod.rs`:

```rust
pub mod framing;
pub mod proto;
pub mod tls;
```

- [ ] **Step 3: Verify compilation**

Run: `cd tauri-client/src-tauri && cargo check`

Expected: compiles successfully. No unit tests for TLS (needs a running TLS server to test against).

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/net/tls.rs tauri-client/src-tauri/src/net/mod.rs
git commit -m "feat: add TLS connector factory with cert verification skip"
```

---

## Chunk 2: Events, State, and Connection Manager

### Task 4: Events module

**Files:**
- Modify: `tauri-client/src-tauri/src/events/mod.rs`

- [ ] **Step 1: Implement event constants and emit helpers**

Replace `tauri-client/src-tauri/src/events/mod.rs`:

```rust
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

// Event name constants
pub const LOGIN_SUCCEEDED: &str = "login_succeeded";
pub const LOGIN_FAILED: &str = "login_failed";
pub const REGISTER_RESPONDED: &str = "register_responded";
pub const LOGGED_OUT: &str = "logged_out";
pub const CONNECTION_LOST: &str = "connection_lost";
pub const CONNECTION_RESTORED: &str = "connection_restored";
pub const SERVER_LIST_RECEIVED: &str = "server_list_received";
pub const COMMUNITY_AUTH_RESPONDED: &str = "community_auth_responded";
pub const MESSAGE_RECEIVED: &str = "message_received";
pub const USER_LIST_UPDATED: &str = "user_list_updated";
pub const JOIN_CHANNEL_RESPONDED: &str = "join_channel_responded";
pub const FRIEND_LIST_RECEIVED: &str = "friend_list_received";
pub const FRIEND_ACTION_RESPONDED: &str = "friend_action_responded";

// --- Payload structs ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginSucceededPayload {
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginFailedPayload {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRespondedPayload {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionEventPayload {
    pub server_type: String, // "central" or "community"
    pub server_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub id: i32,
    pub name: String,
    pub description: String,
    pub host_ip: String,
    pub port: i32,
    pub member_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerListReceivedPayload {
    pub servers: Vec<ServerInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInfoPayload {
    pub id: String,
    pub name: String,
    pub r#type: String, // "text" or "voice"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityAuthRespondedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub channels: Vec<ChannelInfoPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReceivedPayload {
    pub context: String, // "dm" or "channel:<server_id>:<channel_id>"
    pub sender: String,
    pub content: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserListUpdatedPayload {
    pub online_users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinChannelRespondedPayload {
    pub server_id: String,
    pub success: bool,
    pub channel_id: String,
    pub active_users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendInfoPayload {
    pub username: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendListReceivedPayload {
    pub friends: Vec<FriendInfoPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendActionRespondedPayload {
    pub success: bool,
    pub message: String,
}

// --- Emit helper functions ---

pub fn emit_login_succeeded(app: &AppHandle, username: String) {
    let _ = app.emit(LOGIN_SUCCEEDED, LoginSucceededPayload { username });
}

pub fn emit_login_failed(app: &AppHandle, message: String) {
    let _ = app.emit(LOGIN_FAILED, LoginFailedPayload { message });
}

pub fn emit_register_responded(app: &AppHandle, success: bool, message: String) {
    let _ = app.emit(REGISTER_RESPONDED, RegisterRespondedPayload { success, message });
}

pub fn emit_logged_out(app: &AppHandle) {
    let _ = app.emit(LOGGED_OUT, ());
}

pub fn emit_connection_lost(app: &AppHandle, server_type: &str, server_id: Option<String>) {
    let _ = app.emit(
        CONNECTION_LOST,
        ConnectionEventPayload {
            server_type: server_type.to_string(),
            server_id,
        },
    );
}

pub fn emit_connection_restored(app: &AppHandle, server_type: &str, server_id: Option<String>) {
    let _ = app.emit(
        CONNECTION_RESTORED,
        ConnectionEventPayload {
            server_type: server_type.to_string(),
            server_id,
        },
    );
}

pub fn emit_server_list_received(app: &AppHandle, servers: Vec<ServerInfo>) {
    let _ = app.emit(SERVER_LIST_RECEIVED, ServerListReceivedPayload { servers });
}

pub fn emit_community_auth_responded(
    app: &AppHandle,
    server_id: String,
    success: bool,
    message: String,
    channels: Vec<ChannelInfoPayload>,
) {
    let _ = app.emit(
        COMMUNITY_AUTH_RESPONDED,
        CommunityAuthRespondedPayload {
            server_id,
            success,
            message,
            channels,
        },
    );
}

pub fn emit_message_received(
    app: &AppHandle,
    context: String,
    sender: String,
    content: String,
    timestamp: String,
) {
    let _ = app.emit(
        MESSAGE_RECEIVED,
        MessageReceivedPayload {
            context,
            sender,
            content,
            timestamp,
        },
    );
}

pub fn emit_user_list_updated(app: &AppHandle, online_users: Vec<String>) {
    let _ = app.emit(USER_LIST_UPDATED, UserListUpdatedPayload { online_users });
}

pub fn emit_join_channel_responded(
    app: &AppHandle,
    server_id: String,
    success: bool,
    channel_id: String,
    active_users: Vec<String>,
) {
    let _ = app.emit(
        JOIN_CHANNEL_RESPONDED,
        JoinChannelRespondedPayload {
            server_id,
            success,
            channel_id,
            active_users,
        },
    );
}

pub fn emit_friend_list_received(app: &AppHandle, friends: Vec<FriendInfoPayload>) {
    let _ = app.emit(FRIEND_LIST_RECEIVED, FriendListReceivedPayload { friends });
}

pub fn emit_friend_action_responded(app: &AppHandle, success: bool, message: String) {
    let _ = app.emit(
        FRIEND_ACTION_RESPONDED,
        FriendActionRespondedPayload { success, message },
    );
}
```

- [ ] **Step 2: Verify compilation**

Run: `cd tauri-client/src-tauri && cargo check`

Expected: compiles successfully.

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/events/mod.rs
git commit -m "feat: implement event constants and typed emit helpers"
```

---

### Task 5: AppState

**Files:**
- Modify: `tauri-client/src-tauri/src/state.rs`
- Modify: `tauri-client/src-tauri/src/lib.rs`

This task defines the `AppState` struct. It references `CentralClient` and `CommunityClient` which don't exist yet, so we use forward-looking types that compile now and get filled in later.

- [ ] **Step 1: Implement AppState**

Replace `tauri-client/src-tauri/src/state.rs`:

```rust
use std::collections::HashMap;
use tokio::sync::Mutex;

use crate::net::central::CentralClient;
use crate::net::community::CommunityClient;

pub struct AppState {
    pub central: Option<CentralClient>,
    pub communities: HashMap<String, CommunityClient>,
    pub username: Option<String>,
    pub token: Option<String>,
    pub credentials: Option<(String, String)>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            central: None,
            communities: HashMap::new(),
            username: None,
            token: None,
            credentials: None,
        }
    }
}

pub type SharedState = Mutex<AppState>;
```

- [ ] **Step 2: Create placeholder CentralClient and CommunityClient**

Create `tauri-client/src-tauri/src/net/central.rs`:

```rust
/// Central server client. Manages connection to 127.0.0.1:8080.
/// Full implementation in Task 7.
pub struct CentralClient;
```

Create `tauri-client/src-tauri/src/net/community.rs`:

```rust
/// Community server client. Manages connection to a user-hosted community server.
/// Full implementation in Task 8.
pub struct CommunityClient;
```

- [ ] **Step 3: Update net/mod.rs**

Replace `tauri-client/src-tauri/src/net/mod.rs`:

```rust
pub mod central;
pub mod community;
pub mod framing;
pub mod proto;
pub mod tls;
```

- [ ] **Step 4: Update lib.rs to manage AppState**

Replace `tauri-client/src-tauri/src/lib.rs`:

```rust
mod commands;
mod events;
mod media;
mod net;
mod state;

use state::{AppState, SharedState};
use tokio::sync::Mutex;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Mutex::new(AppState::default()) as SharedState)
        .invoke_handler(tauri::generate_handler![commands::ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 5: Verify compilation**

Run: `cd tauri-client/src-tauri && cargo check`

Expected: compiles successfully.

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/state.rs tauri-client/src-tauri/src/lib.rs tauri-client/src-tauri/src/net/central.rs tauri-client/src-tauri/src/net/community.rs tauri-client/src-tauri/src/net/mod.rs
git commit -m "feat: implement AppState and placeholder client structs"
```

---

### Task 6: Connection manager

**Files:**
- Create: `tauri-client/src-tauri/src/net/connection.rs`
- Modify: `tauri-client/src-tauri/src/net/mod.rs`

The connection manager handles TCP/TLS connect, framed read/write loops via channels, and exponential-backoff reconnection. It is protocol-agnostic — CentralClient and CommunityClient use it.

- [ ] **Step 1: Implement Connection struct**

Create `tauri-client/src-tauri/src/net/connection.rs`:

```rust
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::client::TlsStream;
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
        assert_eq!(backoff_duration(100), Duration::from_secs(30)); // capped
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
```

- [ ] **Step 2: Update net/mod.rs**

Replace `tauri-client/src-tauri/src/net/mod.rs`:

```rust
pub mod central;
pub mod community;
pub mod connection;
pub mod framing;
pub mod proto;
pub mod tls;
```

- [ ] **Step 3: Run tests**

Run: `cd tauri-client/src-tauri && cargo test net::connection::tests -- --nocapture`

Expected: all 3 tests pass.

- [ ] **Step 4: Run all tests**

Run: `cd tauri-client/src-tauri && cargo test -- --nocapture`

Expected: all tests pass (proto: 4, framing: 8, connection: 3 = 15 tests).

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src-tauri/src/net/connection.rs tauri-client/src-tauri/src/net/mod.rs
git commit -m "feat: implement connection manager with TLS, read/write loops, and packet builder"
```

---

## Chunk 3: Central and Community Clients

### Task 7: CentralClient

**Files:**
- Modify: `tauri-client/src-tauri/src/net/central.rs`

The CentralClient wraps a Connection and handles:
- Connecting to 127.0.0.1:8080
- Sending auth/friend/messaging packets
- Packet routing loop that emits Tauri events
- Reconnection with re-authentication

- [ ] **Step 1: Implement CentralClient**

Replace `tauri-client/src-tauri/src/net/central.rs`:

```rust
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tauri::AppHandle;

use super::connection::{backoff_duration, build_packet, Connection};
use super::proto::*;
use crate::events;
use crate::state::AppState;

const CENTRAL_HOST: &str = "127.0.0.1";
const CENTRAL_PORT: u16 = 8080;

pub struct CentralClient {
    connection: Option<Connection>,
    router_task: Option<JoinHandle<()>>,
    reconnect_task: Option<JoinHandle<()>>,
}

impl CentralClient {
    /// Connect to the central server and start the packet routing loop.
    pub async fn connect(
        app: AppHandle,
        state: Arc<Mutex<AppState>>,
    ) -> Result<Self, String> {
        let (connection, read_rx) = Connection::connect(CENTRAL_HOST, CENTRAL_PORT).await?;

        let router_task = tokio::spawn(Self::route_packets(read_rx, app.clone(), state.clone()));

        Ok(CentralClient {
            connection: Some(connection),
            router_task: Some(router_task),
            reconnect_task: None,
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

    /// Disconnect from the central server. Stops reconnection.
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

                        if let Some(ref mut central) = s.central {
                            central.connection = Some(connection);
                            central.router_task = Some(router);
                            central.reconnect_task = None;
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
                        let username = if let Some((ref user, _)) = s.credentials {
                            s.username = Some(user.clone());
                            user.clone()
                        } else {
                            "unknown".to_string()
                        };
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
```

- [ ] **Step 2: Verify compilation**

Run: `cd tauri-client/src-tauri && cargo check`

Expected: compiles. Fix any import issues.

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/net/central.rs
git commit -m "feat: implement CentralClient with packet routing and reconnection"
```

---

### Task 8: CommunityClient

**Files:**
- Modify: `tauri-client/src-tauri/src/net/community.rs`

- [ ] **Step 1: Implement CommunityClient**

Replace `tauri-client/src-tauri/src/net/community.rs`:

```rust
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
    host: String,
    port: u16,
    jwt: String,
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
        let task = tokio::spawn(async move {
            events::emit_connection_lost(&app, "community", Some(server_id.clone()));

            let mut attempt = 0u32;
            loop {
                let delay = backoff_duration(attempt);
                log::info!(
                    "Community {} reconnect attempt {} in {:?}",
                    server_id,
                    attempt + 1,
                    delay
                );
                tokio::time::sleep(delay).await;

                match Connection::connect(&host, port).await {
                    Ok((connection, read_rx)) => {
                        log::info!("Reconnected to community server {}", server_id);

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

                        let sid = server_id.clone();
                        let router = tokio::spawn(Self::route_packets(
                            read_rx,
                            app.clone(),
                            state.clone(),
                            sid,
                        ));

                        let mut s = state.lock().await;
                        if let Some(client) = s.communities.get_mut(&server_id) {
                            client.connection = Some(connection);
                            client.router_task = Some(router);
                            client.reconnect_task = None;
                        }
                        drop(s);

                        events::emit_connection_restored(
                            &app,
                            "community",
                            Some(server_id.clone()),
                        );
                        return;
                    }
                    Err(e) => {
                        log::warn!("Community {} reconnect failed: {}", server_id, e);
                        attempt += 1;
                    }
                }
            }
        });

        // Store the reconnect task handle
        let state_clone = state.clone();
        tokio::spawn(async move {
            let mut s = state_clone.lock().await;
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
                    let context = format!("channel:{}:{}", server_id, msg.channel_id);
                    events::emit_message_received(
                        &app,
                        context,
                        msg.sender,
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
```

- [ ] **Step 2: Verify compilation**

Run: `cd tauri-client/src-tauri && cargo check`

Expected: compiles successfully.

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/net/community.rs
git commit -m "feat: implement CommunityClient with JWT auth, channels, and reconnection"
```

---

## Chunk 4: Tauri Commands and Integration

### Task 9: Auth commands

**Files:**
- Create: `tauri-client/src-tauri/src/commands/auth.rs`
- Modify: `tauri-client/src-tauri/src/commands/mod.rs`

- [ ] **Step 1: Implement auth commands**

Create `tauri-client/src-tauri/src/commands/auth.rs`:

```rust
use std::sync::Arc;
use tauri::{AppHandle, State};
use tokio::sync::Mutex;

use crate::events;
use crate::net::central::CentralClient;
use crate::state::AppState;

#[tauri::command]
pub async fn login(
    username: String,
    password: String,
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    // Disconnect existing central client if any
    if let Some(mut old) = s.central.take() {
        old.disconnect();
    }

    // Store credentials for reconnection
    s.credentials = Some((username.clone(), password.clone()));

    // Create state Arc for CentralClient (it needs shared access)
    let state_arc = Arc::new(Mutex::new(std::mem::take(&mut *s)));
    drop(s);

    // Connect to central server
    let client = CentralClient::connect(app.clone(), state_arc.clone()).await?;

    // Send login request
    client.login(&username, &password).await?;

    // Put state back
    let mut inner = state_arc.lock().await;
    inner.central = Some(client);

    // Copy inner state back to managed state
    let mut s = state.lock().await;
    *s = std::mem::take(&mut *inner);

    Ok(())
}

#[tauri::command]
pub async fn register(
    username: String,
    email: String,
    password: String,
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let s = state.lock().await;
    match &s.central {
        Some(client) => {
            client.register(&username, &email, &password).await?;
            Ok(())
        }
        None => Err("Not connected to central server".to_string()),
    }
}

#[tauri::command]
pub async fn logout(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    // Disconnect central
    if let Some(mut central) = s.central.take() {
        central.disconnect();
    }

    // Disconnect all communities
    for (_, mut client) in s.communities.drain() {
        client.disconnect();
    }

    // Clear auth state
    s.username = None;
    s.token = None;
    s.credentials = None;

    drop(s);
    events::emit_logged_out(&app);

    Ok(())
}
```

- [ ] **Step 2: Verify compilation**

Run: `cd tauri-client/src-tauri && cargo check`

Expected: compiles. If there are issues with the Arc/Mutex pattern for shared state between CentralClient and the Tauri managed state, adjust — the key constraint is that CentralClient's route_packets needs to mutate AppState (to store JWT on login).

**Important note for implementer:** The `login` command has a complex state ownership issue. `CentralClient::connect` takes `Arc<Mutex<AppState>>` but Tauri's `State` is a different reference. You may need to restructure so that `AppState` is wrapped in `Arc<Mutex<>>` from the start (in `lib.rs`), and Tauri manages the `Arc<Mutex<AppState>>` directly. This is the recommended approach:

In `state.rs`, change:
```rust
pub type SharedState = Arc<Mutex<AppState>>;
```

In `lib.rs`, change managed state to:
```rust
.manage(Arc::new(Mutex::new(AppState::default())) as SharedState)
```

Then commands take `State<'_, SharedState>` and clone the Arc:
```rust
let state_arc = state.inner().clone();
```

This avoids the double-lock problem. Adjust the auth commands accordingly.

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/commands/auth.rs
git commit -m "feat: implement login, register, logout Tauri commands"
```

---

### Task 10: Server commands

**Files:**
- Create: `tauri-client/src-tauri/src/commands/servers.rs`

- [ ] **Step 1: Implement server commands**

Create `tauri-client/src-tauri/src/commands/servers.rs`:

```rust
use std::sync::Arc;
use tauri::{AppHandle, State};
use tokio::sync::Mutex;

use crate::net::community::CommunityClient;
use crate::state::SharedState;

#[tauri::command]
pub async fn request_server_list(
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let token = s.token.clone();
    match &s.central {
        Some(client) => client.request_server_list(token.as_deref()).await,
        None => Err("Not connected to central server".to_string()),
    }
}

#[tauri::command]
pub async fn connect_to_community(
    server_id: String,
    host: String,
    port: u16,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let state_arc = state.inner().clone();
    let jwt = {
        let s = state_arc.lock().await;
        s.token.clone().ok_or("Not authenticated")?
    };

    let client = CommunityClient::connect(
        server_id.clone(),
        host,
        port,
        jwt,
        app,
        state_arc.clone(),
    )
    .await?;

    let mut s = state_arc.lock().await;
    s.communities.insert(server_id, client);
    Ok(())
}

#[tauri::command]
pub async fn disconnect_from_community(
    server_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    match s.communities.remove(&server_id) {
        Some(mut client) => {
            client.disconnect();
            Ok(())
        }
        None => Err(format!("Not connected to community {}", server_id)),
    }
}
```

- [ ] **Step 2: Verify compilation**

Run: `cd tauri-client/src-tauri && cargo check`

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/commands/servers.rs
git commit -m "feat: implement server list and community connect/disconnect commands"
```

---

### Task 11: Channel, friend, and messaging commands

**Files:**
- Create: `tauri-client/src-tauri/src/commands/channels.rs`
- Create: `tauri-client/src-tauri/src/commands/friends.rs`
- Create: `tauri-client/src-tauri/src/commands/messaging.rs`

- [ ] **Step 1: Implement channel commands**

Create `tauri-client/src-tauri/src/commands/channels.rs`:

```rust
use tauri::State;

use crate::state::SharedState;

#[tauri::command]
pub async fn join_channel(
    server_id: String,
    channel_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    match s.communities.get_mut(&server_id) {
        Some(client) => client.join_channel(&channel_id).await,
        None => Err(format!("Not connected to community {}", server_id)),
    }
}

#[tauri::command]
pub async fn send_channel_message(
    server_id: String,
    channel_id: String,
    message: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let sender = s.username.clone().ok_or("Not authenticated")?;
    match s.communities.get(&server_id) {
        Some(client) => client.send_channel_message(&sender, &channel_id, &message).await,
        None => Err(format!("Not connected to community {}", server_id)),
    }
}
```

- [ ] **Step 2: Implement friend commands**

Create `tauri-client/src-tauri/src/commands/friends.rs`:

```rust
use tauri::State;

use crate::state::SharedState;

#[tauri::command]
pub async fn request_friend_list(
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let token = s.token.clone();
    match &s.central {
        Some(client) => client.request_friend_list(token.as_deref()).await,
        None => Err("Not connected to central server".to_string()),
    }
}

#[tauri::command]
pub async fn send_friend_action(
    action: i32,
    target_username: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let token = s.token.clone();
    match &s.central {
        Some(client) => {
            client
                .send_friend_action(action, &target_username, token.as_deref())
                .await
        }
        None => Err("Not connected to central server".to_string()),
    }
}
```

- [ ] **Step 3: Implement messaging commands**

Create `tauri-client/src-tauri/src/commands/messaging.rs`:

```rust
use tauri::State;

use crate::state::SharedState;

#[tauri::command]
pub async fn send_private_message(
    recipient: String,
    message: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let sender = s.username.clone().ok_or("Not authenticated")?;
    let token = s.token.clone();
    match &s.central {
        Some(client) => {
            client
                .send_private_message(&sender, &recipient, &message, token.as_deref())
                .await
        }
        None => Err("Not connected to central server".to_string()),
    }
}
```

- [ ] **Step 4: Verify compilation**

Run: `cd tauri-client/src-tauri && cargo check`

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src-tauri/src/commands/channels.rs tauri-client/src-tauri/src/commands/friends.rs tauri-client/src-tauri/src/commands/messaging.rs
git commit -m "feat: implement channel, friend, and messaging Tauri commands"
```

---

### Task 12: Wire up commands in lib.rs and mod.rs

**Files:**
- Modify: `tauri-client/src-tauri/src/commands/mod.rs`
- Modify: `tauri-client/src-tauri/src/lib.rs`
- Modify: `tauri-client/src-tauri/src/state.rs`

- [ ] **Step 1: Update commands/mod.rs to re-export all modules**

Replace `tauri-client/src-tauri/src/commands/mod.rs`:

```rust
pub mod auth;
pub mod channels;
pub mod friends;
pub mod messaging;
pub mod servers;

// Keep the ping command for Phase 1 verification
#[tauri::command]
pub fn ping() -> String {
    "pong".to_string()
}
```

- [ ] **Step 2: Update state.rs to use Arc**

Replace `tauri-client/src-tauri/src/state.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::net::central::CentralClient;
use crate::net::community::CommunityClient;

pub struct AppState {
    pub central: Option<CentralClient>,
    pub communities: HashMap<String, CommunityClient>,
    pub username: Option<String>,
    pub token: Option<String>,
    pub credentials: Option<(String, String)>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            central: None,
            communities: HashMap::new(),
            username: None,
            token: None,
            credentials: None,
        }
    }
}

pub type SharedState = Arc<Mutex<AppState>>;
```

- [ ] **Step 3: Update lib.rs to register all commands**

Replace `tauri-client/src-tauri/src/lib.rs`:

```rust
mod commands;
mod events;
mod media;
mod net;
mod state;

use std::sync::Arc;
use tokio::sync::Mutex;
use state::{AppState, SharedState};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Arc::new(Mutex::new(AppState::default())) as SharedState)
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::auth::login,
            commands::auth::register,
            commands::auth::logout,
            commands::servers::request_server_list,
            commands::servers::connect_to_community,
            commands::servers::disconnect_from_community,
            commands::channels::join_channel,
            commands::channels::send_channel_message,
            commands::friends::request_friend_list,
            commands::friends::send_friend_action,
            commands::messaging::send_private_message,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 4: Cargo check**

Run: `cd tauri-client/src-tauri && cargo check`

Expected: compiles with no errors. Fix any type mismatches between `State<'_, SharedState>` and the Arc pattern.

- [ ] **Step 5: Run all tests**

Run: `cd tauri-client/src-tauri && cargo test -- --nocapture`

Expected: all 15 tests pass (proto: 4, framing: 8, connection: 3).

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/commands/mod.rs tauri-client/src-tauri/src/state.rs tauri-client/src-tauri/src/lib.rs
git commit -m "feat: wire up all Phase 2 commands in lib.rs, finalize AppState with Arc"
```

---

### Task 13: Build and verify full compilation

**Files:**
- No new files — verification only

- [ ] **Step 1: Full build**

Run: `cd tauri-client && npm run tauri build -- --debug 2>&1 | head -50`

If the build fails, address errors. Common issues:
- Missing `protoc` binary: install via `choco install protobuf` or `winget install Google.Protobuf`
- Crate version conflicts: check `Cargo.lock` and resolve
- Type errors from `State<SharedState>` usage: ensure all commands use `State<'_, SharedState>` with `state.inner().clone()` or `state.lock().await`

- [ ] **Step 2: Run `cargo clippy`**

Run: `cd tauri-client/src-tauri && cargo clippy -- -W clippy::all`

Fix any warnings.

- [ ] **Step 3: Verify dev mode launches**

Run: `cd tauri-client && npm run tauri dev`

Expected: window opens, dark background, ping button still works.

- [ ] **Step 4: Final commit if any fixes were needed**

```bash
git add -A tauri-client/src-tauri/
git commit -m "fix: resolve build issues from Phase 2 integration"
```

---

## Implementation Notes

### State Ownership Pattern

The trickiest part of this implementation is shared state ownership. `CentralClient` and `CommunityClient` need `Arc<Mutex<AppState>>` for their background tasks (read loop, reconnect) to mutate state (e.g., storing JWT on login). Tauri's `State<>` extractor must wrap the same Arc. The pattern:

1. `lib.rs` creates `Arc<Mutex<AppState>>` and passes it to `.manage()`
2. Commands extract `State<'_, SharedState>` where `SharedState = Arc<Mutex<AppState>>`
3. Commands clone the Arc via `state.inner().clone()` to pass to client constructors
4. Client background tasks hold their own Arc clone

### Reconnection Design

Reconnection is fire-and-forget: when the read loop ends, it spawns a reconnect task. The reconnect task loops with exponential backoff, and on success:
1. Creates a new Connection
2. Re-authenticates (LoginRequest for central, CommunityAuthRequest + JoinChannelRequests for community)
3. Replaces the connection in AppState
4. Spawns a new route_packets task

### Error Handling

All commands return `Result<(), String>`. The `String` error is displayed to the frontend. Network errors in background tasks (read/write loops) trigger reconnection rather than surfacing errors.

### Testing Strategy

- **Unit tests** (automated): framing codec roundtrips, protobuf serialization, backoff timing, packet builder
- **Integration tests** (manual, requires running servers): login flow, server list, community connect, channel messaging, reconnection behavior
