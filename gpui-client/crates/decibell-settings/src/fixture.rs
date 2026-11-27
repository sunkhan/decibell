//! The Settings screens' fixture: the shell's (`decibell_shell::fixture`: perf1 signed in on the
//! local stack, through the model's real router) plus what Settings shows — four devices, an E2EE
//! state, keybinds with the portal's status, audio devices, codec capabilities, transfer caps and
//! the updater's state — and a fixed clock. No network, no config file, no random numbers.
//!
//! [`json`] exports the same state for the Electron capture pages
//! (`tools/electron-captures/pages/settings-*.tsx`), which seed the renderer's stores with it.

use std::collections::HashMap;

use decibell_core::config::{AppSettings, HotkeyBindingSetting};
use decibell_core::events::E2eeStatusPayload;
use decibell_model::live::devices::Device;
use decibell_model::live::e2ee::PassphraseMode;
use decibell_model::live::voice::codecs::{CodecCap, VideoCodec};
use decibell_model::live::voice::devices::AudioDevice;
use decibell_model::stores::overlays::modal;
use decibell_model::stores::update::{UpdateMode, UpdateStatus};
use decibell_model::Model;
use decibell_shell::fixture::Scene;
use gpui::{App, SharedString};

use crate::hooks::{SettingsHooks, Staged};
use crate::tabs::TabId;

/// "Now" for the screens (2025-11-24 16:00 UTC): relative times are the same on every run.
pub const NOW: i64 = 1_764_000_000;
const DAY: i64 = 86_400;

/// The OS Downloads folder the screens show.
pub const DOWNLOADS: &str = "/home/perf1/Downloads";

pub const FINGERPRINT: &str = "K7QM 2XRD 5FTA LW4E HB3N P6CZ U7VY D2GS 5JMR TQ7K XE4W 3NAF";

/// What a screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The window on a tab.
    Tab(TabId),
    /// Privacy with the keys backed up but not unlocked here.
    PrivacyLocked,
    /// Keybinds with the first row recording.
    KeybindsRecording,
    /// The passphrase dialog: set up (no keys yet) or unlock (a backup on central).
    PassphraseSetup,
    PassphraseUnlock,
    /// Account with the rename dialog open.
    Rename,
    /// The change-password dialog over Account.
    Password,
    /// About with the updater in one of its states (`AboutTab.tsx` `UpdateStatusRow`).
    About(UpdateCase),
}

/// The updater's states as About shows them, one screen each (`settings-about-<case>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCase {
    UpToDate,
    Checking,
    /// A package manager's install (notify-only): "Open release page".
    AvailableNotify,
    /// A self-updating install, before the download starts.
    AvailablePreparing,
    Downloading,
    Ready,
    Error,
    /// A development build.
    Disabled,
}

impl UpdateCase {
    pub const ALL: [UpdateCase; 8] = [
        UpdateCase::UpToDate,
        UpdateCase::Checking,
        UpdateCase::AvailableNotify,
        UpdateCase::AvailablePreparing,
        UpdateCase::Downloading,
        UpdateCase::Ready,
        UpdateCase::Error,
        UpdateCase::Disabled,
    ];

