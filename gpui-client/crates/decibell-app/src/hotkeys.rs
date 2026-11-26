//! The focused hotkey fallback (`features/hotkeys/hotkeyRuntime.ts`, "Focused fallback"): where
//! the platform's listener can't see keys typed into Decibell itself, the core says so
//! (`Status::focused_fallback`: XWayland, whose X11 grabs see X11 windows only; no global keys at
//! all; on macOS the hold actions) and the root view feeds the window's keys and mouse buttons to
//! the model's matcher ([`Hotkeys::key_event`], by physical key), which injects what fires. Where
//! the listener sees our window's keys itself (the portal, X11, Windows) the fallback is off and
//! nothing is fed, so a bound combo fires once. The window going to the background releases what
//! is held (keys released meanwhile never reach it). As the renderer's window listeners, these
//! run in the capture phase, before any view.
//!
//! One difference from the renderer, which fed every key: while a text field has the focus, a
//! key that types a character (no Ctrl, Alt or Super held) is the field's, not a keybind's, so
//! typing "v" in a message doesn't press a bare-V push-to-talk (`docs/electron-quirks.md` #13).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use decibell_core::config::{AppSettings, HotkeyBindingSetting};
use decibell_core::hotkeys::Action;
use decibell_harness::{SelfTest, TestCx};
use decibell_model::Model;
use decibell_model::live::hotkeys::{HotkeyBackend, Hotkeys, mouse_key};
use decibell_text::input::{InputState, text_field_focused};
use decibell_ui::{ActiveTheme as _, TextField};
use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement, IntoElement, KeyCode, Keystroke, Modifiers, Render,
    Subscription, Window, div, prelude::*, px,
};

use crate::root::Root;

/// The model's hotkeys store while the fallback is armed.
fn armed(cx: &App) -> Option<Entity<Hotkeys>> {
    let hotkeys = cx.try_global::<Model>()?.live.hotkeys.clone();
    hotkeys.read(cx).fallback_armed().then_some(hotkeys)
}

/// A key down that belongs to the focused text field: it types a character and no command
/// modifier is held.
fn typed_into_field(keystroke: &Keystroke, window: &Window) -> bool {
    let m = &keystroke.modifiers;
    keystroke.key_char.is_some() && !(m.control || m.alt || m.platform || m.function) && text_field_focused(window)
}

/// The window's keys and mouse buttons to the fallback (on the root view's element).
pub fn focused_fallback<E: InteractiveElement>(element: E) -> E {
    element
        .capture_key_down(|event, window, cx| {
            let Some(hotkeys) = armed(cx) else { return };
            if typed_into_field(&event.keystroke, window) {
                return;
            }
            hotkeys.update(cx, |h, _| h.key_event(event.code, Some(&event.keystroke), true));
        })
        .capture_key_up(|event, _, cx| {
            // Always: a key the matcher never saw go down is ignored.
            if let Some(hotkeys) = armed(cx) {
                hotkeys.update(cx, |h, _| h.key_event(event.code, Some(&event.keystroke), false));
            }
        })
        .on_modifiers_changed(|event, _, cx| {
            if let (Some(key), Some(hotkeys)) = (event.key, armed(cx)) {
                hotkeys.update(cx, |h, _| h.key_event(key.code, None, key.pressed));
            }
        })
        .capture_any_mouse_down(|event, _, cx| {
            if let (Some(key), Some(hotkeys)) = (mouse_key(event.button), armed(cx)) {
                hotkeys.update(cx, |h, _| h.key(key, true));
            }
        })
        .capture_any_mouse_up(|event, _, cx| {
            if let (Some(key), Some(hotkeys)) = (mouse_key(event.button), armed(cx)) {
                hotkeys.update(cx, |h, _| h.key(key, false));
            }
        })
}

/// The window going to the background releases what the fallback holds.
pub fn release_on_blur(window: &mut Window, cx: &mut Context<Root>) -> Subscription {
    cx.observe_window_activation(window, |_, window, cx| {
        if !window.is_window_active()
            && let Some(hotkeys) = armed(cx)
        {
            hotkeys.update(cx, |h, _| h.window_blurred());
        }
    })
}

// ── The self-test ────────────────────────────────────────────────────────────────────────────

