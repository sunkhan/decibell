# Settings UI & Config Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a tabbed settings modal with persistent config file, encrypted auto-login, and runtime audio device selection.

**Architecture:** Rust `config.rs` module handles AES-256-GCM encrypted config persistence. New `commands/settings.rs` exposes load/save/device-list commands. Pipeline gains `SetInputDevice`/`SetOutputDevice` control messages for hot-swapping CPAL streams. React frontend gets a redesigned `SettingsModal` with four tab components (Account, Privacy, Audio, About).

**Tech Stack:** Rust (aes-gcm, sha2 crates), Tauri 2 commands, React + Zustand + Tailwind CSS 4

**Spec:** `docs/superpowers/specs/2026-04-03-settings-and-config-design.md`

---

### Task 1: Add Rust Dependencies

**Files:**
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add aes-gcm and sha2 to Cargo.toml**

Add under `[dependencies]` (base64 already exists):

```toml
aes-gcm = "0.10"
sha2 = "0.10"
```

- [ ] **Step 2: Verify it compiles**

Run: `cd src-tauri && cargo check`
Expected: Compiles with warnings only, no errors.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "chore: add aes-gcm, sha2 dependencies for config encryption"
```

---

### Task 2: Config Module (`config.rs`)

**Files:**
- Create: `src-tauri/src/config.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod config;`)

- [ ] **Step 1: Create `src-tauri/src/config.rs`**

```rust
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const SALT: &[u8] = b"decibell-config-v1";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppSettings {
    #[serde(default)]
    pub friends_only_dms: bool,
    #[serde(default)]
    pub stream_stereo: bool,
    /// None means "use system default"
    pub input_device: Option<String>,
    /// None means "use system default"
    pub output_device: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// On-disk config format
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConfigFile {
    /// AES-256-GCM encrypted + base64-encoded credentials JSON
    credentials: Option<String>,
    #[serde(default)]
    settings: AppSettings,
}

/// Returned to the frontend on load
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedConfig {
    pub credentials: Option<Credentials>,
    pub settings: AppSettings,
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    Ok(dir.join("config.json"))
}

fn derive_key() -> [u8; 32] {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown-host".to_string());
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown-user".to_string());

    let mut hasher = Sha256::new();
    hasher.update(SALT);
    hasher.update(hostname.as_bytes());
    hasher.update(user.as_bytes());
    hasher.finalize().into()
}

fn encrypt_credentials(creds: &Credentials) -> Result<String, String> {
    let key = derive_key();
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("Cipher init failed: {}", e))?;

    let json = serde_json::to_vec(creds).map_err(|e| format!("Serialize failed: {}", e))?;

    // Random 12-byte nonce
    let nonce_bytes: [u8; 12] = rand_nonce();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, json.as_ref())
        .map_err(|e| format!("Encrypt failed: {}", e))?;

    // Prepend nonce to ciphertext
    let mut combined = Vec::with_capacity(12 + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
}

fn decrypt_credentials(encrypted: &str) -> Result<Credentials, String> {
    let key = derive_key();
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("Cipher init failed: {}", e))?;

    let combined = base64::engine::general_purpose::STANDARD
        .decode(encrypted)
        .map_err(|e| format!("Base64 decode failed: {}", e))?;

    if combined.len() < 12 {
        return Err("Encrypted data too short".to_string());
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| "Decryption failed (wrong machine or corrupted config)".to_string())?;

    serde_json::from_slice(&plaintext).map_err(|e| format!("Deserialize failed: {}", e))
}

fn rand_nonce() -> [u8; 12] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut hasher = Sha256::new();
    hasher.update(seed.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let hash = hasher.finalize();
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&hash[..12]);
    nonce
}

pub fn load(app: &AppHandle) -> Result<LoadedConfig, String> {
    let path = config_path(app)?;
    if !path.exists() {
        return Ok(LoadedConfig {
            credentials: None,
            settings: AppSettings::default(),
        });
    }

    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read config: {}", e))?;

    let config: ConfigFile =
        serde_json::from_str(&data).map_err(|e| format!("Failed to parse config: {}", e))?;

    let credentials = config
        .credentials
        .as_deref()
        .and_then(|enc| decrypt_credentials(enc).ok());

    Ok(LoadedConfig {
        credentials,
        settings: config.settings,
    })
}