    pub fn name(self) -> &'static str {
        match self {
            UpdateCase::UpToDate => "up-to-date",
            UpdateCase::Checking => "checking",
            UpdateCase::AvailableNotify => "available-notify",
            UpdateCase::AvailablePreparing => "available-preparing",
            UpdateCase::Downloading => "downloading",
            UpdateCase::Ready => "ready",
            UpdateCase::Error => "error",
            UpdateCase::Disabled => "disabled",
        }
    }

    /// The state and mode (the model's), and the same as the renderer's store takes them.
    pub fn state(self) -> (UpdateStatus, UpdateMode, serde_json::Value, &'static str) {
        let v: SharedString = "0.9.5".into();
        let (status, mode, json, mode_name) = match self {
            UpdateCase::UpToDate => (
                UpdateStatus::NotAvailable { checked_at: NOW - 3600 },
                UpdateMode::SelfUpdate,
                serde_json::json!({ "state": "not-available", "checkedAt": (NOW - 3600) * 1000 }),
                "self-update",
            ),
            UpdateCase::Checking => (UpdateStatus::Checking, UpdateMode::SelfUpdate, serde_json::json!({ "state": "checking" }), "self-update"),
            UpdateCase::AvailableNotify => (
                UpdateStatus::Available { version: v.clone() },
                UpdateMode::NotifyOnly,
                serde_json::json!({ "state": "available", "version": "0.9.5" }),
                "notify-only",
            ),
            UpdateCase::AvailablePreparing => (
                UpdateStatus::Available { version: v.clone() },
                UpdateMode::SelfUpdate,
                serde_json::json!({ "state": "available", "version": "0.9.5" }),
                "self-update",
            ),
            UpdateCase::Downloading => (
                UpdateStatus::Downloading { pct: 42.4, version: v.clone() },
                UpdateMode::SelfUpdate,
                serde_json::json!({ "state": "downloading", "pct": 42.4, "version": "0.9.5" }),
                "self-update",
            ),
            UpdateCase::Ready => (
                UpdateStatus::Downloaded { version: v.clone() },
                UpdateMode::SelfUpdate,
                serde_json::json!({ "state": "downloaded", "version": "0.9.5" }),
                "self-update",
            ),
            UpdateCase::Error => (
                UpdateStatus::Error { message: "couldn't reach github.com".into() },
                UpdateMode::SelfUpdate,
                serde_json::json!({ "state": "error", "message": "couldn't reach github.com" }),
                "self-update",
            ),
            UpdateCase::Disabled => (UpdateStatus::Idle, UpdateMode::Disabled, serde_json::json!({ "state": "idle" }), "disabled"),
        };
        (status, mode, json, mode_name)
    }
}

pub fn devices() -> Vec<Device> {
    let device = |id: i64, name: &str, platform: &str, version: &str, address: &str, signed_in: i64, active: i64, online: bool, current: bool| Device {
        id,
        name: name.to_string().into(),
        platform: platform.to_string().into(),
        app_version: version.to_string().into(),
        address: address.to_string().into(),
        signed_in_at: signed_in,
        last_active_at: active,
        online,
        current,
    };
    vec![
        device(11, "CachyOS", "linux", "0.1.0", "127.0.0.1", NOW - 6 * DAY, NOW, true, true),
        device(12, "Windows 11 Pro", "windows", "0.9.4", "192.168.1.24", NOW - 40 * DAY, NOW - 3 * 3600, false, false),
        device(13, "macOS Sequoia 15.1", "macos", "0.9.3", "203.0.113.7", NOW - 75 * DAY, NOW - 26 * 3600, false, false),
        device(14, "", "linux", "0.9.4", "", NOW - 12 * DAY, NOW - 12 * 60, false, false),
    ]
}

pub fn bindings() -> Vec<HotkeyBindingSetting> {
    let b = |id: &str, action: &str, keys: &[&str]| HotkeyBindingSetting {
        id: id.into(),
        action: action.into(),
        keys: keys.iter().map(|k| k.to_string()).collect(),
    };
    vec![
        b("fx-mute", "toggle_mute", &["Control", "Shift", "KeyM"]),
        b("fx-deafen", "toggle_deafen", &["Control", "Shift", "KeyD"]),
        b("fx-ptt", "push_to_talk", &["KeyV"]),
        b("fx-leave", "leave_voice", &["Mouse4"]),
    ]
}

/// The listener as KDE Plasma's GlobalShortcuts portal reports it: active, its own editor, one
/// combo re-assigned in the desktop's settings.
pub fn hotkey_status() -> decibell_core::hotkeys::Status {
    let mut s = decibell_core::hotkeys::status();
    s.backend = "portal";
    s.state = "active";
    s.detail = None;
    s.mouse = false;
    s.focused_fallback = false;
    s.can_configure = true;
    s.triggers = HashMap::from([
        ("fx-mute".to_string(), "Ctrl+Shift+M".to_string()),
        ("fx-deafen".to_string(), "Ctrl+Alt+D".to_string()),
        ("fx-ptt".to_string(), "V".to_string()),
    ]);
    s.failed = Vec::new();
    s.registered = Vec::new();
    s
}

pub fn inputs() -> Vec<AudioDevice> {
    vec![
        AudioDevice { name: "alsa_input.usb-Focusrite_Scarlett_Solo".into(), label: "Scarlett Solo USB".into() },
        AudioDevice { name: "alsa_input.pci-0000_00_1f.3.analog-stereo".into(), label: "Built-in Audio Analog Stereo".into() },
    ]
}