pub fn selftest() -> SelfTest {
    SelfTest {
        name: "hotkeys-focused",
        about: "the focused fallback: a bound combo pressed in our window fires once while the listener asks for it, never when the listener sees the window's keys itself; typing into a text field presses no bare-key bind",
        run: |cx| Box::pin(focused(cx)),
    }
}

/// A window like the main one with a text field in it.
struct Fixture {
    input: Entity<InputState>,
}

impl Render for Fixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_6()
            .bg(cx.theme().color_bg_primary)
            .child(div().w(px(320.)).child(TextField::new("fallback-field").editor(&self.input)))
    }
}

type Injected = Rc<RefCell<Vec<(Action, bool, String)>>>;

fn recording_backend(injected: &Injected) -> HotkeyBackend {
    let injected = injected.clone();
    HotkeyBackend {
        configure: Rc::new(|_| Box::pin(async { Ok(()) })),
        set_input_mode: Rc::new(|_, _| {}),
        set_paused: Rc::new(|_| {}),
        inject: Rc::new(move |action, pressed, source| injected.borrow_mut().push((action, pressed, source.to_string()))),
    }
}

fn binding(id: &str, action: &str, keys: &[&str]) -> HotkeyBindingSetting {
    HotkeyBindingSetting { id: id.into(), action: action.into(), keys: keys.iter().map(|k| k.to_string()).collect() }
}

fn status(backend: &'static str, fallback: bool) -> decibell_core::hotkeys::Status {
    let mut status = decibell_core::hotkeys::status();
    status.backend = backend;
    status.state = "active";
    status.focused_fallback = fallback;
    status
}

/// A key pressed and released as GPUI's Linux layer reports it (what the layout types, which
/// key it is), with `modifiers` held.
fn press(cx: &TestCx, key: &str, typed: Option<&str>, evdev: u16, modifiers: Modifiers) -> Result<()> {
    let keystroke = Keystroke { modifiers, key: key.into(), key_char: typed.map(str::to_string) };
    cx.key_event(keystroke.clone(), KeyCode::Evdev(evdev), true)?;
    cx.key_event(keystroke, KeyCode::Evdev(evdev), false)
}

/// Left Ctrl + the key right of L (it types "ş" on a Turkish keyboard): `Control` + `Semicolon`.
fn ctrl_semicolon(cx: &TestCx) -> Result<()> {
    cx.modifier_key(KeyCode::Evdev(29), true, Modifiers::control())?;
    press(cx, "ş", None, 39, Modifiers::control())?;
    cx.modifier_key(KeyCode::Evdev(29), false, Modifiers::default())
}

