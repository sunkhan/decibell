//! The inputs' self-tests: each opens its own window on the `inputs` screen, drives it with
//! the events the platform would send (key presses through the window's dispatch and its
//! registered input handler, mouse presses, the real clipboard) and counts frames.

use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use decibell_harness::{Frames, SelfTest, TestCx};
use decibell_text::input::{BLINK_INTERVAL, InputEvent, InputState, Selection};
use decibell_ui::{ActiveTheme as _, FontRole, TitleBar, outer_window_size, window_frame};
use gpui::{
    AnyView, AnyWindowHandle, AppContext as _, Bounds, ClipboardItem, Context, Entity, EntityInputHandler as _,
    Focusable as _, IntoElement, Modifiers, Pixels, Point, Render, StyleRefinement, TitlebarOptions, Window,
    WindowBounds, WindowDecorations, WindowOptions, div, point, prelude::*, px, size,
};

use crate::inputs::Inputs;

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "input-typing",
            about: "type, move, select (keys and mouse), delete in a field and a text area: value, caret, one frame per key",
            run: |cx| Box::pin(with_window(cx, typing)),
        },
        SelfTest {
            name: "input-undo",
            about: "undo takes a typing burst back at once; a Backspace, a caret move and a paste start new steps; redo",
            run: |cx| Box::pin(with_window(cx, undo)),
        },
        SelfTest {
            name: "input-clipboard",
            about: "cut, copy, paste through the real clipboard (restored afterwards); a non-text paste goes to the hook",
            run: |cx| Box::pin(with_window(cx, clipboard)),
        },
        SelfTest {
            name: "input-ime",
            about: "a composition driven as an IME drives it: preedit marked and underlined, candidate position, commit, one undo step",
            run: |cx| Box::pin(with_window(cx, ime)),
        },
        SelfTest {
            name: "input-atoms",
            about: "emoji in the composer: one caret step, one deletion, copied as characters; a typed :shortcode: becomes one",
            run: |cx| Box::pin(with_window(cx, atoms)),
        },
        SelfTest {
            name: "input-password",
            about: "a password is drawn as bullets, typed into normally, and never copied or cut",
            run: |cx| Box::pin(with_window(cx, password)),
        },
        SelfTest {
            name: "input-caret-idle",
            about: "the caret blinks at 2 frames/s only while focused in the active window; blurred or inactive: 0 frames in 5 s",
            run: |cx| Box::pin(with_window(cx, caret_idle)),
        },
        SelfTest {
            name: "input-bytes-cap",
            about: "a 1024-byte cap clamps multi-byte text as lib/utf8.ts clampLine does, typed and pasted",
            run: |cx| Box::pin(with_window(cx, bytes_cap)),
        },
        SelfTest {
            name: "hotkeys-layout",
            about: "key events carry the physical key (GPUI patch 0012): through the dispatch, and from the X server where it can be asked",
            run: |cx| Box::pin(key_codes(cx)),
        },
        SelfTest {
            name: "input-select-filter",
            about: "a select's filter row is a text field: typing filters, the arrows and Enter still choose, Escape closes",
            run: |cx| Box::pin(select_filter(cx)),
        },
    ]
}

// ── the test window ─────────────────────────────────────────────────────────────────────────

struct Root {
    content: AnyView,
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let content = decibell_ui::focus_navigation(div())
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme.font(FontRole::Sans))
            .text_size(px(theme.scaled.text_body))
            .text_color(theme.color_text_primary)
            .child(TitleBar::new("Decibell input test"))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.content.clone().cached(StyleRefinement::default().size_full())),
            );
        window_frame(content, window, cx)
    }
}

struct Fixture {
    window: TestCx,
    inputs: Entity<Inputs>,
}

/// Opens a window like the main one on the `inputs` screen, runs `test`, closes it.
async fn with_window<F>(cx: TestCx, test: fn(Fixture) -> F) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    let (handle, inputs) = cx.app().update(|cx| -> Result<(AnyWindowHandle, Entity<Inputs>)> {
        let bounds = Bounds::centered(None, outer_window_size(size(px(1280.), px(800.))), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Decibell input test".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some("decibell".into()),
            inactive_frame_interval: None,
            ..Default::default()
        };
        let mut inputs = None;
        let window = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| Inputs::new(window, cx));
            inputs = Some(view.clone());
            cx.new(|_| Root { content: view.into() })
        })?;
        Ok((window.into(), inputs.context("no view")?))
    })?;
    let window = TestCx::new(cx.app().clone(), handle);
    let result = async {
        window.settle().await?;
        test(Fixture {
            window: window.clone(),
            inputs,
        })
        .await
    }
    .await;
    let _ = window.update(|window, _| window.remove_window());
    result
}

impl Fixture {
    fn field(&self, pick: impl FnOnce(&Inputs) -> &Entity<InputState>) -> Result<Entity<InputState>> {
        self.window.update(|_, cx| pick(self.inputs.read(cx)).clone())
    }

    fn read<R>(&self, field: &Entity<InputState>, read: impl FnOnce(&InputState) -> R) -> Result<R> {
        self.window.update(|_, cx| read(field.read(cx)))
    }

    fn value(&self, field: &Entity<InputState>) -> Result<String> {
        self.read(field, |state| state.value().to_string())
    }

    fn caret(&self, field: &Entity<InputState>) -> Result<usize> {
        self.read(field, |state| state.selection().head)
    }

    fn selected(&self, field: &Entity<InputState>) -> Result<Option<String>> {
        self.read(field, |state| state.selected_text().map(str::to_string))
    }

    /// A point in the field: `x` px from its left edge, vertically centred on its first line.
    fn at(&self, field: &Entity<InputState>, x: f32) -> Result<Point<Pixels>> {
        self.read(field, |state| {
            let bounds = state.bounds();
            point(bounds.left() + px(x), bounds.top() + px(8.))
        })
    }

    /// Clicks into the field and checks that it took the focus.
    fn click_into(&self, field: &Entity<InputState>, x: f32) -> Result<()> {
        self.window.click(self.at(field, x)?)?;
        // The pointer leaves again: the shell's hover transition is not what is measured.
        self.window.mouse_move(point(px(4.), px(4.)))?;
        let focused = self.window.update(|window, cx| field.focus_handle(cx).is_focused(window))?;
        if !focused {
            // Seen only on the Windows runner (input-atoms, 2026-11-27): say where the click went.
            let at = self.at(field, x)?;
            let bounds = self.read(field, |state| state.bounds())?;
            let (viewport, active, scale) =
                self.window.update(|window, _| (window.viewport_size(), window.is_window_active(), window.scale_factor()))?;
            anyhow::bail!(
                "a click did not focus the field (click at {at:?}, field bounds {bounds:?}, window {viewport:?} at scale {scale}, active {active})"
            );
        }
        Ok(())
    }

