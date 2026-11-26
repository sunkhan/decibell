//! Self-tests of what the app joins between crates (`registry::join_features`): each opens a
//! screen of the people crate's fixture (no core, no network) in a window of its own and drives
//! the joined piece with the crates' own test APIs, as only the app knows both sides.

use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use decibell_harness::{SelfTest, TestCx};
use gpui::{AnyWindowHandle, AppContext as _, Bounds, Focusable as _, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, px, size};

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "join-status-picker",
            about: "the custom status dialog opens the composer's emoji picker (emoji tab only): a pick sets the emoji and closes it; Escape closes it, the dialog stays",
            run: |cx| Box::pin(status_picker(cx)),
        },
        SelfTest {
            name: "join-dm-composer",
            about: "a DM conversation hosts the composer crate's composer (DM variant) with its type-to-focus hook",
            run: |cx| Box::pin(dm_composer(cx)),
        },
        SelfTest {
            name: "join-type-to-focus-hotkeys",
            about: "push to talk on a bare V with the focused fallback armed: V with no text field focused presses push to talk and types nothing; \"a\" focuses the composer and types \"a\"; in the DM call's theater no key reaches the hidden composer",
            run: |cx| Box::pin(type_to_focus_hotkeys(cx)),
        },
    ]
}

/// A window like the main one with a registered screen in it; closed by [`close`].
fn open_screen(cx: &TestCx, name: &str) -> Result<TestCx> {
    let screen = crate::registry::screen(name)?;
    let build = screen.build;
    let handle = cx.app().update(|cx| -> Result<AnyWindowHandle> {
        let bounds = Bounds::centered(None, decibell_ui::outer_window_size(size(px(1280.), px(800.))), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(800.), px(600.))),
            titlebar: Some(TitlebarOptions { title: Some("Decibell join test".into()), appears_transparent: true, traffic_light_position: None }),
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some("decibell".into()),
            ..Default::default()
        };
        let window = cx.open_window(options, move |window, cx| {
            let content = build(window, cx);
            cx.new(|cx| crate::root::Root::new(content, window, cx))
        })?;
        Ok(window.into())
    })?;
    Ok(TestCx::new(cx.app().clone(), handle))
}

fn close(cx: &TestCx) {
    let _ = cx.update(|window, _| window.remove_window());
}

async fn status_picker(cx: TestCx) -> Result<()> {
    let w = open_screen(&cx, "custom-status")?;
    let result = status_picker_in(&w).await;
    close(&w);
    result
}

