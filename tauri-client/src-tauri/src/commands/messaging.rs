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