    fn keys(&self, keystrokes: &[&'static str]) -> Result<()> {
        for keystroke in keystrokes {
            let handled = self.window.keystroke(keystroke)?;
            ensure!(handled, "{keystroke} was not handled by the field");
        }
        Ok(())
    }

    /// Frames drawn by `act`, counted until 60 ms pass without a draw (well inside the
    /// half second to the next blink).
    async fn frames(&self, act: impl FnOnce() -> Result<()>) -> Result<u64> {
        let before = self.window.frames()?;
        act()?;
        let mut last = before;
        let mut quiet = 0;
        while quiet < 6 {
            self.window.sleep(Duration::from_millis(10)).await;
            let now = self.window.frames()?;
            quiet = if now.drawn == last.drawn { quiet + 1 } else { 0 };
            last = now;
        }
        Ok(last.since(before).drawn)
    }
}

// ── input-typing ────────────────────────────────────────────────────────────────────────────

async fn typing(fixture: Fixture) -> Result<()> {
    let window = &fixture.window;
    let field = fixture.field(|inputs| &inputs.empty)?;
    fixture.click_into(&field, 20.)?;
    window.settle().await?;

    // One frame per typed character, Turkish and an emoji included.
    let text = "Hello, wörld ığüşöç İ 😀";
    let mut frames = Vec::new();
    for character in text.chars() {
        let typed = character.to_string();
        frames.push(fixture.frames(|| window.type_text(&typed)).await?);
    }
    ensure!(fixture.value(&field)? == text, "typed {text:?}, the field holds {:?}", fixture.value(&field)?);
    ensure!(fixture.caret(&field)? == text.len(), "the caret is not at the end");
    // The first key after a click costs one frame more: GPUI redraws the window when the
    // input changes from pointer to keyboard (focus-visible styling), before the key.
    // And a window of the session taking or giving back the activation redraws the window
    // frame once, whenever it happens: at most two keys may show that extra frame.
    let extra = frames[1..].iter().filter(|drawn| **drawn != 1).count();
    ensure!(
        frames[0] <= 2 && frames[1..].iter().all(|drawn| (1..=2).contains(drawn)) && extra <= 2,
        "frames per typed character: {frames:?}"
    );
    window.log(format_args!(
        "typed {} characters: 1 frame each ({} for the first key after the click{})",
        frames.len(),
        frames[0],
        if extra > 0 { format!("; {extra} with one more, the window's activation changing") } else { String::new() }
    ));

    // Moves: one frame each, and where Chromium goes.
    let moved = fixture.frames(|| fixture.keys(&["home"])).await?;
    ensure!(moved == 1 && fixture.caret(&field)? == 0, "Home: {moved} frames, caret {}", fixture.caret(&field)?);
    fixture.keys(&["ctrl-right"])?;
    ensure!(fixture.caret(&field)? == 5, "Ctrl+Right from 0 lands after \"Hello\"");
    // Punctuation is a stop of its own (Chromium): after the comma, then after "wörld".
    fixture.keys(&["ctrl-right"])?;
    // Windows goes on past the space, to the start of the next word (Chromium's
    // `ShouldSkipSpaceWhenMovingRight`; `tests/input_vectors.rs` has the rule and its source).
    let after_comma = if cfg!(target_os = "windows") { 7 } else { 6 };
    ensure!(
        fixture.caret(&field)? == after_comma,
        "Ctrl+Right stops after the comma (caret {}, expected {after_comma})",
        fixture.caret(&field)?
    );
    fixture.keys(&["ctrl-right", "ctrl-left"])?;
    ensure!(fixture.caret(&field)? == 7, "Ctrl+Left lands before \"wörld\"");
    fixture.keys(&["end", "left"])?;
    ensure!(fixture.caret(&field)? == text.len() - "😀".len(), "Left steps over the whole emoji");
    fixture.keys(&["ctrl-home", "shift-ctrl-right"])?;
    ensure!(fixture.selected(&field)?.as_deref() == Some("Hello"), "Shift+Ctrl+Right selects a word");
    // Left collapses a selection to its start; typing replaces a selection.
    fixture.keys(&["left"])?;
    ensure!(fixture.caret(&field)? == 0 && fixture.selected(&field)?.is_none(), "Left collapses to the start");
    fixture.keys(&["shift-ctrl-right"])?;
    window.type_text("Bye")?;
    ensure!(fixture.value(&field)?.starts_with("Bye, wörld"), "typing replaces the selection");
    // Deleting: by character, by word, a selection.
    fixture.keys(&["end", "backspace", "backspace", "ctrl-backspace"])?;
    ensure!(fixture.value(&field)? == "Bye, wörld ığüşöç ", "Backspace ×2 then Ctrl+Backspace: {:?}", fixture.value(&field)?);
    fixture.keys(&["home", "delete", "ctrl-delete"])?;
    ensure!(fixture.value(&field)? == ", wörld ığüşöç ", "Delete then Ctrl+Delete: {:?}", fixture.value(&field)?);
    fixture.keys(&["ctrl-a"])?;
    ensure!(fixture.selected(&field)?.as_deref() == Some(", wörld ığüşöç "), "Ctrl+A selects all");
    fixture.keys(&["backspace"])?;
    ensure!(fixture.value(&field)?.is_empty(), "Backspace deletes the selection");
    // Enter in a single line submits and inserts nothing; Tab moves the focus on.
    let submitted = std::rc::Rc::new(std::cell::Cell::new(0));
    let subscription = window.update(|_, cx| {
        let submitted = submitted.clone();
        cx.subscribe(&field, move |_, event: &InputEvent, _| {
            if *event == InputEvent::Submit {
                submitted.set(submitted.get() + 1);
            }
        })
    })?;
    window.type_text("go")?;
    fixture.keys(&["enter"])?;
    ensure!(submitted.get() == 1 && fixture.value(&field)? == "go", "Enter submits a single line");
    drop(subscription);

    // The mouse: a double click selects a word, a drag extends by words, Shift+click
    // extends, a triple click takes the line.
    let name = fixture.field(|inputs| &inputs.name)?; // "Daily Gaming"
    window.click(fixture.at(&name, 10.)?)?;
    window.mouse_down(fixture.at(&name, 10.)?, 2, Modifiers::default())?;
    ensure!(fixture.selected(&name)?.as_deref() == Some("Daily"), "a double click selects the word");
    window.mouse_drag(fixture.at(&name, 70.)?)?;
    window.mouse_up(fixture.at(&name, 70.)?)?;
    ensure!(fixture.selected(&name)?.as_deref() == Some("Daily Gaming"), "a double-click drag extends by words: {:?}", fixture.selected(&name)?);
    window.click(fixture.at(&name, 2.)?)?;
    ensure!(fixture.caret(&name)? == 0, "a click places the caret");
    window.mouse_down(fixture.at(&name, 200.)?, 1, Modifiers::shift())?;
    window.mouse_up(fixture.at(&name, 200.)?)?;
    ensure!(fixture.selected(&name)?.as_deref() == Some("Daily Gaming"), "Shift+click extends the selection");
    window.mouse_down(fixture.at(&name, 10.)?, 3, Modifiers::default())?;
    window.mouse_up(fixture.at(&name, 10.)?)?;
    ensure!(fixture.selected(&name)?.as_deref() == Some("Daily Gaming"), "a triple click selects the line");

    // A value wider than its box: the caret stays inside it at both ends.
    let long = fixture.field(|inputs| &inputs.long)?;
    fixture.click_into(&long, 20.)?;
    for key in ["end", "home", "end"] {
        fixture.keys(&[key])?;
        window.settle().await?;
        let (bounds, caret) = fixture.read(&long, |state| (state.bounds(), state.caret_bounds()))?;
        ensure!(
            caret.left() >= bounds.left() - px(0.5) && caret.right() <= bounds.right() + px(0.5),
            "after {key} the caret ({caret:?}) is outside the field ({bounds:?})"
        );
    }
    // A drag past the right edge keeps scrolling and selecting.
    fixture.keys(&["home"])?;
    window.settle().await?;
    let width = fixture.read(&long, |state| f32::from(state.bounds().size.width))?;
    window.mouse_down(fixture.at(&long, 5.)?, 1, Modifiers::default())?;
    window.mouse_drag(fixture.at(&long, width + 40.)?)?;
    window.sleep(Duration::from_millis(1500)).await;
    window.mouse_up(fixture.at(&long, width + 40.)?)?;
    let dragged = fixture.selected(&long)?.map_or(0, |text| text.len());
    let visible = (width / 6.5) as usize;
    ensure!(dragged > visible + 5, "the drag past the edge selected only {dragged} characters ({visible} fit the box)");
    window.log(format_args!("a 1.5 s drag past the edge scrolled and selected {dragged} characters"));

    // A text area: Up and Down keep their column across lines of different lengths.
    let area = fixture.field(|inputs| &inputs.area)?;
    fixture.click_into(&area, 20.)?;
    let value = fixture.value(&area)?;
    let lines: Vec<(usize, usize)> = value
        .split('\n')
        .scan(0, |start, line| {
            let range = (*start, *start + line.len());
            *start += line.len() + 1;
            Some(range)
        })
        .collect();
    fixture.keys(&["ctrl-end", "up", "end"])?;
    ensure!(fixture.caret(&area)? == lines[2].1, "End goes to the end of the third line");
    fixture.keys(&["up", "up"])?;
    ensure!(fixture.caret(&area)? == lines[0].1, "Up ×2 from a long line ends at the first line's end");
    fixture.keys(&["down", "down"])?;
    ensure!(fixture.caret(&area)? == lines[2].1, "Down ×2 returns to the column it started from");
    fixture.keys(&["up", "home", "shift-down"])?;
    ensure!(
        fixture.selected(&area)?.as_deref() == Some("fn main() {\n"),
        "Shift+Down selects a line: {:?}",
        fixture.selected(&area)?
    );
    // Enter in a text area is a line break, one frame.
    fixture.keys(&["ctrl-end"])?;
    window.settle().await?;
    let drawn = fixture.frames(|| fixture.keys(&["enter"])).await?;
    ensure!(drawn == 1 && fixture.value(&area)? == format!("{value}\n"), "Enter adds a line in one frame ({drawn})");
    let stats = fixture.read(&area, |state| state.display_stats())?;
    ensure!(stats.shaped <= 2 && stats.reused >= 3 && stats.wrapped == 0, "the edit re-shaped more than its own lines: {stats:?}");
    window.log(format_args!("a new line in a {}-line text area: {stats:?}", lines.len()));
    Ok(())
}

// ── input-undo ──────────────────────────────────────────────────────────────────────────────

async fn undo(fixture: Fixture) -> Result<()> {
    let window = &fixture.window;
    let field = fixture.field(|inputs| &inputs.empty)?;
    fixture.click_into(&field, 20.)?;
    // A burst is one step, however long the pauses in it; a Backspace starts the next
    // one, and what is typed after it joins that (Chromium's typing command).
    window.type_text("one ")?;
    window.sleep(Duration::from_millis(1200)).await;
    window.type_text("twx")?;
    fixture.keys(&["backspace"])?;
    window.type_text("o")?;
    // A caret move ends it.
    fixture.keys(&["left", "right"])?;
    window.type_text(" three")?;
    // A paste is its own step.
    window.update(|_, cx| field.update(cx, |state, cx| state.insert_text(" PASTED", cx)))?;
    window.type_text("!")?;
    ensure!(fixture.value(&field)? == "one two three PASTED!", "{:?}", fixture.value(&field)?);
    let mut seen = Vec::new();
    for _ in 0..5 {
        fixture.keys(&["ctrl-z"])?;
        seen.push(fixture.value(&field)?);
    }
    ensure!(
        seen == ["one two three PASTED", "one two three", "one two", "one twx", ""],
        "undo went through {seen:?}"
    );
    ensure!(!window.keystroke("ctrl-z")? || fixture.value(&field)?.is_empty(), "undo past the first step");
    ensure!(fixture.caret(&field)? == 0, "undo restores the caret");
    fixture.keys(&["ctrl-shift-z", "ctrl-y", "ctrl-y"])?;
    ensure!(fixture.value(&field)? == "one two three", "redo (Ctrl+Shift+Z, Ctrl+Y): {:?}", fixture.value(&field)?);
    ensure!(fixture.caret(&field)? == 13, "redo restores the caret");
    window.type_text("?")?;
    ensure!(!window.keystroke("ctrl-y")? || fixture.value(&field)? == "one two three?", "typing drops the redo steps");
    window.log("burst with a pause, Backspace + retype, caret move, paste: 5 undo steps; redo; typing drops the redo steps");
    Ok(())
}

// ── input-clipboard ─────────────────────────────────────────────────────────────────────────

/// Whether the test may use the real clipboard. Not under GPUI's Wayland backend: Wayland
/// takes a clipboard write only with the serial of a real key or button press, which
/// injected events do not have, so a write there is refused or, worse, half works and
/// cannot be taken back (the previous content would be lost). On X11 a probe is written
/// and read back; the caller restores what was there before.
fn clipboard_writable(window: &TestCx) -> Result<bool> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty()) {
        return Ok(false);
    }
    const PROBE: &str = "decibell input self-test probe";
    window.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(PROBE.into()));
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref() == Some(PROBE)
    })
}

