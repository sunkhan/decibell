//! Settings → Keybinds (`KeybindsTab.tsx`, `features/hotkeys/{keys.ts,KeybindDisplay.tsx}`; spec
//! `2026-10-10-global-hotkeys-design.md`; CLAUDE.md "Global hotkeys see bound keys only").
//!
//! - "On this computer": what this platform's listener reads, in the renderer's words per backend
//!   (the desktop's portal on Wayland, grabs on X11, system hotkeys and polled keys on Windows),
//!   the keys games won't see, the listener's state from the core, the portal's own editor.
//! - "Your keybinds": a row per binding (the action, the keys, remove) with why a binding won't
//!   work as recorded (another action's keys, what the portal can't bind, another app's grab),
//!   "Add keybind", and the push-to-talk hints against the input mode.
//! - The recorder ([`crate::recorder::Recorder`]): click a row's keys and press a combo; the core
//!   pauses the listener meanwhile (`hotkeys_set_paused`); a new combo is saved under a fresh id
//!   (the desktop keeps its own assignment per id). A mouse side button where the portal can't
//!   bind one opens the remap guide instead.
//! - Linux: the command line for window managers that bind commands.
//!
//! Bindings live in `Settings` (`hotkeys`); the model's hotkeys store pushes them to the core.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher as _, Hasher as _};

use decibell_common::Watch;
use decibell_core::config::HotkeyBindingSetting;
use decibell_core::hotkeys::{Action, Status};
use decibell_model::live::hotkeys::{
    HOTKEY_ACTIONS, InputMode, combo_label, key_label, modifier_of, portal_can_bind, same_combo,
};
use decibell_model::{Model, actions};
use decibell_ui::{
    ActiveTheme as _, Button, ButtonSize, FontRole, Icon, IconName, Label, ScrollHandle, Select, SelectOption,
    title_tooltip,
};
use gpui::{
    AnyElement, App, Context, FocusHandle, Hsla, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*,
    px,
};

use super::TabId;
use crate::hooks::SettingsHooks;
use crate::recorder::{Recorded, Recorder};
use crate::rows::{Btn, Look, TextLink, hover, outline_look, section_label};
use crate::window::CloseLink;

// ── How a keybind reads (`KeybindDisplay.tsx`) ───────────────────────────────────────────────

fn is_mouse(key: &str) -> bool {
    matches!(key, "Mouse3" | "Mouse4" | "Mouse5")
}

fn is_hold(action: Action) -> bool {
    action.is_hold()
}

fn action_label(action: Action) -> &'static str {
    HOTKEY_ACTIONS.iter().find(|(a, _, _)| *a == action).map_or("", |(_, label, _)| label)
}

/// The bound press goes to Decibell only: the portal, macOS accelerators and the X server's grab
/// take it from the app in front; Windows' system hotkeys too (the rest is polled).
pub fn is_swallowed(id: &str, keys: &[String], action: Action, status: Option<&Status>) -> bool {
    if keys.is_empty() {
        return false;
    }
    match status.map(|s| s.backend) {
        Some("portal" | "x11" | "xwayland") => true,
        Some("electron") => !is_hold(action),
        Some("windows") => status.is_some_and(|s| s.registered.iter().any(|r| r == id)),
        _ => false,
    }
}

/// A combo that types text: no Ctrl / Alt / Super and a character key (Shift alone still types).
pub fn types_text(keys: &[String]) -> bool {
    let command_mod = keys.iter().any(|k| {
        let m = if matches!(k.as_str(), "Control" | "Alt" | "Meta") { Some(k.as_str()) } else { modifier_of(k) };
        matches!(m, Some("Control" | "Alt" | "Meta"))
    });
    let typing = |k: &str| {
        (k.len() == 4 && k.starts_with("Key"))
            || (k.len() == 6 && k.starts_with("Digit"))
            || matches!(
                k,
                "Space" | "Minus" | "Equal" | "BracketLeft" | "BracketRight" | "Backslash" | "Semicolon" | "Quote"
                    | "Backquote" | "Comma" | "Period" | "Slash" | "IntlBackslash" | "Enter" | "Backspace" | "Tab"
            )
    };
    !command_mod && keys.iter().any(|k| typing(k))
}

/// What the desktop actually assigned (portal), when it isn't the combo recorded here.
pub fn assigned_trigger(id: &str, keys: &[String], status: Option<&Status>) -> Option<String> {
    let status = status?;
    if status.backend != "portal" || keys.is_empty() {
        return None;
    }
    let trigger = status.triggers.get(id)?;
    let norm = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    (norm(trigger) != norm(&combo_label(keys))).then(|| trigger.clone())
}

/// `KeyChips`: each key in a `kbd`, "+" between.
pub fn key_chips(keys: &[String], cx: &App) -> AnyElement {
    let theme = cx.theme();
    let mut row = div().flex().flex_wrap().items_center().gap_1();
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            row = row.child(Label::new("+").size(11.).color(theme.color_text_faint));
        }
        row = row.child(
            div()
                .rounded(theme.radius_sm)
                .border_1()
                .border_color(theme.color_border)
                .bg(theme.color_bg_lighter)
                .px_1p5()
                .py_0p5()
                .child(Label::new(key_label(key)).font(FontRole::Meta).size(12.).weight(500.).color(theme.color_text_primary)),
        );
    }
    row.into_any_element()
}