pub fn save(app: &AppHandle, credentials: Option<&Credentials>, settings: &AppSettings) -> Result<(), String> {
    let path = config_path(app)?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config dir: {}", e))?;
    }

    let encrypted_creds = match credentials {
        Some(creds) => Some(encrypt_credentials(creds)?),
        None => {
            // Preserve existing encrypted credentials if we're only updating settings
            if path.exists() {
                let data = std::fs::read_to_string(&path).ok();
                data.and_then(|d| serde_json::from_str::<ConfigFile>(&d).ok())
                    .and_then(|c| c.credentials)
            } else {
                None
            }
        }
    };

    let config = ConfigFile {
        credentials: encrypted_creds,
        settings: settings.clone(),
    };

    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    std::fs::write(&path, json).map_err(|e| format!("Failed to write config: {}", e))?;

    Ok(())
}

pub fn clear_credentials(app: &AppHandle) -> Result<(), String> {
    let path = config_path(app)?;
    if !path.exists() {
        return Ok(());
    }

    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read config: {}", e))?;

    let mut config: ConfigFile =
        serde_json::from_str(&data).unwrap_or_default();

    config.credentials = None;

    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    std::fs::write(&path, json).map_err(|e| format!("Failed to write config: {}", e))?;

    Ok(())
}
```

- [ ] **Step 2: Add `hostname` crate to Cargo.toml**

Add under `[dependencies]`:

```toml
hostname = "0.4"
```

- [ ] **Step 3: Register the module in `src-tauri/src/lib.rs`**

Add `mod config;` after the existing module declarations:

```rust
mod commands;
mod config;
mod events;
mod media;
mod net;
mod state;
```

- [ ] **Step 4: Verify it compiles**

Run: `cd src-tauri && cargo check`
Expected: Compiles with warnings only, no errors.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config.rs src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat: add config module with AES-256-GCM encrypted persistence"
```

---

### Task 3: Settings Tauri Commands

**Files:**
- Create: `src-tauri/src/commands/settings.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Create `src-tauri/src/commands/settings.rs`**

```rust
use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::config;
use crate::state::SharedState;

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice {
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioDeviceList {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
}

#[tauri::command]
pub async fn load_config(
    app: AppHandle,
) -> Result<config::LoadedConfig, String> {
    config::load(&app)
}

#[tauri::command]
pub async fn save_settings(
    settings: config::AppSettings,
    app: AppHandle,
) -> Result<(), String> {
    config::save(&app, None, &settings)
}

#[tauri::command]
pub async fn list_audio_devices() -> Result<AudioDeviceList, String> {
    let host = cpal::default_host();

    let inputs: Vec<AudioDevice> = host
        .input_devices()
        .map_err(|e| format!("Failed to list input devices: {}", e))?
        .filter_map(|d| d.name().ok().map(|name| AudioDevice { name }))
        .collect();

    let outputs: Vec<AudioDevice> = host
        .output_devices()
        .map_err(|e| format!("Failed to list output devices: {}", e))?
        .filter_map(|d| d.name().ok().map(|name| AudioDevice { name }))
        .collect();

    Ok(AudioDeviceList { inputs, outputs })
}

#[tauri::command]
pub async fn set_input_device(
    name: Option<String>,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    // Save to config
    let current = config::load(&app)?;
    let mut settings = current.settings;
    settings.input_device = name.clone();
    config::save(&app, None, &settings)?;

    // Send to pipeline if voice is active
    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_input_device(name);
    }
    Ok(())
}

#[tauri::command]
pub async fn set_output_device(
    name: Option<String>,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    // Save to config
    let current = config::load(&app)?;
    let mut settings = current.settings;
    settings.output_device = name.clone();
    config::save(&app, None, &settings)?;

    // Send to pipeline if voice is active
    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_output_device(name);
    }
    Ok(())
}
```

- [ ] **Step 2: Register the module in `src-tauri/src/commands/mod.rs`**

Add `pub mod settings;` to the module list:

```rust
pub mod auth;
pub mod channels;
pub mod friends;
pub mod messaging;
pub mod servers;
pub mod settings;
pub mod streaming;
pub mod voice;
```

- [ ] **Step 3: Register commands in `src-tauri/src/lib.rs`**

Add after the existing `commands::voice::set_user_volume` line in the `invoke_handler`:

```rust
            commands::voice::set_user_volume,
            commands::settings::load_config,
            commands::settings::save_settings,
            commands::settings::list_audio_devices,
            commands::settings::set_input_device,
            commands::settings::set_output_device,
```

- [ ] **Step 4: Add VoiceEngine methods for device hot-swap**

In `src-tauri/src/media/mod.rs`, add these methods to `impl VoiceEngine` after `set_stream_stereo`:

```rust
    pub fn set_input_device(&self, name: Option<String>) {
        let _ = self.control_tx.send(ControlMessage::SetInputDevice(name));
    }

    pub fn set_output_device(&self, name: Option<String>) {
        let _ = self.control_tx.send(ControlMessage::SetOutputDevice(name));
    }
