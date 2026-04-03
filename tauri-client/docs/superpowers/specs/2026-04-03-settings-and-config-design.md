# Settings UI & Config Persistence Design

**Date:** 2026-04-03
**Status:** Approved

## Overview

Replace the current minimal settings modal with a full tabbed settings overlay, and add a persistent config file so the client remembers user settings and login credentials across sessions.

## 1. Config Persistence

### Storage Location

A `config.json` file in Tauri's app data directory:
- **Windows**: `%APPDATA%/com.decibell.app/config.json`
- **Linux**: `~/.config/com.decibell.app/config.json`

### File Structure

```json
{
  "credentials": "<AES-256-GCM encrypted blob, base64>",
  "settings": {
    "friends_only_dms": false,
    "stream_stereo": false,
    "input_device": "Microphone (Realtek Audio)",
    "output_device": null
  }
}
```

- `credentials` — AES-256-GCM encrypted JSON of `{"username":"...","password":"..."}`. Key derived via SHA-256 from a stable machine identifier (hostname + OS username + hardcoded salt). A random 12-byte nonce is prepended to the ciphertext. Base64-encoded for JSON storage.
- `settings` — Plaintext flat key-value map. `null` means "use system default". New settings are added as fields with defaults; missing fields use defaults on load (forward-compatible).
- New Rust dependencies: `aes-gcm`, `sha2`, `base64` (all pure Rust, cross-platform).

### Auto-Login Flow

1. App starts, frontend calls `load_config`
2. If credentials returned: auto-call `login` with saved username/password
3. If settings returned: apply to Zustand stores (`streamStereo`, `friendsOnlyDms`, device names)
4. If no config file or decryption fails: show normal login page

### Save Triggers

- On successful login: save encrypted credentials
- On logout: clear credentials from config, keep settings
- On any setting change: save full settings object

## 2. Settings UI

### Layout

Large modal overlay (not full-page), rendered via `createPortal`:
- **Size**: ~700px wide, ~500px tall, centered
- **Backdrop**: Dimmed (`bg-black/60`), click to close
- **Left sidebar** (220px): Icon + text tabs, active tab highlighted with accent background
- **Right content area** (flex): Tab content with section title and close button
- **Close**: X button, Escape key, or backdrop click

### Tabs (in order)

1. **Account**
   - Avatar circle with user's first letter initial (accent gradient background)
   - Username display with "Logged in" status
   - Log Out button (red/danger styled, clears saved credentials)

2. **Privacy**
   - "Only accept DMs from friends" toggle (moved from current modal)
   - Calls `set_dm_privacy` and `save_settings`

3. **Audio**
   - **Devices section**: Card-based layout with mic/speaker icons
     - Input Device: dropdown inside card, lists available input devices from `list_audio_devices`
     - Output Device: dropdown inside card, lists available output devices
     - "Default" option at top of each list uses system default device
     - Changing a device immediately hot-swaps the CPAL stream (no disconnect from voice)
   - **Stream Audio section**: Stereo stream audio toggle
     - Calls `set_stream_stereo` and `save_settings`

4. **About**
   - App name ("Decibell")
   - Version number
   - Brief tagline

### Extensibility

Adding a new tab requires only:
1. Create a component file in `src/features/settings/tabs/`
2. Add an entry to the `TABS` array (icon SVG, label, component reference)

No other files need modification.

## 3. Backend

### New Tauri Commands

- `load_config() -> { credentials?: {username, password}, settings }` — Read and decrypt config file. Called once on app startup.
- `save_settings(settings)` — Write settings object to config. Called on any setting change.
- `list_audio_devices() -> { inputs: [{name, id}], outputs: [{name, id}] }` — Enumerate CPAL host devices. Called when Audio tab opens.
- `set_input_device(name)` — Save to config and send `ControlMessage::SetInputDevice` to pipeline.
- `set_output_device(name)` — Save to config and send `ControlMessage::SetOutputDevice` to pipeline.

### Pipeline Hot-Swap

Two new `ControlMessage` variants: `SetInputDevice(String)` and `SetOutputDevice(String)`.

When received, the pipeline:
1. Drops the current CPAL input/output stream
2. Looks up the named device from `cpal::default_host().devices()`
3. Builds a new stream with the same config (resampler, ring buffer connections)
4. Resumes — no disconnect from voice channel needed

If the named device isn't found (e.g., unplugged), falls back to system default and logs a warning.

### Config Module (`config.rs`)

- `AppConfig` struct with `credentials` (encrypted) and `settings` fields
- `load()` — Read from disk, decrypt credentials, return config
- `save()` — Encrypt credentials, write to disk
- `config_path()` — Resolve platform-specific app data directory
- `derive_key()` — SHA-256 of hostname + OS username + hardcoded salt
- `encrypt_credentials()` / `decrypt_credentials()` — AES-256-GCM with random nonce

### Auth Integration

- `login` command: on success, save credentials to config via `config::save()`
- `logout` command: clear credentials from config, preserve settings

## 4. File Structure

### New Files

- `src-tauri/src/config.rs` — Config struct, load/save, AES encryption/decryption, machine key derivation
- `src-tauri/src/commands/settings.rs` — Tauri commands for config and audio devices
- `src/features/settings/tabs/AccountTab.tsx`
- `src/features/settings/tabs/PrivacyTab.tsx`
- `src/features/settings/tabs/AudioTab.tsx`
- `src/features/settings/tabs/AboutTab.tsx`

### Modified Files

- `src/features/settings/SettingsModal.tsx` — Full rewrite with tabbed layout
- `src/stores/uiStore.ts` — Add `inputDevice`, `outputDevice`, `settingsTab` state
- `src-tauri/src/media/pipeline.rs` — Add `SetInputDevice`/`SetOutputDevice` control messages and hot-swap logic
- `src-tauri/src/lib.rs` — Register new commands
- `src-tauri/src/commands/mod.rs` — Add `pub mod settings`
- `src-tauri/Cargo.toml` — Add `aes-gcm`, `sha2`, `base64` deps
- `src/App.tsx` — Add `load_config` call on startup, auto-login logic
- `src-tauri/src/commands/auth.rs` — Save credentials on successful login, clear on logout

### Deleted Files

- `src/pages/SettingsPage.tsx` — Unused placeholder

## 5. State Management

- `streamStereo` — stays in `uiStore`
- `friendsOnlyDms` — stays in `dmStore`
- `inputDevice`, `outputDevice` — added to `uiStore`
- `settingsTab` — added to `uiStore` (tracks which tab is active in the modal)
- All setting changes invoke their Tauri command AND trigger `save_settings`

## 6. Platform Considerations

- AES key derivation uses `hostname()` and OS username — both available on Windows and Linux
- CPAL device enumeration works identically on both platforms
- Config directory resolved via Tauri's `app_data_dir()` which handles platform differences
- Device names may differ between platforms; stored as strings, matched by name on load