/// `SwallowBadge`.
pub fn swallow_badge(id: impl Into<gpui::ElementId>, typing: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let why = format!(
        "Your desktop hands this key press to Decibell only, so the game or app you're in won't register it.{} Pick a key your games don't use.",
        if typing { " It won't type in other apps either." } else { "" }
    );
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .rounded(theme.radius_sm)
        .bg(theme.color_warning.opacity(0.15))
        .px_1p5()
        .py_0p5()
        .tooltip(title_tooltip(SharedString::from(why)))
        .child(Icon::new(IconName::SettingsUiEyeOff).size(px(11.)).color(theme.color_warning))
        .child(Label::new("GAMES WON'T SEE IT").size(10.).weight(600.).tracking(0.04).color(theme.color_warning))
        .into_any_element()
}

// ── The tab ──────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    Kde,
    Gnome,
    Other,
}

fn desktop() -> Desktop {
    if !cfg!(target_os = "linux") {
        return Desktop::Other;
    }
    let d = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
    let parts: Vec<&str> = d.split(':').collect();
    if parts.contains(&"KDE") {
        Desktop::Kde
    } else if parts.contains(&"GNOME") {
        Desktop::Gnome
    } else {
        Desktop::Other
    }
}

/// The command that starts this app, for window managers (`launchCommand`): the AppImage, the
/// packaged `decibell`, else the executable; a development build says `decibell`.
fn launch_command() -> String {
    if let Ok(appimage) = std::env::var("APPIMAGE") {
        return quote(appimage);
    }
    if std::path::Path::new("/usr/bin/decibell").exists() {
        return "decibell".into();
    }
    match std::env::current_exe() {
        Ok(exe) if !exe.components().any(|c| c.as_os_str() == "target") => quote(exe.display().to_string()),
        _ => "decibell".into(),
    }
}

fn quote(path: String) -> String {
    if path.contains(' ') { format!("\"{path}\"") } else { path }
}

/// `crypto.randomUUID()`'s shape, from the process's random hasher keys.
fn new_binding_id() -> String {
    let mut a = RandomState::new().build_hasher();
    a.write_u64(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64));
    let hi = a.finish();
    let mut b = RandomState::new().build_hasher();
    b.write_u64(hi);
    let lo = b.finish();
    let hex = format!("{hi:016x}{lo:016x}");
    format!("{}-{}-4{}-a{}-{}", &hex[0..8], &hex[8..12], &hex[13..16], &hex[17..20], &hex[20..32])
}

/// Why a binding won't work as recorded on this machine, if it won't (`bindingProblem`).
pub fn binding_problem(b: &HotkeyBindingSetting, all: &[HotkeyBindingSetting], status: Option<&Status>) -> Option<String> {
    if b.keys.is_empty() {
        return None;
    }
    if let Some(twin) = all.iter().find(|o| o.id != b.id && !o.keys.is_empty() && same_combo(&o.keys, &b.keys)) {
        let label = match Action::parse(&twin.action) {
            Some(a) => action_label(a).to_string(),
            None => twin.action.clone(),
        };
        return Some(format!("Same keys as “{label}”."));
    }
    let backend = status.map(|s| s.backend);
    if backend == Some("portal") && b.keys.iter().any(|k| is_mouse(k)) {
        return Some(
            "Your desktop's shortcut system only takes keys. Click the keys and press the button to see how to remap it."
                .into(),
        );
    }
    if backend == Some("portal") && !portal_can_bind(&b.keys) {
        return Some(
            "Your desktop's shortcut system needs a key, optionally with modifiers — not a modifier on its own.".into(),
        );
    }
    let action = Action::parse(&b.action);
    if backend == Some("electron") && action.is_some_and(|a| !is_hold(a)) {
        let only_mods = b.keys.iter().all(|k| modifier_of(k).is_some() || matches!(k.as_str(), "Control" | "Shift" | "Alt" | "Meta"));
        if b.keys.iter().any(|k| is_mouse(k)) || only_mods {
            return Some("macOS global shortcuts need a key, optionally with modifiers.".into());
        }
    }
    if status.is_some_and(|s| s.failed.iter().any(|f| *f == b.id)) {
        return Some("Another app already uses this shortcut.".into());
    }
    if b.keys.iter().any(|k| is_mouse(k)) && status.is_some_and(|s| !s.mouse && !s.focused_fallback) {
        return Some("Mouse buttons aren't available here.".into());
    }
    None
}

