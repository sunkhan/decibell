# Phase 2 Design: Rust Networking — TCP/TLS + Protobuf

## Overview

Phase 2 implements the Rust networking layer for the Decibell Tauri client. This replaces the C++ `ChatBackend`'s TCP/TLS networking with async Rust, using `tokio` for async I/O, `tokio-rustls` for TLS, and `prost` for Protobuf serialization. It covers connections to both the central server and community servers, exposes Tauri commands for the frontend, and emits Tauri events for incoming data.

No UI changes — Phase 3 builds the React frontend that consumes these commands and events.

## Background

The Decibell client communicates with two types of servers over TCP/TLS:

- **Central server** (`127.0.0.1:8080`): auth (login/register), friend system, DMs, presence, server directory
- **Community servers** (per-server host:port): text channels, channel messages, authenticated via JWT from central

Both use the same wire protocol: 4-byte big-endian length prefix followed by a Protobuf `Packet` body (defined in `proto/messages.proto`). The `Packet` has a `type` enum field and a `payload` bytes field containing the specific message.

Reference: `ARCHITECTURE.md` sections 2-4 and 8.

---

## Rust Crate Dependencies

Add to `tauri-client/src-tauri/Cargo.toml`:

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-rustls = "0.26"
rustls = { version = "0.23", features = ["ring"] }
prost = "0.13"
bytes = "1"
tokio-util = { version = "0.7", features = ["codec"] }
webpki-roots = "1"