async fn clipboard(fixture: Fixture) -> Result<()> {
    let window = &fixture.window;
    let field = fixture.field(|inputs| &inputs.name)?; // "Daily Gaming"
    fixture.click_into(&field, 20.)?;
    let previous = window.update(|_, cx| cx.read_from_clipboard())?;
    let writable = clipboard_writable(window)?;
    let result = async {
        let on_clipboard = || window.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
        fixture.keys(&["ctrl-a", "ctrl-c"])?;
        if writable {
            ensure!(on_clipboard()?.as_deref() == Some("Daily Gaming"), "copy put {:?} on the clipboard", on_clipboard()?);
            fixture.keys(&["end", "ctrl-v"])?;
            ensure!(fixture.value(&field)? == "Daily GamingDaily Gaming", "paste: {:?}", fixture.value(&field)?);
            fixture.keys(&["ctrl-shift-left", "ctrl-x"])?;
            ensure!(on_clipboard()?.as_deref() == Some("Gaming"), "cut put {:?} on the clipboard", on_clipboard()?);
            ensure!(fixture.value(&field)? == "Daily GamingDaily ", "cut left {:?}", fixture.value(&field)?);
            fixture.keys(&["home", "shift-insert"])?;
            ensure!(fixture.value(&field)? == "GamingDaily GamingDaily ", "paste at the start: {:?}", fixture.value(&field)?);
            // A paste is one undo step.
            fixture.keys(&["ctrl-z"])?;
            ensure!(fixture.value(&field)? == "Daily GamingDaily ", "undo of a paste: {:?}", fixture.value(&field)?);
            // A multi-line paste into a single line: the breaks become spaces.
            window.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("a\r\nb\nc".into())))?;
            fixture.keys(&["ctrl-a", "ctrl-v"])?;
            ensure!(fixture.value(&field)? == "a b c", "line breaks pasted into a single line: {:?}", fixture.value(&field)?);
            window.log("copy, paste, cut, Shift+Insert, undo of a paste, a multi-line paste: through the real clipboard");
        } else {
            window.log("Wayland takes clipboard writes only with a real key or button press: the round trip is not run here (run with WAYLAND_DISPLAY= for X11)");
            ensure!(fixture.selected(&field)?.as_deref() == Some("Daily Gaming"), "what a copy would take");
            return Ok(());
        }
        // A paste that is not text goes to the owner's hook (the composer makes attachments).
        let offered = std::rc::Rc::new(std::cell::Cell::new(0));
        window.update(|_, cx| {
            let offered = offered.clone();
            field.update(cx, |state, _| {
                state.on_paste_non_text(move |item, _, _| {
                    offered.set(offered.get() + item.entries().len());
                    true
                })
            });
            let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, vec![0x89, b'P', b'N', b'G']);
            cx.write_to_clipboard(ClipboardItem::new_image(&image));
        })?;
        let before = fixture.value(&field)?;
        fixture.keys(&["ctrl-v"])?;
        let holds_image = window.update(|_, cx| {
            cx.read_from_clipboard()
                .is_some_and(|item| item.entries().iter().any(|entry| matches!(entry, gpui::ClipboardEntry::Image(_))))
        })?;
        if holds_image {
            ensure!(offered.get() == 1 && fixture.value(&field)? == before, "an image paste was not offered to the hook");
            window.log("an image on the clipboard went to the non-text hook, nothing was inserted");
        } else {
            window.log("this platform cannot put an image on the clipboard (GPUI on Linux writes text only): the non-text hook was not exercised here");
        }
        Ok(())
    }
    .await;
    // The owner's clipboard goes back as it was.
    if writable {
        window.update(|_, cx| match previous {
            Some(item) => cx.write_to_clipboard(item),
            None => cx.write_to_clipboard(ClipboardItem::new_string(String::new())),
        })?;
    }
    result
}