async fn status_picker_in(w: &TestCx) -> Result<()> {
    use decibell_composer::picker::Picker;
    use decibell_home::status::CustomStatusModal;
    w.settle().await?;
    let modal = w.update(|_, cx| decibell_home::hooks::shown::<CustomStatusModal>(cx))?.context("the custom status dialog")?;
    let picker = |w: &TestCx| -> Result<Option<gpui::Entity<Picker>>> {
        let modal = modal.clone();
        w.update(move |_, cx| modal.read(cx).picker_view().cloned().and_then(|view| view.downcast::<Picker>().ok()))
    };
    // The emoji button opens the composer's picker, the emoji tab alone.
    w.update(|window, cx| modal.update(cx, |m, cx| m.toggle_picker(window, cx)))?;
    w.settle().await?;
    let opened = picker(w)?.context("the composer's picker in the dialog")?;
    let (tabs, tab) = w.update(|_, cx| (opened.read(cx).has_tabs(), opened.read(cx).tab()))?;
    w.log(format_args!("the dialog's emoji button opened the composer's picker (tabs: {tabs}, {tab:?})"));
    ensure!(!tabs && tab == decibell_composer::picker::Tab::Emoji, "the status picker shows the emoji tab alone");
    // Search, pick: the emoji is the status's, the picker goes.
    let search = w.update(|_, cx| opened.read(cx).search_input().clone())?;
    w.update(|_, cx| search.update(cx, |s, cx| s.set_value("smile", cx)))?;
    w.settle().await?;
    let picked = w.update(|window, cx| opened.update(cx, |p, cx| p.pick_shown(0, window, cx)))?.context("a search result")?;
    w.settle().await?;
    let (emoji, open) = w.update(|_, cx| (modal.read(cx).emoji().to_string(), modal.read(cx).picker_open()))?;
    w.log(format_args!("picked {picked:?}: the status's emoji is {emoji:?}, the picker open: {open}"));
    ensure!(emoji == picked && !open && picker(w)?.is_none(), "a pick sets the emoji and closes the picker");
    // Escape closes the picker, not the dialog.
    w.update(|window, cx| modal.update(cx, |m, cx| m.toggle_picker(window, cx)))?;
    w.settle().await?;
    ensure!(picker(w)?.is_some(), "the picker opens again");
    w.keystroke("escape")?;
    w.settle().await?;
    let (open, dialog) = w.update(|_, cx| {
        let model = decibell_model::Model::global(cx).clone();
        (modal.read(cx).picker_open(), model.overlays.read(cx).is_open(decibell_model::stores::overlays::modal::CUSTOM_STATUS))
    })?;
    w.log(format_args!("Escape: the picker open {open}, the dialog open {dialog}"));
    ensure!(!open && dialog, "Escape closes the picker and leaves the dialog");
    Ok(())
}

async fn dm_composer(cx: TestCx) -> Result<()> {
    let w = open_screen(&cx, "dm-plain")?;
    let result = dm_composer_in(&w).await;
    close(&w);
    result
}

async fn dm_composer_in(w: &TestCx) -> Result<()> {
    use decibell_composer::{Composer, Variant};
    use decibell_home::dm::Conversation;
    w.settle().await?;
    let conversation = w.update(|_, cx| decibell_home::hooks::shown::<Conversation>(cx))?.context("the conversation")?;
    let composer = w
        .update(|_, cx| conversation.read(cx).composer().cloned().and_then(|view| view.downcast::<Composer>().ok()))?
        .context("the composer crate's composer under the conversation")?;
    let (variant, peer_list) = w.update(|_, cx| (composer.read(cx).target().variant(), composer.read(cx).list().is_some()))?;
    w.log(format_args!("the conversation hosts the composer ({variant:?}, under its list: {peer_list})"));
    ensure!(variant == Variant::Dm && peer_list, "a DM composer under the conversation's list");
    // Type anywhere: with nothing focused a printable key lands in the box. (A modal an earlier
    // screen of this process left open in the fixture's model would keep the key, as it should.)
    // And an empty box: a draft an earlier test of this process left in the fixture's model
    // would be loaded into it.
    w.update(|_, cx| {
        let overlays = decibell_model::Model::global(cx).overlays.clone();
        overlays.update(cx, |o, cx| o.close_modal(cx));
        composer.read(cx).input().clone().update(cx, |i, cx| i.clear(cx));
    })?;
    w.settle().await?;
    w.update(|window, cx| window.blur(cx))?;
    w.settle().await?;
    w.type_text("h")?;
    w.sleep(Duration::from_millis(50)).await;
    let (value, focused) = w.update(|window, cx| {
        let input = composer.read(cx).input().clone();
        (input.read(cx).value().to_string(), input.read(cx).focus_handle(cx).is_focused(window))
    })?;
    w.log(format_args!("a key with nothing focused: the box holds {value:?}, focused {focused}"));
    ensure!(value == "h" && focused, "type-to-focus with nothing focused");
    w.update(|_, cx| composer.read(cx).input().clone().update(cx, |i, cx| i.clear(cx)))?;
    Ok(())
}

