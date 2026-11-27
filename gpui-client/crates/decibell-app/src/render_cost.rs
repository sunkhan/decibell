//! `--selftest render-cost`: what one change on screen costs, in cached views rendered and in
//! main-thread CPU time, twice: with clean cached views replayed inside a cached view that
//! renders (GPUI patch `0014`, `patches/README.md`) and as the pinned GPUI did it (every cached
//! view under the topmost dirty one renders: `gpui::set_nested_view_replay(false)`, or
//! `DECIBELL_VIEW_REPLAY=0` for the whole app).
//!
//! The cases are the ones where it matters: a speaking ring changing on a stage of twelve tiles
//! (and beside it the whole main layout: server bar, channel list, user panel), the same ring in
//! the channel list's voice roster while a text channel is open with its member list, a key typed
//! in a field under a message list, and a message arriving in that list.

use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use decibell_core::events::{CoreEvent, VoiceUserSpeakingPayload};
use decibell_harness::{Frames, TestCx};
use decibell_model::Model;
use decibell_text::input::InputState;
use decibell_ui::TextField;
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, Render, StyleRefinement, Window, div, prelude::*, px,
};

use crate::{registry, window};

/// The calling thread's CPU time.
fn thread_cpu() -> Duration {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // Safety: `ts` is a valid timespec to write to.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } == 0 {
            return Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32);
        }
    }
    Duration::ZERO
}

fn views() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (gpui::CACHED_VIEWS_RENDERED.load(Relaxed), gpui::CACHED_VIEWS_REPLAYED.load(Relaxed))
}

#[derive(Debug, Clone, Copy, Default)]
struct Cost {
    /// Per change, means over the rounds.
    rendered: f64,
    replayed: f64,
    frames: f64,
    /// Main-thread CPU per change, in microseconds: the event, the frame it draws (render,
    /// layout, prepaint, paint, the scene handed to the renderer) and nothing else (the
    /// thread sleeps between changes).
    cpu_us: f64,
}

/// Waits until the window has drawn nothing for 300 ms. (Presents alone do not count: a
/// speaking ring breathes by re-presenting a kept sprite, which renders no view.)
async fn settle(cx: &TestCx) -> Result<()> {
    let started = std::time::Instant::now();
    let mut last = cx.frames()?.drawn;
    let mut quiet_since = std::time::Instant::now();
    loop {
        cx.sleep(Duration::from_millis(50)).await;
        let now = cx.frames()?.drawn;
        if now != last {
            last = now;
            quiet_since = std::time::Instant::now();
        } else if quiet_since.elapsed() >= Duration::from_millis(300) {
            return Ok(());
        }
        ensure!(started.elapsed() < Duration::from_secs(15), "the window was still drawing after 15 s");
    }
}

/// `rounds` changes, one frame at a time.
async fn measure(cx: &TestCx, rounds: usize, change: &dyn Fn(usize, &mut Window, &mut App)) -> Result<Cost> {
    // Warm: glyphs, layouts and the first changes' one-off work are not what is measured.
    for round in 0..4 {
        cx.update(|window, cx| change(round, window, cx))?;
        cx.sleep(Duration::from_millis(60)).await;
    }
    settle(cx).await?;
    let mut total = Cost::default();
    let mut counted = 0usize;
    for round in 4..4 + rounds {
        let (frames, views_before, cpu) = (cx.frames()?, views(), thread_cpu());
        cx.update(|window, cx| change(round, window, cx))?;
        // The frame is drawn at the next frame callback; nothing else happens meanwhile.
        cx.sleep(Duration::from_millis(40)).await;
        let cpu = thread_cpu().saturating_sub(cpu);
        let (drawn, views_after): (Frames, _) = (cx.frames()?.since(frames), views());
        if drawn.drawn == 0 {
            continue;
        }
        counted += 1;
        total.rendered += (views_after.0 - views_before.0) as f64;
        total.replayed += (views_after.1 - views_before.1) as f64;
        total.frames += drawn.drawn as f64;
        total.cpu_us += cpu.as_secs_f64() * 1e6;
    }
    ensure!(counted * 2 >= rounds, "only {counted} of {rounds} changes drew a frame");
    let n = counted as f64;
    Ok(Cost { rendered: total.rendered / n, replayed: total.replayed / n, frames: total.frames / n, cpu_us: total.cpu_us / n })
}

/// One case measured both ways, alternating (so the machine's load of the moment falls on
/// both), and logged as a table row.
async fn case(cx: &TestCx, name: &str, rounds: usize, change: &dyn Fn(usize, &mut Window, &mut App)) -> Result<(Cost, Cost)> {
    let mut sums = [Cost::default(); 2];
    const PASSES: usize = 3;
    for _ in 0..PASSES {
        for (index, replay) in [false, true].into_iter().enumerate() {
            gpui::set_nested_view_replay(replay);
            cx.update(|window, _| window.refresh())?;
            settle(cx).await?;
            // Which case and which mode: the message alone does not say (the Windows runner
            // and Linux both lose a case here).
            let cost = measure(cx, rounds, change).await.with_context(|| {
                format!("{name}, {}", if index == 0 { "as GPUI was" } else { "with patch 0014" })
            })?;
            sums[index].rendered += cost.rendered / PASSES as f64;
            sums[index].replayed += cost.replayed / PASSES as f64;
            sums[index].frames += cost.frames / PASSES as f64;
            sums[index].cpu_us += cost.cpu_us / PASSES as f64;
        }
    }
    gpui::set_nested_view_replay(true);
    let [before, after] = sums;
    cx.log(format_args!(
        "{name}: per change, as GPUI was: {:.1} views rendered, {:.1} replayed, {:.0} µs of main thread ({:.2} frames); with 0014: {:.1} rendered, {:.1} replayed, {:.0} µs ({:.2} frames)",
        before.rendered, before.replayed, before.cpu_us, before.frames, after.rendered, after.replayed, after.cpu_us, after.frames,
    ));
    Ok((before, after))
}