```

- [ ] **Step 5: Add ControlMessage variants**

In `src-tauri/src/media/pipeline.rs`, add to the `ControlMessage` enum after `SetUserVolume`:

```rust
    SetInputDevice(Option<String>),  // None = system default
    SetOutputDevice(Option<String>), // None = system default
```

- [ ] **Step 6: Add placeholder handlers in the pipeline control message match block**

In `src-tauri/src/media/pipeline.rs`, in the control message match block (after `SetStreamStereo` handler, before the `Err` arms), add:

```rust
                Ok(ControlMessage::SetInputDevice(name)) => {
                    eprintln!("[pipeline] SetInputDevice: {:?}", name);
                    // Hot-swap implemented in Task 4
                }
                Ok(ControlMessage::SetOutputDevice(name)) => {
                    eprintln!("[pipeline] SetOutputDevice: {:?}", name);
                    // Hot-swap implemented in Task 4
                }
```

- [ ] **Step 7: Verify it compiles**

Run: `cd src-tauri && cargo check`
Expected: Compiles with warnings only, no errors.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/commands/settings.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs src-tauri/src/media/mod.rs src-tauri/src/media/pipeline.rs
git commit -m "feat: add settings Tauri commands, device listing, config save/load"
```

---

### Task 4: Pipeline Audio Device Hot-Swap

**Files:**
- Modify: `src-tauri/src/media/pipeline.rs`

This is the most complex backend task. The pipeline currently creates CPAL streams once at startup. We need to make them replaceable at runtime.

- [ ] **Step 1: Wrap input/output streams in `Arc<Mutex<Option<cpal::Stream>>>`**

Currently the pipeline has:
```rust
let input_stream_opt: Option<cpal::Stream> = ...;
// and later
let output_stream = ...;
```

The input and output streams need to be wrapped so they can be replaced. The ring buffers (`capture_prod`, `voice_cons`, `stream_cons`) are already `Arc<Mutex<...>>` so the new streams can reuse them.

Change the input stream variable at line ~254 from:
```rust
let input_stream_opt: Option<cpal::Stream> = match input_device_opt {
```
to store the stream in an Arc<Mutex<>> that can be swapped:
```rust
let input_stream_holder: Arc<std::sync::Mutex<Option<cpal::Stream>>> = Arc::new(std::sync::Mutex::new(None));
```

Then after building the input stream, store it:
```rust
if let Ok(mut holder) = input_stream_holder.lock() {
    *holder = built_stream; // the Option<cpal::Stream> from the match
}
```

Do the same for the output stream — wrap in `Arc<Mutex<Option<cpal::Stream>>>`.

- [ ] **Step 2: Extract input stream building into a helper function**

Create a function inside `run_audio_pipeline` (or as a standalone function) that builds an input stream given a device name and the shared ring buffer producer:

```rust
fn build_input_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    output_sample_rate: u32,
    capture_prod: Arc<std::sync::Mutex<ringbuf::Producer<i16, Arc<ringbuf::SharedRb<ringbuf::storage::Heap<i16>>>>>>,
) -> Option<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let device = match device_name {
        Some(name) => {
            let found = host.input_devices().ok()?.find(|d| {
                d.name().map(|n| n == name).unwrap_or(false)
            });
            match found {
                Some(d) => d,
                None => {
                    eprintln!("[pipeline] Input device '{}' not found, using default", name);
                    host.default_input_device()?
                }
            }
        }
        None => host.default_input_device()?,
    };

    // Use the same input config logic as the existing code (lines 266-310):
    // get default config, try to match output sample rate, build stream with
    // resampler if needed, push to capture_prod ring buffer.
    // (This is a refactor of the existing code block into the helper.)

    // ... existing input stream build logic moved here ...
}
```

The exact implementation should move the existing input stream build code (lines 261-387) into this helper. The capture callback closure captures `capture_prod` (which is already Arc<Mutex<>>).

- [ ] **Step 3: Extract output stream building into a helper function**

Same pattern — extract the output stream build (lines 390-570) into:

```rust
fn build_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    voice_cons: Arc<std::sync::Mutex<...>>,
    stream_cons: Arc<std::sync::Mutex<...>>,
    stream_stereo: Arc<std::sync::atomic::AtomicBool>,
) -> Option<(cpal::Stream, u32, u16)> // stream, sample_rate, channels
```

- [ ] **Step 4: Implement the hot-swap handlers**

Replace the placeholder handlers from Task 3 Step 6 with:

