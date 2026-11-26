//! One search result (`SearchResult` in `SearchResultsPanel.tsx`): a card with the quoted line of
//! a reply, the avatar, the sender's name and time, the body with the matched words marked
//! (`line-clamp-6`), and the attachments' tiles and file rows; "Jump" shows on hover.
//!
//! Heights are computed here, exactly, for every result (the panel's list is windowed: its rows'
//! offsets must be known up front); the body's layout is built again only for the rows on screen.
//! The body is one paragraph laid out by `decibell-text` from the message's rich-text document,
//! each text run split where the highlighter marks it (`<mark>`: `rounded-sm bg-accent-mid
//! text-text-bright`, drawn as a chip behind the run).
//!
//! Differences from the renderer's `MessageText` inside a result (not seen in search results of
//! the fixtures; listed in CONVENTIONS): fenced code blocks and display formulas are drawn as
//! monospace text in the paragraph (the renderer draws its `CodeBlock` / KaTeX boxes), inline
//! formulas as their TeX source in inline code.

use std::rc::Rc;
use std::sync::Arc;

use decibell_attachments::AttachmentKind;
use decibell_model::chat::{Attachment, DecryptError, Message};
use decibell_richtext::{Document, EmojiAsset, ParseOptions, SpanKind};
use decibell_text::{Chip, Engine, FontSpec, Layout, LineHeight, OverflowWrap, ParagraphBuilder, TextStyle, WhiteSpace, Wrapping};
use decibell_ui::{ActiveTheme as _, FontRole, Theme};
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, LayoutId, Pixels, Style, Window, point, px, size,
};

use crate::highlight::Highlighter;

/// The card's frame (`mb-1.5 rounded-lg border px-3 py-2.5`).
pub const CARD_GAP: f32 = 6.;
pub const CARD_BORDER: f32 = 1.;
pub const CARD_PAD_X: f32 = 12.;
pub const CARD_PAD_Y: f32 = 10.;
/// `UserAvatar size={32}` and `gap-2.5`.
pub const AVATAR: f32 = 32.;
pub const AVATAR_GAP: f32 = 10.;
/// The header's `pr-12`.
pub const HEADER_PAD_RIGHT: f32 = 48.;
/// `mt-0.5` over the body.
pub const BODY_TOP: f32 = 2.;
/// The quoted line: 12 px text (line height 18) and `mb-1`.
pub const REPLY_LINE: f32 = 18.;
pub const REPLY_GAP: f32 = 4.;
/// Media tiles: `mt-2 h-16`; a file row: `mt-2`, `border py-1.5` around a 13 px line.
pub const MEDIA_TOP: f32 = 8.;
pub const TILE: f32 = 64.;
pub const FILE_TOP: f32 = 8.;
/// A channel's heading over its results: `mt-3`, a 12 px line, `mb-1.5`.
pub const HEADING: f32 = 12. + 18. + 6.;
/// The heading's `mt-3` past the card above's `mb-1.5` (the two collapse).
pub const HEADING_AFTER_CARD: f32 = HEADING - CARD_GAP;
/// The sentinel under the last result (`h-10`) and the scroller's `pb-3`.
pub const SENTINEL: f32 = 40.;
pub const LIST_PAD_BOTTOM: f32 = 12.;
/// The list's `px-2`.
pub const LIST_PAD_X: f32 = 8.;
/// The line clamp of a result's body.
pub const BODY_LINES: u16 = 6;

/// `pre-wrap break-words [overflow-wrap:anywhere]`, at most six lines.
const BODY_WRAPPING: Wrapping = Wrapping {
    white_space: WhiteSpace::PreWrap,
    overflow_wrap: OverflowWrap::Anywhere,
    max_lines: BODY_LINES,
    ..Wrapping::NORMAL
};

/// The text styles and colours of a result, made once per theme / text size / scale.
pub struct ResultStyle {
    pub key: u64,
    pub scale: f32,
    pub family: gpui::SharedString,
    pub mono_family: gpui::SharedString,
    pub body: TextStyle,
    pub meta_size: f32,
    pub name: TextStyle,
    pub time: TextStyle,
    pub notice: TextStyle,
    /// A file row's name (`text-[13px] font-medium`) and size (`text-[12px]`).
    pub file_name: TextStyle,
    pub file_size: TextStyle,
    pub file_line: f32,
    /// The header's height and where its baseline sits (`flex items-baseline`).
    pub header_height: f32,
    pub name_top: f32,
    pub time_top: f32,
    pub primary: Hsla,
    pub bright: Hsla,
    pub muted: Hsla,
    pub link: Hsla,
    pub mark: Hsla,
    pub code_background: Hsla,
    pub radius_sm: f32,
}

