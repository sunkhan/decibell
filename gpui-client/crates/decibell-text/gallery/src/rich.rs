//! `rich-text`: fixture messages through [`RichText`], with the smallest selection
//! controller that spans rows: the pattern a message list follows (the rows paint what they
//! are told; one owner turns pointer events into positions with `RichLayout::hit`).

use std::cell::Cell;
use std::rc::Rc;

use decibell_richtext::{Document, ParseOptions, SpanKind, parse};
use decibell_text::{
    CodeStyle, Engine, Formula, MathBlock, MathColors, RichContent, RichLayout, RichPos, RichSelection, RichStyle,
    RichText, Slot, SlotKind, TextBlock, TextStyle, Wrapping,
};
use decibell_ui::{ActiveTheme as _, FontRole, Label};
use gpui::{
    AnyElement, App, Context, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Render,
    Window, canvas, div, prelude::*, px, rgb,
};

/// (source, edited)
const MESSAGES: &[(&str, bool)] = &[
    ("Plain text with **bold**, *italic*, __underline__, ~~strike~~ and ***bold italic***, then `inline code` and a link https://example.com/docs/voice?channel=general to follow.", false),
    ("Nested: **bold with *italic inside* and `code in bold`**, then *italic with __underline__ and ~~strike~~ inside*.", true),
    ("A chip that wraps: `cargo build --release --features local-central --target x86_64-unknown-linux-gnu -- --nocapture` and the text after it.", false),
    ("Emoji inline 😀 in running text 🔥 keep the line box in step 🎉", false),
    ("😀", false),
    ("🔥🎉", false),
    ("👍❤️😂", false),
    ("✨😭🙏👀", false),
    (":wave: a shortcode, and emoji Twemoji has no drawing for: 🫨 🇦", false),
    ("Inline math $x^2+\\frac{a}{b}$ and $\\sqrt{x^2+1}$ sit on the baseline; a TeX error shows its source: $\\frac{a}{$.", false),
    ("Display math:\n$$\\int_0^\\infty e^{-x^2}\\,dx = \\frac{\\sqrt{\\pi}}{2}$$\nand the text goes on after it.", false),
    ("A code block:\n```rust\nfn main() {\n    let rate = 120; // frames per second\n    println!(\"{rate} fps\");\n}\n```", true),
    ("Supercalifragilisticexpialidociouspneumonoultramicroscopicsilicovolcanoconiosisantidisestablishmentarianism-floccinaucinihilipilification", false),
    ("Other scripts need fallback fonts: 中文文本 日本語のテキスト 한국어 Ελληνικά Русский текст", false),
    ("Right to left: مرحبا بالعالم هذا نص عربي and עברית טקסט בעברית in one paragraph.", false),
    ("line one\nline two\n\nline four\ttabbed\tcolumns", false),
];

const WIDTHS: [f32; 3] = [320., 560., 900.];
const ROW_GAP: f32 = 12.;

/// Chromium's default selection colours, as it paints them over the app's dark ground
/// (measured in Electron 33 with `tools/chromium`; the renderer has no `::selection` rule).
const SELECTION_BACKGROUND: u32 = 0x0538a4;
const SELECTION_TEXT: u32 = 0xffffff;

/// A caret somewhere in the gallery: a column, a message and a position in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Caret {
    column: usize,
    row: usize,
    pos: RichPos,
}

struct Row {
    y: f32,
    layout: Rc<RichLayout>,
}

pub struct RichGallery {
    documents: Vec<Document>,
    /// The rows of each column as last rendered, for hit-testing.
    rows: [Vec<Row>; 3],
    /// Where each column was painted.
    origins: [Rc<Cell<Point<Pixels>>>; 3],
    selection: Option<(Caret, Caret)>,
    dragging: bool,
}

impl Default for RichGallery {
    fn default() -> Self {
        let at = |row, offset| Caret {
            column: 1,
            row,
            pos: RichPos {
                block: 0,
                offset,
                ..RichPos::default()
            },
        };
        Self {
            documents: MESSAGES.iter().map(|(source, _)| parse(source, &ParseOptions::default())).collect(),
            rows: Default::default(),
            origins: Default::default(),
            // Something selected from the start, so that a capture shows the selection.
            selection: Some((at(0, 11), at(2, 30))),
            dragging: false,
        }
    }
}

