//! One message row, laid out once and painted from that layout every frame.
//!
//! [`RowLayout::build`] turns a message and its facts into everything a frame needs: the exact
//! height (Blink's, in 1/64 px), the body's [`RichLayout`], the header's label layouts, the code
//! blocks' geometry and the boxes of the slots. [`RowLayout::paint`] draws it with quads and the
//! text engine's own paint calls: no elements, no layout, no shaping, no allocation.
//! [`RowLayout::hit`] says what is under a point (a caret position, a link, a button).
//!
//! The geometry is `MessageBubble.tsx` / `ReplyPreview.tsx` / `CodeBlock.tsx` / `MathTex.tsx`,
//! class by class; the constants are in [`crate::style`].

use std::rc::Rc;
use std::sync::Arc;

use decibell_attachments::AttachmentKind;
use decibell_highlight::Language;
use decibell_model::chat::{DecryptError, Message, RowFacts};
use decibell_model::regional::{Date, Regional};
use decibell_model::stores::settings::TimeFormat;
use decibell_model::{ServerId, Uid};
use decibell_richtext::{Document, ParseOptions, SpanKind};
use decibell_text::{
    Engine, Formula, Layout, Math, PaintOptions, RichContent, RichLayout, RichPaint, RichPos, RichSelection, SlotKind, Wrapping,
};
use decibell_ui::IconName;
use gpui::{
    App, Bounds, BorderStyle, Corners, Edges, Hsla, Pixels, Point, RenderImage, SharedString, TransformationMatrix, Window, fill,
    linear_color_stop, linear_gradient, point, px, quad, size, transparent_black,
};

use crate::media::{MediaBox, MediaBoxKind, SlotSpace, attachment_boxes};
use crate::style::*;

/// A caret somewhere in a row. Parts are ordered as they read: the sender's name, the time, the
/// body (the first two exist only in a row that opens a group).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RowPos {
    pub part: Part,
    pub pos: RichPos,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Part {
    #[default]
    Name,
    Time,
    Body,
}

impl RowPos {
    pub const START: RowPos = RowPos { part: Part::Name, pos: RichPos { block: 0, offset: 0, affinity: decibell_text::Affinity::Downstream } };
    pub const END: RowPos =
        RowPos { part: Part::Body, pos: RichPos { block: u32::MAX, offset: u32::MAX, affinity: decibell_text::Affinity::Upstream } };

    fn at(part: Part, offset: u32) -> RowPos {
        RowPos { part, pos: RichPos { block: 0, offset, ..RichPos::default() } }
    }
}

/// A row action button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    Reply,
    Edit,
    Delete,
}

/// What is under the pointer in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Target {
    /// Row chrome: nothing to click.
    #[default]
    None,
    /// Selectable text (the I-beam).
    Text,
    /// A link in the body (its span index).
    Link(u32),
    /// The sender's name or avatar: the profile popup (left), the user menu (right).
    Sender,
    /// The quoted line: jump to the parent.
    ReplyLine,
    /// The name in the quoted line: the parent author's profile.
    ReplyName,
    Action(RowAction),
    /// A code block's copy button / expand bar / body (its span index).
    CodeCopy(u32),
    CodeToggle(u32),
    CodeBody(u32),
    /// A media placeholder.
    Media,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowHit {
    pub target: Target,
    /// The nearest caret, for a selection.
    pub pos: RowPos,
}

/// The replied-to message as the row shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplySpec {
    pub parent: i64,
    /// `None`: the parent was deleted.
    pub author: Option<(Uid, SharedString, SharedString)>,
    pub content: SharedString,
    pub kinds: Arc<[AttachmentKind]>,
}

/// A row drawn as a notice card instead of a message bubble: `DmChatPanel`'s two legacy refusal
/// strings from central (`ErrorCard`: the warning glyph in its circle, the lead in
/// `font-medium text-warning`, then the text). The list source decides
/// ([`crate::ListSource::notice`]).
#[derive(Clone, Debug, PartialEq)]
pub struct RowNotice {
    pub lead: SharedString,
    pub text: SharedString,
}

/// What a row is built from, beyond the message: everything the list resolved from the model.
pub struct RowSpec<'a> {
    pub message: &'a Arc<Message>,
    pub facts: RowFacts,
    pub server: Option<ServerId>,
    /// The name as shown (nickname) and the account name (the avatar letter, the colour hash).
    pub display_name: SharedString,
    pub user_name: SharedString,
    pub name_color: Hsla,
    pub avatar_color: Hsla,
    pub reply: Option<ReplySpec>,
    /// (name colour, avatar colour) of the parent's author.
    pub reply_colors: (Hsla, Hsla),
    /// Bit `n`: the row's `n`-th code block is expanded.
    pub expanded: u64,
    /// A message that is only a media link whose preview resolved to an image hides its text.
    pub text_hidden: bool,
    pub editing: bool,
    /// The height the editor slot reserves while editing.
    pub editor_height: f32,
    pub today: i32,
    pub time_format: TimeFormat,
    pub space: SlotSpace,
    /// Heights of the attachments / inflight / embeds slots when a crate fills them.
    pub slot_heights: [Option<f32>; 3],
    /// Draw the row as a notice card (no avatar, header, actions or slots).
    pub notice: Option<RowNotice>,
}

/// A fenced code block's box (CodeBlock.tsx).
pub struct CodeBlockLayout {
    pub span: u32,
    /// px from the body's top (the slot's `y`, margins included).
    pub y: f32,
    /// The whole slot's height (margins included).
    pub height: f32,
    /// The top margin: `my-1`, or 0 when it collapsed into the block before it.
    pub margin_top: f32,
    /// The bordered box's width.
    pub width: f32,
    pub lines: usize,
    pub collapsible: bool,
    pub expanded: bool,
    /// The `<pre>`'s box height (padding and a horizontal bar included).
    pub pre_height: f32,
    pub hbar: bool,
    pub vbar: bool,
    pub language: Option<Language>,
    pub label: Rc<Layout>,
    pub foot: Option<Rc<Layout>>,
    pub text: Rc<Layout>,
    pub highlighted: bool,
}

/// A display formula's box (MathTex.tsx).
pub struct MathBlockLayout {
    pub span: u32,
    pub y: f32,
    pub height: f32,
    pub margin_top: f32,
    pub formula: Option<Rc<Math>>,
    /// A TeX error: the source in the error colour.
    pub fallback: Option<Rc<Layout>>,
}

struct ReplyLine {
    avatar_letter: Option<Rc<Layout>>,
    name: Option<Rc<Layout>>,
    snippet: Option<Rc<Layout>>,
    glyph: Option<IconName>,
    name_color: Hsla,
    avatar_color: Hsla,
    author: Uid,
    /// x of the name, the snippet and the glyph, from the reply box's left.
    name_x: f32,
    snippet_x: f32,
    glyph_x: f32,
}

enum Body {
    None,
    Rich { layout: Rc<RichLayout>, document: Rc<Document> },
    /// A sealed message this device couldn't open: the lock and one muted line.
    Notice { layout: Rc<Layout> },
    /// The inline editor's reserved box.
    Editor { height: f32 },
    /// A notice card: its box ([x, y, w, h] from the bubble's top) and its paragraph.
    Card { card: [f32; 4], layout: Rc<Layout> },
}

/// See the module doc.
pub struct RowLayout {
    pub height: f32,
    pub message: Arc<Message>,
    pub facts: RowFacts,
    pub grouped: bool,
    /// The sender's name as the row shows it (a nickname may arrive, or change, later).
    pub shown_name: SharedString,
    /// The bubble's top (under the day divider) and the content column's left.
    bubble_y: f32,
    content_x: f32,
    content_width: f32,
    divider: Option<Rc<Layout>>,
    reply: Option<ReplyLine>,
    avatar_letter: Option<Rc<Layout>>,
    avatar_color: Hsla,
    name_color: Hsla,
    name: Option<Rc<Layout>>,
    /// The header's time, or the hover time of a grouped row.
    time: Option<Rc<Layout>>,
    /// The header line: its top and the shared baseline, from the bubble's top.
    header_y: f32,
    header_baseline: f32,
    body: Body,
    body_y: f32,
    pub codes: Vec<CodeBlockLayout>,
    pub maths: Vec<MathBlockLayout>,
    /// Slot tops (attachments, inflight, embeds) from the bubble's top, and their heights.
    slots: [(f32, f32); 3],
    media: Vec<MediaBox>,
    sender: Uid,
    /// The list draws this row's bubble itself for now (the last row's `fadeUp` entrance, with
    /// an opacity the rows' own paint can't apply): [`Self::paint`] draws only the day divider.
    pub overlaid: std::cell::Cell<bool>,
}

const SLOT_ATTACHMENTS: usize = 0;
const SLOT_INFLIGHT: usize = 1;
const SLOT_EMBEDS: usize = 2;

fn darker(color: Hsla) -> Hsla {
    // `color-mix(in srgb, av-N 72%, #000)`.
    let mut rgba: gpui::Rgba = color.into();
    rgba.r *= 0.72;
    rgba.g *= 0.72;
    rgba.b *= 0.72;
    rgba.into()
}

/// The day divider's label (`DAY_FMT`: the renderer's `{ weekday: "long", year, month: "long", day }`):
/// "Saturday, November 7, 2026" in the system's field order, from a yyyymmdd day ("" for 0).
pub fn day_label(day: i32) -> String {
    Date::from_day_number(day).map_or_else(String::new, |date| Regional::get().long_date(date))
}

fn decrypt_error_text(error: &DecryptError) -> &'static str {
    match error {
        DecryptError::Locked => "Encrypted message — unlock encryption to read it",
        DecryptError::NoKey => "Encrypted with a previous key — can't be read on this device",
        DecryptError::PeerKey => "Encrypted message — the sender's key isn't available",
        _ => "Encrypted message couldn't be decrypted",
    }
}

fn kind_label(kinds: &[AttachmentKind]) -> String {
    let one = |kind: AttachmentKind| match kind {
        AttachmentKind::Image => "Image",
        AttachmentKind::Video => "Video",
        AttachmentKind::Document => "File",
        AttachmentKind::Audio => "Audio",
    };
    match kinds {
        [only] => one(*only).to_string(),
        [first, rest @ ..] if rest.iter().all(|kind| kind == first) => format!("{} {}s", kinds.len(), one(*first)),
        _ => format!("{} Attachments", kinds.len()),
    }
}