[build-dependencies]
prost-build = "0.13"
```

- **`tokio`** — async runtime for TCP I/O and task spawning
- **`tokio-rustls`** + **`rustls`** — TLS over tokio streams, configured to skip cert verification (matching existing `ssl::verify_none`)
- **`prost`** + **`prost-build`** — protobuf code generation from `proto/messages.proto`
- **`bytes`** — efficient byte buffer handling for framing
- **`tokio-util`** (codec) — length-delimited framing codec
- **`webpki-roots`** — Mozilla root certificates (needed by rustls as a base, even though we skip verification)

---

## Module Structure

```
src-tauri/src/
├── net/
│   ├── mod.rs              # Module declarations, re-exports
│   ├── proto.rs            # Generated protobuf code (include! from OUT_DIR)
│   ├── framing.rs          # Length-prefix codec: 4-byte BE length + protobuf body
│   ├── tls.rs              # TLS connector factory (rustls, skip cert verification)
│   ├── connection.rs       # TCP/TLS connection: connect, read loop, write queue, reconnect
│   ├── central.rs          # Central server client: login, register, friends, DMs, server list
│   └── community.rs        # Community server client: JWT auth, channels, messages
├── commands/
│   ├── mod.rs              # Re-exports all command functions
│   ├── auth.rs             # login, register, logout
│   ├── servers.rs          # request_server_list, connect_to_community, disconnect_from_community
│   ├── channels.rs         # join_channel, send_channel_message
│   ├── friends.rs          # request_friend_list, send_friend_action
│   └── messaging.rs        # send_private_message
├── events/
│   └── mod.rs              # Event name constants + typed emit helpers
├── state.rs                # AppState with auth info, central client, community clients map
├── media/
│   └── mod.rs              # Empty (Phase 4-5)
└── lib.rs                  # Register all commands, initialize AppState in Tauri builder
```

### Module Responsibilities

**`net/framing.rs`** — Pure codec. Implements `tokio_util::codec::Encoder` and `Decoder` for the length-prefix protocol. Encodes: takes a `Vec<u8>` (serialized protobuf), prepends 4-byte big-endian length. Decodes: reads 4-byte length, then reads that many bytes as the body. Max body size: 2MB (matching server limit).

**`net/tls.rs`** — Creates a `tokio_rustls::TlsConnector` configured with a custom `rustls::ClientConfig` that skips certificate verification. This matches the existing C++ client's `ssl::verify_none` behavior. Single function: `create_tls_connector() -> TlsConnector`.

**`net/connection.rs`** — Manages a single TCP/TLS connection lifecycle:
- `connect(host, port)` — TLS handshake, returns framed read/write halves
- Spawns an async read loop task that decodes `Packet` objects and sends them through an `mpsc::Receiver`
- Write method sends serialized packets through an `mpsc::Sender` to a write loop task
- Auto-reconnect with exponential backoff: 1s → 2s → 4s → 8s → 16s → 30s (capped)
- Emits `connection_lost` / `connection_restored` Tauri events
- Exposes `disconnect()` to stop reconnection and close cleanly

**`net/central.rs`** — `CentralClient` struct wrapping a `Connection`. Methods:
- `connect(app_handle)` — connect to `127.0.0.1:8080`
- `login(username, password)` — send `LoginRequest`, wait for `LoginResponse` via event
- `register(username, email, password)` — send `RegisterRequest`
- `request_server_list()` — send `ServerListRequest`
- `send_private_message(recipient, message)` — send `DirectMessage`
- `request_friend_list()` — send `FriendListRequest`
- `send_friend_action(action, target)` — send `FriendActionRequest`
- `disconnect()` — close connection, stop reconnection
- Internal: packet routing — matches `Packet.type` and emits corresponding Tauri events

On reconnect: automatically re-sends `LoginRequest` with stored credentials.

**`net/community.rs`** — `CommunityClient` struct wrapping a `Connection`. Methods:
- `connect(server_id, host, port, jwt, app_handle)` — connect + send `CommunityAuthRequest`
- `join_channel(channel_id)` — send `JoinChannelRequest`
- `send_channel_message(channel_id, message)` — send `ChannelMessage`
- `disconnect()` — close connection, stop reconnection
- Internal: packet routing for community-specific types, emits Tauri events

On reconnect: automatically re-sends `CommunityAuthRequest` with stored JWT.

**`commands/*.rs`** — Thin Tauri command handlers. Each command:
1. Extracts `AppState` from Tauri managed state
2. Locks the mutex
3. Calls the corresponding method on `CentralClient` or `CommunityClient`
4. Returns `Result<(), String>` — success means the request was sent; actual response comes via events

**`events/mod.rs`** — Constants for event names and typed helper functions:
- `emit_login_succeeded(app, username)`
- `emit_server_list(app, servers)`
- `emit_message_received(app, context, sender, content, timestamp)`
- etc.

**`state.rs`** — `AppState`:
```rust
pub struct AppState {
    pub central: Option<CentralClient>,
    pub communities: HashMap<String, CommunityClient>,
    pub username: Option<String>,
    pub token: Option<String>,       // JWT
    pub credentials: Option<(String, String)>,  // (username, password) for reconnect
}
```
Wrapped in `tokio::sync::Mutex` and registered as Tauri managed state.

---

## Protobuf Build Integration

**`build.rs`** compiles `proto/messages.proto`:
```rust
fn main() {
    tauri_build::build();
    prost_build::compile_protos(
        &["../../proto/messages.proto"],
        &["../../proto/"],
    ).expect("Failed to compile protobuf");
}
```

**`net/proto.rs`** includes the generated code:
```rust
include!(concat!(env!("OUT_DIR"), "/chatproj.rs"));
```

This generates Rust structs for all messages (`LoginRequest`, `LoginResponse`, `Packet`, etc.) with `prost::Message` derive for serialization.

**Packet construction pattern:**
```rust
use prost::Message;

fn build_packet(msg_type: packet::Type, payload: &impl Message) -> Vec<u8> {
    let packet = Packet {
        r#type: msg_type as i32,
        payload: payload.encode_to_vec(),
        ..Default::default()
    };
    packet.encode_to_vec()
}
```

**Packet routing pattern (in read loop):**
```rust
match packet::Type::try_from(packet.r#type) {
    Ok(packet::Type::LoginRes) => {
        let resp = LoginResponse::decode(&packet.payload[..])?;
        emit_login_result(app, resp);
    }
    Ok(packet::Type::ServerListRes) => { ... }
    // ... other types
    _ => { /* log unknown type */ }
}
```

---

## Tauri Commands

All commands are `async` and take `State<'_, Mutex<AppState>>` + `AppHandle`.

### Auth Commands (`commands/auth.rs`)

**`login(username, password)`**
1. Create `CentralClient`, connect to `127.0.0.1:8080` via TLS
2. Store credentials in `AppState` (for reconnection)
3. Send `LoginRequest` packet
4. Return `Ok(())` — `login_succeeded` or `login_failed` event fires when response arrives

**`register(username, email, password)`**
1. If not connected to central, connect first
2. Send `RegisterRequest` packet
3. Return `Ok(())` — `register_responded` event fires

**`logout()`**
1. Disconnect central client (stops reconnection)
2. Disconnect all community clients
3. Clear auth state (username, token, credentials)
4. Emit `logged_out` event

### Server Commands (`commands/servers.rs`)

**`request_server_list()`**
1. Send `ServerListRequest` to central
2. `server_list_received` event fires with server array

**`connect_to_community(server_id, host, port)`**
1. Create `CommunityClient`, connect via TLS
2. Send `CommunityAuthRequest` with JWT from `AppState`
3. Store in `AppState.communities` map
4. `community_auth_responded` event fires

**`disconnect_from_community(server_id)`**
1. Remove from `AppState.communities`, call `disconnect()`

### Channel Commands (`commands/channels.rs`)

**`join_channel(server_id, channel_id)`**
1. Find community client by server_id
2. Send `JoinChannelRequest`

