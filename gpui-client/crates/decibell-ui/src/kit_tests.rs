//! The kit's self-tests: each opens its own window (as the app's main window is opened) with a
//! kit screen or a small fixture view, drives it with real input, counts frames, and closes it.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{Frames, SelfTest, TestCx, perf};
use gpui::{
    AnyView, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity,
    FocusHandle, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, Pixels, Point, Render,
    ScrollDelta, ScrollEvent, StyleRefinement, TitlebarOptions, TouchPhase, Window,
    WindowBounds, WindowDecorations, WindowOptions, div, point, prelude::*, px, size,
};

use crate::kit::{KitForms, KitScroll};
use crate::modal::CloseRequest;
use crate::scroll::{ScrollAnimator, ScrollKind};
use crate::{
    ActiveTheme as _, FontRole, Modal, SaveBar, ScrollArea, ScrollHandle, Select, SelectHandle,
    SelectOption, TITLEBAR_HEIGHT, TitleBar, Toast, ToastStack, Toasts, dialog_card,
    outer_window_size, visible_window_bounds, window_frame,
};

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "kit-idle",
            about: "every kit screen settles to 0 frames drawn and presented",
            run: |cx| Box::pin(kit_idle(cx)),
        },
        SelfTest {
            name: "kit-switch",
            about: "a click toggles a switch: the 150 ms transition's frames, then idle",
            run: |cx| Box::pin(kit_switch(cx)),
        },
        SelfTest {
            name: "kit-select",
            about: "select: open by click and key, navigate, typeahead, choose, Escape; hover doesn't scroll",
            run: |cx| Box::pin(kit_select(cx)),
        },
        SelfTest {
            name: "kit-modal",
            about: "modal: focus trapped, a vetoed Escape shakes the save bar, closes, focus restored, stacking",
            run: |cx| Box::pin(kit_modal(cx)),
        },
        SelfTest {
            name: "kit-scroll",
            about: "wheel notches, keys and precise deltas follow the Chromium model frame by frame; idle at rest",
            run: |cx| Box::pin(kit_scroll(cx)),
        },
        SelfTest {
            name: "kit-toast",
            about: "a toast appears, its timer dismisses it, and no timer or frame is left",
            run: |cx| Box::pin(kit_toast(cx)),
        },
    ]
}

// ── the test window ─────────────────────────────────────────────────────────────────────────

/// What a kit test window draws: the window frame, the title bar, the content (cached, as the
/// app's root draws its content) and the toast stack.
struct KitRoot {
    content: AnyView,
}

impl Render for KitRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let content = crate::focus_navigation(div())
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme.font(FontRole::Sans))
            .text_size(px(theme.scaled.text_body))
            .text_color(theme.color_text_primary)
            .child(TitleBar::new("Decibell kit test"))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.content.clone().cached(StyleRefinement::default().size_full())),
            )
            .child(ToastStack::new());
        window_frame(content, window, cx)
    }
}

/// A window like the main one (1280 × 800 visible, client-decorated) showing `build`'s view;
/// returns a test context on it and the view.
fn open(cx: &TestCx, build: impl FnOnce(&mut Window, &mut App) -> AnyView + 'static) -> Result<(TestCx, AnyView)> {
    let (handle, view) = cx.app().update(|cx| -> Result<(AnyWindowHandle, AnyView)> {
        let bounds = Bounds::centered(None, outer_window_size(size(px(1280.), px(800.))), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(800.), px(600.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Decibell kit test".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some("decibell".into()),
            inactive_frame_interval: None,
            ..Default::default()
        };
        let mut view = None;
        let window = cx.open_window(options, |window, cx| {
            let content = build(window, cx);
            view = Some(content.clone());
            cx.new(|_| KitRoot { content })
        })?;
        Ok((window.into(), view.context("no view")?))
    })?;
    let test = TestCx::new(cx.app().clone(), handle);
    Ok((test, view))
}

fn close(cx: &TestCx) {
    let _ = cx.update(|window, _| window.remove_window());
}

/// The top-left of the content (under the title bar) in window coordinates.
fn content_origin(cx: &TestCx) -> Result<Point<Pixels>> {
    cx.update(|window, cx| visible_window_bounds(window, cx).origin + point(px(0.), TITLEBAR_HEIGHT))
}

fn input(cx: &TestCx, event: PlatformInput) -> Result<()> {
    cx.update(|window, cx| {
        window.dispatch_event(event, cx);
    })
}

/// A key pressed and released (GPUI activates a focused control on the release).
fn key(cx: &TestCx, keystroke: &'static str) -> Result<()> {
    let keystroke = Keystroke::parse(keystroke)?.with_simulated_ime();
    input(
        cx,
        PlatformInput::KeyDown(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
            code: gpui::KeyCode::Unknown,
        }),
    )?;
    input(cx, PlatformInput::KeyUp(KeyUpEvent {
            keystroke,
            code: gpui::KeyCode::Unknown,
        }))
}