fn kind_glyph(kinds: &[AttachmentKind]) -> IconName {
    match kinds {
        [first, rest @ ..] if rest.iter().all(|kind| kind == first) => match first {
            AttachmentKind::Image => IconName::ChatKindImage,
            AttachmentKind::Video => IconName::ChatKindVideo,
            AttachmentKind::Audio => IconName::ChatKindAudio,
            AttachmentKind::Document => IconName::ChatKindDocument,
        },
        _ => IconName::ChatKindMixed,
    }
}

fn first_letter(name: &str) -> String {
    name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into())
}

/// The names the code block header shows (`langResolved ? lang : "code"`), upper-cased by CSS.
fn code_label(lang: Option<decibell_richtext::Lang>) -> String {
    lang.map_or("CODE".to_string(), |lang| lang.tag().to_uppercase())
}

impl RowLayout {
    /// Lays the row out at `width` (the list's content width). `ready` is called on the UI
    /// thread when a code block's colours arrive (the row is then built again; its height does
    /// not change).
    pub fn build(spec: &RowSpec<'_>, width: f32, style: &ChatStyle, engine: &Rc<Engine>, cx: &mut App, ready: &dyn Fn() -> Box<dyn FnOnce(&mut App)>) -> RowLayout {
        let message = spec.message;
        let facts = spec.facts;
        let scale = style.scale;
        let grouped = facts.grouped;
        let content_x = ROW_PAD_LEFT + AVATAR + GUTTER;
        let content_width = (width - content_x - ROW_PAD_RIGHT).max(1.);
        let label = |text: &str, text_style: &decibell_text::TextStyle, wrapping: Wrapping, max: f32| {
            engine.label_layout(text, text_style, wrapping, max, scale)
        };

        let divider = facts.new_day.then(|| label(&day_label(message.day), &style.day, Wrapping::NOWRAP, f32::INFINITY));
        let mut y = match &divider {
            Some(layout) => DIVIDER_PAD_TOP + layout.height() + DIVIDER_PAD_BOTTOM,
            None => 0.,
        };
        let bubble_y = y;
        y = 0.;

        if let Some(notice) = &spec.notice {
            return Self::card_row(spec, notice, divider, bubble_y, width, style, engine);
        }

        // The quoted line (only a row that opens a group has one: a reply never groups).
        let mut reply = None;
        if let (Some(spec_reply), false) = (&spec.reply, grouped) {
            let mut line = ReplyLine {
                avatar_letter: None,
                name: None,
                snippet: None,
                glyph: None,
                name_color: spec.reply_colors.0,
                avatar_color: spec.reply_colors.1,
                author: 0,
                name_x: REPLY_AVATAR + REPLY_GAP,
                snippet_x: 0.,
                glyph_x: 0.,
            };
            let mut x = REPLY_AVATAR + REPLY_GAP;
            match &spec_reply.author {
                Some((uid, shown, account)) => {
                    line.author = *uid;
                    line.avatar_letter = Some(label(&first_letter(account), &style.reply_avatar_letter, Wrapping::NOWRAP, f32::INFINITY));
                    let name = label(shown, &style.reply_name, Wrapping::TRUNCATE, REPLY_NAME_MAX.min(content_width - x));
                    x += name.width() + REPLY_GAP;
                    line.name = Some(name);
                    let has_text = !spec_reply.content.trim().is_empty();
                    let glyph = (!spec_reply.kinds.is_empty()).then(|| kind_glyph(&spec_reply.kinds));
                    let room = (content_width - x - if glyph.is_some() { ACTION_GLYPH + REPLY_GAP } else { 0. }).max(1.);
                    let text = if has_text {
                        // The preview form: one line, markup gone, block content flattened.
                        let document = decibell_richtext::plain::preview(&spec_reply.content);
                        Some(decibell_richtext::plain::as_displayed(&document, &spec_reply.content).replace('\n', " "))
                    } else if glyph.is_some() {
                        Some(kind_label(&spec_reply.kinds))
                    } else {
                        None
                    };
                    line.snippet_x = x;
                    if let Some(text) = text {
                        let snippet = label(&text, &style.reply, Wrapping::TRUNCATE, room);
                        x += snippet.width() + REPLY_GAP;
                        line.snippet = Some(snippet);
                    }
                    line.glyph = glyph;
                    line.glyph_x = x;
                }
                None => {
                    line.snippet_x = x;
                    line.snippet = Some(label("Original message was deleted", &style.reply, Wrapping::TRUNCATE, (content_width - x).max(1.)));
                }
            }
            reply = Some(line);
        }

        let pad_top = if grouped { GROUPED_PAD_Y } else { ROW_PAD_TOP };
        y += pad_top;
        if reply.is_some() {
            y += style.reply_line + REPLY_MARGIN_BOTTOM;
        }
        let flex_top = y;

        // The header (`flex items-baseline gap-2`): the tallest ascent and descent set the line.
        let (mut name, mut time, mut avatar_letter) = (None, None, None);
        let (mut header_baseline, mut header_height) = (0.0f32, 0.0f32);
        if grouped {
            if let Some(secs) = message.timestamp {
                // `formatClockCompact`: the clock without AM / PM.
                let clock = Regional::get().clock_compact(secs, spec.time_format);
                time = Some(label(&clock, &style.hover_time, Wrapping::NOWRAP, f32::INFINITY));
            }
        } else {
            let name_layout = label(&spec.display_name, &style.name, Wrapping::NOWRAP, f32::INFINITY);
            // `formatTimestamp`: the time today, "Yesterday, at …", else the short date and the time.
            let stamp = message.timestamp.map(|secs| Regional::get().message_stamp(secs, spec.today, spec.time_format)).unwrap_or_default();
            let time_layout = label(&stamp, &style.time, Wrapping::NOWRAP, f32::INFINITY);
            header_baseline = name_layout.first_baseline().max(time_layout.first_baseline());
            let below = (name_layout.height() - name_layout.first_baseline()).max(time_layout.height() - time_layout.first_baseline());
            header_height = layout_unit(header_baseline + below);
            avatar_letter = Some(label(&first_letter(&spec.user_name), &style.avatar_letter, Wrapping::NOWRAP, f32::INFINITY));
            name = Some(name_layout);
            time = Some(time_layout);
        }

        // The content column.
        let mut cy = header_height;
        let mut codes = Vec::new();
        let mut maths = Vec::new();
        let mut body_y = 0.;
        let mut trailing_margin = 0.;
        let body = if spec.editing {
            cy += BODY_MARGIN_TOP;
            body_y = cy;
            cy += spec.editor_height;
            Body::Editor { height: spec.editor_height }
        } else if let Some(error) = &message.decrypt_error {
            if !grouped {
                cy += BODY_MARGIN_TOP;
            }
            body_y = cy;
            // `flex items-center gap-1.5`: a 12 px lock, then the line.
            let layout = label(decrypt_error_text(error), &style.notice, Wrapping::NORMAL, (content_width - 18.).max(1.));
            cy += layout.height().max(12.);
            Body::Notice { layout }
        } else if message.content.is_empty() || spec.text_hidden {
            Body::None
        } else {
            if !grouped {
                cy += BODY_MARGIN_TOP;
            }
            body_y = cy;
            let document = Rc::new(decibell_richtext::parse(&message.content, &ParseOptions::default()));
            let source: &str = &message.content;
            // Code blocks and display formulas first: the body's layout asks for their heights.
            let mut code_index = 0u32;
            // Two blocks in a row: their `my-1` margins collapse into one (CSS), so the second
            // one has no top margin of its own.
            let mut after_block = false;
            for (index, span) in document.spans().iter().enumerate() {
                let text = document.text(source, span);
                let margin_top = if after_block { 0. } else { CODE_MARGIN_Y };
                match span.kind {
                    SpanKind::CodeBlock { lang } => {
                        let expanded = spec.expanded >> code_index.min(63) & 1 == 1;
                        code_index += 1;
                        codes.push(code_block(index as u32, text, lang, expanded, margin_top, content_width, style, engine, cx, ready));
                    }
                    SpanKind::DisplayMath => maths.push(math_block(index as u32, text, margin_top, content_width, style, engine)),
                    _ => {}
                }
                after_block = span.is_block();
            }
            let rich_style = if message.pending { &style.rich_pending } else { &style.rich };
            let key = {
                use std::hash::{Hash, Hasher};
                let mut hasher = decibell_text::KeyHasher::default();
                message.content.as_ref().hash(&mut hasher);
                spec.expanded.hash(&mut hasher);
                hasher.finish()
            };
            let layout = engine.rich_layout(
                key,
                RichContent { document: &document, source, edited: facts.edited },
                rich_style,
                content_width,
                scale,
                &mut |span, kind, _| match kind {
                    SlotKind::Code => codes.iter().find(|code| code.span == span).map_or(0., |code| code.height),
                    SlotKind::Math => maths.iter().find(|math| math.span == span).map_or(0., |math| math.height),
                },
            );
            for slot in layout.slots() {
                match slot.kind {
                    SlotKind::Code => codes.iter_mut().filter(|code| code.span == slot.span).for_each(|code| code.y = slot.y),
                    SlotKind::Math => maths.iter_mut().filter(|math| math.span == slot.span).for_each(|math| math.y = slot.y),
                }
            }
            // A block first in the body: its top margin and the body's `mt-0.5` collapse (the
            // larger one, the block's, is what separates it from the header).
            if !grouped && layout.slots().next().is_some_and(|slot| slot.y == 0.) {
                cy -= BODY_MARGIN_TOP;
                body_y = cy;
            }
            // A block last in the body: its bottom margin collapses with the `mt-1` of what
            // follows (the attachment list, the embeds).
            if layout.slots().last().is_some_and(|slot| (slot.y + slot.height - layout.height()).abs() < 0.01) {
                trailing_margin = CODE_MARGIN_Y;
            }
            cy += layout.height();
            Body::Rich { layout, document }
        };

        // The slots under the body.
        let mut media = Vec::new();
        let mut slots = [(0., 0.); 3];
        let space = SlotSpace { width: content_width, ..spec.space };
        let attachments = match spec.slot_heights[SLOT_ATTACHMENTS] {
            Some(height) => height,
            None => attachment_boxes(message, space, &mut media),
        };
        let inflight = spec.slot_heights[SLOT_INFLIGHT].unwrap_or(0.);
        let embeds = if spec.editing { 0. } else { spec.slot_heights[SLOT_EMBEDS].unwrap_or(0.) };
        for (slot, height) in [(SLOT_ATTACHMENTS, attachments), (SLOT_INFLIGHT, inflight), (SLOT_EMBEDS, embeds)] {
            if height > 0. {
                // The first slot under a body that ends in a block overlaps its own top margin.
                cy -= std::mem::take(&mut trailing_margin);
            }
            slots[slot] = (cy, height);
            cy += height;
        }

        let height = if grouped {
            let gutter = time.as_ref().map_or(0., |time| time.height());
            GROUPED_PAD_Y + cy.max(gutter) + GROUPED_PAD_Y
        } else {
            flex_top + cy.max(AVATAR) + ROW_PAD_BOTTOM
        };
        for slot in &mut slots {
            slot.0 += flex_top;
        }

        RowLayout {
            height: layout_unit(bubble_y + height),
            message: message.clone(),
            facts,
            grouped,
            shown_name: spec.display_name.clone(),
            bubble_y,
            content_x,
            content_width,
            divider,
            reply,
            avatar_letter,
            avatar_color: spec.avatar_color,
            name_color: spec.name_color,
            name,
            time,
            header_y: flex_top,
            header_baseline,
            body,
            body_y: flex_top + body_y,
            codes,
            maths,
            slots,
            media,
            sender: message.sender_uid,
            overlaid: std::cell::Cell::new(false),
        }
    }