impl ResultStyle {
    pub fn new(scale: f32, cx: &mut App) -> ResultStyle {
        let engine = Engine::global(cx);
        let theme = cx.theme();
        Self::build(&engine, theme, scale)
    }

    fn build(engine: &Engine, theme: &Theme, scale: f32) -> ResultStyle {
        // The body's `letter-spacing` (an em of the 16 px root) reaches every text.
        let tracking = theme.palette.t_ls_body * 16.;
        let face = |role: FontRole, weight: u16, italic: bool| engine.face(&FontSpec { family: theme.font(role), weight, italic });
        let text = |role: FontRole, size: f32, weight: u16, color: Hsla| {
            TextStyle::new(face(role, weight, false), size, color).leading(1.5).letter_spacing(tracking)
        };
        let scaled = &theme.scaled;
        let body = TextStyle::new(face(FontRole::Sans, 400, false), scaled.text_body, theme.color_text_primary)
            .leading(theme.palette.t_lh_body)
            .letter_spacing(tracking);
        let name = text(FontRole::Channel, 14., 600, theme.color_text_primary);
        let time = text(FontRole::Meta, 12., 400, theme.color_text_muted);
        let notice = TextStyle::new(face(FontRole::Sans, 400, true), scaled.text_body, theme.color_text_muted)
            .leading(theme.palette.t_lh_body)
            .letter_spacing(tracking);
        // `flex items-baseline`: the tallest ascent and descent above and below the shared
        // baseline set the line.
        let name_layout = engine.label_layout("Ag", &name, Wrapping::NOWRAP, f32::INFINITY, scale);
        let time_layout = engine.label_layout("Ag", &time, Wrapping::NOWRAP, f32::INFINITY, scale);
        let baseline = name_layout.first_baseline().max(time_layout.first_baseline());
        let below = (name_layout.height() - name_layout.first_baseline()).max(time_layout.height() - time_layout.first_baseline());
        let header_height = layout_unit(baseline + below);
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = decibell_text::KeyHasher::default();
            (theme.id.index(), scale.to_bits(), scaled.text_body.to_bits(), scaled.text_meta.to_bits()).hash(&mut hasher);
            hasher.finish()
        };
        ResultStyle {
            key,
            scale,
            family: theme.font(FontRole::Sans),
            mono_family: theme.font(FontRole::Mono),
            body,
            meta_size: scaled.text_meta,
            name,
            time,
            notice,
            file_name: text(FontRole::Sans, 13., 500, theme.color_text_primary),
            file_size: text(FontRole::Sans, 12., 400, theme.color_text_muted),
            file_line: 13. * 1.5,
            header_height,
            name_top: baseline - name_layout.first_baseline(),
            time_top: baseline - time_layout.first_baseline(),
            primary: theme.color_text_primary,
            bright: theme.color_text_bright,
            muted: theme.color_text_muted,
            link: theme.color_accent,
            mark: theme.color_accent_mid,
            code_background: theme.color_bg_darkest,
            radius_sm: theme.radius_sm.into(),
        }
    }

    /// A run of the body in the grammar's style bits (`<strong>` 700, `<em>` slanted, `<code>`
    /// mono at 0.85 em in a chip).
    fn run(&self, engine: &Engine, bits: decibell_richtext::Style, color: Hsla) -> TextStyle {
        let code = bits.is_code();
        let face = engine.face(&FontSpec {
            family: if code { self.mono_family.clone() } else { self.family.clone() },
            weight: if bits.is_bold() { 700 } else { 400 },
            italic: bits.is_italic(),
        });
        let mut style = TextStyle { face, size: if code { self.body.size * 0.85 } else { self.body.size }, color, ..self.body };
        style.underline = bits.is_underline();
        style.strikethrough = bits.is_strike();
        if code {
            style.chip = Some(Chip { background: self.code_background, radius: self.radius_sm, pad_x: 6., pad_y: 2. });
        }
        style
    }

    /// `<mark>`: the run in `text-text-bright` on `bg-accent-mid rounded-sm`.
    fn marked(&self, mut run: TextStyle) -> TextStyle {
        run.color = self.bright;
        run.chip = Some(Chip { background: self.mark, radius: self.radius_sm, pad_x: 0., pad_y: 0. });
        run
    }
}

/// Blink's `LayoutUnit`: 1/64 px, rounded up.
fn layout_unit(value: f32) -> f32 {
    (value * 64.).ceil() / 64.
}