fn rich_style(cx: &App) -> RichStyle {
    let theme = cx.theme();
    RichStyle {
        family: theme.font(FontRole::Sans),
        mono_family: theme.font(FontRole::Mono),
        size: theme.scaled.text_body,
        leading: theme.palette.t_lh_body,
        // `letter-spacing: var(--t-ls-body)` on <body> (16 px), inherited as px.
        letter_spacing: theme.palette.t_ls_body * 16.,
        color: theme.color_text_primary,
        link: theme.color_accent,
        muted: theme.color_text_muted,
        meta_size: theme.scaled.text_meta,
        error: theme.color_error,
        code_background: theme.color_bg_darkest,
        code_radius: theme.radius_sm.into(),
    }
}

/// CodeBlock.tsx's `<pre>`: `font-mono text-[12.5px] leading-[1.5]`.
fn code_style(engine: &Engine, cx: &App) -> CodeStyle {
    let theme = cx.theme();
    let mono = theme.font(FontRole::Mono);
    let face = |italic| {
        engine.face(&decibell_text::FontSpec {
            family: mono.clone(),
            weight: 400,
            italic,
        })
    };
    CodeStyle {
        text: TextStyle::new(face(false), 12.5, theme.color_text_primary).leading(1.5),
        italic: face(true),
        keyword: theme.color_accent_bright,
        title: theme.color_accent,
        string: theme.color_success,
        number: theme.color_warning,
        comment: theme.color_text_muted,
        deletion: theme.color_error,
        operator: theme.color_text_secondary,
    }
}

const CODE_PAD: f32 = 10.;
const BLOCK_MARGIN: f32 = 4.;

impl RichGallery {
    fn caret_at(&self, column: usize, position: Point<Pixels>) -> Option<Caret> {
        let origin = self.origins[column].get();
        let (x, y) = (f32::from(position.x - origin.x), f32::from(position.y - origin.y));
        let rows = &self.rows[column];
        let row = rows.partition_point(|row| row.y + row.layout.height() + ROW_GAP / 2. <= y).min(rows.len().checked_sub(1)?);
        let hit = rows[row].layout.hit(x, y - rows[row].y);
        Some(Caret { column, row, pos: hit.pos })
    }

    fn selection_for(&self, column: usize, row: usize, layout: &RichLayout) -> RichSelection {
        let Some((anchor, focus)) = self.selection else { return RichSelection::None };
        let (from, to) = if anchor <= focus { (anchor, focus) } else { (focus, anchor) };
        if from.column != column || row < from.row || row > to.row || from == to {
            return RichSelection::None;
        }
        let start = if row == from.row { from.pos } else { layout.start() };
        let end = if row == to.row { to.pos } else { layout.end() };
        RichSelection::Range(start, end)
    }

