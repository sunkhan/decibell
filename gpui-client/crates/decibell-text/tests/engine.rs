//! The engine without a window: layout rules, positions, hit-testing, the caches, the emoji
//! pack. GPUI's real Linux text system with the bundled fonts.

#![cfg(any(target_os = "linux", target_os = "freebsd"))]

mod common;

use std::rc::Rc;

use decibell_richtext::{ParseOptions, parse};
use decibell_text::{
    Affinity, Engine, FontSpec, Layout, LineHeight, ParagraphBuilder, Pos, RichContent, RichPos, RichSelection,
    RichStyle, SlotKind, TextStyle, Wrapping,
};
use gpui::Hsla;

fn grey(l: f32) -> Hsla {
    Hsla { h: 0., s: 0., l, a: 1. }
}

fn body(engine: &Engine) -> TextStyle {
    let face = engine.face(&FontSpec::new("Decibell Sans", 400));
    TextStyle::new(face, 14.5, grey(0.9)).leading(1.45).letter_spacing(-0.16)
}

fn layout(engine: &Engine, text: &str, wrapping: Wrapping, width: f32) -> Layout {
    let style = body(engine);
    let mut builder = ParagraphBuilder::new(engine, &style, wrapping, 1.);
    builder.text(text, &style, 7);
    Layout::new(Rc::new(builder.build()), width, engine)
}

fn rich_style() -> RichStyle {
    RichStyle {
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
    }
}

#[test]
fn line_boxes_are_chromiums() {
    let engine = common::engine();
    // 14.5 px × 1.45 = 21.025 px, truncated to 1/64 px: 21.015625 per line.
    let one = layout(&engine, "one line", Wrapping::MESSAGE, 560.);
    assert_eq!(one.line_count(), 1);
    assert_eq!(one.height(), 21.015625);
    // Mona Sans at 14.5 px: ascent 15.8 → 16; no leading above it.
    assert_eq!(one.first_baseline(), 16.);
    let three = layout(&engine, "a\n\nb", Wrapping::MESSAGE, 560.);
    assert_eq!(three.line_count(), 3, "an empty line between two breaks is a line");
    assert_eq!(three.height(), 3. * 21.015625);
    let trailing = layout(&engine, "a\n", Wrapping::MESSAGE, 560.);
    assert_eq!(trailing.line_count(), 1, "a break at the very end opens no line");
    assert_eq!(layout(&engine, "", Wrapping::MESSAGE, 560.).height(), 0.);
    // A px line height is used as given.
    let style = body(&engine).line_height_px(30.);
    let mut builder = ParagraphBuilder::new(&engine, &style, Wrapping::NORMAL, 1.);
    builder.text("tall", &style, 0);
    assert_eq!(Layout::new(Rc::new(builder.build()), 100., &engine).height(), 30.);
    assert_eq!(LineHeight::Factor(1.5).px(10.), 15.);
}

#[test]
fn spaces_hang_and_words_break_as_css_says() {
    let engine = common::engine();
    // `break-all` fills the line; the space at the wrap point stays on the first line.
    let text = "aaaa bbbb cccc dddd eeee ffff gggg hhhh";
    let broken = layout(&engine, text, Wrapping::MESSAGE, 100.);
    assert!(broken.line_count() > 1);
    for index in 0..broken.line_count() {
        let line = broken.line(index).unwrap();
        assert!(line.width <= 100. + 1. / 64., "line {index} is {} px wide", line.width);
        assert!(!text[line.text.0 as usize..].starts_with(' '), "line {index} starts with a space");
    }
    // Ordinary wrapping breaks at spaces only, and a word longer than the box overflows.
    let words = layout(&engine, "short incomprehensibilities short", Wrapping::NORMAL, 60.);
    assert_eq!(words.line_count(), 3);
    assert!(words.width() > 60.);
    // `nowrap` is one line whatever the width.
    assert_eq!(layout(&engine, text, Wrapping::NOWRAP, 50.).line_count(), 1);
    // `white-space: normal` collapses runs of spaces and newlines.
    let collapsed = layout(&engine, "  a   b\n\nc  ", Wrapping::NORMAL, 500.);
    assert_eq!(collapsed.paragraph().text(), "a b c ");
    assert_eq!(collapsed.line_count(), 1);
}

