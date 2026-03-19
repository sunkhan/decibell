use tauri::{AppHandle, State};

use crate::events;
use crate::net::central::CentralClient;
use crate::state::SharedState;

#[tauri::command]
pub async fn login(
    username: String,
    password: String,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let state_arc = state.inner().clone();

    // Disconnect existing central client if any
    {
        let mut s = state_arc.lock().await;
        if let Some(mut old) = s.central.take() {
            old.disconnect();
        }
        // Store credentials for reconnection
        s.credentials = Some((username.clone(), password.clone()));
    }

    // Connect to central server
    let client = CentralClient::connect(app.clone(), state_arc.clone()).await?;

    // Send login request
    client.login(&username, &password).await?;

    // Store client in state
    let mut s = state_arc.lock().await;
    s.central = Some(client);

    Ok(())
}

#[tauri::command]
pub async fn register(
    username: String,
    email: String,
    password: String,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let state_arc = state.inner().clone();

    // If not connected yet, connect first
    {
        let s = state_arc.lock().await;
        if s.central.is_none() {
            drop(s);
            let client = CentralClient::connect(app.clone(), state_arc.clone()).await?;
            let mut s = state_arc.lock().await;
            s.central = Some(client);
        }
    }

    let s = state_arc.lock().await;
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
    state: State<'_, SharedState>,
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