async fn type_to_focus_hotkeys(cx: TestCx) -> Result<()> {
    use decibell_core::config::AppSettings;
    use decibell_model::Model;

    let w = open_screen(&cx, "dm-plain")?;
    let model = w.update(|_, cx| Model::global(cx).clone())?;
    let injected: crate::hotkeys::Injected = Default::default();
    let bindings = AppSettings { hotkeys: vec![crate::hotkeys::binding("ptt", "push_to_talk", &["KeyV"])], ..AppSettings::default() };
    // A recording backend stands in for the core; XWayland's status arms the fallback.
    let (previous_backend, previous_status) = w.update(|_, cx| {
        model.live.hotkeys.update(cx, |h, cx| {
            let backend = h.swap_backend(crate::hotkeys::recording_backend(&injected));
            let status = h.status.clone();
            h.sync(&bindings, cx);
            h.set_status(crate::hotkeys::status("xwayland", true), &bindings, cx);
            (backend, status)
        })
    })?;
    let result = type_to_focus_hotkeys_in(&w, &model, &injected).await;
    close(&w);
    let result = match result {
        Ok(()) => theater_hides_composer(&cx).await,
        error => error,
    };
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

async fn type_to_focus_hotkeys_in(w: &TestCx, model: &decibell_model::Model, injected: &crate::hotkeys::Injected) -> Result<()> {
    use decibell_composer::Composer;
    use decibell_core::hotkeys::Action;
    use decibell_home::dm::Conversation;
    use gpui::{KeyCode, Keystroke, Modifiers};

    w.settle().await?;
    ensure!(w.update(|_, cx| model.live.hotkeys.read(cx).fallback_armed())?, "the fallback didn't arm");
    let conversation = w.update(|_, cx| decibell_home::hooks::shown::<Conversation>(cx))?.context("the conversation")?;
    let composer = w
        .update(|_, cx| conversation.read(cx).composer().cloned().and_then(|view| view.downcast::<Composer>().ok()))?
        .context("the composer under the conversation")?;
    let input = w.update(|_, cx| composer.read(cx).input().clone())?;
    let state = |w: &TestCx| -> Result<(String, bool)> {
        let input = input.clone();
        w.update(move |window, cx| (input.read(cx).value().to_string(), input.read(cx).focus_handle(cx).is_focused(window)))
    };
    // A key pressed and released as the platform reports it (no text inserted by the harness: only
    // a listener that takes it can type it).
    let press = |key: &str, evdev: u16| -> Result<()> {
        let keystroke = Keystroke { modifiers: Modifiers::default(), key: key.into(), key_char: Some(key.into()) };
        w.key_event(keystroke.clone(), KeyCode::Evdev(evdev), true)?;
        w.key_event(keystroke, KeyCode::Evdev(evdev), false)
    };
    w.update(|_, cx| model.overlays.update(cx, |o, cx| o.close_modal(cx)))?;
    w.update(|_, cx| input.update(cx, |i, cx| i.clear(cx)))?;
    w.update(|window, cx| window.blur(cx))?;
    w.settle().await?;
    injected.borrow_mut().clear();

    // V: the fallback's (push to talk held, then released), not typed.
    press("v", 47)?;
    w.sleep(std::time::Duration::from_millis(30)).await;
    let fired = std::mem::take(&mut *injected.borrow_mut());
    let (value, focused) = state(w)?;
    w.log(format_args!("V with no text field focused: injected {fired:?}; the box holds {value:?}, focused {focused}"));
    ensure!(
        fired == [(Action::PushToTalk, true, "ptt".to_string()), (Action::PushToTalk, false, "ptt".to_string())],
        "V didn't press and release push to talk: {fired:?}"
    );
    ensure!(value.is_empty() && !focused, "V was typed into the composer ({value:?}, focused {focused})");

    // a: no binding has it: the composer takes the focus and the character.
    press("a", 30)?;
    w.sleep(std::time::Duration::from_millis(30)).await;
    let fired = std::mem::take(&mut *injected.borrow_mut());
    let (value, focused) = state(w)?;
    w.log(format_args!("\"a\": injected {fired:?}; the box holds {value:?}, focused {focused}"));
    ensure!(fired.is_empty() && value == "a" && focused, "\"a\" didn't land in the composer ({value:?}, focused {focused}, injected {fired:?})");

    // In the box, V is the field's (`docs/electron-quirks.md` #13): typed, nothing pressed.
    w.type_text("v")?;
    let fired = std::mem::take(&mut *injected.borrow_mut());
    let (value, _) = state(w)?;
    ensure!(fired.is_empty() && value == "av", "V typed in the focused box: {value:?}, injected {fired:?}");
    w.log("in the focused box V types and presses nothing");
    // The fixture's model outlives this window: leave no draft for the next test's composer.
    w.update(|_, cx| input.update(cx, |i, cx| i.clear(cx)))?;

    Ok(())
}

/// The call stage screen (a call with the conversation's peer, connected): with the theater on the
/// conversation hides its composer, and a key typed with no text field focused reaches nobody;
/// with the theater off the same key lands in the box.
async fn theater_hides_composer(cx: &TestCx) -> Result<()> {
    use decibell_composer::Composer;
    use decibell_home::dm::Conversation;
    use gpui::{KeyCode, Keystroke, Modifiers};

    let w = open_screen(cx, "call-stage")?;
    let result = async {
        w.settle().await?;
        let model = w.update(|_, cx| decibell_model::Model::global(cx).clone())?;
        let conversation = w.update(|_, cx| decibell_home::hooks::shown::<Conversation>(cx))?.context("the conversation")?;
        let composer = w
            .update(|_, cx| conversation.read(cx).composer().cloned().and_then(|view| view.downcast::<Composer>().ok()))?
            .context("the composer under the conversation")?;
        let input = w.update(|_, cx| composer.read(cx).input().clone())?;
        let press = |key: &str, evdev: u16| -> Result<()> {
            let keystroke = Keystroke { modifiers: Modifiers::default(), key: key.into(), key_char: Some(key.into()) };
            w.key_event(keystroke.clone(), KeyCode::Evdev(evdev), true)?;
            w.key_event(keystroke, KeyCode::Evdev(evdev), false)
        };
        let state = |w: &TestCx| -> Result<(String, bool)> {
            let input = input.clone();
            w.update(move |window, cx| (input.read(cx).value().to_string(), input.read(cx).focus_handle(cx).is_focused(window)))
        };
        let calls = model.live.calls.clone();
        w.update(|_, cx| model.overlays.update(cx, |o, cx| o.close_modal(cx)))?;
        for theater in [true, false] {
            w.update(|_, cx| {
                input.update(cx, |i, cx| i.clear(cx));
                calls.update(cx, |c, cx| c.set_theater(theater, 0, cx));
            })?;
            w.update(|window, cx| window.blur(cx))?;
            w.settle().await?;
            press("b", 48)?;
            w.sleep(std::time::Duration::from_millis(30)).await;
            let (value, focused) = state(&w)?;
            w.log(format_args!("theater {theater}: a key with no text field focused; the box holds {value:?}, focused {focused}"));
            if theater {
                ensure!(value.is_empty() && !focused, "a key reached the composer the theater hides ({value:?})");
            } else {
                ensure!(value == "b" && focused, "with the theater off the key didn't land in the box ({value:?})");
            }
        }
        w.update(|_, cx| input.update(cx, |i, cx| i.clear(cx)))?;
        // The screen's fixture accepted a call (its transport never answers): leave no call behind
        // for the screens later tests of this process open.
        w.update(|_, cx| {
            calls.update(cx, |c, cx| {
                c.status = decibell_model::live::calls::CallStatus::Idle;
                c.peer = None;
                c.call_id = None;
                c.started_at = None;
                cx.notify();
            })
        })?;
        anyhow::Ok(())
    }
    .await;
    close(&w);
    result
}