// ── input-ime ───────────────────────────────────────────────────────────────────────────────

async fn ime(fixture: Fixture) -> Result<()> {
    let window = &fixture.window;
    let field = fixture.field(|inputs| &inputs.name)?; // "Daily Gaming"
    fixture.click_into(&field, 20.)?;
    fixture.keys(&["ctrl-left", "ctrl-left", "ctrl-right"])?; // after "Daily"
    if cfg!(target_os = "windows") {
        // A word move forward ends before "Gaming" there (past the space): one step back.
        fixture.keys(&["left"])?;
    }
    ensure!(fixture.caret(&field)? == 5, "the caret is at {}, not after \"Daily\"", fixture.caret(&field)?);
    window.settle().await?;
    let ime = |act: &mut dyn FnMut(&mut InputState, &mut Window, &mut Context<InputState>)| {
        window.update(|window, cx| field.update(cx, |state, cx| act(state, window, cx)))
    };
    // The preedit grows and is replaced, as an IME sends it (Wayland: no range, no caret).
    for preedit in ["k", "か", "かん", "かんじ"] {
        let drawn = fixture
            .frames(|| ime(&mut |state, window, cx| state.replace_and_mark_text_in_range(None, preedit, None, window, cx)))
            .await?;
        ensure!(drawn == 1, "a preedit update drew {drawn} frames");
        let (value, marked) = fixture.read(&field, |state| (state.value().to_string(), state.marked_range()))?;
        ensure!(value == format!("Daily{preedit} Gaming"), "preedit {preedit:?}: {value:?}");
        ensure!(marked == Some(5..5 + preedit.len()), "the marked range is {marked:?}");
    }
    // UTF-16 across the protocol: the marked range and the selection in code units.
    let (marked, selected) = window.update(|window, cx| {
        field.update(cx, |state, cx| {
            (
                state.marked_text_range(window, cx),
                state.selected_text_range(false, window, cx).map(|selection| selection.range),
            )
        })
    })?;
    ensure!(marked == Some(5..8) && selected == Some(8..8), "UTF-16 ranges: marked {marked:?}, selection {selected:?}");
    // The candidate window goes where the marked text starts, inside the field.
    let (candidate, bounds) = window.update(|window, cx| {
        field.update(cx, |state, cx| {
            let bounds = state.bounds();
            (state.bounds_for_range(5..5, bounds, window, cx), bounds)
        })
    })?;
    let candidate = candidate.context("no bounds for the marked range")?;
    ensure!(
        candidate.left() > bounds.left() + px(20.) && candidate.right() < bounds.right() && candidate.top() >= bounds.top() - px(1.) && candidate.bottom() <= bounds.bottom() + px(1.),
        "the candidate rectangle {candidate:?} is not at the preedit inside {bounds:?}"
    );
    // Enter belongs to the IME while it composes.
    ensure!(!window.keystroke("enter")? || fixture.read(&field, |state| state.is_composing())?, "Enter ended the composition");
    // Commit: the marked text is replaced, nothing stays marked.
    ime(&mut |state, window, cx| state.replace_text_in_range(None, "漢字", window, cx))?;
    let (value, marked) = fixture.read(&field, |state| (state.value().to_string(), state.marked_range()))?;
    ensure!(value == "Daily漢字 Gaming" && marked.is_none(), "after the commit: {value:?}, marked {marked:?}");
    ensure!(fixture.caret(&field)? == "Daily漢字".len(), "the caret follows the commit");
    // The whole composition is one undo step.
    fixture.keys(&["ctrl-z"])?;
    ensure!(fixture.value(&field)? == "Daily Gaming", "undo of a composition: {:?}", fixture.value(&field)?);
    // A cancelled composition (an empty preedit) leaves no trace; losing focus keeps the text.
    ime(&mut |state, window, cx| state.replace_and_mark_text_in_range(None, "x", None, window, cx))?;
    ime(&mut |state, window, cx| state.replace_and_mark_text_in_range(None, "", None, window, cx))?;
    ensure!(fixture.value(&field)? == "Daily Gaming" && !fixture.read(&field, |state| state.is_composing())?, "a cancelled preedit");
    ime(&mut |state, window, cx| state.replace_and_mark_text_in_range(None, "y", None, window, cx))?;
    window.update(|window, cx| window.blur(cx))?;
    window.settle().await?;
    ensure!(fixture.value(&field)? == "Dailyy Gaming" && !fixture.read(&field, |state| state.is_composing())?, "blur keeps the preedit as text");
    // A password gives the IME no text to read.
    let password = fixture.field(|inputs| &inputs.password)?;
    let read = window.update(|window, cx| password.update(cx, |state, cx| state.text_for_range(0..4, &mut None, window, cx)))?;
    ensure!(read.is_none(), "an input method could read a password");
    window.log("preedit ×4 (1 frame each), UTF-16 ranges, candidate position, commit, one undo step, cancel, blur");
    Ok(())
}