/// `backendSummary`.
pub fn backend_summary(status: Option<&Status>) -> &'static str {
    match status.map(|s| s.backend) {
        Some("portal") => "Your desktop runs these shortcuts and tells Decibell only when one of them fires. New keybinds are confirmed in a desktop dialog, and you can change their keys in your system settings too.",
        Some("x11") => "Keybinds work in every app, including games. X tells Decibell only when a bound combo is pressed. One exception: while a push to talk or push to mute key is held, Decibell asks X every 10 ms whether it's still down, and X answers with every key held at that moment — Decibell reads only yours. A mouse button or a lone Ctrl, Shift, Alt or Super key avoids that.",
        Some("xwayland") => "Your desktop has no global shortcut service, so keybinds work while Decibell or an X11 app (most games) is focused. X tells Decibell only when a bound combo is pressed — except while a push to talk or push to mute key is held, when Decibell checks every 10 ms and X answers with every key held at that moment (Decibell reads only yours). For everything else, bind the command line below in your window manager.",
        Some("windows") => "Keybinds work in every app, including games. Mute, deafen, leave and call keybinds are Windows shortcuts: Windows tells Decibell only when one is pressed, and that combo no longer reaches the app you're in. Push to talk and push to mute keys do still reach it: Decibell checks only those keys (and Ctrl, Shift, Alt and Win when a combo needs them) every 20 ms, never anything else you type. So does a keybind on a mouse button, on a key that types without Ctrl, Alt or Win, or on a combo another app already uses. A game running as administrator may hide its keys unless Decibell runs as administrator too.",
        Some("electron") => "Mute, deafen, leave and call keybinds work in every app; macOS tells Decibell only when one of them fires. Push to talk and push to mute work while Decibell is focused.",
        Some("none") => "Global keys aren't available here, so keybinds work while Decibell is focused.",
        _ => "",
    }
}

#[derive(Clone, PartialEq)]
struct Shown {
    bindings: Vec<(String, String, Vec<String>)>,
    input_mode: InputMode,
}

fn shown(s: &decibell_core::config::AppSettings) -> Shown {
    Shown {
        bindings: s.hotkeys.iter().map(|b| (b.id.clone(), b.action.clone(), b.keys.clone())).collect(),
        input_mode: InputMode::of(s),
    }
}

pub struct KeybindsTab {
    scroll: ScrollHandle,
    close: CloseLink,
    shown: Shown,
    /// The row recording (its binding id) and the recorder.
    recording: Option<String>,
    recorder: Recorder,
    record_focus: FocusHandle,
    /// A side button recorded where the desktop can't bind it: (row, button).
    mouse_guide: Option<(String, String)>,
    open_failed: bool,
    configure_error: Option<SharedString>,
    command_open: bool,
    desktop: Desktop,
    command: String,
    _subscriptions: Vec<Subscription>,
    _watch: Watch,
}