fn mouse_move(cx: &TestCx, position: Point<Pixels>) -> Result<()> {
    input(
        cx,
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        }),
    )
}

fn click(cx: &TestCx, position: Point<Pixels>) -> Result<()> {
    mouse_move(cx, position)?;
    input(
        cx,
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }),
    )?;
    input(
        cx,
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        }),
    )
}

fn wheel(cx: &TestCx, position: Point<Pixels>, delta: ScrollDelta) -> Result<()> {
    input(
        cx,
        PlatformInput::Scroll(ScrollEvent {
            position,
            delta,
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
            source: Default::default(),
        }),
    )
}

/// Waits for drawing to stop, then checks that nothing is drawn or presented for `window`.
async fn assert_idle(cx: &TestCx, window: Duration, what: &str) -> Result<Frames> {
    let mut last = None;
    for _ in 0..3 {
        let before = cx.settle().await?;
        cx.sleep(window).await;
        let frames = cx.frames()?.since(before);
        if frames.drawn == 0 && frames.presented == 0 {
            return Ok(frames);
        }
        last = Some(frames);
    }
    bail!("{what}: still drawing at rest ({last:?})")
}

/// Polls the frame counters every 2 ms until nothing has been drawn or presented for 250 ms;
/// returns the frames drawn and when the last one was drawn (after `since`). Presents can run on
/// after the last draw (GPUI keeps presenting for 1 s after a burst of input), which is why the
/// draws are what is timed.
async fn frames_until_quiet(cx: &TestCx, since: Instant) -> Result<(u64, Duration)> {
    let start = cx.frames()?;
    let mut last = start;
    let mut last_draw = since;
    let mut last_change = since;
    loop {
        cx.sleep(Duration::from_millis(2)).await;
        let now = cx.frames()?;
        if now != last {
            if now.drawn != last.drawn {
                last_draw = Instant::now();
            }
            last = now;
            last_change = Instant::now();
        } else if last_change.elapsed() > Duration::from_millis(250) {
            break;
        }
        ensure!(since.elapsed() < Duration::from_secs(10), "still drawing after 10 s");
    }
    Ok((last.since(start).drawn, last_draw - since))
}

// ── kit-idle ────────────────────────────────────────────────────────────────────────────────

async fn kit_idle(cx: TestCx) -> Result<()> {
    for screen in crate::kit::screens() {
        let build = screen.build;
        let (window, _) = open(&cx, build)?;
        // Entrances (the toasts', the menus') run first; then nothing may draw. kit-buttons shows
        // spinners: they turn as scene animations, presented (≤ 60/s) without drawing.
        let spinners = screen.name == "kit-buttons";
        let result = if spinners {
            spinning_idle(&window).await
        } else {
            assert_idle(&window, Duration::from_secs(2), screen.name).await.map(|_| String::new())
        };
        close(&window);
        cx.update(|_, cx| Toasts::dismiss_all(cx))?;
        let note = result?;
        cx.log(format_args!("{}: 0 frames drawn over 2 s at rest{note}", screen.name));
    }
    Ok(())
}

/// A screen with spinners: after the entrances, no frame is drawn (no view re-renders) and the
/// spinners' presents stay at or under 60 a second.
async fn spinning_idle(cx: &TestCx) -> Result<String> {
    cx.sleep(Duration::from_millis(800)).await;
    let before = cx.frames()?;
    cx.sleep(Duration::from_secs(2)).await;
    let frames = cx.frames()?.since(before);
    ensure!(frames.drawn == 0, "{} frames drawn at rest", frames.drawn);
    ensure!(frames.presented <= 2 * 62, "the spinners presented {} times in 2 s", frames.presented);
    Ok(format!(", {} presents in 2 s (the spinners' scene animation)", frames.presented))
}