    /// A notice card row (`<div className="pl-4 pr-2 py-1.5"><ErrorCard>…</ErrorCard></div>`):
    /// the card is at most 520 px wide, its paragraph wraps beside the 32 px icon, and the row is
    /// the card plus 6 px above and below (exact, as every row's height).
    fn card_row(spec: &RowSpec<'_>, notice: &RowNotice, divider: Option<Rc<Layout>>, bubble_y: f32, width: f32, style: &ChatStyle, engine: &Rc<Engine>) -> RowLayout {
        let message = spec.message;
        let card_width = (width - CARD_ROW_PAD_LEFT - CARD_ROW_PAD_RIGHT).clamp(1., CARD_MAX_WIDTH);
        let text_width = (card_width - 2. * CARD_BORDER - 2. * CARD_PAD_X - CARD_ICON - CARD_GAP).max(1.);
        let mut builder = decibell_text::ParagraphBuilder::new(engine, &style.card_text, Wrapping::NORMAL, style.scale);
        builder.text(&notice.lead, &style.card_lead, 0);
        builder.text(&format!(" {}", notice.text), &style.card_text, 1);
        let layout = Rc::new(Layout::new(Rc::new(builder.build()), text_width, engine));
        let inner = layout.height().max(CARD_ICON_MARGIN_TOP + CARD_ICON);
        let card_height = 2. * CARD_BORDER + 2. * CARD_PAD_Y + inner;
        let card = [CARD_ROW_PAD_LEFT, CARD_ROW_PAD_Y, card_width, card_height];
        let content_x = CARD_ROW_PAD_LEFT + CARD_BORDER + CARD_PAD_X + CARD_ICON + CARD_GAP;
        RowLayout {
            height: layout_unit(bubble_y + CARD_ROW_PAD_Y + card_height + CARD_ROW_PAD_Y),
            message: message.clone(),
            facts: spec.facts,
            grouped: false,
            shown_name: spec.display_name.clone(),
            bubble_y,
            content_x,
            content_width: text_width,
            divider,
            reply: None,
            avatar_letter: None,
            avatar_color: spec.avatar_color,
            name_color: spec.name_color,
            name: None,
            time: None,
            header_y: 0.,
            header_baseline: 0.,
            body: Body::Card { card, layout },
            body_y: CARD_ROW_PAD_Y + CARD_BORDER + CARD_PAD_Y,
            codes: Vec::new(),
            maths: Vec::new(),
            slots: [(0., 0.); 3],
            media: Vec::new(),
            sender: message.sender_uid,
            overlaid: std::cell::Cell::new(false),
        }
    }

    /// A notice card instead of a message bubble.
    pub fn is_card(&self) -> bool {
        matches!(self.body, Body::Card { .. })
    }

    pub fn sender(&self) -> Uid {
        self.sender
    }

    /// The row's top edge for a jump or a hover: under the day divider.
    pub fn bubble_top(&self) -> f32 {
        self.bubble_y
    }

    pub fn document(&self) -> Option<&Rc<Document>> {
        match &self.body {
            Body::Rich { document, .. } => Some(document),
            _ => None,
        }
    }

    pub fn rich(&self) -> Option<&Rc<RichLayout>> {
        match &self.body {
            Body::Rich { layout, .. } => Some(layout),
            _ => None,
        }
    }

    /// The text the row draws for its day divider and for its time (the header's, or a grouped
    /// row's hover time), as laid out.
    pub fn divider_text(&self) -> Option<&str> {
        self.divider.as_ref().map(|layout| layout.paragraph().text())
    }

    pub fn time_text(&self) -> Option<&str> {
        self.time.as_ref().map(|layout| layout.paragraph().text())
    }

    /// The box of slot `index` (attachments, inflight, embeds) in the row, when it is not empty.
    pub fn slot_bounds(&self, index: usize) -> Option<[f32; 4]> {
        let (y, height) = self.slots[index];
        (height > 0.).then_some([self.content_x, self.bubble_y + y, self.content_width, height])
    }

    /// The body's box in the row: [x, y, width, height].
    pub fn body_bounds(&self) -> [f32; 4] {
        let height = self.rich().map_or(0., |layout| layout.height());
        [self.content_x, self.bubble_y + self.body_y, self.content_width, height]
    }

    /// The editor's box while the row is edited.
    pub fn editor_bounds(&self) -> Option<[f32; 4]> {
        match self.body {
            Body::Editor { height } => Some([self.content_x, self.bubble_y + self.body_y, self.content_width, height]),
            _ => None,
        }
    }

    /// Bytes this layout keeps beyond what the text engine's caches own.
    pub fn retained_bytes(&self) -> usize {
        size_of::<RowLayout>()
            + self.codes.capacity() * size_of::<CodeBlockLayout>()
            + self.maths.capacity() * size_of::<MathBlockLayout>()
            + self.media.capacity() * size_of::<MediaBox>()
    }

    fn name_x(&self) -> f32 {
        self.content_x
    }

    fn time_origin(&self) -> (f32, f32) {
        let Some(time) = &self.time else { return (0., 0.) };
        if self.grouped {
            // `justify-end` in the 38 px gutter, at the top (`leading-none`).
            (ROW_PAD_LEFT + AVATAR - time.width(), self.bubble_y + GROUPED_PAD_Y)
        } else {
            let name_width = self.name.as_ref().map_or(0., |name| name.width());
            (self.content_x + name_width + HEADER_GAP, self.bubble_y + self.header_y + self.header_baseline - time.first_baseline())
        }
    }

    fn name_origin(&self) -> (f32, f32) {
        let baseline = self.name.as_ref().map_or(0., |name| name.first_baseline());
        (self.name_x(), self.bubble_y + self.header_y + self.header_baseline - baseline)
    }

    /// The action buttons' boxes, right to left as they are laid out: (action, [x, y, w, h]).
    pub fn actions(&self, width: f32, can_delete: bool, has_reply: bool, has_edit: bool) -> impl Iterator<Item = (RowAction, [f32; 4])> {
        let shown = [
            (RowAction::Reply, self.facts.can_reply && has_reply),
            (RowAction::Edit, self.facts.can_edit && has_edit),
            (RowAction::Delete, can_delete),
        ];
        let count = shown.iter().filter(|(_, on)| *on).count() as f32;
        let top = self.bubble_y + if self.grouped { 0. } else { 4. };
        let mut x = width - ACTION_RIGHT - count * ACTION - (count - 1.).max(0.) * ACTION_GAP;
        // The inline editor and a notice card have no actions.
        let editing = matches!(self.body, Body::Editor { .. } | Body::Card { .. });
        shown.into_iter().filter(move |(_, on)| *on && !editing).map(move |(action, _)| {
            let bounds = [x, top, ACTION, ACTION];
            x += ACTION + ACTION_GAP;
            (action, bounds)
        })
    }