impl KeybindsTab {
    pub fn new(close: CloseLink, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .store_if(&model.settings, cx, |this: &mut KeybindsTab, s, _| {
                let next = shown(s.get());
                std::mem::replace(&mut this.shown, next.clone()) != next
            })
            .store(&model.live.hotkeys, cx);
        let record_focus = cx.focus_handle();
        let subscriptions = vec![
            // Losing the window, or the focus to anything else, ends a recording (`onBlur`).
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.finish(Some(Recorded::Cancelled), cx);
                }
            }),
            cx.on_focus_out(&record_focus, window, |this, _, _, cx| this.finish(Some(Recorded::Cancelled), cx)),
            cx.on_release(|this: &mut KeybindsTab, cx| {
                if this.recording.is_some() {
                    actions::hotkeys::set_recording(false, cx);
                }
            }),
        ];
        let staged = SettingsHooks::get(cx).staged.record_first;
        let first = model.settings.read(cx).get().hotkeys.first().map(|b| b.id.clone());
        Self {
            scroll: ScrollHandle::new(),
            close,
            shown: shown(model.settings.read(cx).get()),
            recording: if staged { first } else { None },
            recorder: Recorder::new(),
            record_focus,
            mouse_guide: None,
            open_failed: false,
            configure_error: None,
            command_open: false,
            desktop: desktop(),
            command: if decibell_model::core::is_running() { launch_command() } else { "decibell".into() },
            _subscriptions: subscriptions,
            _watch: watch,
        }
    }

    pub fn recording(&self) -> Option<&str> {
        self.recording.as_deref()
    }

    fn change(&self, cx: &mut App, f: impl FnOnce(&mut Vec<HotkeyBindingSetting>)) {
        let settings = Model::global(cx).settings.clone();
        settings.update(cx, |s, cx| s.change(|v| f(&mut v.hotkeys), cx));
    }

    /// One recorder at a time; the core swallows hotkeys while it runs.
    pub fn start_recording(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.mouse_guide = None;
        self.open_failed = false;
        let was = self.recording.take();
        self.recorder = Recorder::new();
        match id {
            Some(id) => {
                self.recording = Some(id);
                if was.is_none() {
                    actions::hotkeys::set_recording(true, cx);
                }
                self.record_focus.focus(window, cx);
            }
            None => {
                if was.is_some() {
                    actions::hotkeys::set_recording(false, cx);
                }
            }
        }
        cx.notify();
    }

    /// The recorder's answer (`recordCombo`'s `onDone`).
    pub fn finish(&mut self, done: Option<Recorded>, cx: &mut Context<Self>) {
        let (Some(done), Some(id)) = (done, self.recording.clone()) else { return };
        self.recording = None;
        actions::hotkeys::set_recording(false, cx);
        let model = Model::global(cx).clone();
        let status = model.live.hotkeys.read(cx).status.clone();
        let bindings = model.settings.read(cx).get().hotkeys.clone();
        let row = bindings.iter().find(|b| b.id == id).cloned();
        match (&done, row) {
            (Recorded::Combo(keys), Some(_)) if keys.iter().any(|k| is_mouse(k)) && status.as_ref().is_some_and(|s| s.backend == "portal") => {
                // Keep whatever the row had; the guide explains the remap.
                let button = keys.iter().find(|k| is_mouse(k)).cloned().unwrap_or_default();
                self.mouse_guide = Some((id, button));
            }
            (Recorded::Combo(keys), Some(_)) => {
                // A fresh id: the desktop keeps its own assignment per id, so a new combo only
                // takes effect under a new one.
                let keys = keys.clone();
                let new_id = new_binding_id();
                self.change(cx, move |list| {
                    if let Some(b) = list.iter_mut().find(|b| b.id == id) {
                        b.keys = keys;
                        b.id = new_id;
                    }
                });
            }
            (Recorded::Cancelled, Some(b)) if b.keys.is_empty() => {
                self.change(cx, move |list| list.retain(|x| x.id != id));
            }
            _ => {}
        }
        cx.notify();
    }

    /// `addBinding`: the first action not bound yet (push to talk first while that is the mode).
    pub fn add_binding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bound: Vec<&str> = self.shown.bindings.iter().map(|(_, a, _)| a.as_str()).collect();
        let wire = |a: Action| serde_json::to_value(a).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        let action = if self.shown.input_mode == InputMode::PushToTalk && !bound.contains(&"push_to_talk") {
            "push_to_talk".to_string()
        } else {
            HOTKEY_ACTIONS
                .iter()
                .map(|(a, _, _)| wire(*a))
                .find(|a| !bound.contains(&a.as_str()))
                .unwrap_or_else(|| "toggle_mute".into())
        };
        let id = new_binding_id();
        let row = HotkeyBindingSetting { id: id.clone(), action, keys: Vec::new() };
        self.change(cx, move |list| list.push(row));
        self.start_recording(Some(id), window, cx);
    }

    fn set_action(&self, id: String, action: Action, cx: &mut App) {
        let wire = serde_json::to_value(action).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        self.change(cx, move |list| {
            if let Some(b) = list.iter_mut().find(|b| b.id == id) {
                b.action = wire;
            }
        });
    }

    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.mouse_guide.as_ref().is_some_and(|(g, _)| g == id) {
            self.mouse_guide = None;
        }
        if self.recording.as_deref() == Some(id) {
            self.recording = None;
            actions::hotkeys::set_recording(false, cx);
        }
        let id = id.to_string();
        self.change(cx, move |list| list.retain(|b| b.id != id));
        cx.notify();
    }

    fn dismiss_guide(&mut self, cx: &mut Context<Self>) {
        let Some((id, _)) = self.mouse_guide.take() else { return };
        // A new row that never got a key isn't worth keeping.
        let empty = Model::global(cx).settings.read(cx).get().hotkeys.iter().any(|b| b.id == id && b.keys.is_empty());
        if empty {
            self.change(cx, move |list| list.retain(|b| b.id != id));
        }
        cx.notify();
    }

    fn open_mouse_settings(&mut self, cx: &mut Context<Self>) {
        let ok = self.desktop == Desktop::Kde
            && std::process::Command::new("systemsettings")
                .arg("kcm_mouse")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok();
        self.open_failed = !ok;
        cx.notify();
    }
}

fn note(text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    div().mt_2().child(Label::new(text).size(12.).line_height(12. * 1.55).color(color)).into_any_element()
}