// ── kit-switch ──────────────────────────────────────────────────────────────────────────────

async fn kit_switch(cx: TestCx) -> Result<()> {
    let (window, view) = open(&cx, |_, cx| cx.new(|_| KitForms::fixture()).into())?;
    let result = async {
        let forms: Entity<KitForms> = view.downcast().map_err(|_| anyhow::anyhow!("not the forms view"))?;
        assert_idle(&window, Duration::from_millis(500), "kit-forms").await?;
        // kit-forms: p-6, a 18 px section title, gap-2: the first switch's 40 × 22 box at (24, 50).
        let at = content_origin(&window)? + point(px(44.), px(61.));
        for expected in [true, false] {
            let clicked = Instant::now();
            click(&window, at)?;
            let (frames, last) = frames_until_quiet(&window, clicked).await?;
            let on = window.update(|_, cx| forms.read(cx).switch(0))?;
            ensure!(on == expected, "the click did not toggle the switch");
            window.log(format_args!(
                "toggled {}: {frames} frames, the last {:.0} ms after the click",
                if on { "on" } else { "off" },
                last.as_secs_f64() * 1e3
            ));
            ensure!(
                (120..=320).contains(&(last.as_millis() as u64)),
                "the transition's frames ended {:.0} ms after the click, expected ~150",
                last.as_secs_f64() * 1e3
            );
            assert_idle(&window, Duration::from_secs(1), "after the toggle").await?;
        }
        Ok(())
    }
    .await;
    close(&window);
    result
}

// ── kit-select ──────────────────────────────────────────────────────────────────────────────

const LETTERS: [&str; 10] = ["Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India", "Juliet"];

struct SelectFixture {
    handle: SelectHandle,
    value: usize,
    escapes_below: usize,
}

impl Render for SelectFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let options = LETTERS
            .iter()
            .enumerate()
            .map(|(i, label)| SelectOption::new(i, *label))
            .collect();
        div()
            .id("select-fixture")
            .size_full()
            .bg(cx.theme().color_bg_primary)
            // Escape must not reach what is under an open select (a modal).
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.escapes_below += 1;
                    cx.notify();
                }
            }))
            .child(
                div().absolute().left(px(40.)).top(px(40.)).w(px(240.)).child(
                    Select::options("letters", Some(self.value), options)
                        .track(&self.handle)
                        .on_change(cx.listener(|this, value: &usize, _, cx| {
                            this.value = *value;
                            cx.notify();
                        })),
                ),
            )
    }
}

async fn kit_select(cx: TestCx) -> Result<()> {
    let (window, view) = open(&cx, |_, cx| {
        let handle = SelectHandle::new(cx);
        cx.new(|_| SelectFixture {
            handle,
            value: 0,
            escapes_below: 0,
        })
        .into()
    })?;
    let result = async {
        let fixture: Entity<SelectFixture> = view.downcast().map_err(|_| anyhow::anyhow!("fixture"))?;
        let read = |window: &TestCx| {
            window.update(|_, cx| {
                let f = fixture.read(cx);
                (f.handle.is_open(cx), f.handle.active(cx), f.value, f.handle.list_offset(cx), f.escapes_below)
            })
        };
        assert_idle(&window, Duration::from_millis(500), "select fixture").await?;
        let origin = content_origin(&window)?;
        let trigger = origin + point(px(160.), px(60.));

        // Open by click: the selected option is active.
        click(&window, trigger)?;
        window.settle().await?;
        let (open, active, ..) = read(&window)?;
        ensure!(open && active == Some(0), "a click did not open it on the selected option ({open}, {active:?})");
        // Arrows move, typing jumps (h → Hotel), Enter chooses and closes.
        key(&window, "down")?;
        key(&window, "down")?;
        ensure!(read(&window)?.1 == Some(2), "↓↓ did not move to Charlie");
        key(&window, "h")?;
        ensure!(read(&window)?.1 == Some(7), "typing h did not jump to Hotel");
        key(&window, "enter")?;
        window.settle().await?;
        let (open, _, value, ..) = read(&window)?;
        ensure!(!open && value == 7, "Enter did not choose Hotel and close ({open}, {value})");
        window.log("click opens; ↓ ↓ h Enter chooses Hotel");

        // Open by keyboard (focus stayed on the trigger): the menu scrolls Hotel into the middle.
        key(&window, "down")?;
        window.settle().await?;
        let (open, active, _, offset, _) = read(&window)?;
        ensure!(open && active == Some(7), "↓ on the trigger did not open it on Hotel");
        ensure!(offset > px(0.), "the list was not scrolled to the selected option");
        // Escape closes it without reaching the view underneath.
        let escapes = read(&window)?.4;
        key(&window, "escape")?;
        window.settle().await?;
        let (open, .., escapes_after) = read(&window)?;
        ensure!(!open && escapes_after == escapes, "Escape did not close it, or leaked ({open}, {escapes_after})");
        window.log(format_args!("↓ opens centred on Hotel (list at {:.1} px); Escape closes and stays in the select", f32::from(offset)));

        // Hover: the pointer makes a row active but never scrolls the list (Select.tsx activeBy).
        click(&window, trigger)?;
        window.settle().await?;
        let offset_before = read(&window)?.3;
        // The menu's last visible pixels: 4 px under the trigger, 320 px tall.
        let menu_top = origin.y + px(40. + 40. + 4.);
        for dy in [300., 310., 316.] {
            mouse_move(&window, point(origin.x + px(100.), menu_top + px(dy)))?;
            window.settle().await?;
        }
        let (open, active, _, offset_after, _) = read(&window)?;
        ensure!(open, "hovering closed the menu");
        ensure!(offset_after == offset_before, "hover scrolled the list ({offset_before:?} → {offset_after:?})");
        window.log(format_args!("hover moved the active row to {active:?}; the list stayed at {:.1} px", f32::from(offset_after)));
        key(&window, "escape")?;
        assert_idle(&window, Duration::from_secs(1), "select at rest").await?;
        Ok(())
    }
    .await;
    close(&window);
    result
}