    /// What is at (`x`, `y`) in the row's own coordinates.
    pub fn hit(&self, x: f32, y: f32, width: f32, hovered: bool, can_delete: bool) -> RowHit {
        let inside = |b: [f32; 4]| x >= b[0] && x < b[0] + b[2] && y >= b[1] && y < b[1] + b[3];
        let body_start = RowPos { part: Part::Body, pos: RichPos::default() };
        if y < self.bubble_y {
            // The day divider: the selection treats it as the row's start.
            return RowHit { target: Target::None, pos: RowPos::START };
        }
        if hovered {
            for (action, bounds) in self.actions(width, can_delete, true, true) {
                if inside(bounds) {
                    return RowHit { target: Target::Action(action), pos: body_start };
                }
            }
        }
        if let Some(reply) = &self.reply {
            let top = self.bubble_y + ROW_PAD_TOP;
            let line_height = self.header_y - ROW_PAD_TOP - REPLY_MARGIN_BOTTOM;
            if y >= top && y < top + line_height && x >= self.content_x {
                let local = x - self.content_x;
                let end = reply.glyph_x + if reply.glyph.is_some() { ACTION_GLYPH } else { 0. };
                let target = match &reply.name {
                    Some(name) if local >= reply.name_x && local < reply.name_x + name.width() && reply.author != 0 => Target::ReplyName,
                    Some(_) if local < end => Target::ReplyLine,
                    _ => Target::None,
                };
                return RowHit { target, pos: RowPos::START };
            }
        }
        if !self.grouped && self.name.is_some() {
            let avatar = [ROW_PAD_LEFT, self.bubble_y + self.header_y, AVATAR, AVATAR];
            if inside(avatar) {
                return RowHit { target: Target::Sender, pos: RowPos::START };
            }
            if let (Some(name), Some(time)) = (&self.name, &self.time) {
                let (name_x, name_y) = self.name_origin();
                let (time_x, time_y) = self.time_origin();
                let header_bottom = self.bubble_y + self.header_y + name.height().max(time.height());
                if y < header_bottom || matches!(self.body, Body::None | Body::Editor { .. }) && y < header_bottom {
                    // Left of the time's start belongs to the name, the rest to the time.
                    return if x < name_x + name.width() + HEADER_GAP / 2. {
                        let hit = name.hit(x - name_x, y - name_y);
                        let over = x >= name_x && x < name_x + name.width();
                        RowHit { target: if over { Target::Sender } else { Target::None }, pos: RowPos::at(Part::Name, hit.pos.offset) }
                    } else {
                        let hit = time.hit(x - time_x, y - time_y);
                        let over = x >= time_x && x < time_x + time.width();
                        RowHit { target: if over { Target::Text } else { Target::None }, pos: RowPos::at(Part::Time, hit.pos.offset) }
                    };
                }
            }
        }
        for boxed in &self.media {
            let (slot_y, _) = self.slots[SLOT_ATTACHMENTS];
            if inside([self.content_x + boxed.x, self.bubble_y + slot_y + boxed.y, boxed.width, boxed.height]) && boxed.kind != MediaBoxKind::Grid {
                return RowHit { target: Target::Media, pos: RowPos::END };
            }
        }
        match &self.body {
            Body::Rich { layout, .. } => {
                let (local_x, local_y) = (x - self.content_x, y - self.bubble_y - self.body_y);
                let hit = layout.hit(local_x, local_y);
                let pos = RowPos { part: Part::Body, pos: hit.pos };
                let over = local_x >= 0. && local_x < self.content_width && local_y >= 0. && local_y < layout.height();
                if !over {
                    return RowHit { target: Target::None, pos };
                }
                if let Some(slot) = hit.slot {
                    if let Some(code) = self.codes.iter().find(|code| code.span == slot.span) {
                        return RowHit { target: code.target_at(local_x, local_y - code.y), pos };
                    }
                    return RowHit { target: Target::None, pos };
                }
                let target = match layout.link_at(local_x, local_y) {
                    Some(span) => Target::Link(span),
                    None if hit.span.is_some() => Target::Text,
                    None => Target::None,
                };
                RowHit { target, pos }
            }
            Body::Notice { .. } | Body::None | Body::Editor { .. } | Body::Card { .. } => {
                RowHit { target: Target::None, pos: if y < self.bubble_y + self.body_y { RowPos::START } else { RowPos::END } }
            }
        }
    }

    /// Clamps a position to this row's real content (a [`RowPos::START`] / [`RowPos::END`]
    /// sentinel, or a part the row doesn't have).
    pub fn resolve(&self, pos: RowPos, toward_end: bool) -> RowPos {
        let body_len = |end: bool| match &self.body {
            Body::Rich { layout, .. } => Some(if end { layout.end() } else { layout.start() }),
            _ => None,
        };
        let has_header = self.name.is_some();
        match pos.part {
            Part::Name | Part::Time if !has_header => match body_len(false) {
                Some(start) => RowPos { part: Part::Body, pos: start },
                None => pos,
            },
            Part::Body => match &self.body {
                Body::Rich { layout, .. } => RowPos { part: Part::Body, pos: pos.pos.min(layout.end()) },
                _ if has_header => {
                    // No body: the end of the time stands for it.
                    let len = self.time.as_ref().map_or(0, |time| time.paragraph().text().len() as u32);
                    RowPos::at(Part::Time, if toward_end || pos.pos.block > 0 { len } else { 0 })
                }
                _ => pos,
            },
            _ => pos,
        }
    }

    /// The word (`count` 2) or the block (`count` 3) around a position: a double / triple click.
    pub fn unit_at(&self, pos: RowPos, count: usize) -> (RowPos, RowPos) {
        let label_unit = |layout: &Rc<Layout>, part: Part| {
            let (from, to) = if count == 2 { layout.word_at(pos.pos.offset) } else { (0, layout.paragraph().text().len() as u32) };
            (RowPos::at(part, from), RowPos::at(part, to))
        };
        match (pos.part, &self.name, &self.time, &self.body) {
            (Part::Name, Some(name), _, _) => label_unit(name, Part::Name),
            (Part::Time, _, Some(time), _) => label_unit(time, Part::Time),
            (Part::Body, _, _, Body::Rich { layout, .. }) => {
                let (from, to) = if count == 2 { layout.word_at(pos.pos) } else { rich_block(layout, pos.pos.block) };
                (RowPos { part: Part::Body, pos: from }, RowPos { part: Part::Body, pos: to })
            }
            _ => (pos, pos),
        }
    }

    /// What a selection from `from` to `to` (both in this row) adds to a copy: what Chromium's
    /// Ctrl+C serialises from the renderer's DOM for the same selection (measured against the
    /// Electron client: see [`CopyText`]). The header's name and time and the body are blocks
    /// of their own; the avatar, the quoted reply line, the day divider and a grouped row's hover
    /// time are `user-select: none` and copy nothing; "(edited)" and the attachments' text come
    /// with the body's end.
    pub fn copy_text(&self, from: RowPos, to: RowPos, out: &mut CopyText) {
        let label_text = |layout: &Rc<Layout>, a: u32, b: u32| -> String {
            let text = layout.paragraph().text();
            let (a, b) = ((a as usize).min(text.len()), (b as usize).min(text.len()));
            text.get(a..b).unwrap_or("").to_string()
        };
        out.newlines(1);
        if let (Some(name), true) = (&self.name, from.part == Part::Name) {
            let end = if to.part == Part::Name { to.pos.offset } else { u32::MAX };
            out.text(&label_text(name, from.pos.offset, end));
            out.newlines(1);
        }
        if let (Some(time), true) = (&self.time, !self.grouped && from.part <= Part::Time && to.part >= Part::Time) {
            let start = if from.part == Part::Time { from.pos.offset } else { 0 };
            let end = if to.part == Part::Time { to.pos.offset } else { u32::MAX };
            out.text(&label_text(time, start, end));
            out.newlines(1);
        }
        match (&self.body, to.part == Part::Body) {
            (Body::Rich { layout, document }, true) => {
                let start = if from.part == Part::Body { from.pos } else { layout.start() };
                let end = to.pos.min(layout.end());
                let text = decibell_richtext::plain::as_copied_range(
                    document,
                    &self.message.content,
                    layout.text_pos(start, document),
                    layout.text_pos(end, document),
                );
                // `<span class="ml-1 select-none">(edited)</span>`: the global `.select-text *`
                // rule (`!important`) makes it selectable after all; its margin is no space.
                let edited = self.facts.edited && end >= layout.end();
                if text.is_empty() {
                    out.text(if edited { "(edited)" } else { "" });
                } else {
                    out.text(&text);
                    if edited {
                        out.append("(edited)");
                    }
                }
            }
            (Body::Notice { layout }, true) => out.text(&label_text(layout, 0, u32::MAX)),
            (Body::Card { layout, .. }, true) => out.text(&label_text(layout, 0, u32::MAX)),
            _ => {}
        }
        // The attachments come with the end of the row (a selection can't end inside them).
        if to >= self.resolve(RowPos::END, true) {
            out.newlines(1);
            attachments_copy(&self.message, out);
        }
    }
}

/// A copy being written: text with the line breaks between blocks held until more text comes
/// (Chromium's `TextIterator` writes a block's closing line break only before what follows it,
/// so a copy never ends in one; a "blank line" is two).
#[derive(Default)]
pub struct CopyText {
    out: String,
    pending: usize,
}

impl CopyText {
    /// At least `count` line breaks before the next text.
    pub fn newlines(&mut self, count: usize) {
        self.pending = self.pending.max(count);
    }

    /// `text` after the held line breaks (nothing for empty text).
    pub fn text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.out.is_empty() {
            self.out.extend(std::iter::repeat_n('\n', self.pending));
        }
        self.pending = 0;
        self.out.push_str(text);
    }

    /// `text` right after what came before, whatever line breaks were held (an inline element).
    pub fn append(&mut self, text: &str) {
        self.pending = 0;
        self.out.push_str(text);
    }

    pub fn finish(self) -> String {
        self.out
    }
}

/// A whole row's copy, what [`RowLayout::copy_text`] adds for a selection over all of it:
/// `header` = (the name as shown, the header's time) of a row that opens a group.
pub fn whole_row_copy(header: Option<(&str, &str)>, message: &Message, edited: bool, out: &mut CopyText) {
    out.newlines(1);
    if let Some((name, time)) = header {
        out.text(name);
        out.newlines(1);
        out.text(time);
        out.newlines(1);
    }
    if message.decrypt_error.is_none() && !message.content.is_empty() {
        let document = decibell_richtext::parse(&message.content, &decibell_richtext::ParseOptions::default());
        let end = decibell_richtext::plain::TextPos { span: document.spans().len(), offset: 0 };
        let text = decibell_richtext::plain::as_copied_range(&document, &message.content, decibell_richtext::plain::TextPos { span: 0, offset: 0 }, end);
        if text.is_empty() {
            out.text(if edited { "(edited)" } else { "" });
        } else {
            out.text(&text);
            if edited {
                out.append("(edited)");
            }
        }
    }
    out.newlines(1);
    attachments_copy(message, out);
}

/// JavaScript's `n.toFixed(1)` (ties round up, as the exact decimal value is compared).
fn to_fixed_1(value: f64) -> String {
    format!("{:.1}", (value * 10.).round() / 10.)
}

/// `formatFileSize` (attachmentHelpers.ts).
pub fn format_file_size(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    if bytes < 1024. {
        format!("{bytes} B")
    } else if bytes < 1024. * 1024. {
        format!("{} KB", to_fixed_1(bytes / 1024.))
    } else if bytes < 1024. * 1024. * 1024. {
        format!("{} MB", to_fixed_1(bytes / 1024. / 1024.))
    } else {
        format!("{} GB", to_fixed_1(bytes / 1024. / 1024. / 1024.))
    }
}

