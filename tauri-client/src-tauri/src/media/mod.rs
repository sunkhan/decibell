pub mod capture;
#[cfg(target_os = "linux")]
pub mod capture_pipewire;
#[cfg(target_os = "windows")]
pub mod capture_wgc;
pub mod codec;
pub mod packet;
pub mod pipeline;
pub mod speaking;
pub mod video_packet;

use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use tauri::{AppHandle, Emitter};

use pipeline::{ControlMessage, VoiceEvent};

pub struct VoiceEngine {
    audio_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    control_tx: mpsc::Sender<ControlMessage>,
    is_muted: bool,
    is_deafened: bool,
    was_muted_before_deafen: bool,
}

impl VoiceEngine {
    pub fn start(
        server_host: &str,
        server_port: u16,
        jwt: &str,
        app: AppHandle,
    ) -> Result<Self, String> {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        // UDP audio port is server_port + 1
        let udp_addr = format!("{}:{}", server_host, server_port + 1);

        // Sender ID: last 31 chars of JWT
        let jwt_bytes = jwt.as_bytes();
        let sender_id = if jwt_bytes.len() > 31 {
            String::from_utf8_lossy(&jwt_bytes[jwt_bytes.len() - 31..]).to_string()
        } else {
            jwt.to_string()
        };

        let audio_thread = thread::Builder::new()
            .name("decibell-audio".to_string())
            .spawn(move || {
                pipeline::run_audio_pipeline(udp_addr, sender_id, control_rx, event_tx);
            })
            .map_err(|e| format!("Failed to spawn audio thread: {}", e))?;

        // Voice event bridge: poll event_rx and emit Tauri events
        let event_bridge = tokio::spawn(async move {
            loop {
                let rx_result = tokio::task::block_in_place(|| {
                    event_rx.recv_timeout(std::time::Duration::from_millis(50))
                });

                match rx_result {
                    Ok(event) => match event {
                        VoiceEvent::SpeakingChanged(username, speaking) => {
                            let _ = app.emit("voice_user_speaking", serde_json::json!({
                                "username": username,
                                "speaking": speaking,
                            }));
                        }
                        VoiceEvent::UserStateChanged(username, is_muted, is_deafened) => {
                            let _ = app.emit("voice_user_state_changed", serde_json::json!({
                                "username": username,
                                "isMuted": is_muted,
                                "isDeafened": is_deafened,
                            }));
                        }
                        VoiceEvent::PingMeasured(ms) => {
                            let _ = app.emit("voice_ping_updated", serde_json::json!({
                                "latencyMs": ms,
                            }));
                        }
                        VoiceEvent::Error(msg) => {
                            let _ = app.emit("voice_error", serde_json::json!({
                                "message": msg,
                            }));
                        }
                    },
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        Ok(VoiceEngine {
            audio_thread: Some(audio_thread),
            event_bridge: Some(event_bridge),
            control_tx,
            is_muted: false,
            is_deafened: false,
            was_muted_before_deafen: false,
        })
    }

    pub fn stop(&mut self) {
        let _ = self.control_tx.send(ControlMessage::Shutdown);
        if let Some(handle) = self.audio_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.event_bridge.take() {
            handle.abort();
        }
    }

    pub fn set_mute(&mut self, muted: bool) {
        self.is_muted = muted;
        let _ = self.control_tx.send(ControlMessage::SetMute(muted));
    }

    pub fn set_deafen(&mut self, deafened: bool) {
        self.is_deafened = deafened;
        let _ = self.control_tx.send(ControlMessage::SetDeafen(deafened));
        if deafened {
            self.was_muted_before_deafen = self.is_muted;
            self.is_muted = true;
        } else {
            self.is_muted = self.was_muted_before_deafen;
        }
    }

    pub fn set_voice_threshold(&self, db: f32) {
        let _ = self.control_tx.send(ControlMessage::SetVoiceThreshold(db));
    }

    pub fn is_muted(&self) -> bool { self.is_muted }
    pub fn is_deafened(&self) -> bool { self.is_deafened }
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        self.stop();
    }
}