/// A result's body, laid out for one width.
pub struct Body {
    pub layout: Rc<Layout>,
    /// The emoji drawn in the text's boxes: (box tag, asset).
    pub emoji: Vec<(u32, EmojiAsset)>,
}

/// Lays out `message`'s body at `width` with `highlight`'s marks.
pub fn body(message: &Message, highlight: Option<&Highlighter>, width: f32, style: &ResultStyle, engine: &Rc<Engine>) -> Body {
    let source: &str = &message.content;
    let document: Document = decibell_richtext::parse(source, &ParseOptions::default());
    let emoji_size = document.jumbo_size().unwrap_or(decibell_richtext::INLINE_EMOJI_SIZE) as f32;
    let mut builder = ParagraphBuilder::new(engine, &style.body, BODY_WRAPPING, style.scale);
    let mut emoji = Vec::new();
    let mut texts = 0u32;
    let push = |builder: &mut ParagraphBuilder, text: &str, run: TextStyle, tag: u32| match highlight {
        Some(h) => {
            let mut last = 0;
            for mark in h.marks(text) {
                if mark.start > last {
                    builder.text(&text[last..mark.start], &run, tag);
                }
                builder.text(&text[mark.clone()], &style.marked(run), tag);
                last = mark.end;
            }
            if last < text.len() {
                builder.text(&text[last..], &run, tag);
            }
        }
        None => {
            builder.text(text, &run, tag);
        }
    };
    // Blocks (a code block, a display formula) are lines of their own: a line break between
    // two blocks unless the text before already ends in one.
    let mut index = 0u32;
    let mut ends_in_break = true;
    // (tag, span); `None`: a line break between blocks.
    let mut spans: Vec<(u32, Option<&decibell_richtext::Span>)> = Vec::with_capacity(document.spans().len());
    for block in document.blocks() {
        let (first, list): (u32, &[decibell_richtext::Span]) = match block {
            decibell_richtext::Block::Paragraph(list) => (index, list),
            decibell_richtext::Block::Code(span) | decibell_richtext::Block::Math(span) => (index, std::slice::from_ref(span)),
        };
        if texts > 0 && !ends_in_break {
            spans.push((u32::MAX - 9, None));
        }
        for (offset, span) in list.iter().enumerate() {
            spans.push((first + offset as u32, Some(span)));
            let text = document.text(source, span);
            if !text.is_empty() {
                ends_in_break = text.ends_with('\n');
            }
        }
        texts += 1;
        index += list.len() as u32;
    }
    for (tag, span) in spans {
        let Some(span) = span else {
            builder.text("\n", &style.body, tag);
            continue;
        };
        let text = document.text(source, span);
        match span.kind {
            SpanKind::Text => push(&mut builder, text, style.run(engine, span.style, style.primary), tag),
            SpanKind::Link { .. } => push(&mut builder, text, style.run(engine, span.style, style.link), tag),
            SpanKind::Emoji { asset } => {
                let device_px = (emoji_size * style.scale).round() as u32;
                match asset.filter(|asset| engine.emoji.prepare(*asset, device_px).is_some()) {
                    Some(asset) => {
                        // Twemoji.tsx: an inline-block of the emoji size, `align-[-0.2em]`.
                        builder.inline_box(emoji_size, emoji_size, 0.2 * style.body.size, tag);
                        emoji.push((tag, asset));
                    }
                    None => {
                        let mut run = style.run(engine, span.style, style.primary);
                        run.size = emoji_size;
                        run.line_height = LineHeight::Factor(1.);
                        builder.text(text, &run, tag);
                    }
                }
            }
            SpanKind::Math => push(&mut builder, text, style.run(engine, span.style | decibell_richtext::Style::CODE, style.primary), tag),
            SpanKind::CodeBlock { .. } | SpanKind::DisplayMath => {
                let mut run = style.run(engine, decibell_richtext::Style::CODE, style.primary);
                run.chip = None;
                push(&mut builder, text, run, tag);
            }
        }
    }
    if message.edited_at > 0 {
        // `ml-1 text-meta text-text-muted`.
        let mut run = style.body;
        run.size = style.meta_size;
        run.color = style.muted;
        run.margin_start = 4.;
        run.selectable = false;
        builder.text("(edited)", &run, u32::MAX - 8);
    }
    let paragraph = Rc::new(builder.build());
    Body { layout: Rc::new(Layout::new(paragraph, width, engine)), emoji }
}