/// `formatDuration`: whole seconds, rounded.
fn format_duration(ms: u32) -> String {
    let total = (ms as f64 / 1000.).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// The audio card's clock (`fmt`: seconds floored).
fn format_clock(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0. {
        return "0:00".into();
    }
    format!("{}:{:02}", (seconds / 60.).floor() as u64, (seconds % 60.).floor() as u64)
}

/// The attachment list's share of a copy (`AttachmentList.tsx` as Chromium serialises it,
/// measured against the Electron client): the pictures and videos first (their `<img alt>` is
/// the file name; a video card shows its name and size), a blank line after them (the download
/// button), then audio cards, documents and purged files in position order.
pub fn attachments_copy(message: &Message, out: &mut CopyText) {
    let mut sorted: Vec<&decibell_model::chat::Attachment> = message.attachments.iter().collect();
    sorted.sort_by_key(|a| a.position);
    let (media, rest): (Vec<_>, Vec<_>) =
        sorted.into_iter().partition(|a| a.purged_at == 0 && matches!(a.kind, AttachmentKind::Image | AttachmentKind::Video));
    for attachment in &media {
        out.newlines(1);
        out.text(&attachment.filename);
        if attachment.kind == AttachmentKind::Video {
            // The caption's name and size (or "Paused at …" once played; not tracked here).
            out.newlines(1);
            out.text(&format_file_size(attachment.size_bytes));
        }
    }
    if !media.is_empty() {
        out.newlines(2);
    }
    for attachment in rest {
        if attachment.purged_at > 0 {
            let name = if attachment.filename.is_empty() { "Attachment" } else { &attachment.filename };
            out.newlines(1);
            out.text(&format!("{name} (deleted by retention policy)"));
            continue;
        }
        match attachment.kind {
            AttachmentKind::Audio => {
                out.newlines(1);
                out.text(&attachment.filename);
                out.newlines(1);
                let size = format_file_size(attachment.size_bytes);
                if attachment.duration_ms > 0 {
                    out.text(&format!("{} · {size}", format_duration(attachment.duration_ms)));
                } else {
                    out.text(&size);
                }
                out.newlines(3);
                out.text(&format!("0:00 / {}", format_clock(attachment.duration_ms as f64 / 1000.)));
                out.newlines(2);
            }
            _ => {
                out.newlines(2);
                out.text(&attachment.filename);
                out.newlines(1);
                out.text(&format_file_size(attachment.size_bytes));
            }
        }
    }
}

/// The start and end of block `block` of a rich layout (a paragraph between code blocks).
fn rich_block(layout: &RichLayout, block: u32) -> (RichPos, RichPos) {
    // Blocks are text layouts and slots in y order: walk both lists together.
    let mut texts = layout.texts().peekable();
    let mut slots = layout.slots().peekable();
    let mut index = 0u32;
    loop {
        let text_first = match (texts.peek(), slots.peek()) {
            (Some((text_y, _)), Some(slot)) => *text_y <= slot.y,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return (layout.start(), layout.end()),
        };
        let len = if text_first { texts.next().map_or(0, |(_, text)| text.paragraph().text().len() as u32) } else { slots.next().map_or(1, |_| 1) };
        if index == block {
            let at = |offset, affinity| RichPos { block, offset, affinity };
            return (at(0, decibell_text::Affinity::Downstream), at(len, decibell_text::Affinity::Upstream));
        }
        index += 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn code_block(
    span: u32,
    code: &str,
    lang: Option<decibell_richtext::Lang>,
    expanded: bool,
    margin_top: f32,
    available: f32,
    style: &ChatStyle,
    engine: &Rc<Engine>,
    cx: &mut App,
    ready: &dyn Fn() -> Box<dyn FnOnce(&mut App)>,
) -> CodeBlockLayout {
    let scale = style.scale;
    let language = lang.and_then(|lang| Language::from_name(lang.grammar()));
    let spans = language.and_then(|language| engine.highlighted(language, code, cx, ready()));
    let text = engine.code_layout(code, spans.as_deref(), &style.code, Wrapping::PRE, f32::INFINITY, scale);
    let lines = code.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let collapsible = lines > CODE_COLLAPSE_LINES;
    let collapsed = collapsible && !expanded;
    let label = engine.label_layout(&code_label(lang), &style.code_label, Wrapping::NOWRAP, f32::INFINITY, scale);
    let foot = collapsible.then(|| {
        let text = if expanded { "COLLAPSE".to_string() } else { format!("EXPAND · {lines} LINES") };
        engine.label_layout(&text, &style.code_foot, Wrapping::NOWRAP, f32::INFINITY, scale)
    });

    let text_height = text.height();
    // A collapsed block scrolls inside `max-h-60`; its bar takes 10 px of the width.
    let mut vbar = collapsed && 2. * CODE_PAD_Y + text_height > CODE_COLLAPSED_MAX;
    let header = CODE_PAD_X + label.width() + 12. + 24. + CODE_PAD_X;
    let fit = |vbar: bool| (text.width() + 2. * CODE_PAD_X + if vbar { BAR } else { 0. }).max(header) + 2. * CODE_BORDER;
    // `w-fit min-w-48 max-w-full`.
    let width = fit(vbar).max(CODE_MIN_WIDTH).min(available);
    let room = |vbar: bool| width - 2. * CODE_BORDER - 2. * CODE_PAD_X - if vbar { BAR } else { 0. };
    let mut hbar = text.width() > room(vbar) + 0.01;
    if collapsed && hbar && !vbar && 2. * CODE_PAD_Y + text_height + BAR > CODE_COLLAPSED_MAX {
        vbar = true;
        hbar = text.width() > room(vbar) + 0.01;
    }
    let natural = 2. * CODE_PAD_Y + text_height + if hbar { BAR } else { 0. };
    let pre_height = if collapsed { natural.min(CODE_COLLAPSED_MAX) } else { natural };
    let foot_height = foot.as_ref().map_or(0., |foot| CODE_BORDER + 2. * CODE_FOOT_PAD_Y + foot.height().max(10.));
    CodeBlockLayout {
        span,
        y: 0.,
        height: layout_unit(margin_top + CODE_MARGIN_Y + 2. * CODE_BORDER + CODE_HEADER + pre_height + foot_height),
        margin_top,
        width,
        lines,
        collapsible,
        expanded,
        pre_height,
        hbar,
        vbar,
        language,
        label,
        foot,
        text,
        highlighted: spans.is_some(),
    }
}

fn math_block(span: u32, tex: &str, margin_top: f32, available: f32, style: &ChatStyle, engine: &Rc<Engine>) -> MathBlockLayout {
    let (formula, fallback, inner) = match engine.formula(tex, true, style.rich.size, style.scale) {
        Formula::Math(math) => {
            // `overflow-x-auto`: a formula wider than the column gets a bar.
            let height = math.height() + if math.width > available + 0.01 { BAR } else { 0. };
            (Some(math), None, height)
        }
        _ => {
            let mut error = style.notice;
            error.color = style.error;
            let layout = engine.label_layout(tex, &error, Wrapping::NORMAL, available, style.scale);
            let height = layout.height();
            (None, Some(layout), height)
        }
    };
    MathBlockLayout { span, y: 0., height: layout_unit(margin_top + MATH_MARGIN_Y + 2. * MATH_PAD_Y + inner), margin_top, formula, fallback }
}

impl CodeBlockLayout {
    /// The copy button's box, from the slot's top-left.
    fn copy_bounds(&self) -> [f32; 4] {
        [self.width - CODE_BORDER - CODE_PAD_X - 24., self.margin_top + CODE_BORDER + 4., 24., 24.]
    }

    fn pre_top(&self) -> f32 {
        self.margin_top + CODE_BORDER + CODE_HEADER
    }

    /// The range the code can scroll inside its box: (horizontal, vertical).
    pub fn scroll_range(&self) -> (f32, f32) {
        let room_x = self.width - 2. * CODE_BORDER - 2. * CODE_PAD_X - if self.vbar { BAR } else { 0. };
        let room_y = self.pre_height - if self.hbar { BAR } else { 0. };
        ((self.text.width() - room_x).max(0.), (2. * CODE_PAD_Y + self.text.height() - room_y).max(0.))
    }

    fn target_at(&self, x: f32, y: f32) -> Target {
        let copy = self.copy_bounds();
        if x >= copy[0] && x < copy[0] + copy[2] && y >= copy[1] && y < copy[1] + copy[3] {
            return Target::CodeCopy(self.span);
        }
        if x >= self.width || y < self.margin_top || y >= self.height - CODE_MARGIN_Y {
            return Target::None;
        }
        let pre_bottom = self.pre_top() + self.pre_height;
        if self.collapsible && y >= pre_bottom {
            return Target::CodeToggle(self.span);
        }
        if y >= self.pre_top() && y < pre_bottom { Target::CodeBody(self.span) } else { Target::None }
    }
}

/// What varies between two paints of the same row.
#[derive(Clone, Copy, Default)]
pub struct RowPaint<'a> {
    /// The pointer is over the row (the wash, the actions, a grouped row's time).
    pub hovered: bool,
    pub target: Target,
    /// The jump flash.
    pub highlighted: bool,
    /// The composer is replying to this row.
    pub reply_target: bool,
    pub can_delete: bool,
    /// The selection's share of the row.
    pub selection: Option<(RowPos, RowPos)>,
    /// The code block whose "copied" tick shows.
    pub copied: Option<u32>,
    /// Scroll offsets (x, y) of the row's code blocks, by span.
    pub code_scroll: &'a [(u32, f32, f32)],
    pub avatar: Option<&'a Arc<RenderImage>>,
    pub reply_avatar: Option<&'a Arc<RenderImage>>,
    /// Slots drawn by another crate: skip the placeholders.
    pub slots_filled: bool,
}

fn rect(origin: Point<Pixels>, b: [f32; 4]) -> Bounds<Pixels> {
    Bounds::new(point(origin.x + px(b[0]), origin.y + px(b[1])), size(px(b[2]), px(b[3])))
}

fn icon(name: IconName, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window, cx: &mut App) {
    let _ = window.paint_svg(bounds, SharedString::new_static(name.path()), None, TransformationMatrix::unit(), color, cx);
}

fn label_options(color: Hsla, selection: Option<(u32, u32)>, style: &ChatStyle) -> PaintOptions {
    PaintOptions {
        selection,
        selection_background: style.selection,
        selection_color: Some(style.selection_text),
        underline_tag: None,
        color: Some(color),
    }
}

#[allow(clippy::too_many_arguments)]
fn avatar(
    bounds: Bounds<Pixels>,
    radius: Pixels,
    color: Hsla,
    letter: Option<&Rc<Layout>>,
    image: Option<&Arc<RenderImage>>,
    style: &ChatStyle,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(image) = image {
        let _ = window.paint_image(bounds, bounds, Corners::all(radius), image.clone(), 0, false);
        return;
    }
    let background = linear_gradient(135., linear_color_stop(color, 0.), linear_color_stop(darker(color), 1.));
    window.paint_quad(quad(bounds, Corners::all(radius), background, Edges::default(), transparent_black(), BorderStyle::default()));
    if let Some(letter) = letter {
        let origin = point(
            bounds.origin.x + (bounds.size.width - px(letter.width())) / 2.,
            bounds.origin.y + (bounds.size.height - px(letter.height())) / 2.,
        );
        letter.paint(origin, &label_options(style.av_fg, None, style), window, cx);
    }
}

impl RowLayout {
    /// Paints the row with its top-left corner at `origin` (a whole device pixel), `width` wide.
    pub fn paint(&self, origin: Point<Pixels>, width: f32, paint: &RowPaint<'_>, style: &ChatStyle, window: &mut Window, cx: &mut App) {
        let scale = style.scale;
        // Boxes sit on whole device pixels, as Blink snaps them; text snaps its own baselines.
        let snap = |value: f32| px((value * scale).round() / scale);
        let at = |x: f32, y: f32| point(origin.x + snap(x), origin.y + snap(y));

        // The day divider: two hairlines and the date between them.
        if let Some(label) = &self.divider {
            let label_width = label.width();
            let line_y = DIVIDER_PAD_TOP + label.height() / 2.;
            let inner = width - ROW_PAD_LEFT - ROW_PAD_RIGHT;
            let side = ((inner - label_width - 2. * DIVIDER_GAP) / 2.).max(0.);
            for x in [ROW_PAD_LEFT, ROW_PAD_LEFT + side + DIVIDER_GAP + label_width + DIVIDER_GAP] {
                window.paint_quad(fill(Bounds::new(at(x, line_y), size(px(side), px(1.))), style.border));
            }
            label.paint(at(ROW_PAD_LEFT + side + DIVIDER_GAP, DIVIDER_PAD_TOP), &label_options(style.text_muted, None, style), window, cx);
        }
        if !self.overlaid.get() {
            self.paint_bubble(origin, width, paint, style, window, cx);
        }
    }

    /// Everything of the row under its day divider (`MessageBubble`; the divider is a sibling
    /// element in the renderer, outside the bubble's entrance animation).
    pub fn paint_bubble(&self, origin: Point<Pixels>, width: f32, paint: &RowPaint<'_>, style: &ChatStyle, window: &mut Window, cx: &mut App) {
        let scale = style.scale;
        let snap = |value: f32| px((value * scale).round() / scale);
        let at = |x: f32, y: f32| point(origin.x + snap(x), origin.y + snap(y));
        let faded = |color: Hsla| if self.message.pending { Hsla { a: color.a * 0.5, ..color } } else { color };

        // The row's wash: jump flash > reply rail > hover.
        let bubble = Bounds::new(at(0., self.bubble_y), size(px(width), px(self.height - self.bubble_y)));
        let railed = paint.reply_target || self.facts.replies_to_me;
        let card = matches!(self.body, Body::Card { .. });
        let wash = if paint.highlighted || railed {
            Some(style.accent_soft)
        } else if paint.hovered && !card {
            // A notice card is no `MessageBubble`: no hover wash.
            Some(style.row_hover)
        } else {
            None
        };
        if let Some(wash) = wash {
            window.paint_quad(quad(bubble, Corners::all(style.radius_lg), wash, Edges::default(), transparent_black(), BorderStyle::default()));
        }
        if railed && !paint.highlighted {
            let rail = Bounds::new(at(0., self.bubble_y + RAIL_INSET_Y), size(px(RAIL_WIDTH), px(self.height - self.bubble_y - 2. * RAIL_INSET_Y)));
            window.paint_quad(quad(rail, Corners::all(px(RAIL_WIDTH / 2.)), style.accent, Edges::default(), transparent_black(), BorderStyle::default()));
        }

        // The quoted line and its spine.
        if let Some(reply) = &self.reply {
            let top = self.bubble_y + ROW_PAD_TOP;
            let line = style.reply_line;
            // From the avatar's centre line up to the line's middle, then right to 4 px short of
            // the mini avatar; it stops 2 px above the avatar.
            let spine_left = self.content_x - (GUTTER + AVATAR / 2. + 1.);
            let spine = Bounds::new(at(spine_left, top + line / 2. - 1.), size(px(GUTTER + AVATAR / 2. + 1. - 4.), px(line / 2. + 1. + 2.)));
            window.paint_quad(quad(
                spine,
                Corners { top_left: style.radius_sm, ..Corners::default() },
                transparent_black(),
                Edges { left: px(2.), top: px(2.), ..Edges::default() },
                style.spine,
                BorderStyle::default(),
            ));
            let hovered = matches!(paint.target, Target::ReplyLine | Target::ReplyName);
            let text = if hovered && reply.name.is_some() { style.text_primary } else { style.text_muted };
            let mini = Bounds::new(at(self.content_x, top + (line - REPLY_AVATAR) / 2.), size(px(REPLY_AVATAR), px(REPLY_AVATAR)));
            match &reply.name {
                Some(name) => {
                    avatar(mini, style.reply_avatar_radius, reply.avatar_color, reply.avatar_letter.as_ref(), paint.reply_avatar, style, window, cx);
                    let mut options = label_options(reply.name_color, None, style);
                    if paint.target == Target::ReplyName {
                        options.underline_tag = Some(0);
                    }
                    name.paint(at(self.content_x + reply.name_x, top + (line - name.height()) / 2.), &options, window, cx);
                }
                None => {
                    window.paint_quad(quad(mini, Corners::all(style.reply_avatar_radius), style.surface_active, Edges::default(), transparent_black(), BorderStyle::default()));
                    let glyph = Bounds::new(mini.origin + point(px(3.), px(3.)), size(px(10.), px(10.)));
                    icon(IconName::ChatReplyDeleted, glyph, style.text_muted, window, cx);
                }
            }
            if let Some(snippet) = &reply.snippet {
                snippet.paint(at(self.content_x + reply.snippet_x, top + (line - snippet.height()) / 2.), &label_options(text, None, style), window, cx);
            }
            if let Some(glyph) = reply.glyph {
                let bounds = Bounds::new(at(self.content_x + reply.glyph_x, top + (line - ACTION_GLYPH) / 2.), size(px(ACTION_GLYPH), px(ACTION_GLYPH)));
                icon(glyph, bounds, text, window, cx);
            }
        }

        // Avatar, name, time.
        let selection_in = |part: Part| -> Option<(u32, u32)> {
            let (from, to) = paint.selection?;
            if from.part > part || to.part < part {
                return None;
            }
            Some((if from.part == part { from.pos.offset } else { 0 }, if to.part == part { to.pos.offset } else { u32::MAX }))
        };
        if let Some(name) = &self.name {
            let bounds = Bounds::new(at(ROW_PAD_LEFT, self.bubble_y + self.header_y), size(px(AVATAR), px(AVATAR)));
            avatar(bounds, style.avatar_radius, self.avatar_color, self.avatar_letter.as_ref(), paint.avatar, style, window, cx);
            let (x, y) = self.name_origin();
            let mut options = label_options(faded(self.name_color), selection_in(Part::Name), style);
            if paint.target == Target::Sender {
                options.underline_tag = Some(0);
            }
            name.paint(at(x, y), &options, window, cx);
        }
        if let Some(time) = &self.time
            && (!self.grouped || paint.hovered)
        {
            let (x, y) = self.time_origin();
            let color = if self.grouped { style.text_muted } else { faded(style.text_muted) };
            time.paint(at(x, y), &label_options(color, if self.grouped { None } else { selection_in(Part::Time) }, style), window, cx);
        }

        // The body.
        let body_origin = at(self.content_x, self.bubble_y + self.body_y);
        match &self.body {
            Body::Rich { layout, .. } => {
                let selection = match paint.selection {
                    Some((from, to)) if to.part == Part::Body => {
                        let start = if from.part == Part::Body { from.pos } else { layout.start() };
                        if start == layout.start() && to.pos >= layout.end() { RichSelection::All } else { RichSelection::Range(start, to.pos) }
                    }
                    _ => RichSelection::None,
                };
                let rich = RichPaint {
                    selection,
                    selection_background: style.selection,
                    selection_color: Some(style.selection_text),
                    hovered_link: match paint.target {
                        Target::Link(span) => Some(span),
                        _ => None,
                    },
                };
                layout.paint(body_origin, &rich, window, cx);
                for code in &self.codes {
                    let selected = slot_selected(&selection, layout, code.span);
                    let scroll = paint.code_scroll.iter().find(|(span, ..)| *span == code.span).map_or((0., 0.), |(_, x, y)| (*x, *y));
                    code.paint(point(body_origin.x, body_origin.y + snap(code.y)), paint, selected, scroll, style, window, cx);
                }
                for math in &self.maths {
                    let top = body_origin.y + snap(math.y + math.margin_top + MATH_PAD_Y);
                    let clip = Bounds::new(point(body_origin.x, top), size(px(self.content_width), px(math.height - math.margin_top - MATH_MARGIN_Y - 2. * MATH_PAD_Y)));
                    window.with_content_mask(Some(gpui::ContentMask { bounds: clip }), |window| {
                        if let Some(formula) = &math.formula {
                            formula.paint(point(body_origin.x, top + px(formula.ascent)), self.content_width, &style.math, window, cx);
                        } else if let Some(fallback) = &math.fallback {
                            fallback.paint(point(body_origin.x, top), &PaintOptions::default(), window, cx);
                        }
                    });
                }
            }
            Body::Notice { layout } => {
                let line = layout.height().max(12.);
                let lock = Bounds::new(body_origin + point(px(0.), snap((line - 12.) / 2.)), size(px(12.), px(12.)));
                icon(IconName::ChatLock, lock, style.text_muted, window, cx);
                layout.paint(body_origin + point(px(18.), snap((line - layout.height()) / 2.)), &PaintOptions::default(), window, cx);
            }
            Body::Card { card, layout } => {
                // `rounded-md border border-warning/10 bg-warning/[0.06]`.
                let bounds = Bounds::new(at(card[0], self.bubble_y + card[1]), size(snap(card[2]), snap(card[3])));
                let tint = |alpha: f32| Hsla { a: style.warning.a * alpha, ..style.warning };
                window.paint_quad(quad(bounds, Corners::all(style.radius_md), tint(0.06), Edges::all(px(CARD_BORDER)), tint(0.1), BorderStyle::default()));
                // `mt-px h-8 w-8 rounded-full bg-warning/[0.12]` with the 16 px glyph in it.
                let circle = Bounds::new(
                    at(card[0] + CARD_BORDER + CARD_PAD_X, self.bubble_y + card[1] + CARD_BORDER + CARD_PAD_Y + CARD_ICON_MARGIN_TOP),
                    size(px(CARD_ICON), px(CARD_ICON)),
                );
                window.paint_quad(quad(circle, Corners::all(px(CARD_ICON / 2.)), tint(0.12), Edges::default(), transparent_black(), BorderStyle::default()));
                let glyph = Bounds::new(circle.origin + point(px(8.), px(8.)), size(px(16.), px(16.)));
                icon(IconName::AlertTriangle, glyph, style.warning, window, cx);
                let selection = match paint.selection {
                    Some((_, to)) if to.part == Part::Body => Some((0, u32::MAX)),
                    _ => None,
                };
                let options = PaintOptions { selection, selection_background: style.selection, selection_color: Some(style.selection_text), underline_tag: None, color: None };
                layout.paint(body_origin, &options, window, cx);
            }
            Body::None | Body::Editor { .. } => {}
        }

        // The placeholders of the attachment slot.
        if !paint.slots_filled {
            let (slot_y, _) = self.slots[SLOT_ATTACHMENTS];
            for boxed in &self.media {
                // The download button shows with the row's hover (`opacity-0 group-hover/media`).
                if boxed.kind == MediaBoxKind::Download && !paint.hovered {
                    continue;
                }
                let bounds = Bounds::new(at(self.content_x + boxed.x, self.bubble_y + slot_y + boxed.y), size(px(boxed.width), px(boxed.height)));
                let (radius, background, border) = match boxed.kind {
                    MediaBoxKind::Image => (style.radius_lg, style.bg_secondary, style.border),
                    MediaBoxKind::Grid => (style.radius_md, transparent_black(), style.border_divider),
                    MediaBoxKind::Card => (style.radius_lg, style.bg_light, style.border),
                    MediaBoxKind::Download => (style.radius_sm, style.bg_secondary, style.border),
                };
                // Grid cells are square-cornered inside the grid's rounded frame.
                let in_grid = boxed.kind == MediaBoxKind::Image && self.media.first().is_some_and(|first| first.kind == MediaBoxKind::Grid);
                let (radius, edges) = if in_grid { (px(0.), Edges::default()) } else { (radius, Edges::all(px(1.))) };
                window.paint_quad(quad(bounds, Corners::all(radius), faded(background), edges, border, BorderStyle::default()));
                if boxed.kind == MediaBoxKind::Download {
                    let glyph = Bounds::new(bounds.origin + point(px(8.), px(8.)), size(px(16.), px(16.)));
                    icon(IconName::ViewerDownload, glyph, style.text_muted, window, cx);
                }
            }
        }

        // The hover actions (above other crates' slot elements when those draw: the list paints
        // them after the slots then, as the renderer's toolbar sits over the media).
        if !paint.slots_filled {
            self.paint_actions(origin, width, paint, style, window, cx);
        }
    }

    /// The hover action bar (`MessageBubble.tsx`' `renderActions`).
    pub fn paint_actions(&self, origin: Point<Pixels>, width: f32, paint: &RowPaint<'_>, style: &ChatStyle, window: &mut Window, cx: &mut App) {
        if paint.hovered {
            for (action, bounds) in self.actions(width, paint.can_delete, true, true) {
                let over = paint.target == Target::Action(action);
                let (name, color, background) = match action {
                    RowAction::Reply => (IconName::ChatReply, style.text_muted, style.row_hover),
                    RowAction::Edit => (IconName::ChatEdit, style.text_muted, style.row_hover),
                    // `text-error hover:bg-error/10`.
                    RowAction::Delete => (IconName::ChatTrash, style.error, Hsla { a: style.error.a * 0.1, ..style.error }),
                };
                let button = rect(origin, bounds);
                window.paint_quad(quad(button, Corners::all(style.radius_sm), style.bg_secondary, Edges::default(), transparent_black(), BorderStyle::default()));
                if over {
                    window.paint_quad(quad(button, Corners::all(style.radius_sm), background, Edges::default(), transparent_black(), BorderStyle::default()));
                }
                let color = if over && action != RowAction::Delete { style.text_primary } else { color };
                let inset = (ACTION - ACTION_GLYPH) / 2.;
                let glyph = Bounds::new(button.origin + point(px(inset), px(inset)), size(px(ACTION_GLYPH), px(ACTION_GLYPH)));
                icon(name, glyph, color, window, cx);
            }
        }
    }
}

fn slot_selected(selection: &RichSelection, layout: &RichLayout, span: u32) -> bool {
    match selection {
        RichSelection::None => false,
        RichSelection::All => true,
        RichSelection::Range(from, to) => {
            // The slot's block is the `n`-th block; find it by its y among the blocks.
            let Some(slot) = layout.slots().find(|slot| slot.span == span) else { return false };
            let hit = layout.hit(0., slot.y + slot.height / 4.);
            let (start, end) = (RichPos { offset: 0, ..hit.pos }, RichPos { offset: 1, ..hit.pos });
            *from <= start && end <= *to
        }
    }
}

impl CodeBlockLayout {
    #[allow(clippy::too_many_arguments)]
    fn paint(&self, origin: Point<Pixels>, paint: &RowPaint<'_>, selected: bool, scroll: (f32, f32), style: &ChatStyle, window: &mut Window, cx: &mut App) {
        let scale = style.scale;
        let snap = |value: f32| px((value * scale).round() / scale);
        let frame = Bounds::new(point(origin.x, origin.y + px(self.margin_top)), size(px(self.width), snap(self.height - self.margin_top - CODE_MARGIN_Y)));
        window.paint_quad(quad(frame, Corners::all(style.radius_md), style.bg_darkest, Edges::all(px(CODE_BORDER)), style.border, BorderStyle::default()));
        let inner_x = origin.x + px(CODE_BORDER);
        let inner_width = self.width - 2. * CODE_BORDER;

        // The header: the language, the copy button, a hairline.
        let header_top = origin.y + px(self.margin_top + CODE_BORDER);
        let label_y = header_top + px((CODE_HEADER - 1. - self.label.height()) / 2.);
        self.label.paint(point(inner_x + px(CODE_PAD_X), label_y), &label_options(style.accent_bright, None, style), window, cx);
        let copy = rect(origin, self.copy_bounds());
        let over_copy = paint.target == Target::CodeCopy(self.span);
        if over_copy {
            window.paint_quad(quad(copy, Corners::all(style.radius_sm), style.surface_hover, Edges::default(), transparent_black(), BorderStyle::default()));
        }
        let glyph = Bounds::new(copy.origin + point(px(5.), px(5.)), size(px(14.), px(14.)));
        if paint.copied == Some(self.span) {
            icon(IconName::ChatCopied, glyph, style.success, window, cx);
        } else {
            icon(IconName::ChatCopy, glyph, if over_copy { style.text_secondary } else { style.text_muted }, window, cx);
        }
        window.paint_quad(fill(Bounds::new(point(inner_x, header_top + px(CODE_HEADER - 1.)), size(px(inner_width), px(1.))), style.border_divider));

        // The code, clipped to the `<pre>`'s box and moved by its scroll offsets.
        let pre_top = origin.y + px(self.pre_top());
        let (range_x, range_y) = self.scroll_range();
        let (scroll_x, scroll_y) = (scroll.0.clamp(0., range_x), scroll.1.clamp(0., range_y));
        let view_width = inner_width - if self.vbar { BAR } else { 0. };
        let view_height = self.pre_height - if self.hbar { BAR } else { 0. };
        let clip = Bounds::new(point(inner_x, pre_top), size(px(view_width), snap(view_height)));
        window.with_content_mask(Some(gpui::ContentMask { bounds: clip }), |window| {
            let text_origin = point(inner_x + px(CODE_PAD_X - scroll_x), pre_top + px(CODE_PAD_Y - scroll_y));
            let options = PaintOptions {
                selection: selected.then_some((0, u32::MAX)),
                selection_background: style.selection,
                selection_color: Some(style.selection_text),
                ..PaintOptions::default()
            };
            self.text.paint(text_origin, &options, window, cx);
        });
        // The block's own bars (the kit's look: the thumb 2 device px in from each side).
        let inset = 2. / scale;
        if self.hbar && range_x > 0. {
            let track = view_width;
            let thumb = (track * track / (track + range_x)).max(20.).min(track);
            let left = (track - thumb) * scroll_x / range_x;
            let bar = Bounds::new(point(inner_x + px(left), pre_top + px(view_height + inset)), size(px(thumb), px(BAR - 2. * inset)));
            window.paint_quad(fill(bar, style.scrollbar));
        }
        if self.vbar && range_y > 0. {
            let track = view_height;
            let thumb = (track * track / (track + range_y)).max(20.).min(track);
            let top = (track - thumb) * scroll_y / range_y;
            let bar = Bounds::new(point(inner_x + px(view_width + inset), pre_top + px(top)), size(px(BAR - 2. * inset), px(thumb)));
            window.paint_quad(fill(bar, style.scrollbar));
        }

        // The expand bar.
        if let Some(foot) = &self.foot {
            let top = pre_top + snap(self.pre_height);
            let height = CODE_BORDER + 2. * CODE_FOOT_PAD_Y + foot.height().max(10.);
            let over = paint.target == Target::CodeToggle(self.span);
            if over {
                window.paint_quad(fill(Bounds::new(point(inner_x, top + px(1.)), size(px(inner_width), px(height - 1.))), style.surface_hover));
            }
            window.paint_quad(fill(Bounds::new(point(inner_x, top), size(px(inner_width), px(1.))), style.border_divider));
            let color = if over { style.text_secondary } else { style.text_muted };
            // `gap-1.5` between the 10 px chevron and the label, centred.
            let content = 10. + 6. + foot.width();
            let left = inner_x + px((inner_width - content) / 2.);
            let middle = top + px(1. + (height - 1.) / 2.);
            let chevron = Bounds::new(point(left, middle - px(5.)), size(px(10.), px(10.)));
            icon(if self.expanded { IconName::ChatCollapse } else { IconName::ChatExpand }, chevron, color, window, cx);
            foot.paint(point(left + px(16.), middle - px(foot.height() / 2.)), &label_options(color, None, style), window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_kinds_as_the_renderer_writes_them() {
        // Dates and times: `decibell_model::regional`'s tests (against the renderer's strings).
        assert_eq!(day_label(0), "");
        assert_eq!(kind_label(&[AttachmentKind::Image]), "Image");
        assert_eq!(kind_label(&[AttachmentKind::Video, AttachmentKind::Video]), "2 Videos");
        assert_eq!(kind_label(&[AttachmentKind::Video, AttachmentKind::Image, AttachmentKind::Audio]), "3 Attachments");
    }

    fn attachment(id: i64, kind: AttachmentKind, filename: &str, size_bytes: i64, position: i32) -> decibell_model::chat::Attachment {
        decibell_model::chat::Attachment {
            id,
            message_id: 0,
            kind,
            filename: filename.to_string().into(),
            mime: SharedString::default(),
            size_bytes,
            url: SharedString::default(),
            position,
            created_at: 0,
            purged_at: 0,
            width: 0,
            height: 0,
            thumbnail_size_bytes: 0,
            thumbnail_sizes_mask: 0,
            duration_ms: 0,
            placeholder: SharedString::default(),
            encrypted: false,
            key_b64: None,
            chunk_bytes: 0,
        }
    }

    fn with(message: Arc<Message>, attachments: Vec<decibell_model::chat::Attachment>) -> Arc<Message> {
        let mut message = (*message).clone();
        message.attachments = attachments.into();
        Arc::new(message)
    }

    /// A row of a copy: (name, time) of a row that opens a group, the message, "(edited)".
    type CopyRow<'a> = (Option<(&'a str, &'a str)>, Arc<Message>, bool);

    fn copy(rows: &[CopyRow<'_>]) -> String {
        let mut out = CopyText::default();
        for (header, message, edited) in rows {
            whole_row_copy(*header, message, *edited, &mut out);
        }
        out.finish()
    }

    /// The Electron client's clipboard after selecting whole rows and Ctrl+C (Chromium's
    /// `document.execCommand('copy')` over a DOM range from the first row's `.select-text` to the
    /// last's, read back with `wl-paste`), against the local stack's Daily Gaming `#general`
    /// rows 9431–9442: inline code, a picture grid, a code block, a grouped row, nicknames, an
    /// edited reply, a day divider, a jumbo emoji and an inline one.
    #[test]
    fn a_copy_is_what_electrons_clipboard_holds() {
        use crate::testing::message as m;
        let images = with(
            m(9432, 100029, "Nice driver voice selam loud build reboot now what noise tonight server loud lag this leave how patch fps match stream wow when a thanks?", 0),
            vec![attachment(1, AttachmentKind::Image, "photo-22-640x480.jpg", 28_909, 0), attachment(2, AttachmentKind::Image, "photo-11-1280x960.jpg", 64_605, 1)],
        );
        let image = with(m(9437, 100014, "Echo evet lost `--release` how", 0), vec![attachment(3, AttachmentKind::Image, "photo-20-1600x1066.jpg", 106_684, 0)]);
        let long = "No what hayır match server driver it noise stream fps yes selam how sorry echo güncelleme where clip naber server later yes selam frame build channel server queue encoder. Echo works thanks later join thanks fixed lag bitrate quiet wayland broken watch voice when lol game great. Reboot again wow selam wayland nvidia oyun güncelleme where it look lol noise release where clip selam queue fixed loud what sorry update windows queue watch config maç how tonight sonra. Watch win leave wow queue now driver bitrate driver the windows maybe voice yes thanks fine broken great naber server lobby nvidia release this maybe broken thanks works mic wayland sorry güncelleme watch game.";
        let rows = [
            (Some(("carol", "11/12/2026, at 10:21 PM")), m(9431, 3, "How wow `fps=120` how lobby encoder noise ranked now again", 0), false),
            (Some(("rapid.falcon", "11/12/2026, at 10:23 PM")), images, false),
            (Some(("carol", "11/12/2026, at 10:38 PM")), m(9433, 3, "Win why queue frame tamam what:\n```sh\ncargo test --lib\nnpx napi build --platform\n```", 0), false),
            (Some(("dave", "11/12/2026, at 10:38 PM")), m(9434, 4, "Naber today wayland", 0), false),
            (None, m(9435, 4, "Bugün lag", 0), false),
            (Some(("Mert (AFK)", "11/12/2026, at 10:39 PM")), m(9436, 100015, long, 0), false),
            (Some(("Zeynep ✨", "11/12/2026, at 10:40 PM")), image, false),
            (Some(("turbo.quokka", "11/12/2026, at 10:46 PM")), m(9438, 100033, "Yes kanal patch what", 0), true),
            (Some(("erin", "11/13/2026, at 7:46 AM")), m(9439, 5, "🎮😂", 0), false),
            (Some(("shy.quokka94", "11/13/2026, at 7:47 AM")), m(9440, 100028, "A how broken the headset win cool yes tamam selam ok test lag windows driver.", 0), false),
            (Some(("dave", "11/13/2026, at 7:48 AM")), m(9441, 4, "Voice when 🎮", 0), false),
            (Some(("Bobby 🛠", "11/13/2026, at 7:48 AM")), m(9442, 2, "When audio who hayır headset oyun tomorrow server", 0), false),
        ];
        let electron = format!(
            "carol\n11/12/2026, at 10:21 PM\nHow wow fps=120 how lobby encoder noise ranked now again\nrapid.falcon\n11/12/2026, at 10:23 PM\nNice driver voice selam loud build reboot now what noise tonight server loud lag this leave how patch fps match stream wow when a thanks?\nphoto-22-640x480.jpg\nphoto-11-1280x960.jpg\n\ncarol\n11/12/2026, at 10:38 PM\nWin why queue frame tamam what:\nsh\n\ncargo test --lib\nnpx napi build --platform\ndave\n11/12/2026, at 10:38 PM\nNaber today wayland\nBugün lag\nMert (AFK)\n11/12/2026, at 10:39 PM\n{long}\nZeynep ✨\n11/12/2026, at 10:40 PM\nEcho evet lost --release how\nphoto-20-1600x1066.jpg\n\nturbo.quokka\n11/12/2026, at 10:46 PM\nYes kanal patch what(edited)\nerin\n11/13/2026, at 7:46 AM\nshy.quokka94\n11/13/2026, at 7:47 AM\nA how broken the headset win cool yes tamam selam ok test lag windows driver.\ndave\n11/13/2026, at 7:48 AM\nVoice when \nBobby 🛠\n11/13/2026, at 7:48 AM\nWhen audio who hayır headset oyun tomorrow server"
        );
        assert_eq!(copy(&rows), electron);
    }

    /// The same for the stack's `#clips` (all five rows): a picture grid, one picture, two
    /// documents, an audio file and a picture last.
    #[test]
    fn attachments_copy_as_electron_serialises_them() {
        use crate::testing::message as m;
        let rows = [
            (
                Some(("alice", "11:11 AM")),
                with(
                    m(10308, 1, "new clips from last night's session", 0),
                    vec![
                        attachment(1, AttachmentKind::Image, "clip-night-1.jpg", 89_249, 0),
                        attachment(2, AttachmentKind::Image, "clip-night-2.jpg", 68_498, 1),
                        attachment(3, AttachmentKind::Image, "clip-night-3.jpg", 62_560, 2),
                    ],
                ),
                false,
            ),
            (Some(("Bobby 🛠", "11:11 AM")), with(m(10309, 2, "phone screenshot of the scoreboard", 0), vec![attachment(4, AttachmentKind::Image, "phone-screenshot.jpg", 84_139, 0)]), false),
            (
                Some(("carol", "11:11 AM")),
                with(
                    m(10310, 3, "patch notes + my settings export, for anyone asking", 0),
                    vec![attachment(5, AttachmentKind::Document, "patch-notes.txt", 105, 0), attachment(6, AttachmentKind::Document, "settings-export.zip", 301, 1)],
                ),
                false,
            ),
            (Some(("dave", "11:11 AM")), with(m(10311, 4, "mic test, does this sound clipped?", 0), vec![attachment(7, AttachmentKind::Audio, "voice-memo.wav", 132_344, 0)]), false),
            (Some(("erin", "11:11 AM")), with(m(10312, 5, "ultrawide finally works in this game", 0), vec![attachment(8, AttachmentKind::Image, "ultrawide.png", 361_656, 0)]), false),
        ];
        assert_eq!(
            copy(&rows),
            "alice\n11:11 AM\nnew clips from last night's session\nclip-night-1.jpg\nclip-night-2.jpg\nclip-night-3.jpg\n\nBobby 🛠\n11:11 AM\nphone screenshot of the scoreboard\nphone-screenshot.jpg\n\ncarol\n11:11 AM\npatch notes + my settings export, for anyone asking\n\npatch-notes.txt\n105 B\n\nsettings-export.zip\n301 B\ndave\n11:11 AM\nmic test, does this sound clipped?\nvoice-memo.wav\n129.2 KB\n\n\n0:00 / 0:00\n\nerin\n11:11 AM\nultrawide finally works in this game\nultrawide.png"
        );
        assert_eq!(format_file_size(1280), "1.3 KB", "toFixed rounds a tie up");
    }

    #[test]
    fn positions_order_as_they_read() {
        let body = RowPos { part: Part::Body, pos: RichPos::default() };
        assert!(RowPos::START < RowPos::at(Part::Time, 0));
        assert!(RowPos::at(Part::Time, 99) < body);
        assert!(body < RowPos::END);
    }
}
