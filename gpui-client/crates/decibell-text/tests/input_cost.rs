//! What typing costs: a 200-character message typed into a `TextArea` in a real (headless,
//! offscreen) GPUI window, counted per keystroke: allocations and time of the edit (key
//! dispatch → buffer → re-shaping the edited line) and of the frame that shows it, and what
//! a field holds in memory. Run with `--release -- --nocapture` for the numbers in the README;
//! the assertions hold in a debug build too.
//!
//! Needs a GPU (or a software Vulkan / GL driver): without one the test says so and passes.

#![cfg(any(target_os = "linux", target_os = "freebsd"))]

mod common;
#[path = "common/counting.rs"]
mod counting;

use std::sync::Arc;
use std::time::Instant;

use decibell_text::input::{InputState, InputStyle, TextArea};
use decibell_text::{Engine, FontSpec, TextStyle};
use gpui::{
    AnyWindowHandle, AppContext as _, Context, Entity, HeadlessAppContext, Hsla, IntoElement, Keystroke, Modifiers,
    ParentElement as _, Render, Styled as _, Window, div, px, size,
};

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting;

const MESSAGE: &str = "Selam! Yarınki toplantı için notları hazırladım: önce ses kanalındaki gecikmeyi konuşalım, \
    sonra yeni sürümün çıkış tarihini. Akşam altıda uygun musunuz, yoksa biraz daha geç mi olsun? Haber verin. :)";

struct Root {
    input: Entity<InputState>,
    style: InputStyle,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(560.)).p_4().child(TextArea::new(&self.input, self.style))
    }
}

#[derive(Default, Clone, Copy)]
struct Cost {
    allocations: u64,
    micros: f64,
}