#[test]
fn truncation_and_clamp() {
    let engine = common::engine();
    let text = "A channel topic that goes on for longer than its box allows, so it is cut somewhere";
    let full = layout(&engine, text, Wrapping::TRUNCATE, f32::INFINITY);
    assert!(!full.is_truncated());
    let cut = layout(&engine, text, Wrapping::TRUNCATE, 120.);
    assert!(cut.is_truncated());
    assert_eq!(cut.line_count(), 1);
    assert!(cut.width() <= 120. && cut.width() > 90., "{}", cut.width());
    let fits = layout(&engine, "short", Wrapping::TRUNCATE, 120.);
    assert!(!fits.is_truncated());

    let clamped = layout(&engine, text, Wrapping::NORMAL.clamp(2), 160.);
    assert_eq!(clamped.line_count(), 2);
    assert!(clamped.is_truncated());
    assert!(clamped.line(1).unwrap().width <= 160.);
    let unclamped = layout(&engine, "two words", Wrapping::NORMAL.clamp(2), 160.);
    assert!(!unclamped.is_truncated());
}

#[test]
fn positions_round_trip_through_hit_testing() {
    let engine = common::engine();
    let text = "The quick brown fox jumps over the lazy dog and keeps on running for a while";
    let laid = layout(&engine, text, Wrapping::NORMAL, 200.);
    assert!(laid.line_count() >= 3);
    for offset in [0u32, 1, 4, 10, 19, 20, 43, text.len() as u32] {
        if !text.is_char_boundary(offset as usize) {
            continue;
        }
        let (x, top, height) = laid.caret(Pos::new(offset));
        let hit = laid.hit(x + 0.01, top + height / 2.);
        assert_eq!(hit.pos.offset, offset, "offset {offset} at x {x}");
    }
    // Outside the text: the nearest position.
    assert_eq!(laid.hit(-50., -50.).pos.offset, 0);
    assert_eq!(laid.hit(1000., 1000.).pos.offset, text.len() as u32);
    let second = laid.line(1).unwrap();
    // The end of a wrapped line and the start of the next are one offset, two places.
    let end_of_first = laid.hit(1000., 5.).pos;
    assert_eq!(end_of_first.affinity, Affinity::Upstream);
    assert_eq!(end_of_first.offset, second.text.0);
    assert_eq!(laid.line_of(end_of_first), 0);
    assert_eq!(laid.line_of(Pos::new(second.text.0)), 1);
    assert!(laid.caret(end_of_first).0 > 100.);
    assert_eq!(laid.caret(Pos::new(second.text.0)).0, 0.);

    // The run under a point carries the tag it was added with.
    assert_eq!(laid.hit(10., 5.).tag, Some(7));
    assert_eq!(laid.paragraph().locate(4), Some((7, 4)));
    assert_eq!(laid.paragraph().offset_of(7, 9), Some(9));

    // Words and lines.
    assert_eq!(laid.word_at(5), (4, 9), "quick");
    assert_eq!(laid.word_at(3), (3, 4), "the space after The");
    assert_eq!(laid.line_range(Pos::new(second.text.0)), second.text);

    // A selection over two lines is one rectangle per line, each as high as its line.
    let mut rects = Vec::new();
    laid.rects_for_range(4, second.text.0 + 3, |rect| rects.push(rect));
    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0][3], 21.015625);
    assert_eq!(rects[1][0], 0.);
    assert!(rects[0][0] > 20. && rects[1][2] > 5.);
}

#[test]
fn boxes_flow_with_the_text() {
    let engine = common::engine();
    let style = body(&engine);
    let mut builder = ParagraphBuilder::new(&engine, &style, Wrapping::MESSAGE, 1.);
    builder.text("hi ", &style, 0).inline_box(24., 24., 0.2 * 14.5, 1).text(" there", &style, 2);
    let laid = Layout::new(Rc::new(builder.build()), 300., &engine);
    // The 24 px box sits 2.890625 px below the baseline and lifts the line box (Chromium: 26.125).
    assert_eq!(laid.height(), 26.125);
    let boxes: Vec<_> = laid.boxes().collect();
    assert_eq!(boxes.len(), 1);
    let (tag, [x, y, width, height]) = boxes[0];
    assert_eq!((tag, width, height), (1, 24., 24.));
    assert_eq!(y, 0.);
    assert!(x > 10. && x < 30.);
    // A box is one position wide and reports its tag.
    assert_eq!(laid.hit(x + 12., 10.).tag, Some(1));
    assert_eq!(laid.paragraph().text(), "hi \u{fffc} there");
}

#[test]
fn italics_and_bold_are_synthesised_as_their_own_fonts() {
    let engine = common::engine();
    let upright = engine.face(&FontSpec::new("Decibell Sans", 400));
    let italic = engine.face(&FontSpec::new("Decibell Sans", 400).italic());
    let bold = engine.face(&FontSpec::new("Decibell Sans", 700));
    assert_ne!(upright.font_id(), italic.font_id(), "a slanted face is a font of its own");
    assert_ne!(upright.font_id(), bold.font_id());
    // The same face again is the same font: the glyph atlas is shared.
    assert_eq!(italic.font_id(), engine.face(&FontSpec::new("Decibell Sans", 400).italic()).font_id());
    // Slanting changes no advance.
    let width = |face| {
        let style = TextStyle::new(face, 14.5, grey(0.9));
        let mut builder = ParagraphBuilder::new(&engine, &style, Wrapping::NOWRAP, 1.);
        builder.text("Illinois fjord quiz", &style, 0);
        Layout::new(Rc::new(builder.build()), f32::INFINITY, &engine).width()
    };
    assert_eq!(width(upright), width(italic));
    assert!(width(bold) > width(upright));
}