```rust
                Ok(ControlMessage::SetInputDevice(name)) => {
                    eprintln!("[pipeline] Hot-swapping input device to: {:?}", name);
                    // Drop old stream
                    if let Ok(mut holder) = input_stream_holder.lock() {
                        *holder = None; // drops the old stream
                    }
                    // Build new stream
                    let new_stream = build_input_stream(
                        &host,
                        name.as_deref(),
                        output_sample_rate,
                        Arc::clone(&capture_prod),
                    );
                    if let Ok(mut holder) = input_stream_holder.lock() {
                        *holder = new_stream;
                    }
                }
                Ok(ControlMessage::SetOutputDevice(name)) => {
                    eprintln!("[pipeline] Hot-swapping output device to: {:?}", name);
                    if let Ok(mut holder) = output_stream_holder.lock() {
                        *holder = None;
                    }
                    let new_stream = build_output_stream(
                        &host,
                        name.as_deref(),
                        Arc::clone(&voice_cons),
                        Arc::clone(&stream_cons),
                        Arc::clone(&stream_stereo),
                    );
                    if let Some((stream, _rate, _ch)) = new_stream {
                        if let Ok(mut holder) = output_stream_holder.lock() {
                            *holder = Some(stream);
                        }
                    }
                }
```

- [ ] **Step 5: Verify it compiles**

Run: `cd src-tauri && cargo check`
Expected: Compiles with warnings only, no errors.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/media/pipeline.rs
git commit -m "feat: audio device hot-swap via control messages"
```

---

### Task 5: Auth Integration — Save/Clear Credentials

**Files:**
- Modify: `src-tauri/src/commands/auth.rs`

- [ ] **Step 1: Save credentials on successful login**

The login flow works via events — `CentralClient::connect` + `client.login()` sends a login request, and the server responds with a `login_succeeded` event. The credentials are already stored in `AppState.credentials`. We need to save them to the config file when the login succeeds.

In `src-tauri/src/commands/auth.rs`, add at the top:

```rust
use crate::config;
```

Then in the `login` function, after `s.central = Some(client);` (line 35), add:

```rust
    // Save credentials to config for auto-login
    let creds = config::Credentials {
        username: username.clone(),
        password: password.clone(),
    };
    // Load existing settings to preserve them
    let settings = config::load(&app)
        .map(|c| c.settings)
        .unwrap_or_default();
    let _ = config::save(&app, Some(&creds), &settings);
```

- [ ] **Step 2: Clear credentials on logout**

In the `logout` function, after `events::emit_logged_out(&app);` (line 112), add:

```rust
    // Clear saved credentials but keep settings
    let _ = config::clear_credentials(&app);
```

- [ ] **Step 3: Verify it compiles**

Run: `cd src-tauri && cargo check`
Expected: Compiles with warnings only, no errors.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands/auth.rs
git commit -m "feat: save encrypted credentials on login, clear on logout"
```

---

### Task 6: Frontend — Settings Store Updates

**Files:**
- Modify: `src/stores/uiStore.ts`

- [ ] **Step 1: Add settings state to uiStore**

Add these fields to the `UiState` interface, after `contextMenuAnchor`:

```typescript
  streamStereo: boolean;           // already exists
  setStreamStereo: (value: boolean) => void;  // already exists
  inputDevice: string | null;
  outputDevice: string | null;
  settingsTab: string;
  setInputDevice: (device: string | null) => void;
  setOutputDevice: (device: string | null) => void;
  setSettingsTab: (tab: string) => void;
```

And add the corresponding implementations in the `create` call, after the existing `setStreamStereo`:

```typescript
  inputDevice: null,
  outputDevice: null,
  settingsTab: "account",
  setInputDevice: (device) => set({ inputDevice: device }),
  setOutputDevice: (device) => set({ outputDevice: device }),
  setSettingsTab: (tab) => set({ settingsTab: tab }),
```

- [ ] **Step 2: Commit**

```bash
git add src/stores/uiStore.ts
git commit -m "feat: add device and settings tab state to uiStore"
```

---

### Task 7: Frontend — Auto-Login on Startup

**Files:**
- Modify: `src/App.tsx`

- [ ] **Step 1: Add auto-login logic to App.tsx**

Add a `useEffect` in the `App` component that loads config on startup and auto-logs in. Place it inside the `App` function, before the return:

```tsx
import { useState, useEffect, Component, type ReactNode } from "react";
```

Replace the existing `App` function with:

```tsx
export default function App() {
  const [ready, setReady] = useState(false);

  useEffect(() => {
    (async () => {
      try {
        const config = await invoke<{
          credentials?: { username: string; password: string };
          settings: {
            friends_only_dms: boolean;
            stream_stereo: boolean;
            input_device: string | null;
            output_device: string | null;
          };
        }>("load_config");

        // Apply saved settings to stores
        const { settings } = config;
        useDmStore.getState().setFriendsOnlyDms(settings.friends_only_dms);
        useUiStore.getState().setStreamStereo(settings.stream_stereo);
        useUiStore.getState().setInputDevice(settings.input_device);
        useUiStore.getState().setOutputDevice(settings.output_device);

        // Auto-login if credentials saved
        if (config.credentials) {
          useAuthStore.getState().setLoggingIn(true);
          try {
            await invoke("login", {
              username: config.credentials.username,
              password: config.credentials.password,
            });
          } catch {
            // Login failed — clear the error, user can log in manually
            useAuthStore.getState().setLoginError(null);
          }
        }
      } catch {
        // No config file or load failed — that's fine
      }
      setReady(true);
    })();
  }, []);

  if (!ready) {
    return (
      <div className="flex h-screen w-screen items-center justify-center bg-bg-primary">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-accent border-t-transparent" />
      </div>
    );
  }

  return (
    <ErrorBoundary>
      <BrowserRouter>
        <Routes>
          <Route element={<AppLayout />}>
            <Route path="/login" element={<LoginPage />} />
            <Route path="/" element={<ProtectedRoute><MainLayout /></ProtectedRoute>} />
          </Route>
        </Routes>
      </BrowserRouter>
    </ErrorBoundary>
  );
}
```

Also add the missing imports at the top of App.tsx:

```tsx
import { useState, useEffect, Component, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUiStore } from "./stores/uiStore";
import { useDmStore } from "./stores/dmStore";
```