pub fn outputs() -> Vec<AudioDevice> {
    vec![
        AudioDevice { name: "alsa_output.usb-Focusrite_Scarlett_Solo".into(), label: "Scarlett Solo USB".into() },
        AudioDevice { name: "alsa_output.pci-0000_01_00.1.hdmi-stereo".into(), label: "HDMI / DisplayPort (RTX 4080)".into() },
        AudioDevice { name: "alsa_output.pci-0000_00_1f.3.analog-stereo".into(), label: "Built-in Audio Analog Stereo".into() },
    ]
}

pub fn codec_caps() -> (Vec<CodecCap>, Vec<CodecCap>) {
    let cap = |codec, hardware| CodecCap { codec, max_width: 3840, max_height: 2160, max_fps: 120, hardware };
    let encode = vec![cap(VideoCodec::Av1, true), cap(VideoCodec::H265, true), cap(VideoCodec::H264Hw, true)];
    let decode = vec![cap(VideoCodec::Av1, true), cap(VideoCodec::H265, true), cap(VideoCodec::H264Hw, true)];
    (encode, decode)
}

pub fn encoder_labels() -> HashMap<VideoCodec, SharedString> {
    [VideoCodec::Av1, VideoCodec::H265, VideoCodec::H264Hw].into_iter().map(|c| (c, SharedString::from("NVENC (NVIDIA)"))).collect()
}

/// The settings the screens show (on top of the shell fixture's).
pub fn settings(base: &AppSettings) -> AppSettings {
    let mut s = base.clone();
    s.input_device = Some("alsa_input.usb-Focusrite_Scarlett_Solo".into());
    s.output_device = None;
    s.separate_stream_output = true;
    s.stream_output_device = Some("alsa_output.pci-0000_01_00.1.hdmi-stereo".into());
    s.stream_stereo = true;
    s.aec_enabled = true;
    s.noise_suppression_level = 1;
    s.agc_enabled = true;
    s.voice_threshold_db = Some(-42.);
    s.input_mode = Some("voice_activity".into());
    s.ptt_release_delay_ms = Some(120);
    s.hotkeys = bindings();
    s.friends_only_dms = false;
    s.away_after_minutes = 10;
    s.link_previews_enabled = true;
    s.gif_unfiltered = false;
    s.crash_reporting_enabled = true;
    s.upload_limit_bps = (5.0 * 1024. * 1024.) as u64;
    s.download_limit_bps = (7.5 * 1024. * 1024.) as u64;
    s.download_dir = String::new();
    s.ask_download_location = false;
    s.take_stream_on_channel_switch = false;
    s.use_av1 = true;
    s.use_h265 = true;
    s
}

fn e2ee_status(status: &str) -> E2eeStatusPayload {
    E2eeStatusPayload {
        supported: true,
        status: status.into(),
        key_id: if status == "ready" { 1 } else { 0 },
        fingerprint: if status == "ready" { FINGERPRINT.into() } else { String::new() },
    }
}

/// Fill the model with the fixture and open what `stage` shows.
pub fn apply(stage: Stage, cx: &mut App) -> Model {
    let model = decibell_shell::fixture::apply(Scene::Home, cx);
    crate::hooks::ensure(cx);
    SettingsHooks::update(cx, |h| {
        h.now = Some(NOW);
        h.app_version = SharedString::new_static("0.1.0");
        h.staged = Staged {
            open_rename: stage == Stage::Rename,
            record_first: stage == Stage::KeybindsRecording,
            meter_level: None,
            encoder_labels: Some(encoder_labels()),
            downloads_default: Some(DOWNLOADS.into()),
        };
    });

    let next = settings(model.settings.read(cx).get());
    model.settings.update(cx, |s, cx| s.apply_loaded(next, cx));
    model.live.devices.update(cx, |d, cx| d.set_list(Ok(devices()), cx));
    let status = hotkey_status();
    let current = model.settings.read(cx).get().clone();
    model.live.hotkeys.update(cx, |h, cx| h.set_status(status, &current, cx));
    model.live.voice.devices.update(cx, |d, cx| {
        d.inputs = inputs();
        d.outputs = outputs();
        d.loaded = true;
        cx.notify();
    });
    let (encode, decode) = codec_caps();
    model.live.voice.codecs.update(cx, |c, cx| {
        c.set_toggles(true, true, cx);
        c.set_caps(encode, decode, cx);
    });
    let (update_status, update_mode, ..) = match stage {
        Stage::About(case) => case.state(),
        _ => UpdateCase::UpToDate.state(),
    };
    model.update.update(cx, |u, cx| u.set(update_status, update_mode, SharedString::default(), cx));

    // The E2EE state: ready, except where a screen shows another.
    let e2ee = match stage {
        Stage::PrivacyLocked | Stage::PassphraseUnlock => "locked",
        Stage::PassphraseSetup => "not_set_up",
        _ => "ready",
    };
    model.live.e2ee.update(cx, |e, cx| {
        e.apply_status(&e2ee_status(e2ee), cx);
        // The first "locked" of a sign-in opens the unlock prompt; only its own screen keeps it.
        if stage != Stage::PassphraseUnlock {
            e.cancel(cx);
        }
        if stage == Stage::PassphraseSetup {
            e.open(PassphraseMode::Setup, cx);
        }
    });

    let tab = match stage {
        Stage::Tab(tab) => tab,
        Stage::PrivacyLocked | Stage::PassphraseSetup | Stage::PassphraseUnlock => TabId::Privacy,
        Stage::KeybindsRecording => TabId::Keybinds,
        Stage::Rename | Stage::Password => TabId::Account,
        Stage::About(_) => TabId::About,
    };
    model.nav.update(cx, |n, cx| n.set_settings_tab(tab.id(), cx));
    model.overlays.update(cx, |o, cx| o.open_modal(modal::SETTINGS, cx));
    if stage == Stage::Password {
        model.live.devices.update(cx, |d, cx| d.open_password(cx));
    }
    model
}