// ── input-atoms ─────────────────────────────────────────────────────────────────────────────

async fn atoms(fixture: Fixture) -> Result<()> {
    let window = &fixture.window;
    let field = fixture.field(|inputs| &inputs.composer_empty)?;
    fixture.click_into(&field, 20.)?;
    let family = "👨‍👩‍👧";
    window.update(|_, cx| field.update(cx, |state, cx| state.set_value(&format!("a{family}b🇹🇷"), cx)))?;
    window.settle().await?;
    // Drawn as boxes: one U+FFFC per atom.
    let shown = fixture.read(&field, |state| state.displayed_text())?;
    ensure!(shown == "a\u{fffc}b\u{fffc}", "the atoms are drawn as {shown:?}");
    // One step over each, both ways.
    fixture.keys(&["ctrl-home"])?;
    let mut stops = vec![0];
    for _ in 0..4 {
        fixture.keys(&["right"])?;
        stops.push(fixture.caret(&field)?);
    }
    let end = 2 + family.len() + "🇹🇷".len();
    ensure!(stops == [0, 1, 1 + family.len(), 2 + family.len(), end], "caret stops: {stops:?}");
    fixture.keys(&["left"])?;
    ensure!(fixture.caret(&field)? == 2 + family.len(), "Left steps back over the flag");
    // A click on an atom lands on one of its sides, never inside.
    let (left, right) = window.update(|_, cx| {
        field.update(cx, |state, cx| {
            state.select(Selection::caret(1), cx);
            let left = state.caret_bounds().left();
            state.select(Selection::caret(1 + family.len()), cx);
            (left, state.caret_bounds().left())
        })
    })?;
    ensure!(right - left > px(15.), "the family emoji is {:?} wide", right - left);
    let origin = fixture.read(&field, |state| state.bounds().left())?;
    window.click(fixture.at(&field, f32::from(left - origin) + 6.)?)?;
    ensure!(fixture.caret(&field)? == 1, "a click on the atom's left half");
    window.click(fixture.at(&field, f32::from(right - origin) - 6.)?)?;
    ensure!(fixture.caret(&field)? == 1 + family.len(), "a click on the atom's right half");
    // Copied as its characters.
    fixture.keys(&["shift-left"])?;
    ensure!(fixture.selected(&field)?.as_deref() == Some(family), "an atom copies as {:?}", fixture.selected(&field)?);
    // One Backspace, one Delete.
    fixture.keys(&["right", "backspace"])?;
    ensure!(fixture.value(&field)? == "ab🇹🇷", "Backspace removes the whole atom: {:?}", fixture.value(&field)?);
    fixture.keys(&["right", "delete"])?;
    ensure!(fixture.value(&field)? == "ab", "Delete removes the whole atom: {:?}", fixture.value(&field)?);
    // A typed shortcode becomes its emoji when it is complete; undo takes the burst back.
    window.type_text(" :fire")?;
    ensure!(fixture.value(&field)? == "ab :fire", "an open shortcode stays text");
    window.type_text(":")?;
    ensure!(fixture.value(&field)? == "ab 🔥", "a complete shortcode: {:?}", fixture.value(&field)?);
    ensure!(fixture.caret(&field)? == "ab 🔥".len(), "the caret is behind the new emoji");
    ensure!(fixture.read(&field, |state| state.displayed_text())? == "ab \u{fffc}", "the new emoji is an atom");
    // Enter sends, Shift+Enter breaks, an open code fence keeps Enter in the block.
    let composer = fixture.field(|inputs| &inputs.composer)?;
    fixture.click_into(&composer, 20.)?;
    window.update(|_, cx| composer.update(cx, |state, cx| state.set_value("line", cx)))?;
    fixture.keys(&["shift-enter"])?;
    ensure!(fixture.value(&composer)? == "line\n", "Shift+Enter breaks the line");
    window.type_text("```")?;
    fixture.keys(&["enter"])?;
    ensure!(fixture.value(&composer)? == "line\n```\n", "Enter inside an open fence breaks the line");
    window.type_text("```")?;
    fixture.keys(&["enter"])?;
    window.settle().await?;
    let sent = window.update(|_, cx| fixture.inputs.read(cx).sent.clone())?;
    ensure!(sent.as_deref() == Some("line\n```\n```") && fixture.value(&composer)?.is_empty(), "Enter sends: {sent:?}");
    window.log("atoms: one step, one deletion, copied as characters, clicked by halves; :fire: → 🔥; Enter / Shift+Enter / open fence");
    Ok(())
}

// ── input-password ──────────────────────────────────────────────────────────────────────────

async fn password(fixture: Fixture) -> Result<()> {
    let window = &fixture.window;
    let field = fixture.field(|inputs| &inputs.password)?; // "hunter2 şifre"
    fixture.click_into(&field, 20.)?;
    fixture.keys(&["ctrl-a"])?;
    window.type_text("pässwörd 1")?;
    window.settle().await?;
    ensure!(fixture.value(&field)? == "pässwörd 1", "the owner reads the real text");
    let shown = fixture.read(&field, |state| state.displayed_text())?;
    ensure!(shown == "•".repeat(10), "a password is drawn as {shown:?}");
    // Nothing of it reaches a clipboard.
    fixture.keys(&["ctrl-a"])?;
    ensure!(fixture.selected(&field)?.is_none(), "a password offers text to copy");
    let previous = window.update(|_, cx| cx.read_from_clipboard())?;
    if clipboard_writable(window)? {
        window.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())))?;
        fixture.keys(&["ctrl-c", "ctrl-x"])?;
        let after = window.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))?;
        window.update(|_, cx| cx.write_to_clipboard(previous.unwrap_or_else(|| ClipboardItem::new_string(String::new()))))?;
        ensure!(after.as_deref() == Some("sentinel"), "copy / cut of a password changed the clipboard");
    } else {
        // Copy and cut write nothing for a password, so the keys are safe to press anywhere.
        fixture.keys(&["ctrl-c", "ctrl-x"])?;
    }
    ensure!(fixture.value(&field)? == "pässwörd 1", "cut removed the password");
    // Words do not show through: Ctrl+Left goes to the start, a double click takes it all.
    fixture.keys(&["end", "ctrl-left"])?;
    ensure!(fixture.caret(&field)? == 0, "Ctrl+Left stops inside a password");
    window.log("drawn as 10 bullets, value intact, copy and cut refused, no word stops");
    Ok(())
}