impl KeybindsTab {
    fn mouse_guide(&self, button: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let name = key_label(button);
        let (primary, secondary, muted) = (theme.color_text_primary, theme.color_text_secondary, theme.color_text_muted);
        // A line of runs: plain, and `font-medium text-text-primary` for the names of things.
        let line = |runs: Vec<(String, bool)>| {
            div().flex().flex_wrap().children(runs.into_iter().map(move |(text, strong)| {
                Label::new(text)
                    .size(12.)
                    .line_height(12. * 1.6)
                    .weight(if strong { 500. } else { 400. })
                    .color(if strong { primary } else { secondary })
            }))
        };
        let step = |n: usize, runs: Vec<(String, bool)>| {
            div().flex().gap_1().child(Label::new(format!("{n}.")).size(12.).line_height(12. * 1.6).color(secondary)).child(line(runs))
        };
        let body: AnyElement = match self.desktop {
            Desktop::Kde => div()
                .mt_2()
                .pl_4()
                .flex()
                .flex_col()
                .gap_1()
                .child(step(1, vec![("Open your mouse settings and go to\u{a0}".into(), false), ("Extra Mouse Buttons".into(), true), (".".into(), false)]))
                .child(step(
                    2,
                    vec![
                        ("Click\u{a0}".into(), false),
                        ("Add Binding".into(), true),
                        (format!(", press {name}, then type a combination you don't use anywhere else, like Ctrl + Meta + F9."), false),
                    ],
                ))
                .child(step(3, vec![(format!("Come back and press {name} here — Decibell records that combination."), false)]))
                .into_any_element(),
            Desktop::Gnome => div()
                .mt_2()
                .child(Label::new(format!(
                    "GNOME can't remap mouse buttons on its own. A tool like Piper (for gaming mice) or input-remapper can turn {name} into a key combination — then press it here again."
                ))
                .size(12.)
                .line_height(12. * 1.6)
                .color(secondary))
                .into_any_element(),
            Desktop::Other => div()
                .mt_2()
                .child(Label::new(format!(
                    "If your desktop's input settings can remap {name} to a key combination, do that and press it here again. Sway and Hyprland can also bind the button to the command line below."
                ))
                .size(12.)
                .line_height(12. * 1.6)
                .color(secondary))
                .into_any_element(),
        };
        let theme = cx.theme();
        let row_id = self.mouse_guide.as_ref().map(|(id, _)| id.clone()).unwrap_or_default();
        let dismiss_look = Look {
            bg: Hsla::transparent_black(),
            bg_hover: Hsla::transparent_black(),
            text: muted,
            text_hover: primary,
            border: None,
        };
        div()
            .mt_3()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_lighter)
            .px(px(14.))
            .py_3()
            .child(Label::new(format!("Turn {name} into a key first")).size(13.).weight(500.).color(primary))
            .child(
                div().mt_0p5().child(
                    Label::new("Your desktop's shortcut system only takes keys, and only the desktop can see your mouse.")
                        .size(12.)
                        .line_height(12. * 1.6)
                        .color(muted),
                ),
            )
            .child(body)
            .when(self.open_failed, |this| {
                this.child(note("Couldn't open System Settings — open it from your app menu.", theme.color_danger))
            })
            .child(
                div()
                    .mt_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(self.desktop == Desktop::Kde, |this| {
                        this.child(
                            Button::new("keybinds-guide-open", "Open mouse settings")
                                .size(ButtonSize::Sm)
                                .on_click(cx.listener(|this, _, _, cx| this.open_mouse_settings(cx))),
                        )
                    })
                    .child(
                        Btn::new("keybinds-guide-again", "Record again", outline_look(cx), theme.radius_sm)
                            .text(12., 500.)
                            .pad(12., 6.)
                            .on_click(cx.listener(move |this, _, window, cx| this.start_recording(Some(row_id.clone()), window, cx))),
                    )
                    .child(
                        Btn::new("keybinds-guide-dismiss", "Dismiss", dismiss_look, theme.radius_sm)
                            .text(12., 500.)
                            .pad(8., 6.)
                            .on_click(cx.listener(|this, _, _, cx| this.dismiss_guide(cx))),
                    ),
            )
            .into_any_element()
    }

    fn row(
        &self,
        index: usize,
        b: &HotkeyBindingSetting,
        all: &[HotkeyBindingSetting],
        status: Option<&Status>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(action) = Action::parse(&b.action) else { return div().into_any_element() };
        let recording = self.recording.as_deref() == Some(b.id.as_str());
        let problem = binding_problem(b, all, status);
        let assigned = assigned_trigger(&b.id, &b.keys, status);
        let portal_note = if status.map(|s| s.backend) != Some("portal") || b.keys.is_empty() || problem.is_some() {
            None
        } else if let Some(assigned) = &assigned {
            Some(format!("Your desktop assigned {assigned}."))
        } else if status.is_some_and(|s| !s.triggers.contains_key(&b.id) && s.state == "active") {
            Some("No key assigned in your desktop's shortcut settings.".to_string())
        } else {
            None
        };
        let swallowed = problem.is_none() && is_swallowed(&b.id, &b.keys, action, status);
        let guide = self.mouse_guide.as_ref().filter(|(id, _)| *id == b.id).map(|(_, button)| button.clone());
        let record_id: gpui::ElementId = ("keybind-record", index).into();
        let (h, listener) = hover(window, cx, &record_id);
        let theme = cx.theme();

        let options: Vec<SelectOption<Action>> = HOTKEY_ACTIONS
            .iter()
            .map(|(a, label, hold)| {
                let o = SelectOption::new(*a, *label);
                if *hold { o.hint("Hold the keys") } else { o }
            })
            .collect();
        let id_for_action = b.id.clone();
        let select = div().w(px(210.)).flex_none().child(
            Select::options(("keybind-action", index), Some(action), options).on_change(cx.listener(
                move |this, a: &Action, _, cx| this.set_action(id_for_action.clone(), *a, cx),
            )),
        );
        let content: AnyElement = if recording {
            Label::new("Press keys…").size(13.).color(theme.color_accent_bright).into_any_element()
        } else if !b.keys.is_empty() {
            div()
                .flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_3()
                .child(key_chips(&b.keys, cx))
                .when(swallowed, |this| this.child(swallow_badge(("keybind-swallow", index), types_text(&b.keys), cx)))
                .into_any_element()
        } else {
            Label::new("Click to record").size(13.).color(theme.color_text_muted).into_any_element()
        };
        let theme = cx.theme();
        let border = if recording {
            theme.color_accent
        } else {
            decibell_ui::motion::mix(theme.color_border, theme.color_accent.opacity(0.4), h)
        };
        let id_for_record = b.id.clone();
        let record = div()
            .id(record_id)
            .when(recording, |this| this.track_focus(&self.record_focus))
            .flex()
            .min_h_10()
            .flex_1()
            .min_w_0()
            .items_center()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(border)
            .bg(if recording { theme.color_accent_soft } else { theme.color_bg_lighter })
            .px_3()
            .py_1p5()
            .cursor_pointer()
            .when(recording, |this| this.shadow(theme.shadow_ring.clone()))
            .when(!recording, |this| this.tooltip(title_tooltip("Click to change")))
            .on_hover(listener)
            .on_click(cx.listener(move |this, _, window, cx| {
                let next = if this.recording.as_deref() == Some(id_for_record.as_str()) { None } else { Some(id_for_record.clone()) };
                this.start_recording(next, window, cx);
            }))
            .children(decibell_shell::probes::probe(format!("keybind-record:{index}")))
            .child(content);
        let remove_id: gpui::ElementId = ("keybind-remove", index).into();
        let (rh, remove_listener) = hover(window, cx, &remove_id);
        let theme = cx.theme();
        let id_for_remove = b.id.clone();
        let remove = div()
            .id(remove_id)
            .flex()
            .size(px(38.))
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(theme.radius_md)
            .bg(theme.color_surface_hover.opacity(rh))
            .cursor_pointer()
            .tooltip(title_tooltip("Remove keybind"))
            .on_hover(remove_listener)
            .on_click(cx.listener(move |this, _, _, cx| this.remove(&id_for_remove, cx)))
            .children(decibell_shell::probes::probe(format!("keybind-remove:{index}")))
            .child(
                Icon::new(IconName::SettingsX)
                    .size(px(15.))
                    .color(decibell_ui::motion::mix(theme.color_text_muted, theme.color_danger, rh)),
            );

        let backend = status.map(|s| s.backend);
        let mut row = div()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_light)
            .px_4()
            .py_3()
            .child(div().flex().items_center().gap_3().child(select).child(record).child(remove));
        if recording {
            let hint = match backend {
                Some("portal") => "Press a key or a combo. Mouse side buttons need a quick remap in your desktop first — press one to see how. Esc cancels.",
                Some("electron") => "Press a key or a combo. Esc cancels.",
                _ => "Press a key or a combo, or a middle / back / forward mouse button. Esc cancels.",
            };
            row = row.child(note(hint, theme.color_text_muted));
        } else if let Some(button) = guide {
            row = row.child(self.mouse_guide(&button, cx));
        } else {
            let theme = cx.theme();
            if let Some(problem) = problem {
                row = row.child(note(problem, theme.color_danger));
            }
            if let Some(portal_note) = portal_note {
                row = row.child(note(portal_note, theme.color_text_muted));
            }
            if swallowed && types_text(&b.keys) {
                row = row.child(note("While bound, this key won't type in other apps.", theme.color_text_muted));
            }
        }
        row.into_any_element()
    }
}