// ── kit-modal ───────────────────────────────────────────────────────────────────────────────

struct ModalFixture {
    open: bool,
    inner_open: bool,
    dirty: bool,
    nudge: u32,
    scroll: ScrollHandle,
    outside: FocusHandle,
    inside: [FocusHandle; 3],
}

impl Render for ModalFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let field = |handle: &FocusHandle| {
            div()
                .track_focus(handle)
                .h_8()
                .w_full()
                .rounded(theme.radius_sm)
                .bg(theme.color_bg_lighter)
        };
        div()
            .id("modal-fixture")
            .size_full()
            .p_6()
            .bg(theme.color_bg_primary)
            .child(field(&self.outside).w(px(200.)))
            .child(
                Modal::new("outer", self.open)
                    .on_close(cx.listener(|this, _: &CloseRequest, _, cx| {
                        // Unsaved changes hold it: shake the save bar instead.
                        if this.dirty {
                            this.nudge += 1;
                        } else {
                            this.open = false;
                        }
                        cx.notify();
                    }))
                    .child(
                        dialog_card(400., cx)
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(self.inside.iter().map(field))
                            .child(
                                ScrollArea::new("bar-scroll").track_scroll(&self.scroll).h(px(120.)).child(
                                    SaveBar::new("bar", &self.scroll, true, self.nudge),
                                ),
                            )
                            .child(
                                Modal::new("inner", self.inner_open)
                                    .on_close(cx.listener(|this, _: &CloseRequest, _, cx| {
                                        this.inner_open = false;
                                        cx.notify();
                                    }))
                                    .child(dialog_card(300., cx).h_24()),
                            ),
                    ),
            )
    }
}