Remove the `/settings` route (we're using a modal now) and the `SettingsPage` import.

- [ ] **Step 2: Commit**

```bash
git add src/App.tsx
git commit -m "feat: auto-login from saved config on app startup"
```

---

### Task 8: Frontend — Settings Modal Rewrite

**Files:**
- Rewrite: `src/features/settings/SettingsModal.tsx`

- [ ] **Step 1: Rewrite SettingsModal.tsx with tabbed layout**

```tsx
import { createPortal } from "react-dom";
import { useUiStore } from "../../stores/uiStore";
import AccountTab from "./tabs/AccountTab";
import PrivacyTab from "./tabs/PrivacyTab";
import AudioTab from "./tabs/AudioTab";
import AboutTab from "./tabs/AboutTab";
import { useEffect } from "react";

const TABS = [
  {
    id: "account",
    label: "Account",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <circle cx="12" cy="8" r="4" />
        <path d="M20 21a8 8 0 10-16 0" />
      </svg>
    ),
    component: AccountTab,
  },
  {
    id: "privacy",
    label: "Privacy",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <rect x="3" y="11" width="18" height="11" rx="2" />
        <path d="M7 11V7a5 5 0 0110 0v4" />
      </svg>
    ),
    component: PrivacyTab,
  },
  {
    id: "audio",
    label: "Audio",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <path d="M9 18V5l12-2v13" />
        <circle cx="6" cy="18" r="3" />
        <circle cx="18" cy="16" r="3" />
      </svg>
    ),
    component: AudioTab,
  },
  {
    id: "about",
    label: "About",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <circle cx="12" cy="12" r="10" />
        <line x1="12" y1="16" x2="12" y2="12" />
        <line x1="12" y1="8" x2="12.01" y2="8" />
      </svg>
    ),
    component: AboutTab,
  },
];

export default function SettingsModal() {
  const activeModal = useUiStore((s) => s.activeModal);
  const closeModal = useUiStore((s) => s.closeModal);
  const settingsTab = useUiStore((s) => s.settingsTab);
  const setSettingsTab = useUiStore((s) => s.setSettingsTab);

  useEffect(() => {
    if (activeModal !== "settings") return;
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeModal();
    };
    window.addEventListener("keydown", handleEscape);
    return () => window.removeEventListener("keydown", handleEscape);
  }, [activeModal, closeModal]);

  if (activeModal !== "settings") return null;

  const activeTab = TABS.find((t) => t.id === settingsTab) ?? TABS[0];
  const TabComponent = activeTab.component;

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={closeModal}
    >
      <div
        className="flex h-[500px] w-[700px] overflow-hidden rounded-2xl border border-border bg-bg-secondary shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Left sidebar */}
        <div className="flex w-[220px] shrink-0 flex-col gap-0.5 border-r border-border bg-bg-tertiary px-3 py-5">
          <div className="mb-1 px-3 text-[10px] font-bold uppercase tracking-[0.08em] text-text-muted">
            Settings
          </div>
          {TABS.map((tab) => (
            <button
              key={tab.id}
              onClick={() => setSettingsTab(tab.id)}
              className={`flex items-center gap-2.5 rounded-lg px-3 py-2 text-[13px] font-semibold transition-colors ${
                settingsTab === tab.id
                  ? "bg-accent-soft text-accent-bright"
                  : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
              }`}
            >
              {tab.icon}
              {tab.label}
            </button>
          ))}
        </div>

        {/* Right content */}
        <div className="flex flex-1 flex-col overflow-y-auto">
          <div className="flex items-center justify-between px-7 pt-6 pb-4">
            <h2 className="text-lg font-extrabold text-text-bright">
              {activeTab.label}
            </h2>
            <button
              onClick={closeModal}
              className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>
          <div className="flex-1 px-7 pb-6">
            <TabComponent />
          </div>
        </div>
      </div>
    </div>,
    document.body
  );
}
```

- [ ] **Step 2: Commit**

```bash
git add src/features/settings/SettingsModal.tsx
git commit -m "feat: rewrite SettingsModal with tabbed sidebar layout"
```

---

### Task 9: Frontend — Account Tab

**Files:**
- Create: `src/features/settings/tabs/AccountTab.tsx`

- [ ] **Step 1: Create AccountTab.tsx**

```tsx
import { invoke } from "@tauri-apps/api/core";
import { useAuthStore } from "../../../stores/authStore";

export default function AccountTab() {
  const username = useAuthStore((s) => s.username);

  const handleLogout = async () => {
    try {
      await invoke("logout");
    } catch (err) {
      console.error("Logout failed:", err);
    }
  };

  const initial = username ? username.charAt(0).toUpperCase() : "?";

  return (
    <div>
      {/* User card */}
      <div className="flex items-center gap-3.5 rounded-xl bg-bg-primary px-5 py-4">
        <div className="flex h-11 w-11 shrink-0 items-center justify-center rounded-full bg-gradient-to-br from-accent to-accent-bright text-lg font-bold text-white">
          {initial}
        </div>
        <div>
          <div className="text-sm font-bold text-text-bright">{username}</div>
          <div className="mt-0.5 text-[11px] text-text-muted">Logged in</div>
        </div>
      </div>

      {/* Logout */}
      <button
        onClick={handleLogout}
        className="mt-5 rounded-lg border border-danger/20 bg-danger/10 px-5 py-2 text-[13px] font-semibold text-danger transition-colors hover:bg-danger/20"
      >
        Log Out
      </button>
    </div>
  );
}
```

- [ ] **Step 2: Commit**

```bash
git add src/features/settings/tabs/AccountTab.tsx
git commit -m "feat: add Account settings tab"
```

---

### Task 10: Frontend — Shared Settings Helper + Privacy Tab

**Files:**
- Create: `src/features/settings/saveSettings.ts`
- Create: `src/features/settings/tabs/PrivacyTab.tsx`

- [ ] **Step 1: Create shared `saveSettings` helper**

Create `src/features/settings/saveSettings.ts`:

```ts
import { invoke } from "@tauri-apps/api/core";
import { useUiStore } from "../../stores/uiStore";
import { useDmStore } from "../../stores/dmStore";

export function saveSettings() {
  const { streamStereo, inputDevice, outputDevice } = useUiStore.getState();
  const { friendsOnlyDms } = useDmStore.getState();
  invoke("save_settings", {
    settings: {
      friends_only_dms: friendsOnlyDms,
      stream_stereo: streamStereo,
      input_device: inputDevice,
      output_device: outputDevice,
    },
  }).catch(console.error);
}
```

- [ ] **Step 2: Create PrivacyTab.tsx**

```tsx
import { invoke } from "@tauri-apps/api/core";
import { useDmStore } from "../../../stores/dmStore";
import { saveSettings } from "../saveSettings";

export default function PrivacyTab() {
  const friendsOnlyDms = useDmStore((s) => s.friendsOnlyDms);

  const handleToggle = () => {
    const newValue = !friendsOnlyDms;
    useDmStore.getState().setFriendsOnlyDms(newValue);
    invoke("set_dm_privacy", { friendsOnly: newValue }).catch(console.error);
    saveSettings();
  };

  return (
    <div>
      <div className="flex items-center justify-between rounded-xl bg-bg-primary px-4 py-3">
        <div>
          <div className="text-[13px] font-semibold text-text-primary">
            Only accept DMs from friends
          </div>
          <div className="mt-0.5 text-[11px] text-text-muted">
            When enabled, only users in your friends list can send you direct messages
          </div>
        </div>
        <button
          onClick={handleToggle}
          className={`relative h-6 w-11 shrink-0 rounded-full transition-colors ${
            friendsOnlyDms ? "bg-accent" : "bg-text-muted/30"
          }`}
        >
          <div
            className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-transform ${
              friendsOnlyDms ? "translate-x-[22px]" : "translate-x-0.5"
            }`}
          />
        </button>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Commit**