fn measured<R>(act: impl FnOnce() -> R) -> (R, Cost) {
    let (before, _) = counting::counters();
    let started = Instant::now();
    let result = act();
    let cost = Cost {
        allocations: counting::counters().0 - before,
        micros: started.elapsed().as_secs_f64() * 1e6,
    };
    (result, cost)
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

#[test]
fn typing_a_message_costs_one_line_per_key() {
    assert_eq!(MESSAGE.chars().count(), 200);
    let platform = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Decibell Inter"));
    let mut cx = HeadlessAppContext::with_platform(platform, Arc::new(()), gpui_platform::current_headless_renderer);
    let fonts = std::fs::read_dir(common::assets().join("fonts/static"))
        .unwrap()
        .map(|entry| std::borrow::Cow::Owned(std::fs::read(entry.unwrap().path()).unwrap()))
        .collect();
    cx.update(|cx| cx.text_system().add_fonts(fonts)).unwrap();

    let mut input = None;
    let window = cx.open_window(size(px(900.), px(600.)), |window, cx| {
        let engine = Engine::global(cx);
        let face = engine.face(&FontSpec::new("Decibell Sans", 400));
        let grey = |l| Hsla { h: 0., s: 0., l, a: 1. };
        let mut text = TextStyle::new(face, 14.5, grey(0.9));
        text = text.leading(1.4);
        let style = InputStyle::new(text);
        let held = counting::counters().1;
        let state = cx.new(|cx| InputState::composer(window, cx).placeholder("Message #general"));
        println!("an empty field: {} bytes on the heap for its entity, focus handle and subscriptions; InputState is {} bytes", counting::counters().1 - held, size_of::<InputState>());
        state.update(cx, |state, cx| state.focus(window, cx));
        input = Some(state.clone());
        cx.new(|_| Root { input: state, style })
    });
    let window: AnyWindowHandle = match window {
        Ok(window) => window.into(),
        Err(error) => {
            eprintln!("skipped: no headless renderer here ({error:#})");
            return;
        }
    };
    let input = input.unwrap();
    cx.run_until_parked();
    if let Err(error) = cx.capture_screenshot(window) {
        eprintln!("skipped: the headless renderer cannot draw here ({error:#})");
        return;
    }

    // Type the message one key at a time: the edit, then the frame.
    let type_text = |cx: &mut HeadlessAppContext, text: &str, log: Option<&str>| -> (Vec<Cost>, Vec<Cost>) {
        let (mut edits, mut frames) = (Vec::new(), Vec::new());
        for character in text.chars() {
            let typed = character.to_string();
            let keystroke = Keystroke {
                modifiers: Modifiers::default(),
                key: if character == ' ' { "space".into() } else { typed.to_lowercase() },
                key_char: Some(typed),
            };
            // Both inside one update: leaving it flushes the effects, and the headless
            // window then draws once more by itself, which is not what is counted here.
            let (edit, frame) = cx
                .update_window(window, |_, window, cx| {
                    let (handled, edit) = measured(|| window.dispatch_keystroke(keystroke, cx));
                    assert!(handled, "{character:?} did not reach the field");
                    let (_, frame) = measured(|| window.draw(cx).clear(cx));
                    (edit, frame)
                })
                .unwrap();
            edits.push(edit);
            frames.push(frame);
        }
        if let Some(what) = log {
            let summary = |costs: &[Cost]| {
                let mut micros: Vec<f64> = costs.iter().map(|cost| cost.micros).collect();
                let mut allocations: Vec<f64> = costs.iter().map(|cost| cost.allocations as f64).collect();
                let worst = micros.iter().cloned().fold(0., f64::max);
                format!("{:.0} µs (max {:.0}), {:.0} allocations", median(&mut micros), worst, median(&mut allocations))
            };
            println!("{what}: per key, edit {}; frame {}", summary(&edits), summary(&frames));
        }
        (edits, frames)
    };

    // Warm the glyph atlas and the caches with the same text, then measure it typed again.
    type_text(&mut cx, MESSAGE, None);
    cx.update(|cx| input.update(cx, |state, cx| state.clear(cx)));
    let (edits, _) = type_text(&mut cx, MESSAGE, Some("typing a 200-character message"));
    let value = cx.update(|cx| input.read(cx).value().to_string());
    assert_eq!(value, MESSAGE);
    let stats = cx.update(|cx| input.read(cx).display_stats());
    assert_eq!((stats.shaped, stats.wrapped), (1, 0), "a key re-shaped more than its line: {stats:?}");
    let held = cx.update(|cx| input.read(cx).heap_bytes());
    println!("the field with the message typed: {held} bytes (text, undo history, paragraph and layout)");

    // The edit's allocations do not grow with the text's length within a line beyond what
    // shaping the line costs, and not at all with the lines above it.
    let first: f64 = edits[10..30].iter().map(|cost| cost.allocations as f64).sum::<f64>() / 20.;
    let last: f64 = edits[180..200].iter().map(|cost| cost.allocations as f64).sum::<f64>() / 20.;
    println!("allocations per edit: {first:.0} at 10-30 characters, {last:.0} at 180-200");

    // Forty lines above: a key in the last line costs what it cost in an empty field.
    cx.update(|cx| {
        input.update(cx, |state, cx| {
            let draft = format!("{}\n", MESSAGE).repeat(40);
            state.set_value(&draft, cx);
        })
    });
    cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    let (long_edits, _) = type_text(&mut cx, &MESSAGE[..MESSAGE.char_indices().nth(40).unwrap().0], Some("typing 40 characters under 40 such lines (8 KB)"));
    let stats = cx.update(|cx| input.read(cx).display_stats());
    assert_eq!((stats.shaped, stats.reused, stats.wrapped), (1, 40, 0), "{stats:?}");
    let short: f64 = edits[10..40].iter().map(|cost| cost.allocations as f64).sum::<f64>() / 30.;
    let long: f64 = long_edits[10..40].iter().map(|cost| cost.allocations as f64).sum::<f64>() / 30.;
    println!("allocations per edit in a short line: {short:.0} alone, {long:.0} under 8 KB of other lines");
    assert!(long < short * 1.5 + 8., "an edit's allocations grew with the text above it: {short:.0} → {long:.0}");
}
