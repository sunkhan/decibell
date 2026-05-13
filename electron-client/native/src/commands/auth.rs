//! Login / register / logout. The flow:
//!
//!   1. login(args) opens a TLS connection to the central server (if
//!      one isn't already up), sends LoginReq, persists credentials,
//!      and returns Ok. The actual `login_succeeded` event fires
//!      asynchronously when central's LoginRes arrives — see
//!      net/central.rs::route_packets.
//!   2. register(args) reuses the central connection and sends a
//!      RegisterReq. Response goes through `register_responded`.
//!   3. logout(args) tears down all connections, clears in-memory and
//!      on-disk credentials, and emits `logged_out`.

use crate::config;
use crate::events;
use crate::net::central::CentralClient;
use crate::net::connection::build_packet;
use crate::net::proto::{packet, FetchAvatarReq, RegisterRequest, UpdateAvatarReq};
use crate::state;
use tokio::sync::oneshot;

#[napi(object)]
pub struct LoginArgs {
    pub username: String,
    pub password: String,
}

#[napi]
pub async fn login(args: LoginArgs) -> napi::Result<()> {
    let state_arc = state::shared();

    let LoginArgs { username, password } = args;

    // Disconnect any existing central client first. Stash credentials
    // for reconnect-time re-authentication.
    let old_central = {
        let mut s = state_arc.lock().await;
        s.credentials = Some((username.clone(), password.clone()));
        s.central.take()
    };
    if let Some(mut old) = old_central {
        old.disconnect();
    }

    let client = CentralClient::connect(state_arc.clone())
        .await
        .map_err(napi::Error::from_reason)?;
    client
        .login(&username, &password)
        .await
        .map_err(napi::Error::from_reason)?;

    let mut s = state_arc.lock().await;
    s.central = Some(client);
    drop(s);

    // Persist credentials so a restart auto-logs-in. If save fails we
    // still proceed — the user is logged in for this session.
    let creds = config::Credentials {
        username: username.clone(),
        password: password.clone(),
    };
    let settings = config::load().map(|c| c.settings).unwrap_or_default();
    let _ = config::save(Some(&creds), &settings);

    Ok(())
}

#[napi(object)]
pub struct RegisterArgs {
    pub username: String,
    pub email: String,
    pub password: String,
}