#[test]
fn rich_layout_positions_copy_and_slots() {
    let engine = common::engine();
    let source = "Say **hello** to `code` 😀 and https://example.com/a\n```rust\nfn main() {}\n```\nafter";
    let document = parse(source, &ParseOptions::default());
    let mut asked = Vec::new();
    let rich = engine.rich_layout(
        1,
        RichContent { document: &document, source, edited: true },
        &rich_style(),
        400.,
        1.,
        &mut |span, kind, width| {
            asked.push((span, kind, width));
            80.
        },
    );
    assert_eq!(asked.len(), 1);
    assert_eq!((asked[0].1, asked[0].2), (SlotKind::Code, 400.));
    let slots: Vec<_> = rich.slots().collect();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].height, 80.);
    assert!(slots[0].y > 20.);
    // Text, the slot, text (with the edited marker).
    assert_eq!(rich.texts().count(), 2);
    assert!(rich.height() > slots[0].y + 80. + 20.);

    // Everything copies as displayed: markup gone, the emoji as its characters.
    let all = rich.copy_text(rich.start(), rich.end(), &document, source);
    assert!(all.starts_with("Say hello to code 😀 and https://example.com/a"), "{all:?}");
    assert!(all.contains("fn main() {}") && all.ends_with("after"), "{all:?}");
    assert!(!all.contains("edited") && !all.contains("**"));

    // A hit on the link names its span; elsewhere there is none.
    let (top, first) = rich.texts().next().unwrap();
    let text = first.paragraph().text();
    let link_at = text.find("https").unwrap() as u32;
    let (x, y, height) = first.caret(Pos::new(link_at + 3));
    let link = rich.link_at(x + 1., top + y + height / 2.).expect("the link");
    assert!(matches!(document.spans()[link as usize].kind, decibell_richtext::SpanKind::Link { .. }));
    assert_eq!(rich.link_at(2., top + 5.), None);

    // A double click on "hello" takes the word.
    let hello = text.find("hello").unwrap() as u32;
    let (from, to) = rich.word_at(RichPos { block: 0, offset: hello + 2, ..RichPos::default() });
    assert_eq!(rich.copy_text(from, to, &document, source), "hello");

    // The same content again is the same layout; another width is another.
    let again = engine.rich_layout(1, RichContent { document: &document, source, edited: true }, &rich_style(), 400., 1., &mut |_, _, _| 80.);
    assert!(Rc::ptr_eq(&rich, &again));
    let stats = engine.stats();
    let narrower = engine.rich_layout(1, RichContent { document: &document, source, edited: true }, &rich_style(), 300., 1., &mut |_, _, _| 80.);
    assert!(!Rc::ptr_eq(&rich, &narrower));
    assert_eq!(engine.stats().shapes, stats.shapes, "another width shapes nothing again");
    let _ = RichSelection::All;
}

#[test]
fn emoji_pack_holds_every_asset_the_grammar_names() {
    if decibell_text::packed_emoji_count() == 0 {
        eprintln!("skipped: built without gpui-client/assets/twemoji");
        return;
    }
    assert_eq!(decibell_text::packed_emoji_count(), decibell_richtext::EmojiAsset::count());
    for emoji in ["😀", "❤️", "👨‍👩‍👧‍👦", "🇹🇷", "©️"] {
        let asset = decibell_richtext::emoji::twemoji_asset(emoji).unwrap_or_else(|| panic!("{emoji} has an asset"));
        let svg = decibell_text::emoji_svg(asset).unwrap_or_else(|| panic!("{emoji} is in the pack"));
        assert!(svg.starts_with(b"<svg"), "{emoji}");
        let image = decibell_text::rasterize_emoji(asset, 24).expect("it rasterises");
        assert_eq!(image.size(0).width.0, 24);
    }
    // The store hands the same image back until it is evicted.
    let engine = common::engine();
    let asset = decibell_richtext::emoji::twemoji_asset("😀").unwrap();
    let first = engine.emoji.prepare(asset, 24).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &engine.emoji.prepare(asset, 24).unwrap()));
    assert_eq!(engine.emoji.usage(), (1, 24 * 24 * 4));
}