impl Render for KeybindsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let status = model.live.hotkeys.read(cx).status.clone();
        let bindings: Vec<HotkeyBindingSetting> = model.settings.read(cx).get().hotkeys.clone();
        let input_mode = self.shown.input_mode;
        let theme = cx.theme();

        // On this computer.
        let any_swallowed = bindings
            .iter()
            .any(|b| Action::parse(&b.action).is_some_and(|a| is_swallowed(&b.id, &b.keys, a, status.as_ref())));
        let mut computer = div()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_light)
            .px_4()
            .py_3p5()
            .child(Label::new(backend_summary(status.as_ref())).size(13.).line_height(13. * 1.6).color(theme.color_text_secondary));
        if any_swallowed {
            computer = computer.child(
                div()
                    .mt_2p5()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x_2()
                    .gap_y_1()
                    .child(swallow_badge("keybinds-swallow-note", false, cx))
                    .child(
                        Label::new(
                            "Here, a bound key press goes to Decibell only — the game or app you're in won't register it. Pick keys your games don't use.",
                        )
                        .size(12.)
                        .line_height(12. * 1.55)
                        .color(cx.theme().color_text_muted),
                    ),
            );
        }
        let theme = cx.theme();
        if let Some(s) = &status {
            if s.state == "starting" && s.backend == "portal" {
                computer = computer.child(div().mt_2().child(Label::new("Waiting for your desktop to confirm…").size(12.).color(theme.color_text_muted)));
            }
            if s.state == "error"
                && let Some(detail) = &s.detail
            {
                computer = computer.child(note(detail.clone(), theme.color_danger));
            }
            if s.backend == "portal" && s.can_configure && s.state == "active" {
                computer = computer.child(
                    div().mt_3().flex().child(
                        Btn::new("keybinds-system-settings", "Open system shortcut settings", outline_look(cx), theme.radius_sm)
                            .text(12., 500.)
                            .pad(12., 6.)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.configure_error = None;
                                let weak = cx.weak_entity();
                                actions::hotkeys::open_system_settings(cx, move |error, cx| {
                                    if let Some(this) = weak.upgrade() {
                                        this.update(cx, |this, cx| {
                                            this.configure_error = error.map(SharedString::from);
                                            cx.notify();
                                        });
                                    }
                                });
                                cx.notify();
                            })),
                    ),
                );
            }
        }
        let theme = cx.theme();
        if let Some(error) = &self.configure_error {
            computer = computer.child(div().mt_2().child(Label::new(error.clone()).size(12.).color(theme.color_danger)));
        }

        // Your keybinds.
        let mut list = div().flex().flex_col().gap_2p5().child(section_label("Your keybinds", true, cx));
        if bindings.is_empty() {
            let theme = cx.theme();
            list = list.child(
                div()
                    .rounded(theme.radius_md)
                    .border_1()
                    .border_dashed()
                    .border_color(theme.color_border_divider)
                    .px_4()
                    .py_6()
                    .flex()
                    .justify_center()
                    .child(Label::new("No keybinds yet.").size(13.).color(theme.color_text_muted)),
            );
        }
        for (index, b) in bindings.iter().enumerate() {
            list = list.child(self.row(index, b, &bindings, status.as_ref(), window, cx));
        }
        let has_ptt = bindings.iter().any(|b| b.action == "push_to_talk" && !b.keys.is_empty());
        let theme = cx.theme();
        let mut add = div().flex().items_center().gap_3().child(
            div()
                .relative()
                .children(decibell_shell::probes::probe("keybind-add"))
                .child(Button::new("keybinds-add", "Add keybind").on_click(cx.listener(|this, _, window, cx| this.add_binding(window, cx)))),
        );
        if has_ptt && input_mode != InputMode::PushToTalk {
            add = add.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .child(Label::new("Push to talk keys work once Input Mode is Push to talk.\u{a0}").size(12.).color(theme.color_text_muted))
                    .child(
                        TextLink::new("keybinds-switch-ptt", "Switch now", theme.color_accent_bright, theme.color_accent_bright)
                            .text(12., 500.)
                            .on_click(|_, _, cx| actions::hotkeys::set_input_mode(true, None, cx)),
                    ),
            );
        }
        if !has_ptt && input_mode == InputMode::PushToTalk {
            add = add.child(Label::new("Input Mode is Push to talk, but no push to talk key is bound.").size(12.).color(theme.color_danger));
        }
        list = list.child(add);

        let mut body = div()
            .flex()
            .flex_col()
            .gap_6()
            .child(div().child(section_label("On this computer", true, cx)).child(computer))
            .child(list);

        if cfg!(target_os = "linux") {
            let theme = cx.theme();
            let open = self.command_open;
            let mut details = div()
                .rounded(theme.radius_md)
                .border_1()
                .border_color(theme.color_border_divider)
                .bg(theme.color_bg_light)
                .px_4()
                .py_3()
                .child(
                    div()
                        .id("keybinds-command-line")
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .cursor_pointer()
                        .when(open, |this| this.mb_2p5())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.command_open = !this.command_open;
                            cx.notify();
                        }))
                        .child(
                            Icon::new(if open { IconName::SettingsUiDisclosureOpen } else { IconName::SettingsUiDisclosure })
                                .size(px(8.))
                                .color(theme.color_text_secondary),
                        )
                        .child(Label::new("Command line").size(13.).weight(500.).color(theme.color_text_secondary)),
                );
            if open {
                let muted = theme.color_text_muted;
                let code = |text: &'static str| Label::new(text).font(FontRole::Mono).size(12.).line_height(12. * 1.6).color(muted);
                let plain = |text: &'static str| Label::new(text).size(12.).line_height(12. * 1.6).color(muted);
                details = details
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .child(plain("Runs an action in the open Decibell, for window managers that bind commands (Sway, Hyprland, i3…). Hold actions take\u{a0}"))
                            .child(code(":down"))
                            .child(plain("\u{a0}on press and\u{a0}"))
                            .child(code(":up"))
                            .child(plain("\u{a0}on release.")),
                    )
                    .child(div().mt_2p5().flex().flex_col().gap_1().children(HOTKEY_ACTIONS.iter().map(|(a, _, hold)| {
                        let wire = serde_json::to_value(a).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                        div().rounded(theme.radius_sm).bg(theme.color_bg_lighter).px_2().py_1().child(
                            Label::new(format!("{} --hotkey={wire}{}", self.command, if *hold { ":down" } else { "" }))
                                .font(FontRole::Meta)
                                .size(12.)
                                .color(theme.color_text_primary),
                        )
                    })));
            }
            body = body.child(details);
        }

        let recording = self.recording.is_some();
        let root = div()
            .key_context("KeybindsRecorder")
            .when(recording, |this| {
                this.capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if this.recording.is_none() {
                        return;
                    }
                    cx.stop_propagation();
                    window.prevent_default();
                    let done = this.recorder.gpui_key_down(&event.keystroke.key, event.is_held);
                    this.finish(done, cx);
                }))
                .capture_key_up(cx.listener(|this, event: &gpui::KeyUpEvent, window, cx| {
                    if this.recording.is_none() {
                        return;
                    }
                    cx.stop_propagation();
                    window.prevent_default();
                    let done = this.recorder.gpui_key_up(&event.keystroke.key);
                    this.finish(done, cx);
                }))
                .on_modifiers_changed(cx.listener(|this, event: &gpui::ModifiersChangedEvent, _, cx| {
                    if this.recording.is_none() {
                        return;
                    }
                    let done = this.recorder.modifiers_changed(event.modifiers);
                    this.finish(done, cx);
                }))
                .capture_any_mouse_down(cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    if this.recording.is_none() {
                        return;
                    }
                    if let Some(done) = this.recorder.mouse_down(event.button) {
                        cx.stop_propagation();
                        window.prevent_default();
                        this.finish(Some(done), cx);
                    }
                }))
            })
            .child(body);
        super::pane(TabId::Keybinds, &self.scroll, &self.close).child(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(id: &str, action: &str, keys: &[&str]) -> HotkeyBindingSetting {
        HotkeyBindingSetting { id: id.into(), action: action.into(), keys: keys.iter().map(|k| k.to_string()).collect() }
    }

    fn status(backend: &'static str) -> Status {
        let mut s = decibell_core::hotkeys::status();
        s.backend = backend;
        s.state = "active";
        s
    }

    #[test]
    fn problems_in_the_renderers_order() {
        let all = vec![b("a", "toggle_mute", &["Control", "KeyM"]), b("b", "toggle_deafen", &["KeyM", "Control"]), b("c", "push_to_talk", &["Mouse4"])];
        assert_eq!(binding_problem(&all[1], &all, None).as_deref(), Some("Same keys as “Toggle mute”."));
        let portal = status("portal");
        assert_eq!(
            binding_problem(&all[2], &all, Some(&portal)).as_deref(),
            Some("Your desktop's shortcut system only takes keys. Click the keys and press the button to see how to remap it.")
        );
        let lone = vec![b("d", "toggle_mute", &["ControlLeft"])];
        assert_eq!(
            binding_problem(&lone[0], &lone, Some(&portal)).as_deref(),
            Some("Your desktop's shortcut system needs a key, optionally with modifiers — not a modifier on its own.")
        );
        let mut x11 = status("x11");
        x11.failed = vec!["d".into()];
        assert_eq!(binding_problem(&lone[0], &lone, Some(&x11)).as_deref(), Some("Another app already uses this shortcut."));
        let mut none = status("none");
        none.mouse = false;
        none.focused_fallback = false;
        let mouse = vec![b("m", "push_to_talk", &["Mouse5"])];
        assert_eq!(binding_problem(&mouse[0], &mouse, Some(&none)).as_deref(), Some("Mouse buttons aren't available here."));
    }

    #[test]
    fn swallowing_and_typing() {
        let keys = |k: &[&str]| k.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_swallowed("a", &keys(&["KeyV"]), Action::PushToTalk, Some(&status("x11"))));
        assert!(!is_swallowed("a", &keys(&["KeyV"]), Action::PushToTalk, Some(&status("electron"))));
        let mut win = status("windows");
        win.registered = vec!["a".into()];
        assert!(is_swallowed("a", &keys(&["F9"]), Action::ToggleMute, Some(&win)));
        assert!(!is_swallowed("b", &keys(&["F9"]), Action::ToggleMute, Some(&win)));
        assert!(types_text(&keys(&["Shift", "KeyV"])));
        assert!(!types_text(&keys(&["Control", "KeyV"])));
        assert!(!types_text(&keys(&["F9"])));
    }

    #[test]
    fn the_portal_names_what_it_assigned() {
        let mut portal = status("portal");
        portal.triggers.insert("a".into(), "Ctrl+Alt+M".into());
        portal.triggers.insert("b".into(), "Ctrl + M".into());
        let keys = vec!["Control".to_string(), "KeyM".to_string()];
        assert_eq!(assigned_trigger("a", &keys, Some(&portal)).as_deref(), Some("Ctrl+Alt+M"));
        assert_eq!(assigned_trigger("b", &keys, Some(&portal)), None, "the same combo, spaced differently");
    }

    #[test]
    fn binding_ids_look_like_uuids_and_differ() {
        let a = new_binding_id();
        let b = new_binding_id();
        assert_eq!(a.len(), 36);
        assert_ne!(a, b);
    }
}