// ── input-caret-idle ────────────────────────────────────────────────────────────────────────

/// CPU time this process has used so far, ms (all threads; 10 ms steps). Linux only.
fn cpu_ms() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks: u64 = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
    Some(ticks * 10)
}

/// Waits for the window to settle and counts the frames of the next `duration`; a count that
/// is not zero is taken again, up to three times. Another window of the session taking or
/// giving back the activation redraws the window frame once (as the app's `idle-frames`
/// test allows for): only a window that draws in every attempt is drawing by itself.
async fn idle_frames(window: &TestCx, duration: Duration) -> Result<Frames> {
    let mut frames = Frames { drawn: 0, presented: 0 };
    for _ in 0..3 {
        window.settle().await?;
        frames = frames_over(window, duration).await?;
        if frames.drawn == 0 && frames.presented == 0 {
            break;
        }
    }
    Ok(frames)
}

async fn frames_over(window: &TestCx, duration: Duration) -> Result<Frames> {
    let before = window.frames()?;
    window.sleep(duration).await;
    Ok(window.frames()?.since(before))
}

async fn caret_idle(fixture: Fixture) -> Result<()> {
    let window = &fixture.window;
    let field = fixture.field(|inputs| &inputs.name)?;
    window.update(|window, _| window.activate_window())?;
    window.settle().await?;
    let active = window.update(|window, _| window.is_window_active())?;
    fixture.click_into(&field, 20.)?;
    let five = Duration::from_secs(5);
    if active {
        window.sleep(Duration::from_millis(300)).await;
        // 500 ms shown, 500 ms hidden: 10 changes in 5 s. Other windows of the session may
        // take the activation meanwhile, and its real pointer may cross the test window (a
        // hover frame): a run that was not active throughout, or is off, is measured again.
        let expected = (five.as_millis() / BLINK_INTERVAL.as_millis()) as u64;
        let fits = |frames: &Frames| (expected - 1..=expected + 1).contains(&frames.drawn);
        let is_active = || window.update(|window, _| window.is_window_active());
        let mut measured = None;
        for attempt in 0..4 {
            window.update(|window, _| window.activate_window())?;
            window.settle().await?;
            if !is_active()? {
                continue;
            }
            let cpu = cpu_ms();
            let frames = frames_over(window, five).await?;
            if !is_active()? {
                window.log("the window lost the activation during the measurement: again");
                continue;
            }
            if let (Some(before), Some(after)) = (cpu, cpu_ms()) {
                window.log(format_args!(
                    "{} ms of CPU in those 5 s ({:.2} % of a core, all threads)",
                    after - before,
                    (after - before) as f64 / 50.
                ));
            }
            measured = Some(frames);
            if fits(&frames) {
                break;
            }
            window.log(format_args!("attempt {attempt}: {} frames in 5 s: again", frames.drawn));
        }
        let Some(frames) = measured else {
            window.log("the test window did not stay active for 5 s in four attempts (other windows of the session keep taking the activation): the blink was not measured");
            return idle_cases(&fixture, &field).await;
        };
        ensure!(fits(&frames), "focused and active: {} frames in 5 s, expected {expected}", frames.drawn);
        window.log(format_args!("focused, window active: {} frames in 5 s (the blink, 2/s)", frames.drawn));
        // Typing holds the caret solid: no blink frame lands between keys.
        let before = window.frames()?;
        for _ in 0..10 {
            window.type_text("x")?;
            window.sleep(Duration::from_millis(100)).await;
            ensure!(fixture.read(&field, |state| state.caret_shown())?, "the caret went off while typing");
        }
        let typed = window.frames()?.since(before).drawn;
        // (One more for the first key after the click: GPUI's pointer → keyboard redraw.)
        ensure!((10..=11).contains(&typed), "10 keys at 10/s drew {typed} frames");
        window.log(format_args!("typing 10 keys at 10/s: {typed} frames, the caret solid throughout, no blink frame in between"));
    } else {
        window.log("the compositor did not activate the test window: the focused-and-active blink was not measured");
    }

    idle_cases(&fixture, &field).await
}

/// The two cases that must draw nothing: the field blurred, and focused in a window that
/// is not the active one.
async fn idle_cases(fixture: &Fixture, field: &Entity<InputState>) -> Result<()> {
    let window = &fixture.window;
    let five = Duration::from_secs(5);
    // Blurred: no timer, no frames.
    window.update(|window, cx| window.blur(cx))?;
    let frames = idle_frames(window, five).await?;
    ensure!(frames.drawn == 0 && frames.presented == 0, "blurred: {frames:?} in 5 s");
    ensure!(!fixture.read(field, |state| state.caret_shown())?, "a blurred field shows a caret");
    window.log("blurred: 0 frames in 5 s");

    // Focused in a window that is not the active one: another window takes the activation.
    fixture.click_into(field, 20.)?;
    let other = window.app().update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(320.), px(120.)), cx))),
                app_id: Some("decibell".into()),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Blank),
        )
    })?;
    window.sleep(Duration::from_millis(800)).await;
    let inactive = !window.update(|window, _| window.is_window_active())?;
    let result = if inactive {
        let frames = idle_frames(window, five).await?;
        ensure!(frames.drawn == 0 && frames.presented == 0, "focused in an inactive window: {frames:?} in 5 s");
        window.log("focused, window inactive: 0 frames in 5 s");
        Ok(())
    } else {
        window.log("the compositor kept the test window active under a new window: the inactive case was not measured");
        Ok(())
    };
    let _ = window.app().update(|cx| other.update(cx, |_, window, _| window.remove_window()));
    result
}

struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(cx.theme().color_bg_primary)
    }
}

// ── input-bytes-cap ─────────────────────────────────────────────────────────────────────────

/// `lib/utf8.ts` `clampLine`, written out again as the reference.
fn clamp_line(input: &str, max_bytes: usize) -> String {
    let mut line = String::new();
    let mut in_break = false;
    for c in input.chars() {
        if matches!(c, '\r' | '\n' | '\t') {
            if !in_break {
                line.push(' ');
            }
            in_break = true;
        } else {
            in_break = false;
            if !(c < '\u{20}' || c == '\u{7f}') {
                line.push(c);
            }
        }
    }
    let mut out = String::new();
    for c in line.chars() {
        if out.len() + c.len_utf8() > max_bytes {
            break;
        }
        out.push(c);
    }
    out
}

