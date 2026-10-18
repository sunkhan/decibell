//! Token renewal. Central mints a fresh 24 h token for the signed-in
//! session (TOKEN_REFRESH_REQ, every few hours, and with a rename). It
//! replaces ours and goes to every community session as
//! SESSION_TOKEN_UPDATE, which takes the name it carries without a
//! reconnect (uid design: a rename is only a database change). A
//! community connection's UDP key stays derived from the token it
//! authenticated with (`CommunityClient::auth_jwt`); a reconnect
//! authenticates with the newest token. Electron main's attachment
//! registry re-reads its targets on `session_token_updated`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use crate::net::connection::build_packet;
use crate::net::proto::{packet, SessionTokenUpdate, TokenRefreshRequest};
use crate::state::AppState;

/// Tokens live 24 h; renew well inside that.
const REFRESH_EVERY: Duration = Duration::from_secs(6 * 3600);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// A fresh central token: ours from now on, and every community session's.
pub async fn adopt(state: &Arc<Mutex<AppState>>, jwt: String) {
    if jwt.is_empty() {
        return;
    }
    let sends: Vec<(mpsc::Sender<Vec<u8>>, Vec<u8>)> = {
        let mut s = state.lock().await;
        s.token = Some(jwt.clone());
        s.communities
            .values_mut()
            .filter_map(|c| {
                c.jwt = jwt.clone();
                let tx = c.connection_write_tx()?;
                let data = build_packet(
                    packet::Type::SessionTokenUpdate,
                    packet::Payload::SessionTokenUpdate(SessionTokenUpdate { jwt_token: jwt.clone() }),
                    None,
                );
                Some((tx, data))
            })
            .collect()
    };
    for (tx, data) in sends {
        let _ = tokio::time::timeout(SEND_TIMEOUT, tx.send(data)).await;
    }
    // Electron main authenticates attachment fetches and uploads with its
    // own copy of the token; without this it kept the one from community
    // auth, and every attachment failed once that expired (review B2).
    crate::events::emit_session_token_updated();
}

/// Ask central for a fresh token every REFRESH_EVERY while signed in. Owned
/// by the CentralClient (aborted with it); the reply lands in its router,
/// which calls `adopt`.
pub fn spawn_refresh_loop(state: Arc<Mutex<AppState>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(REFRESH_EVERY).await;
            let send = {
                let s = state.lock().await;
                // Never without a token: central drops the connection for a
                // packet that doesn't carry a valid one.
                match (s.token.as_deref(), s.central.as_ref().and_then(|c| c.connection_write_tx())) {
                    (Some(token), Some(tx)) => Some((
                        tx,
                        build_packet(
                            packet::Type::TokenRefreshReq,
                            packet::Payload::TokenRefreshReq(TokenRefreshRequest {}),
                            Some(token),
                        ),
                    )),
                    _ => None,
                }
            };
            if let Some((tx, data)) = send {
                let _ = tokio::time::timeout(SEND_TIMEOUT, tx.send(data)).await;
            }
        }
    })
}