#[napi]
pub async fn register(args: RegisterArgs) -> napi::Result<()> {
    let state_arc = state::shared();

    // If not connected yet, open a central connection on the way in
    // so the register packet has somewhere to land.
    {
        let s = state_arc.lock().await;
        if s.central.is_none() {
            drop(s);
            let client = CentralClient::connect(state_arc.clone())
                .await
                .map_err(napi::Error::from_reason)?;
            let mut s = state_arc.lock().await;
            s.central = Some(client);
        }
    }

    // Build the packet + clone the write_tx under lock, then drop the
    // lock before awaiting the bounded-channel send. Same pattern as
    // every other AppState-touching network path — never hold the lock
    // across an await that can block on a dead socket.
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let client = s
            .central
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not connected to central server"))?;
        let tx = client
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Central connection lost"))?;
        let pkt = build_packet(
            packet::Type::RegisterReq,
            packet::Payload::RegisterReq(RegisterRequest {
                username: args.username,
                password: args.password,
                email: args.email,
            }),
            None,
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi]
pub async fn logout() -> napi::Result<()> {
    let state_arc = state::shared();

    // Extract clients and clear state under lock, then disconnect outside.
    let (old_central, old_communities) = {
        let mut s = state_arc.lock().await;
        let central = s.central.take();
        let communities: Vec<_> = s.communities.drain().map(|(_, c)| c).collect();
        s.username = None;
        s.token = None;
        s.credentials = None;
        (central, communities)
    };

    if let Some(mut central) = old_central {
        central.disconnect();
    }
    for mut client in old_communities {
        client.disconnect();
    }

    events::emit_logged_out();

    let _ = config::clear_credentials();

    Ok(())
}

// ─── Avatar upload / fetch ───────────────────────────────────────────
// In-band protobuf round-trips against the central server (see docs/
// superpowers/specs/2026-05-12-custom-profile-pictures-design.md §6).
// The central router (net/central.rs route_packets) resolves the
// oneshot Sender we stash on AppState when the matching response
// arrives.

#[napi(object)]
pub struct UploadAvatarResult {
    pub success: bool,
    pub message: String,
    /// sha256-hex of the uploaded bytes; '' on removal.
    pub version: String,
}

#[napi(object)]
pub struct UploadAvatarArgs {
    pub jpeg: napi::bindgen_prelude::Buffer,
}

/// Upload or remove the authenticated user's avatar. Empty `jpeg`
/// argument = remove. Returns the server-computed sha256-hex version
/// on success.
#[napi]
pub async fn upload_avatar(args: UploadAvatarArgs) -> napi::Result<UploadAvatarResult> {
    let UploadAvatarArgs { jpeg } = args;
    let state_arc = state::shared();

    let (write_tx, data, rx) = {
        let mut s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        let token = s.token.clone();
        let pkt = build_packet(
            packet::Type::UpdateAvatarReq,
            packet::Payload::UpdateAvatarReq(UpdateAvatarReq {
                data: jpeg.as_ref().to_vec(),
            }),
            token.as_deref(),
        );
        let (otx, orx) = oneshot::channel();
        // Single-slot — replace any earlier in-flight upload's
        // waiter (the previous .await will time out).
        s.pending_avatar_update = Some(otx);
        (tx, pkt, orx)
    };

    if tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data))
        .await
        .is_err()
    {
        state_arc.lock().await.pending_avatar_update = None;
        return Err(napi::Error::from_reason("Failed to send avatar"));
    }

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(resp)) => Ok(UploadAvatarResult {
            success: resp.success,
            message: resp.message,
            version: resp.version,
        }),
        Ok(Err(_)) => Err(napi::Error::from_reason(
            "Central connection closed before response",
        )),
        Err(_) => {
            state_arc.lock().await.pending_avatar_update = None;
            Err(napi::Error::from_reason("Upload timed out"))
        }
    }
}

#[napi(object)]
pub struct FetchAvatarResult {
    pub version: String,
    /// Empty Buffer when version == '' (no avatar).
    pub data: napi::bindgen_prelude::Buffer,
}

#[napi(object)]
pub struct FetchAvatarArgs {
    pub username: String,
}

/// Fetch a specific user's avatar bytes + current version. Empty
/// version + empty data means the user has no avatar (or doesn't
/// exist — same response shape).
#[napi]
pub async fn fetch_avatar(args: FetchAvatarArgs) -> napi::Result<FetchAvatarResult> {
    let FetchAvatarArgs { username } = args;
    let state_arc = state::shared();

    let (write_tx, data, rx) = {
        let mut s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        let token = s.token.clone();
        let pkt = build_packet(
            packet::Type::FetchAvatarReq,
            packet::Payload::FetchAvatarReq(FetchAvatarReq {
                username: username.clone(),
            }),
            token.as_deref(),
        );
        let (otx, orx) = oneshot::channel();
        // Last-request-wins per username — a previous in-flight
        // fetch for the same user gets its oneshot replaced; the
        // earlier .await times out, no harm.
        s.pending_avatar_fetches.insert(username.clone(), otx);
        (tx, pkt, orx)
    };

    if tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data))
        .await
        .is_err()
    {
        state_arc.lock().await.pending_avatar_fetches.remove(&username);
        return Err(napi::Error::from_reason("Failed to send fetch request"));
    }

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(resp)) => Ok(FetchAvatarResult {
            version: resp.version,
            data: resp.data.into(),
        }),
        Ok(Err(_)) => Err(napi::Error::from_reason(
            "Central connection closed before response",
        )),
        Err(_) => {
            state_arc
                .lock()
                .await
                .pending_avatar_fetches
                .remove(&username);
            Err(napi::Error::from_reason("Fetch timed out"))
        }
    }
}