async fn bytes_cap(fixture: Fixture) -> Result<()> {
    let window = &fixture.window;
    let field = fixture.field(|inputs| &inputs.empty)?;
    window.update(|window, cx| {
        field.update(cx, |state, cx| {
            let mut rules = *state.rules();
            rules.max_utf8 = Some(1024);
            state.set_rules(rules, cx);
            state.focus(window, cx);
        })
    })?;
    // 2-, 3- and 4-byte characters, so that the cut falls inside one of them at some length.
    let unit = "ığ日😀ş";
    let pasted = unit.repeat(120); // 14 bytes × 120 = 1680
    window.update(|_, cx| field.update(cx, |state, cx| state.insert_text(&pasted, cx)))?;
    let value = fixture.value(&field)?;
    ensure!(value == clamp_line(&pasted, 1024), "a paste past the cap: {} bytes", value.len());
    ensure!(value.len() <= 1024 && value.len() >= 1021, "clamped to {} bytes", value.len());
    // Typing at the cap changes nothing; typing in the middle pushes the tail out.
    window.type_text("😀")?;
    ensure!(fixture.value(&field)? == value, "typing a character that does not fit changed the value");
    fixture.keys(&["home"])?;
    window.type_text("şş")?;
    let expected = clamp_line(&format!("şş{value}"), 1024);
    ensure!(fixture.value(&field)? == expected, "typing at the start: {} bytes", fixture.value(&field)?.len());
    ensure!(fixture.caret(&field)? == 4, "the caret stays behind what was typed");
    // Line breaks and tabs become one space, other control characters go.
    fixture.keys(&["ctrl-a"])?;
    window.update(|_, cx| field.update(cx, |state, cx| state.insert_text("a\r\n\t b\u{7}c", cx)))?;
    ensure!(fixture.value(&field)? == clamp_line("a\r\n\t b\u{7}c", 1024), "control characters: {:?}", fixture.value(&field)?);
    window.log(format_args!("1,680 bytes pasted → {} kept on a character boundary; typing at the cap and in the middle; control characters", value.len()));
    Ok(())
}

// ── input-select-filter ─────────────────────────────────────────────────────────────────────

const LETTERS: [&str; 12] = [
    "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India", "Juliet", "Kilo", "Lima",
];

struct SelectFixture {
    handle: decibell_ui::SelectHandle,
    value: usize,
}

impl Render for SelectFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let options = LETTERS
            .iter()
            .enumerate()
            .map(|(index, label)| decibell_ui::SelectOption::new(index, *label))
            .collect();
        div().size_full().bg(cx.theme().color_bg_primary).p_6().child(
            div().w(px(240.)).child(
                decibell_ui::Select::options("letters", Some(self.value), options)
                    .track(&self.handle)
                    .on_change(cx.listener(|this, value: &usize, _, cx| {
                        this.value = *value;
                        cx.notify();
                    })),
            ),
        )
    }
}

async fn select_filter(cx: TestCx) -> Result<()> {
    let (handle, view) = cx.app().update(|cx| -> Result<(AnyWindowHandle, Entity<SelectFixture>)> {
        let bounds = Bounds::centered(None, outer_window_size(size(px(800.), px(600.))), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some("decibell".into()),
            ..Default::default()
        };
        let mut view = None;
        let window = cx.open_window(options, |_, cx| {
            let fixture = cx.new(|cx| SelectFixture {
                handle: decibell_ui::SelectHandle::new(cx),
                value: 0,
            });
            view = Some(fixture.clone());
            cx.new(|_| Root { content: fixture.into() })
        })?;
        Ok((window.into(), view.context("no view")?))
    })?;
    let window = TestCx::new(cx.app().clone(), handle);
    // An open select closes when its window loses the activation, and other windows of the
    // session (other tests, the owner) can take it at any moment: an attempt that fails is
    // run again, up to three times, and every failure is logged.
    let attempt = async || -> Result<()> {
        window.update(|_, cx| {
            view.update(cx, |fixture, cx| {
                fixture.handle.close(cx);
                fixture.value = 0;
                cx.notify();
            })
        })?;
        window.settle().await?;
        let value = || window.update(|_, cx| view.read(cx).value);
        let is_open = || window.update(|_, cx| view.read(cx).handle.is_open(cx));
        let trigger = window.update(|window, cx| {
            decibell_ui::visible_window_bounds(window, cx).origin + point(px(60.), decibell_ui::TITLEBAR_HEIGHT + px(40.))
        })?;
        window.click(trigger)?;
        window.settle().await?;
        ensure!(is_open()?, "a click did not open the select");
        // Typed text goes to the filter row; Enter chooses the first match.
        window.type_text("fox")?;
        window.settle().await?;
        ensure!(window.keystroke("enter")?, "Enter was not handled");
        window.settle().await?;
        ensure!(value()? == 5 && !is_open()?, "\"fox\" + Enter chose {}", LETTERS[value()?]);
        // The filter edits like any field (Backspace, a word deleted), and starts empty again.
        window.click(trigger)?;
        window.settle().await?;
        window.type_text("zz top")?;
        for key in ["backspace", "ctrl-backspace", "backspace", "backspace", "backspace"] {
            ensure!(window.keystroke(key)?, "{key} was not handled");
        }
        window.type_text("li")?;
        window.settle().await?;
        // Charlie, Juliet and Lima match "li": ↓ moves to the second.
        for key in ["down", "enter"] {
            ensure!(window.keystroke(key)?, "{key} was not handled");
        }
        window.settle().await?;
        ensure!(value()? == 9, "\"li\" ↓ Enter chose {}", LETTERS[value()?]);
        // Escape closes without choosing.
        window.click(trigger)?;
        window.settle().await?;
        window.type_text("a")?;
        ensure!(window.keystroke("escape")?, "Escape was not handled");
        window.settle().await?;
        ensure!(!is_open()? && value()? == 9, "Escape chose something");
        window.log("typed \"fox\" Enter → Foxtrot; edited the filter to \"li\", ↓ Enter → Juliet; Escape closes");
        Ok(())
    };
    let mut result = attempt().await;
    for _ in 0..2 {
        let Err(error) = &result else { break };
        window.log(format_args!("failed ({error:#}): once more"));
        result = attempt().await;
    }
    let _ = window.update(|window, _| window.remove_window());
    result
}

// ── hotkeys-layout ──────────────────────────────────────────────────────────────────────────

/// What the key view saw.
#[derive(Default)]
struct Keys {
    focus: Option<gpui::FocusHandle>,
    seen: Vec<String>,
}

