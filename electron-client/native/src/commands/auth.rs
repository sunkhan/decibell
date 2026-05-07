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
use crate::net::proto::{packet, RegisterRequest};
use crate::state;

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
