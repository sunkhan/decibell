//! Painting from cached layouts: a full screen of message rows through GPUI's real renderer
//! (headless, offscreen) must allocate nothing once its glyphs are in the atlas.
//!
//! Needs a GPU (or a software Vulkan / GL driver): without one the test says so and passes.

#![cfg(any(target_os = "linux", target_os = "freebsd"))]

mod common;
#[path = "common/counting.rs"]
mod counting;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use decibell_richtext::{Document, ParseOptions, parse};
use decibell_text::{Engine, RichContent, RichLayout, RichPaint, RichPos, RichSelection, RichStyle};
use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, HeadlessAppContext, Hsla, IntoElement, Pixels, Render, Styled as _,
    Window, canvas, point, px, size,
};

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting;

const MESSAGES: &[&str] = &[
    "Plain text with **bold**, *italic*, __underline__, ~~strike~~ and `inline code`, then a link https://example.com/docs/voice to follow.",
    "Emoji inline 😀 in running text 🔥 and at the end 🎉",
    "A longer message that wraps over several lines at this width, so that a row is more than one line box high and the painter walks a few fragments for it, as most messages in a busy channel do.",
    "Inline math $x^2+\\frac{a}{b}$ and $\\sqrt{x^2+1}$ in a sentence.",
    "😀😂",
    "Selam, naber? Güncelleme sonrası ses kanalı çalışıyor mu: ığüşöç İĞÜŞÖÇ",
    "short",
    "`cargo build --release` then `npx napi build --platform` and run the tests again before the release goes out",
];

/// What one paint of the rows cost.
#[derive(Clone, Copy, Default)]
struct Cost {
    allocations: u64,
    micros: f64,
    paints: u32,
}

struct Rows {
    rows: Vec<Rc<RichLayout>>,
    cost: Rc<Cell<Cost>>,
}

impl Render for Rows {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.clone();
        let cost = self.cost.clone();
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window: &mut Window, cx: &mut App| {
                let grey = |l| Hsla { h: 0., s: 0., l, a: 1. };
                let paint = RichPaint {
                    selection: RichSelection::Range(RichPos { block: 0, offset: 4, ..RichPos::default() }, RichPos { block: 0, offset: 40, ..RichPos::default() }),
                    selection_background: grey(0.4),
                    selection_color: Some(grey(1.)),
                    hovered_link: None,
                };
                // Resolved before the measured stretch: the app does this once per frame too.
                std::hint::black_box(Engine::global(cx));
                let (before, _) = counting::counters();
                let started = Instant::now();
                let mut y = 0.;
                for row in &rows {
                    row.paint(point(bounds.origin.x + px(16.), bounds.origin.y + px(y)), &paint, window, cx);
                    y += (row.height() + 8.).ceil();
                }
                cost.set(Cost {
                    allocations: counting::counters().0 - before,
                    micros: started.elapsed().as_secs_f64() * 1e6,
                    paints: cost.get().paints + 1,
                });
            },
        )
        .size_full()
    }
}

#[test]
fn painting_cached_rows_allocates_nothing() {
    let platform = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Decibell Inter"));
    let mut cx = HeadlessAppContext::with_platform(platform, Arc::new(()), gpui_platform::current_headless_renderer);
    let fonts = std::fs::read_dir(common::assets().join("fonts/static"))
        .unwrap()
        .map(|entry| std::borrow::Cow::Owned(std::fs::read(entry.unwrap().path()).unwrap()))
        .collect();
    cx.update(|cx| cx.text_system().add_fonts(fonts)).unwrap();

    let documents: Vec<Document> = MESSAGES.iter().map(|source| parse(source, &ParseOptions::default())).collect();
    let cost = Rc::new(Cell::new(Cost::default()));
    let window = {
        let cost = cost.clone();
        cx.open_window(size(px(900.), px(1400.)), move |window, cx| {
            let engine = Engine::global(cx);
            let grey = |l| Hsla { h: 0., s: 0., l, a: 1. };
            let style = RichStyle {
                family: "Decibell Sans".into(),
                mono_family: "Decibell Mono".into(),
                size: 14.5,
                leading: 1.45,
                letter_spacing: -0.16,
                color: grey(0.9),
                link: grey(0.7),
                muted: grey(0.5),
                meta_size: 11.5,
                error: grey(0.6),
                code_background: grey(0.1),
                code_radius: 6.,
            };
            // 48 rows: more than a 1440-px-high window of messages shows.
            let rows = (0..48)
                .map(|index| {
                    let message = index % MESSAGES.len();
                    let content = RichContent {
                        document: &documents[message],
                        source: MESSAGES[message],
                        edited: index % 5 == 0,
                    };
                    engine.rich_layout(index as u64, content, &style, 560., window.scale_factor(), &mut |_, _, _| 0.)
                })
                .collect();
            cx.new(|_| Rows { rows, cost })
        })
    };
    let window: AnyWindowHandle = match window {
        Ok(window) => window.into(),
        Err(error) => {
            eprintln!("skipped: no headless renderer here ({error:#})");
            return;
        }
    };

    let mut last = Cost::default();
    for frame in 0..6 {
        cx.update_window(window, |_, window, _| window.refresh()).unwrap();
        cx.run_until_parked();
        if let Err(error) = cx.capture_screenshot(window) {
            eprintln!("skipped: the headless renderer cannot draw here ({error:#})");
            return;
        }
        last = cost.get();
        println!("frame {frame}: {} allocations, {:.0} µs to paint 48 rows", last.allocations, last.micros);
    }
    assert!(last.paints >= 3, "the rows were painted {} times", last.paints);
    assert_eq!(last.allocations, 0, "painting cached layouts allocated");
}