async fn focused(cx: TestCx) -> Result<()> {
    let model = cx.update(|_, cx| cx.try_global::<Model>().cloned())?.context("no model in this process")?;
    let injected: Injected = Rc::default();
    let bindings = AppSettings {
        hotkeys: vec![
            binding("mute", "toggle_mute", &["Control", "Semicolon"]),
            binding("ptt", "push_to_talk", &["KeyV"]),
            binding("deafen", "toggle_deafen", &["F9"]),
        ],
        ..AppSettings::default()
    };
    // The store keeps the core in step: a recording backend stands in for the core here.
    let (previous_backend, previous_status) = cx.update(|_, cx| {
        model.live.hotkeys.update(cx, |h, cx| {
            let backend = h.swap_backend(recording_backend(&injected));
            let status = h.status.clone();
            h.sync(&bindings, cx);
            (backend, status)
        })
    })?;
    let set_status = |status: decibell_core::hotkeys::Status| {
        let (model, bindings) = (model.clone(), bindings.clone());
        move |_: &mut Window, cx: &mut App| model.live.hotkeys.update(cx, |h, cx| h.set_status(status, &bindings, cx))
    };
    let take = || std::mem::take(&mut *injected.borrow_mut());

    let field: Rc<RefCell<Option<Entity<InputState>>>> = Rc::default();
    let handle = cx.app().update(|cx| {
        let field = field.clone();
        crate::window::open_main(
            move |window, cx| {
                let input = cx.new(|cx| InputState::single_line(window, cx).placeholder("Message"));
                *field.borrow_mut() = Some(input.clone());
                cx.new(|_| Fixture { input }).into()
            },
            None,
            cx,
        )
    })?;
    let window = TestCx::new(cx.app().clone(), handle.into());
    let result = async {
        let field = field.borrow().clone().context("no field")?;
        window.settle().await?;

        // XWayland: the X server's grabs don't see keys typed into a Wayland window: armed.
        window.update(set_status(status("xwayland", true)))?;
        ensure!(window.update(|_, cx| model.live.hotkeys.read(cx).fallback_armed())?, "the fallback didn't arm");
        take();
        ctrl_semicolon(&window)?;
        let fired = take();
        ensure!(fired == [(Action::ToggleMute, true, "mute".to_string())], "Ctrl + ş (Control + Semicolon) injected {fired:?}");
        press(&window, "v", Some("v"), 47, Modifiers::default())?;
        let fired = take();
        ensure!(
            fired == [(Action::PushToTalk, true, "ptt".to_string()), (Action::PushToTalk, false, "ptt".to_string())],
            "bare V injected {fired:?}"
        );
        window.log("armed (xwayland): Ctrl + the ş key fired toggle mute once; V held and released push to talk");

        // A text field with the focus: typing is the field's; command combos and keys that type
        // nothing still fire.
        window.update(|window, cx| field.update(cx, |input, cx| input.focus(window, cx)))?;
        window.settle().await?;
        window.type_text("v")?;
        let fired = take();
        let typed = window.update(|_, cx| field.read(cx).value().to_string())?;
        ensure!(fired.is_empty() && typed == "v", "typing v into the field: injected {fired:?}, the field holds {typed:?}");
        ctrl_semicolon(&window)?;
        press(&window, "f9", None, 67, Modifiers::default())?;
        let fired = take();
        ensure!(
            fired == [(Action::ToggleMute, true, "mute".to_string()), (Action::ToggleDeafen, true, "deafen".to_string())],
            "in the field, Ctrl + ş and F9 injected {fired:?}"
        );
        window.log("in a text field: v typed into it and pressed nothing; Ctrl + ş and F9 still fired, once each");

        // The listener sees our window's keys itself (X11's grabs, the portal, Windows): the
        // fallback is off, the listener's press is the only one.
        window.update(set_status(status("x11", false)))?;
        window.update(|window, cx| window.blur(cx))?;
        ctrl_semicolon(&window)?;
        press(&window, "v", Some("v"), 47, Modifiers::default())?;
        press(&window, "f9", None, 67, Modifiers::default())?;
        let fired = take();
        ensure!(fired.is_empty(), "the fallback fed keys while the listener sees them: {fired:?}");
        window.log("listener sees the window (x11): nothing fed from the window, so a bound combo fires once (the listener's)");

        // Held keys go when the window loses the focus (if the desktop lets another window
        // take it).
        window.update(set_status(status("xwayland", true)))?;
        take();
        window.key_event(Keystroke { modifiers: Modifiers::default(), key: "v".into(), key_char: Some("v".into()) }, KeyCode::Evdev(47), true)?;
        ensure!(take() == [(Action::PushToTalk, true, "ptt".to_string())], "V held");
        cx.update(|window, _| window.activate_window())?;
        let mut released = false;
        for _ in 0..20 {
            window.sleep(Duration::from_millis(50)).await;
            if !take().is_empty() {
                released = true;
                break;
            }
        }
        if released {
            window.log("another window took the focus: the held push to talk was released");
        } else {
            window.log("the desktop kept the focus on the test window: the release on blur wasn't exercised here (unit-tested in the model)");
            window.key_event(Keystroke { modifiers: Modifiers::default(), key: "v".into(), key_char: Some("v".into()) }, KeyCode::Evdev(47), false)?;
        }
        Ok(())
    }
    .await;
    let _ = window.update(|window, _| window.remove_window());
    // Put the store back as it was.
    let restore = cx.update(|_, cx| {
        let settings = model.settings.read(cx).get().clone();
        model.live.hotkeys.update(cx, |h, cx| {
            h.status = previous_status;
            h.sync(&settings, cx);
            h.swap_backend(previous_backend);
        })
    });
    result.and(restore)
}