async fn kit_modal(cx: TestCx) -> Result<()> {
    let (window, view) = open(&cx, |_, cx| {
        let inside = [cx.focus_handle().tab_stop(true), cx.focus_handle().tab_stop(true), cx.focus_handle().tab_stop(true)];
        let outside = cx.focus_handle().tab_stop(true);
        cx.new(|_| ModalFixture {
            open: false,
            inner_open: false,
            dirty: false,
            nudge: 0,
            scroll: ScrollHandle::new(),
            outside,
            inside,
        })
        .into()
    })?;
    let result = async {
        let fixture: Entity<ModalFixture> = view.downcast().map_err(|_| anyhow::anyhow!("fixture"))?;
        assert_idle(&window, Duration::from_millis(500), "modal fixture").await?;
        // Focus outside, then open: focus moves in.
        window.update(|window, cx| {
            let outside = fixture.read(cx).outside.clone();
            outside.focus(window, cx);
            fixture.update(cx, |f, cx| {
                f.open = true;
                cx.notify();
            });
        })?;
        window.settle().await?;
        let focus_inside = |window: &TestCx| {
            window.update(|window, cx| {
                let f = fixture.read(cx);
                (f.inside.iter().position(|h| h.is_focused(window)), f.outside.is_focused(window))
            })
        };
        ensure!(!focus_inside(&window)?.1, "focus stayed outside the open modal");
        // Tab cycles inside, never out (8 presses over 3 fields).
        let mut visited = Vec::new();
        for _ in 0..8 {
            key(&window, "tab")?;
            let (inside, outside) = focus_inside(&window)?;
            ensure!(!outside, "Tab left the modal");
            visited.push(inside);
        }
        // The fields, and the save bar's two buttons (also inside): every field is reached.
        ensure!((0..3).all(|i| visited.contains(&Some(i))), "Tab did not reach every field: {visited:?}");
        window.log(format_args!("Tab ×8 stayed inside the modal (fields {visited:?}; None = its save bar's buttons)"));

        // A vetoed Escape: the bar shakes (360 ms), the modal stays.
        window.update(|_, cx| fixture.update(cx, |f, _| f.dirty = true))?;
        let pressed = Instant::now();
        key(&window, "escape")?;
        let (frames, last) = frames_until_quiet(&window, pressed).await?;
        let (open, nudge) = window.update(|_, cx| (fixture.read(cx).open, fixture.read(cx).nudge))?;
        ensure!(open && nudge == 1, "Escape with unsaved changes closed it, or didn't nudge ({open}, {nudge})");
        ensure!(last >= Duration::from_millis(300), "the shake ended after {:.0} ms, expected 360", last.as_secs_f64() * 1e3);
        window.log(format_args!("vetoed Escape: shake {frames} frames over {:.0} ms", last.as_secs_f64() * 1e3));
        assert_idle(&window, Duration::from_secs(1), "after the shake").await?;

        // Stacking: a modal opened inside takes Escape first.
        window.update(|_, cx| fixture.update(cx, |f, cx| { f.dirty = false; f.inner_open = true; cx.notify(); }))?;
        window.settle().await?;
        key(&window, "escape")?;
        window.settle().await?;
        let (outer, inner) = window.update(|_, cx| (fixture.read(cx).open, fixture.read(cx).inner_open))?;
        ensure!(outer && !inner, "Escape over two modals did not close only the top one ({outer}, {inner})");
        window.log("two modals: Escape closed the top one only");

        // Closing: it fades out (300 ms) and focus goes back outside.
        let pressed = Instant::now();
        key(&window, "escape")?;
        let (frames, last) = frames_until_quiet(&window, pressed).await?;
        let (open, outside) = (window.update(|_, cx| fixture.read(cx).open)?, focus_inside(&window)?.1);
        ensure!(!open, "Escape did not close it");
        ensure!(outside, "focus did not go back to where it was");
        window.log(format_args!("closed: fade-out {frames} frames over {:.0} ms; focus restored", last.as_secs_f64() * 1e3));
        assert_idle(&window, Duration::from_secs(1), "modal closed").await?;
        Ok(())
    }
    .await;
    close(&window);
    result
}

// ── kit-scroll ──────────────────────────────────────────────────────────────────────────────

struct ScrollFixture {
    handle: ScrollHandle,
}

impl Render for ScrollFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let stripe = theme.color_bg_secondary;
        div().size_full().bg(theme.color_bg_primary).child(
            div().absolute().left(px(40.)).top(px(40.)).w(px(300.)).h(px(400.)).child(
                ScrollArea::new("scroll-fixture").track_scroll(&self.handle).size_full().children(
                    (0..40).map(|i| div().h_8().flex_none().when(i % 2 == 0, |this| this.bg(stripe))),
                ),
            ),
        )
    }
}

/// A scroll input: (px, + = down; ms after the first; kind).
type Input = (f64, u64, ScrollKind);