    /// What the selection would put on the clipboard: each row's share, joined by newlines.
    fn copied(&self) -> String {
        let Some((anchor, focus)) = self.selection else { return String::new() };
        let (from, to) = if anchor <= focus { (anchor, focus) } else { (focus, anchor) };
        let mut out = String::new();
        for row in from.row..=to.row {
            let Some(shown) = self.rows[from.column].get(row) else { break };
            let start = if row == from.row { from.pos } else { shown.layout.start() };
            let end = if row == to.row { to.pos } else { shown.layout.end() };
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&shown.layout.copy_text(start, end, &self.documents[row], MESSAGES[row].0));
        }
        out
    }

    fn column(&mut self, column: usize, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let engine = Engine::global(cx);
        let style = rich_style(cx);
        let code = code_style(&engine, cx);
        let scale = window.scale_factor();
        let width = WIDTHS[column];
        let (background, border, radius) = {
            let theme = cx.theme();
            (theme.color_bg_darkest, theme.color_border, theme.radius_md)
        };

        let mut rows = Vec::new();
        let mut y = 0.0f32;
        for (index, document) in self.documents.iter().enumerate() {
            let (source, edited) = MESSAGES[index];
            let layout = engine.rich_layout(
                index as u64,
                RichContent { document, source, edited },
                &style,
                width,
                scale,
                &mut |span, kind, _| {
                    let text = document.text(source, &document.spans()[span as usize]);
                    match kind {
                        SlotKind::Code => {
                            let lines = text.split('\n').count() as f32;
                            lines * 12.5 * 1.5 + 2. * CODE_PAD + 2. * BLOCK_MARGIN
                        }
                        SlotKind::Math => match engine.formula(text, true, style.size, scale) {
                            Formula::Math(math) => math.height() + 4. * BLOCK_MARGIN,
                            _ => style.size * style.leading,
                        },
                    }
                },
            );
            rows.push(Row { y, layout });
            // Rows start on whole pixels, as a list would place them.
            y = (y + rows[index].layout.height() + ROW_GAP).ceil();
        }

        let origin = self.origins[column].clone();
        let mut content = div().relative().w(px(width)).h(px(y)).flex_none().child(
            canvas(move |bounds, _, _| origin.set(bounds.origin), |_, _, _, _| {})
                .absolute()
                .size_full(),
        );
        for (index, row) in rows.iter().enumerate() {
            let (source, _) = MESSAGES[index];
            let document = self.documents[index].clone();
            let (engine, code, math_colors) = (engine.clone(), code, MathColors { text: style.color, error: style.error });
            let text_px = style.size;
            let view = cx.entity_id();
            let element = RichText::new(("rich", column * 1000 + index), row.layout.clone())
                .selection(
                    self.selection_for(column, index, &row.layout),
                    rgb(SELECTION_BACKGROUND).into(),
                    Some(rgb(SELECTION_TEXT).into()),
                )
                .on_link(move |span, _, _, _| {
                    // The app opens the browser (or the invite dialog) here.
                    eprintln!("link clicked: span {span} of message {index}");
                })
                .slot(move |slot: Slot, window: &mut Window, cx: &mut App| -> AnyElement {
                    let span = &document.spans()[slot.span as usize];
                    let text = document.text(source, span);
                    match (slot.kind, span.kind) {
                        (SlotKind::Code, SpanKind::CodeBlock { lang }) => {
                            let language = lang.and_then(|lang| decibell_highlight::Language::from_name(lang.grammar()));
                            let highlighted = language.and_then(|language| {
                                engine.highlighted(language, text, cx, move |cx| cx.notify(view))
                            });
                            let layout = engine.code_layout(text, highlighted.as_deref(), &code, Wrapping::PRE, f32::INFINITY, window.scale_factor());
                            div()
                                .size_full()
                                .py(px(BLOCK_MARGIN))
                                .child(
                                    div()
                                        .w_full()
                                        .p(px(CODE_PAD))
                                        .rounded(radius)
                                        .border_1()
                                        .border_color(border)
                                        .bg(background)
                                        .overflow_hidden()
                                        .child(TextBlock::new(layout)),
                                )
                                .into_any_element()
                        }
                        (SlotKind::Math, _) => match engine.formula(text, true, text_px, window.scale_factor()) {
                            Formula::Math(math) => div()
                                .size_full()
                                .py(px(2. * BLOCK_MARGIN))
                                .overflow_hidden()
                                .child(MathBlock::new(math, math_colors))
                                .into_any_element(),
                            _ => Label::new(text.to_string()).font(FontRole::Mono).into_any_element(),
                        },
                        _ => div().into_any_element(),
                    }
                });
            content = content.child(div().absolute().left_0().top(px(row.y)).child(element));
        }
        self.rows[column] = rows;

        content
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let Some(caret) = this.caret_at(column, event.position) else { return };
                    let rows = &this.rows[column];
                    this.selection = Some(match event.click_count {
                        // A double click takes the word, a triple click the message.
                        2 => {
                            let (from, to) = rows[caret.row].layout.word_at(caret.pos);
                            (Caret { pos: from, ..caret }, Caret { pos: to, ..caret })
                        }
                        3 => {
                            let layout = &rows[caret.row].layout;
                            (Caret { pos: layout.start(), ..caret }, Caret { pos: layout.end(), ..caret })
                        }
                        _ if event.modifiers.shift => match this.selection {
                            Some((anchor, _)) if anchor.column == column => (anchor, caret),
                            _ => (caret, caret),
                        },
                        _ => (caret, caret),
                    });
                    this.dragging = event.click_count == 1;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if !this.dragging || event.pressed_button != Some(MouseButton::Left) {
                    this.dragging = false;
                    return;
                }
                let (Some((anchor, focus)), Some(caret)) = (this.selection, this.caret_at(column, event.position)) else {
                    return;
                };
                // A frame only when the caret really moved.
                if anchor.column == column && caret != focus {
                    this.selection = Some((anchor, caret));
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.dragging = false))
    }
}

impl Render for RichGallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (background, text, muted): (Hsla, Hsla, Hsla) = {
            let theme = cx.theme();
            (theme.color_bg_primary, theme.color_text_primary, theme.color_text_muted)
        };
        let mut columns = div().flex().items_start().gap_6();
        for (column, width) in WIDTHS.into_iter().enumerate() {
            let content = self.column(column, window, cx);
            columns = columns.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Label::new(format!("{width} px")).size(11.).color(muted).nowrap())
                    .child(content),
            );
        }
        let copied = self.copied();
        div()
            .id("rich-text")
            .size_full()
            .overflow_scroll()
            .bg(background)
            .text_color(text)
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div().w(px(900.)).child(
                    Label::new(format!("Drag to select across messages (double click: word, triple: message). It copies as: {copied:?}"))
                        .size(12.)
                        .color(muted)
                        .clamp(3),
                ),
            )
            .child(columns)
    }
}