impl Render for Keys {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        div()
            .id("keys")
            .track_focus(&focus)
            .size_full()
            .bg(cx.theme().color_bg_primary)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, _| {
                this.seen.push(format!("down {:?} {:?}", event.keystroke.key, event.code));
            }))
            .on_key_up(cx.listener(|this, event: &gpui::KeyUpEvent, _, _| {
                this.seen.push(format!("up {:?} {:?}", event.keystroke.key, event.code));
            }))
            .on_modifiers_changed(cx.listener(|this, event: &gpui::ModifiersChangedEvent, _, _| {
                if let Some(key) = event.key {
                    this.seen.push(format!("modifier {:?} pressed={}", key.code, key.pressed));
                }
            }))
    }
}

const KEY_TEST_TITLE: &str = "Decibell key code test";

/// Sends key presses to one X window by its title with `XSendEvent` (python-xlib): the
/// events reach only that window, as X key events with these keycodes; nothing is typed
/// into the session and its keyboard layout is not touched.
const X_SEND_KEYS: &str = r#"
import sys
from Xlib import X, display, protocol
d = display.Display()
root = d.screen().root
def find(window):
    try:
        names = [window.get_wm_name(), window.get_full_text_property(d.intern_atom('_NET_WM_NAME'))]
    except Exception:
        names = []
    if sys.argv[1] in names:
        return window
    try:
        children = window.query_tree().children
    except Exception:
        return None
    for child in children:
        found = find(child)
        if found is not None:
            return found
window = find(root)
if window is None:
    sys.exit(3)
for keycode in map(int, sys.argv[2:]):
    for kind in (protocol.event.KeyPress, protocol.event.KeyRelease):
        event = kind(time=X.CurrentTime, root=root, window=window, same_screen=1, child=X.NONE,
                     root_x=0, root_y=0, event_x=1, event_y=1, state=0, detail=keycode)
        window.send_event(event, propagate=False, event_mask=X.KeyPressMask | X.KeyReleaseMask)
        d.sync()
"#;

async fn key_codes(cx: TestCx) -> Result<()> {
    use gpui::{KeyCode, Keystroke};
    let (handle, view) = cx.app().update(|cx| -> Result<(AnyWindowHandle, Entity<Keys>)> {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(420.), px(160.)), cx))),
            titlebar: Some(TitlebarOptions {
                title: Some(KEY_TEST_TITLE.into()),
                ..Default::default()
            }),
            app_id: Some("decibell".into()),
            ..Default::default()
        };
        let mut view = None;
        let window = cx.open_window(options, |window, cx| {
            let keys = cx.new(|_| Keys::default());
            view = Some(keys.clone());
            let _ = window;
            keys
        })?;
        Ok((window.into(), view.context("no view")?))
    })?;
    let window = TestCx::new(cx.app().clone(), handle);
    let result = async {
        window.settle().await?;
        window.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            if let Some(focus) = focus {
                focus.focus(window, cx);
            }
        })?;
        window.settle().await?;
        let take = || window.update(|_, cx| view.update(cx, |keys, _| std::mem::take(&mut keys.seen)));

        // 1. Through GPUI's dispatch: events as the Linux layer builds them under a Turkish
        //    layout (the character the layout types, the key's evdev code) reach listeners
        //    with the code intact, modifier keys included.
        let key = |typed: &str| Keystroke {
            modifiers: Modifiers::default(),
            key: typed.into(),
            key_char: Some(typed.into()),
        };
        window.modifier_key(KeyCode::Evdev(29), true, Modifiers::control())?;
        window.key_event(key("ş"), KeyCode::Evdev(39), true)?;
        window.key_event(key("ş"), KeyCode::Evdev(39), false)?;
        window.modifier_key(KeyCode::Evdev(29), false, Modifiers::default())?;
        window.key_event(key("ı"), KeyCode::Evdev(23), true)?;
        window.key_event(key("i"), KeyCode::Evdev(40), true)?;
        // A keystroke made from text (a test, a command palette) has no physical key.
        window.keystroke("a")?;
        let seen = take()?;
        let expected = [
            "modifier Evdev(29) pressed=true",
            "down \"ş\" Evdev(39)",
            "up \"ş\" Evdev(39)",
            "modifier Evdev(29) pressed=false",
            "down \"ı\" Evdev(23)",
            "down \"i\" Evdev(40)",
            "down \"a\" Unknown",
        ];
        ensure!(seen == expected, "the listeners saw {seen:#?}");
        window.log("dispatch: Ctrl + ş → Evdev(29) + Evdev(39) (ControlLeft + Semicolon); ı → Evdev(23) (KeyI); i → Evdev(40) (Quote); a simulated keystroke → Unknown");

        // 2. From the platform: X key events sent to this window alone. Keycode = evdev + 8.
        let on_x11 = std::env::var_os("WAYLAND_DISPLAY").is_none_or(|display| display.is_empty())
            && std::env::var_os("DISPLAY").is_some();
        if !on_x11 {
            window.log("Wayland session: no way to send this window a key event without typing into the session; the X11 path is checked with WAYLAND_DISPLAY unset, the Wayland path by reading (wl_keyboard.key carries the evdev code itself) and by hand (HANDOFF)");
            return Ok(());
        }
        // The key right of L (47), the key under Escape's row end (49 = Backquote), Left (113),
        // right Control (105): what they type depends on the session's layout, the code not.
        let keycodes = [47u16, 49, 113, 105];
        let sent = std::process::Command::new("python3")
            .args(["-I", "-c", X_SEND_KEYS, KEY_TEST_TITLE])
            .args(keycodes.iter().map(|keycode| keycode.to_string()))
            .status();
        match sent {
            Ok(status) if status.success() => {}
            Ok(status) => {
                window.log(format_args!("could not send X key events (python3 with python-xlib, {status}): the platform path was not run"));
                return Ok(());
            }
            Err(error) => {
                window.log(format_args!("could not run python3 ({error}): the platform path was not run"));
                return Ok(());
            }
        }
        window.sleep(Duration::from_millis(300)).await;
        let seen = take()?;
        window.log(format_args!("X key events for keycodes {keycodes:?} arrived as: {seen:?}"));
        for (keycode, name) in [(47, "Semicolon"), (49, "Backquote"), (113, "ArrowLeft")] {
            let code = format!("{:?}", KeyCode::Evdev(keycode - 8));
            ensure!(
                seen.iter().any(|line| line.starts_with("down") && line.ends_with(&code))
                    && seen.iter().any(|line| line.starts_with("up") && line.ends_with(&code)),
                "X keycode {keycode} ({name}) did not arrive as {code}"
            );
        }
        ensure!(
            seen.contains(&"modifier Evdev(97) pressed=true".to_string()) && seen.contains(&"modifier Evdev(97) pressed=false".to_string()),
            "the right Control key (keycode 105) did not arrive as a modifier key with Evdev(97)"
        );
        window.log("platform (X11): keycode − 8 is the evdev code on key down and up; the right Control arrives as a modifier key with its own code");
        Ok(())
    }
    .await;
    let _ = window.update(|window, _| window.remove_window());
    result
}