/// Feeds `inputs` (px, time, kind) to the area and checks every frame it draws against the
/// reference model fed the same inputs. The area stamps an input when it handles it, somewhere
/// between the test's clock readings just before and just after dispatching it, so the model runs
/// twice (inputs at the earlier and at the later readings) and each frame must lie between the
/// two (± 0.05 px). Returns (frames compared, the largest bracket seen, the end position).
async fn follow(
    window: &TestCx,
    handle: &ScrollHandle,
    pointer: Point<Pixels>,
    start: f64,
    max: f64,
    inputs: &[Input],
) -> Result<(usize, f64, f64)> {
    let reference = || {
        let mut model = ScrollAnimator::new();
        model.set_max_offset(max);
        model.jump_to(start);
        model
    };
    let (mut early, mut late) = (reference(), reference());
    let t0 = Instant::now();
    let mut pending = inputs.iter().peekable();
    let mut samples = 0;
    let mut widest: f64 = 0.;
    let mut last_seen = handle.last_frame().1;
    loop {
        while let Some(&&(delta, at, kind)) = pending.peek() {
            if t0.elapsed() < Duration::from_millis(at) {
                break;
            }
            let before = Instant::now();
            match kind {
                ScrollKind::Wheel => wheel(window, pointer, ScrollDelta::Lines(point(0., -(delta / crate::scroll::PX_PER_GPUI_LINE) as f32)))?,
                ScrollKind::Key => key(window, if delta > 0. { if delta > 40. { "pagedown" } else { "down" } } else if delta < -40. { "pageup" } else { "up" })?,
            }
            let after = Instant::now();
            early.input(before, delta, kind);
            late.input(after, delta, kind);
            pending.next();
        }
        window.sleep(Duration::from_millis(1)).await;
        let (drawn, at) = handle.last_frame();
        if at != last_seen
            && let Some(at) = at
        {
            last_seen = Some(at);
            let (a, b) = (early.position(at), late.position(at));
            let (lo, hi) = (a.min(b) - 0.05, a.max(b) + 0.05);
            ensure!(
                (lo..=hi).contains(&drawn),
                "frame at {:.1} ms: {drawn:.2} px, the model says {a:.2}–{b:.2}",
                at.saturating_duration_since(t0).as_secs_f64() * 1e3
            );
            widest = widest.max(hi - lo - 0.1);
            samples += 1;
        }
        if pending.peek().is_none() && !handle.is_animating() && t0.elapsed() > Duration::from_millis(50) {
            break;
        }
        ensure!(t0.elapsed() < Duration::from_secs(5), "the animation did not end");
    }
    Ok((samples, widest, handle.last_frame().0))
}