```bash
git add src/features/settings/saveSettings.ts src/features/settings/tabs/PrivacyTab.tsx
git commit -m "feat: add shared saveSettings helper and Privacy tab"
```

---

### Task 11: Frontend — Audio Tab

**Files:**
- Create: `src/features/settings/tabs/AudioTab.tsx`

- [ ] **Step 1: Create AudioTab.tsx**

```tsx
import { useEffect, useState, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUiStore } from "../../../stores/uiStore";
import { saveSettings } from "../saveSettings";

interface AudioDevice {
  name: string;
}

interface AudioDeviceList {
  inputs: AudioDevice[];
  outputs: AudioDevice[];
}

function DeviceSelector({
  label,
  icon,
  devices,
  selected,
  onChange,
}: {
  label: string;
  icon: React.ReactNode;
  devices: AudioDevice[];
  selected: string | null;
  onChange: (name: string | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  const displayName = selected ?? "Default";

  return (
    <div className="rounded-xl bg-bg-primary p-4" ref={ref}>
      <div className="mb-2.5 flex items-center gap-2.5">
        {icon}
        <span className="text-[13px] font-semibold text-text-primary">{label}</span>
      </div>
      <div className="relative">
        <button
          onClick={() => setOpen(!open)}
          className="flex w-full items-center justify-between rounded-lg border border-border bg-bg-secondary px-3 py-2.5 text-left text-[12px] text-text-primary transition-colors hover:border-accent/40"
        >
          <span className="truncate">{displayName}</span>
          <svg
            width="12"
            height="12"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.5"
            strokeLinecap="round"
            className={`shrink-0 text-text-muted transition-transform ${open ? "rotate-180" : ""}`}
          >
            <path d="M6 9l6 6 6-6" />
          </svg>
        </button>
        {open && (
          <div className="absolute left-0 right-0 top-full z-10 mt-1 max-h-48 overflow-y-auto rounded-lg border border-border bg-bg-secondary shadow-xl">
            <button
              onClick={() => {
                onChange(null);
                setOpen(false);
              }}
              className={`flex w-full items-center px-3 py-2 text-left text-[12px] transition-colors hover:bg-surface-hover ${
                selected === null ? "text-accent-bright font-semibold" : "text-text-secondary"
              }`}
            >
              Default
            </button>
            {devices.map((device) => (
              <button
                key={device.name}
                onClick={() => {
                  onChange(device.name);
                  setOpen(false);
                }}
                className={`flex w-full items-center px-3 py-2 text-left text-[12px] transition-colors hover:bg-surface-hover ${
                  selected === device.name ? "text-accent-bright font-semibold" : "text-text-secondary"
                }`}
              >
                {device.name}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

export default function AudioTab() {
  const [devices, setDevices] = useState<AudioDeviceList>({ inputs: [], outputs: [] });
  const inputDevice = useUiStore((s) => s.inputDevice);
  const outputDevice = useUiStore((s) => s.outputDevice);
  const streamStereo = useUiStore((s) => s.streamStereo);

  useEffect(() => {
    invoke<AudioDeviceList>("list_audio_devices")
      .then(setDevices)
      .catch(console.error);
  }, []);

  const handleInputChange = (name: string | null) => {
    useUiStore.getState().setInputDevice(name);
    invoke("set_input_device", { name }).catch(console.error);
    saveSettings();
  };

  const handleOutputChange = (name: string | null) => {
    useUiStore.getState().setOutputDevice(name);
    invoke("set_output_device", { name }).catch(console.error);
    saveSettings();
  };

  const handleStereoToggle = () => {
    const newValue = !streamStereo;
    useUiStore.getState().setStreamStereo(newValue);
    invoke("set_stream_stereo", { enabled: newValue }).catch(console.error);
    saveSettings();
  };

  return (
    <div>
      {/* Devices section */}
      <div className="mb-1.5 text-[10px] font-bold uppercase tracking-[0.08em] text-text-muted">
        Devices
      </div>
      <div className="mb-2 flex flex-col gap-2">
        <DeviceSelector
          label="Input Device"
          icon={
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="text-text-secondary">
              <path d="M12 1a3 3 0 00-3 3v8a3 3 0 006 0V4a3 3 0 00-3-3z" />
              <path d="M19 10v2a7 7 0 01-14 0v-2" />
              <line x1="12" y1="19" x2="12" y2="23" />
            </svg>
          }
          devices={devices.inputs}
          selected={inputDevice}
          onChange={handleInputChange}
        />
        <DeviceSelector
          label="Output Device"
          icon={
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="text-text-secondary">
              <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
              <path d="M19.07 4.93a10 10 0 010 14.14" />
              <path d="M15.54 8.46a5 5 0 010 7.07" />
            </svg>
          }
          devices={devices.outputs}
          selected={outputDevice}
          onChange={handleOutputChange}
        />
      </div>

      {/* Divider */}
      <div className="my-4 h-px bg-border" />

      {/* Stream Audio section */}
      <div className="mb-1.5 text-[10px] font-bold uppercase tracking-[0.08em] text-text-muted">
        Stream Audio
      </div>
      <div className="flex items-center justify-between rounded-xl bg-bg-primary px-4 py-3">
        <div>
          <div className="text-[13px] font-semibold text-text-primary">
            Stereo stream audio
          </div>
          <div className="mt-0.5 text-[11px] text-text-muted">
            Preserve left/right stereo positioning when watching streams
          </div>
        </div>
        <button
          onClick={handleStereoToggle}
          className={`relative h-6 w-11 shrink-0 rounded-full transition-colors ${
            streamStereo ? "bg-accent" : "bg-text-muted/30"
          }`}
        >
          <div
            className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-transform ${
              streamStereo ? "translate-x-[22px]" : "translate-x-0.5"
            }`}
          />
        </button>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Commit**

