//! Hotkeys: the store's way to the core, the actions a press runs, and what the Keybinds
//! tab and the app's window call (`features/hotkeys/hotkeyRuntime.ts`, `KeybindsTab.tsx`).

use std::rc::Rc;

use decibell_core::api;
use decibell_core::hotkeys::{Action, Binding};
use gpui::App;

use super::voice::core;
use crate::Model;
use crate::call::call;
use crate::live::calls::CallStatus;
use crate::live::hotkeys::{HotkeyBackend, store_bindings};

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

/// The core's hotkey calls (`hotkeys_configure`, `set_input_mode`, `hotkeys_set_paused`,
/// `hotkeys_inject`).
pub(crate) fn backend() -> HotkeyBackend {
    HotkeyBackend {
        configure: Rc::new(|bindings: Vec<Binding>| {
            Box::pin(async move {
                let bindings = bindings
                    .into_iter()
                    .map(|b| api::hotkeys::HotkeyBindingArg {
                        id: b.id,
                        action: serde_json::to_value(b.action).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(),
                        keys: b.keys,
                    })
                    .collect();
                core(api::hotkeys::hotkeys_configure(api::hotkeys::HotkeysConfigureArgs { bindings })).await
            })
        }),
        set_input_mode: Rc::new(|push_to_talk, release_delay_ms| {
            if crate::core::is_running() {
                api::hotkeys::set_input_mode(api::hotkeys::SetInputModeArgs { push_to_talk, release_delay_ms });
            }
        }),
        set_paused: Rc::new(|paused| {
            if crate::core::is_running() {
                api::hotkeys::hotkeys_set_paused(api::hotkeys::HotkeysSetPausedArgs { paused });
            }
        }),
        inject: Rc::new(|action, pressed, source| {
            if !crate::core::is_running() {
                return;
            }
            let action = serde_json::to_value(action).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            if let Err(e) = api::hotkeys::hotkeys_inject(api::hotkeys::HotkeysInjectArgs { action, pressed, source: source.into() }) {
                log::error!("[hotkeys] inject failed: {e}");
            }
        }),
    }
}

/// A press action fired (`hotkey_action`; `runAction`). Hold actions and releases never
/// run anything here: the core gates the mic itself.
pub fn run_action(action: Action, pressed: bool, cx: &mut App) {
    if !pressed || action.is_hold() {
        return;
    }
    let Some(model) = model(cx) else { return };
    let incoming = model.live.calls.read(cx).status == CallStatus::Incoming;
    match action {
        Action::ToggleMute => super::voice::toggle_mute(cx),
        Action::ToggleDeafen => super::voice::toggle_deafen(cx),
        Action::LeaveVoice => super::voice::leave_voice_or_call(cx),
        Action::AnswerCall if incoming => super::calls::accept_call(cx),
        Action::DeclineCall if incoming => super::calls::decline_call(cx),
        _ => {}
    }
}

/// Pull the listener's status once (an event may already have brought a newer one).
pub fn pull_status(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if !crate::core::is_running() || model.live.hotkeys.read(cx).status.is_some() {
        return;
    }
    let status = decibell_core::hotkeys::status();
    let settings = model.settings.read(cx).get().clone();
    model.live.hotkeys.update(cx, |h, cx| h.set_status(status, &settings, cx));
}

/// Replace the bindings (the Keybinds tab: add, change, remove). Saved, and pushed to the
/// core by the store's settings observer.
pub fn set_bindings(bindings: Vec<Binding>, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.settings.update(cx, |s, cx| s.change(|v| store_bindings(v, &bindings), cx));
}

/// Voice activity or push to talk, and the release delay (ms, 0..=1000).
pub fn set_input_mode(push_to_talk: bool, release_delay_ms: Option<u32>, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.settings.update(cx, |s, cx| {
        s.change(
            |v| {
                v.input_mode = Some(if push_to_talk { "push_to_talk" } else { "voice_activity" }.into());
                if let Some(ms) = release_delay_ms {
                    v.ptt_release_delay_ms = Some(ms.min(crate::live::hotkeys::MAX_PTT_RELEASE_DELAY_MS));
                }
            },
            cx,
        )
    });
}

/// The recorder starts / stops capturing a combo (presses are swallowed meanwhile).
pub fn set_recording(recording: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let settings = model.settings.read(cx).get().clone();
    model.live.hotkeys.update(cx, |h, cx| h.set_recording(recording, &settings, cx));
}

/// Open the desktop's own shortcut editor (GlobalShortcuts portal v2). `done` gets the
/// tab's inline error.
pub fn open_system_settings(cx: &mut App, done: impl FnOnce(Option<String>, &mut App) + 'static) {
    call(cx, api::hotkeys::hotkeys_open_system_settings(), move |r, cx| done(r.err().map(|e| e.js_string().to_string()), cx));
}

/// `decibell --hotkey=<action>[:down|:up]` from a second launch (the app's single-instance
/// glue calls this with the argument): the same dispatcher the listeners use, source
/// "cli". A bare hold action means "down"; ":up" only for hold actions.
pub fn inject_cli(arg: &str) -> bool {
    let (action, edge) = arg.split_once(':').map_or((arg, None), |(a, e)| (a, Some(e)));
    let Some(parsed) = Action::parse(action) else { return false };
    let pressed = match edge {
        None | Some("down") => true,
        Some("up") if parsed.is_hold() => false,
        _ => return false,
    };
    if crate::core::is_running() {
        let _ = api::hotkeys::hotkeys_inject(api::hotkeys::HotkeysInjectArgs {
            action: action.into(),
            pressed,
            source: decibell_core::hotkeys::CLI_SOURCE.into(),
        });
    }
    true
}