async fn kit_scroll(cx: TestCx) -> Result<()> {
    let handle = ScrollHandle::new();
    let (window, _) = open(&cx, {
        let handle = handle.clone();
        move |_, cx| cx.new(|_| ScrollFixture { handle }).into()
    })?;
    let result = async {
        assert_idle(&window, Duration::from_millis(500), "scroll fixture").await?;
        let origin = content_origin(&window)?;
        let pointer = origin + point(px(150.), px(200.));
        mouse_move(&window, pointer)?;
        window.settle().await?;
        let max = 40. * 32. - 400.;
        let cases: [(&str, Vec<Input>); 4] = [
            ("W1 one notch", vec![(120., 0, ScrollKind::Wheel)]),
            ("W3 two notches 60 ms apart", vec![(120., 0, ScrollKind::Wheel), (120., 60, ScrollKind::Wheel)]),
            ("W6 reversal", vec![(120., 0, ScrollKind::Wheel), (-120., 60, ScrollKind::Wheel)]),
            ("W4 three notches 30 ms apart", vec![(120., 0, ScrollKind::Wheel), (120., 30, ScrollKind::Wheel), (120., 60, ScrollKind::Wheel)]),
        ];
        for (name, inputs) in cases {
            let start = handle.last_frame().0;
            let target: f64 = (start + inputs.iter().map(|i| i.0).sum::<f64>()).clamp(0., max);
            let before = window.frames()?;
            let (samples, bracket, end) = follow(&window, &handle, pointer, start, max, &inputs).await?;
            let frames = window.frames()?.since(before).drawn;
            window.log(format_args!("{name}: {samples} frames on the model (input-time bracket ≤ {bracket:.2} px), ended at {end:.0} (expected {target:.0}), {frames} drawn"));
            ensure!((end - target).abs() < 0.01, "{name}: ended at {end}, expected {target}");
            assert_idle(&window, Duration::from_millis(500), name).await?;
        }

        // Keys scroll once the area has focus (a click inside it gives it).
        click(&window, pointer)?;
        window.settle().await?;
        for (name, inputs) in [
            ("K1 ArrowDown", vec![(40., 0, ScrollKind::Key)]),
            ("K2 PageDown (⌊0.875 × 400⌋ = 350)", vec![(350., 0, ScrollKind::Key)]),
        ] {
            let start = handle.last_frame().0;
            let target = (start + inputs[0].0).clamp(0., max);
            let (samples, bracket, end) = follow(&window, &handle, pointer, start, max, &inputs).await?;
            window.log(format_args!("{name}: {samples} frames on the model (input-time bracket ≤ {bracket:.2} px), ended at {end:.0}"));
            ensure!((end - target).abs() < 0.01, "{name}: ended at {end}, expected {target}");
        }

        // A precise (touchpad) delta moves at once, without an animation (+50: 50 px up).
        let start = handle.last_frame().0;
        let before = window.frames()?;
        wheel(&window, pointer, ScrollDelta::Pixels(point(px(0.), px(50.))))?;
        let after = window.settle().await?;
        ensure!(!handle.is_animating(), "a precise delta animated");
        let moved = handle.last_frame().0 - start;
        ensure!((moved + 50.).abs() < 0.01, "a precise 50 px delta moved {moved}");
        window.log(format_args!("precise 50 px: moved {moved:.0} px at once, {} frames drawn", after.since(before).drawn));

        // At the end, a notch further down moves nothing and draws nothing.
        key(&window, "end")?;
        window.settle().await?;
        ensure!((handle.last_frame().0 - max).abs() < 0.01, "End did not reach the bottom");
        let before = window.settle().await?;
        wheel(&window, pointer, ScrollDelta::Lines(point(0., -3.)))?;
        window.sleep(Duration::from_millis(300)).await;
        let frames = window.frames()?.since(before);
        ensure!(frames.drawn == 0, "a notch past the end drew {} frames", frames.drawn);
        window.log("at the end: a notch draws nothing");
        assert_idle(&window, Duration::from_secs(1), "scroll at rest").await?;
        Ok(())
    }
    .await;
    close(&window);
    // The kit-scroll screen must also come to rest (its areas start scrolled).
    let (window, _) = open(&cx, |_, cx| cx.new(|_| KitScroll::fixture()).into())?;
    let rest = assert_idle(&window, Duration::from_secs(1), "kit-scroll").await;
    close(&window);
    result.and(rest.map(|_| ()))
}

// ── kit-toast ───────────────────────────────────────────────────────────────────────────────

struct Empty;

impl Render for Empty {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(cx.theme().color_bg_primary)
    }
}

async fn kit_toast(cx: TestCx) -> Result<()> {
    let (window, _) = open(&cx, |_, cx| cx.new(|_| Empty).into())?;
    let result = async {
        assert_idle(&window, Duration::from_millis(500), "empty").await?;
        let pushed = Instant::now();
        window.update(|_, cx| {
            Toasts::push(cx, Toast::success("Saved").duration(Duration::from_millis(600)));
        })?;
        let (frames, last) = frames_until_quiet(&window, pushed).await?;
        let shown = window.update(|_, cx| Toasts::count(cx))?;
        ensure!(shown == 1, "the toast was not shown ({shown})");
        window.log(format_args!("shown: entrance {frames} frames over {:.0} ms", last.as_secs_f64() * 1e3));
        window.sleep(Duration::from_millis(800)).await;
        let left = window.update(|_, cx| Toasts::count(cx))?;
        ensure!(left == 0, "the timer did not dismiss it");
        // No toast, so no timer: the main thread sleeps and nothing draws.
        let before = window.settle().await?;
        let wakeups_before = perf::thread_wakeups();
        window.sleep(Duration::from_secs(2)).await;
        let frames = window.frames()?.since(before);
        let wakeups = perf::thread_wakeups().zip(wakeups_before).map(|(a, b)| a - b);
        window.log(format_args!(
            "dismissed by its timer; then {} drawn, {} presented, {} main-thread wakeups in 2 s",
            frames.drawn,
            frames.presented,
            wakeups.map_or("?".into(), |count| count.to_string())
        ));
        ensure!(frames.drawn == 0 && frames.presented == 0, "drawing after the toast left");
        ensure!(wakeups.is_none_or(|count| count <= 5), "the main thread kept waking ({wakeups:?})");
        Ok(())
    }
    .await;
    close(&window);
    cx.update(|_, cx| Toasts::dismiss_all(cx))?;
    result
}