fn speaking(uid: i64) -> impl Fn(usize, &mut Window, &mut App) {
    move |round, _, cx| {
        let model = Model::global(cx).clone();
        decibell_model::events::route(
            &model,
            CoreEvent::VoiceUserSpeaking(VoiceUserSpeakingPayload { uid, speaking: round % 2 == 0 }),
            cx,
        );
    }
}

/// A text field under a message list, both cached views of one cached pane: the chat pane's
/// shape (the composer is another crate's and lands later).
struct Pane {
    list: Entity<decibell_chat::MessageList>,
    field: Entity<FieldHost>,
}

struct FieldHost {
    input: Entity<InputState>,
}

impl Render for FieldHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().px_3().py_2().child(TextField::new("render-cost-field").editor(&self.input))
    }
}

impl Render for Pane {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(self.list.clone().cached(StyleRefinement::default().size_full())))
            .child(div().flex_none().h(px(60.)).child(self.field.clone().cached(StyleRefinement::default().size_full())))
    }
}

/// What builds a test window's content.
type Build = Box<dyn FnOnce(&mut Window, &mut App) -> gpui::AnyView>;

pub async fn run(cx: TestCx) -> Result<()> {
    let app = cx.app().clone();
    let open = |build: Build| -> Result<TestCx> {
        let handle = app.update(|cx| window::open_main(build, None, cx))?;
        Ok(TestCx::new(app.clone(), handle.into()))
    };
    let close = |test: &TestCx| test.update(|window, _| window.remove_window());
    let was = std::env::var("DECIBELL_VIEW_REPLAY").is_ok_and(|v| v == "0");

    // 1. Twelve people on the stage; one of them starts and stops talking.
    let stage = open(Box::new(registry::screen("voice-stage-12")?.build))?;
    settle(&stage).await?;
    let result = case(&stage, "a speaking ring on a 12-tile stage", 30, &speaking(2)).await;
    close(&stage)?;
    let (stage_before, stage_after) = result?;

    // 2. A text channel open with its member list; somebody talks in the channel list's roster.
    let server = open(Box::new(registry::screen("main-server")?.build))?;
    settle(&server).await?;
    let result = case(&server, "a speaking ring in the channel list (text channel open, members shown)", 30, &speaking(7)).await;
    close(&server)?;
    let (roster_before, roster_after) = result?;

    // 3. and 4. A field under the 10,000-message channel's list.
    let source = std::rc::Rc::new(std::cell::RefCell::new(None));
    let pane = open(Box::new({
        let source = source.clone();
        move |window, cx| {
            let fixture = decibell_chat::fixture::open(decibell_chat::fixture::Which::General, cx);
            *source.borrow_mut() = Some(fixture.clone());
            let list_source: std::rc::Rc<dyn decibell_chat::ListSource> = fixture;
            let list = cx.new(|cx| decibell_chat::MessageList::new(list_source, cx));
            let input = cx.new(|cx| InputState::single_line(window, cx));
            input.update(cx, |input, cx| input.focus(window, cx));
            let field = cx.new(|_| FieldHost { input });
            cx.new(|_| Pane { list, field }).into()
        }
    }))?;
    settle(&pane).await?;
    let typed = case(&pane, "a key typed in a field under the message list", 30, &|round, window, cx| {
        let key = if round % 8 == 7 { "backspace" } else { "a" };
        let keystroke = gpui::Keystroke::parse(key).expect("a key");
        let key_char = (key == "a").then(|| "a".to_string());
        window.dispatch_keystroke(gpui::Keystroke { key_char, ..keystroke }, cx);
    })
    .await;
    let fixture = source.borrow().clone().context("the fixture channel")?;
    let arrived = match typed {
        Ok(_) => {
            case(&pane, "a message arriving in the open channel (a field under it)", 20, &move |round, _, cx| {
                fixture.live_message(900_000 + rand_free_id(round), "a live message for the render-cost test", cx);
            })
            .await
        }
        Err(_) => Ok((Cost::default(), Cost::default())),
    };
    close(&pane)?;
    gpui::set_nested_view_replay(!was);
    let (typed_before, typed_after) = typed?;
    let (arrived_before, arrived_after) = arrived?;

    // What the patch is for: fewer views rendered in every case, never more.
    for (name, before, after) in [
        ("the stage", stage_before, stage_after),
        ("the channel list", roster_before, roster_after),
        ("typing", typed_before, typed_after),
        ("a message", arrived_before, arrived_after),
    ] {
        ensure!(
            after.rendered <= before.rendered,
            "{name}: {:.1} views rendered per change with nested replay, {:.1} without",
            after.rendered,
            before.rendered
        );
    }
    ensure!(
        stage_after.rendered < stage_before.rendered && typed_after.rendered < typed_before.rendered,
        "nested cached views are not replayed (stage {:.1} → {:.1}, typing {:.1} → {:.1} views per change)",
        stage_before.rendered,
        stage_after.rendered,
        typed_before.rendered,
        typed_after.rendered
    );
    Ok(())
}

/// Ids for the live messages, distinct across the passes of one run.
fn rand_free_id(round: usize) -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static NEXT: AtomicI64 = AtomicI64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed) * 1000 + round as i64
}