/// The fixture as the Electron pages seed it: the shell fixture's events (signed in as perf1),
/// then the settings, devices, E2EE status, hotkey status, audio devices, codecs, the updater,
/// the clock and what the screen opens.
pub fn json(stage: Stage) -> serde_json::Value {
    let base = AppSettings::default();
    let settings = settings(&base);
    let (encode, decode) = codec_caps();
    let cap_json = |c: &CodecCap| {
        serde_json::json!({
            "codec": c.codec.wire(), "maxWidth": c.max_width, "maxHeight": c.max_height, "maxFps": c.max_fps, "hardware": c.hardware,
        })
    };
    let status = hotkey_status();
    let e2ee = match stage {
        Stage::PrivacyLocked | Stage::PassphraseUnlock => "locked",
        Stage::PassphraseSetup => "not_set_up",
        _ => "ready",
    };
    let tab = match stage {
        Stage::Tab(tab) => tab,
        Stage::PrivacyLocked | Stage::PassphraseSetup | Stage::PassphraseUnlock => TabId::Privacy,
        Stage::KeybindsRecording => TabId::Keybinds,
        Stage::Rename | Stage::Password => TabId::Account,
        Stage::About(_) => TabId::About,
    };
    let (_, _, update_status, update_mode) = match stage {
        Stage::About(case) => case.state(),
        _ => UpdateCase::UpToDate.state(),
    };
    serde_json::json!({
        "shell": decibell_shell::fixture::json(Scene::Home),
        "now": NOW,
        "settings": settings,
        "devices": devices(),
        "e2ee": { "supported": true, "status": e2ee, "keyId": if e2ee == "ready" { 1 } else { 0 }, "fingerprint": if e2ee == "ready" { FINGERPRINT } else { "" } },
        "hotkeys": {
            "bindings": bindings().iter().map(|b| serde_json::json!({ "id": b.id, "action": b.action, "keys": b.keys })).collect::<Vec<_>>(),
            "status": {
                "backend": status.backend, "state": status.state, "detail": status.detail, "mouse": status.mouse,
                "focusedFallback": status.focused_fallback, "canConfigure": status.can_configure,
                "triggers": status.triggers, "failed": status.failed, "registered": status.registered,
            },
            "recording": stage == Stage::KeybindsRecording,
        },
        "audio": {
            "inputs": inputs(), "outputs": outputs(), "level": -28.0,
        },
        "codecs": {
            "useAv1": true, "useH265": true,
            "encode": encode.iter().map(cap_json).collect::<Vec<_>>(),
            "decode": decode.iter().map(cap_json).collect::<Vec<_>>(),
        },
        "update": { "status": update_status, "mode": update_mode, "currentVersion": "0.1.0" },
        "tab": tab.id(),
        "dialog": match stage {
            Stage::PassphraseSetup => "passphrase-setup",
            Stage::PassphraseUnlock => "passphrase-unlock",
            Stage::Rename => "rename",
            Stage::Password => "password",
            _ => "",
        },
        "downloadsDefault": DOWNLOADS,
    })
}