/// `text` as runs of `run`, the parts `highlight` marks as `<mark>`s (a file name:
/// `highlight(a.filename)`).
pub fn marked_runs(text: &str, highlight: Option<&Highlighter>, run: TextStyle, style: &ResultStyle) -> Vec<(String, TextStyle)> {
    let mut out = Vec::new();
    let mut last = 0;
    if let Some(h) = highlight {
        for mark in h.marks(text) {
            if mark.start > last {
                out.push((text[last..mark.start].to_string(), run));
            }
            out.push((text[mark.clone()].to_string(), style.marked(run)));
            last = mark.end;
        }
    }
    if last < text.len() {
        out.push((text[last..].to_string(), run));
    }
    out
}

/// What a result shows under its header.
pub fn has_text(message: &Message) -> bool {
    message.decrypt_error.is_some() || !message.content.is_empty()
}

/// Media tiles (`kind image | video`, not purged) and file rows (the rest).
pub fn split_attachments(message: &Message) -> (Vec<&Attachment>, Vec<&Attachment>) {
    message
        .attachments
        .iter()
        .partition(|a| matches!(a.kind, AttachmentKind::Image | AttachmentKind::Video) && a.purged_at == 0)
}

/// A file row's height: `border` + `py-1.5` around the 13 px line.
pub fn file_row_height(style: &ResultStyle) -> f32 {
    2. * CARD_BORDER + 12. + style.file_line
}

/// The card's height for `message` (its `mb-1.5` included), `body` laid out at the column's width.
pub fn card_height(message: &Message, body: Option<&Body>, decrypt_line: f32, style: &ResultStyle) -> f32 {
    let mut column = style.header_height;
    if message.decrypt_error.is_some() {
        column += BODY_TOP + decrypt_line.max(12.);
    } else if let Some(body) = body {
        column += BODY_TOP + body.layout.height();
    }
    let (media, files) = split_attachments(message);
    if !media.is_empty() {
        column += MEDIA_TOP + TILE;
    }
    column += files.len() as f32 * (FILE_TOP + file_row_height(style));
    let reply = if message.reply.is_some() { REPLY_LINE + REPLY_GAP } else { 0. };
    2. * CARD_BORDER + 2. * CARD_PAD_Y + reply + column.max(AVATAR) + CARD_GAP
}

/// The width of a result's text column in a list `content` px wide.
pub fn column_width(content: f32) -> f32 {
    (content - 2. * CARD_BORDER - 2. * CARD_PAD_X - AVATAR - AVATAR_GAP).max(1.)
}

/// `decryptErrorText` for a result (the panel says it the same for every reason).
pub fn decrypt_text(_: &DecryptError) -> &'static str {
    "This message couldn't be decrypted."
}

/// `formatFileSize`.
pub fn format_file_size(bytes: i64) -> String {
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", b / 1024.)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", b / 1024. / 1024.)
    } else {
        format!("{:.1} GB", b / 1024. / 1024. / 1024.)
    }
}

/// Draws a [`Body`]: the paragraph and its emoji, at exactly the layout's size.
pub struct BodyText {
    body: Rc<Body>,
}

impl BodyText {
    pub fn new(body: Rc<Body>) -> Self {
        Self { body }
    }
}

impl Element for BodyText {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        let scale = window.scale_factor();
        let snap = |value: f32| (value * scale).ceil() / scale;
        style.size.width = px(snap(self.body.layout.width())).into();
        style.size.height = px(snap(self.body.layout.height())).into();
        style.flex_shrink = 0.;
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = &self.body.layout;
        layout.paint(bounds.origin, &decibell_text::PaintOptions::default(), window, cx);
        if self.body.emoji.is_empty() {
            return;
        }
        let engine = Engine::global(cx);
        for (tag, [x, y, width, height]) in layout.boxes() {
            let Some((_, asset)) = self.body.emoji.iter().find(|(known, _)| *known == tag) else { continue };
            let at = point(bounds.origin.x + px(x), bounds.origin.y + px(y));
            engine.emoji.paint(*asset, Bounds::new(at, size(px(width), px(height))), window, cx);
        }
    }
}

/// The results' rows as the windowed list draws them: a channel's heading, a result, the
/// sentinel ("Loading more…" / "That's everything.").
#[derive(Clone)]
pub enum Item {
    /// `first`: the list's first row. A later heading's `mt-3` collapses with the `mb-1.5` of the
    /// card above it (block margins): 6 px more, not 12.
    Heading { channel: gpui::SharedString, first: bool },
    Result(Arc<Message>),
    Sentinel,
}