```bash
git add src/features/settings/tabs/AudioTab.tsx
git commit -m "feat: add Audio settings tab with device selectors"
```

---

### Task 12: Frontend — About Tab

**Files:**
- Create: `src/features/settings/tabs/AboutTab.tsx`

- [ ] **Step 1: Create AboutTab.tsx**

```tsx
export default function AboutTab() {
  return (
    <div>
      {/* App info card */}
      <div className="rounded-xl bg-bg-primary px-5 py-5">
        <div className="flex items-center gap-3 mb-3">
          <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-gradient-to-br from-accent to-accent-bright">
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="white" strokeWidth="2.5" strokeLinecap="round">
              <path d="M9 18V5l12-2v13" />
              <circle cx="6" cy="18" r="3" fill="white" stroke="none" />
              <circle cx="18" cy="16" r="3" fill="white" stroke="none" />
            </svg>
          </div>
          <div>
            <div className="text-sm font-extrabold text-text-bright">Decibell</div>
            <div className="text-[11px] text-text-muted">Decentralized game chat</div>
          </div>
        </div>
        <div className="text-[12px] text-text-secondary">
          Version <span className="font-semibold text-text-primary">0.2.5</span>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Commit**

```bash
git add src/features/settings/tabs/AboutTab.tsx
git commit -m "feat: add About settings tab"
```

---

### Task 13: Cleanup — Remove Unused SettingsPage

**Files:**
- Delete: `src/pages/SettingsPage.tsx`
- Modify: `src/App.tsx` (remove import and route — already done in Task 7 if followed correctly)

- [ ] **Step 1: Delete the placeholder SettingsPage**

```bash
rm src/pages/SettingsPage.tsx
```

- [ ] **Step 2: Verify no remaining references**

Run: `grep -r "SettingsPage" src/`
Expected: No matches.

- [ ] **Step 3: Verify the app compiles**

Run: `cd src-tauri && cargo check && cd .. && npx tsc --noEmit`
Expected: Both compile with no errors.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore: remove unused SettingsPage placeholder"
```

---

### Task 14: Integration Test — Full Flow Verification

- [ ] **Step 1: Build and run the app**

Run: `npx tauri dev`
Expected: App compiles and launches.

- [ ] **Step 2: Verify settings modal**

1. Click the gear icon in the channel sidebar
2. Modal appears with four tabs: Account, Privacy, Audio, About
3. Each tab switches content correctly
4. Escape and backdrop click close the modal

- [ ] **Step 3: Verify audio device listing**

1. Open Settings > Audio tab
2. Input device dropdown lists available microphones
3. Output device dropdown lists available speakers/headphones
4. "Default" option is present at the top of each list

- [ ] **Step 4: Verify config persistence**

1. Toggle a setting (e.g., stereo stream audio)
2. Close and reopen the app
3. Setting should be remembered

- [ ] **Step 5: Verify auto-login**

1. Log in with credentials
2. Close the app completely
3. Reopen — should auto-login without showing the login page

- [ ] **Step 6: Verify logout clears credentials**

1. Open Settings > Account > Log Out
2. Close and reopen the app
3. Should show login page (credentials were cleared)

- [ ] **Step 7: Commit final version bump and push**

```bash
# Update version to 0.2.6 in:
# - package.json
# - src-tauri/Cargo.toml
# - src-tauri/tauri.conf.json
# - src/features/settings/tabs/AboutTab.tsx
# - src/features/auth/LoginPage.tsx

git add -A
git commit -m "feat(settings): tabbed settings modal, config persistence, audio device selection, auto-login, bump v0.2.6"
git push origin main
git tag v0.2.6
git push origin v0.2.6
```