**`send_channel_message(server_id, channel_id, message)`**
1. Find community client by server_id
2. Send `ChannelMessage` with sender from `AppState.username`

### Friend Commands (`commands/friends.rs`)

**`request_friend_list()`**
1. Send `FriendListRequest` to central
2. `friend_list_received` event fires

**`send_friend_action(action, target_username)`**
1. Send `FriendActionRequest` to central
2. `friend_action_responded` event fires

### Messaging Commands (`commands/messaging.rs`)

**`send_private_message(recipient, message)`**
1. Send `DirectMessage` to central with sender from `AppState.username`

---

## Tauri Events

All events emitted from the Rust read loops when server responses arrive.

| Event Name | Payload Type | Trigger |
|------------|-------------|---------|
| `login_succeeded` | `{ username: string }` | `LoginResponse` with success=true |
| `login_failed` | `{ message: string }` | `LoginResponse` with success=false |
| `register_responded` | `{ success: bool, message: string }` | `RegisterResponse` |
| `logged_out` | `{}` | After logout cleanup |
| `connection_lost` | `{ serverType: "central" \| "community", serverId?: string }` | TCP disconnect |
| `connection_restored` | `{ serverType: "central" \| "community", serverId?: string }` | Successful reconnect |
| `server_list_received` | `{ servers: [{ id, name, description, hostIp, port, memberCount }] }` | `ServerListResponse` |
| `community_auth_responded` | `{ serverId: string, success: bool, message: string, channels: [{ id, name, type }] }` | `CommunityAuthResponse` |
| `message_received` | `{ context: string, sender: string, content: string, timestamp: string }` | `DirectMessage` or `ChannelMessage` |
| `presence_updated` | `{ users: string[] }` | `PresenceUpdate` |
| `friend_list_received` | `{ friends: [{ username, status }] }` | `FriendListResponse` |
| `friend_action_responded` | `{ success: bool, message: string }` | `FriendActionResponse` |

Event payloads are Rust structs with `#[derive(Serialize, Clone)]` emitted via `app_handle.emit("event_name", payload)`.

---

## Connection Lifecycle

### Reconnection with Exponential Backoff

```
Connected
    ↓ (disconnect detected)
Emit connection_lost
    ↓
Wait 1s → try reconnect
    ↓ fail → wait 2s → try reconnect
    ↓ fail → wait 4s → try reconnect
    ↓ fail → wait 8s → try reconnect
    ↓ fail → wait 16s → try reconnect
    ↓ fail → wait 30s → try reconnect (capped at 30s)
    ↓ success
Reset backoff to 1s
Re-authenticate (LoginRequest for central, CommunityAuthRequest for community)
Emit connection_restored
```

Reconnection stops when:
- User explicitly calls `logout` or `disconnect_from_community`
- A new `login` call replaces the connection

### Central Server Lifecycle
1. `login` command → TLS connect → send `LoginRequest`
2. Read loop starts, routes packets to Tauri events
3. On disconnect → auto-reconnect → re-send `LoginRequest` with stored credentials
4. On `logout` → disconnect, stop reconnection, clear state

### Community Server Lifecycle
1. `connect_to_community` → TLS connect → send `CommunityAuthRequest` with JWT
2. Read loop starts, routes community packets
3. On disconnect → auto-reconnect → re-send `CommunityAuthRequest` with stored JWT
4. On `disconnect_from_community` → disconnect, stop reconnection, remove from map

---

## Error Handling

- Tauri commands return `Result<(), String>`. Errors: "Not connected", "Not authenticated", "Server not found", etc.
- Network errors in read/write loops trigger reconnection, not panics
- Protobuf decode errors are logged and the packet is skipped
- TLS handshake failures during reconnect increment the backoff timer

---

## Verification Criteria

Phase 2 is complete when:

1. `cargo check` passes with all networking code
2. Unit test: framing codec correctly round-trips length-prefixed packets
3. Unit test: prost serialization/deserialization works for `LoginRequest`/`LoginResponse`
4. Integration: `login` Tauri command can be invoked from frontend (temporary test button)
5. With local central server running: login succeeds, `login_succeeded` event fires, friend list and server list can be requested
6. With local community server running: connect + auth + join channel + send/receive channel message works
7. Reconnection: disconnect a server mid-session, observe `connection_lost` event, automatic reconnect after backoff, `connection_restored` event

---

## Out of Scope

- UDP networking (voice/video transport) — Phase 4-5
- Voice/video commands (`joinVoiceChannel`, `leaveVoiceChannel`, `startVideoStream`) — Phase 4-5
- Frontend UI or store wiring — Phase 3
- Settings UI for server address — Phase 6
- Offline message queueing — not planned
