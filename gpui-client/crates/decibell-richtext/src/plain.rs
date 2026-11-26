//! Plain-text views of a message: the one-line preview the DM list and reply headers draw
//! (MessageText's `preview` mode: `mediaLabelFor(loneLink)` or `richTextToPlain`, then emoji),
//! and the "as displayed" text a copy of the rendered message yields.

use crate::doc::{with_events, Block, Document, Span, SpanKind, SrcRange};
use crate::emoji;
use crate::js;
use crate::links::media_label_for;
use crate::parse::{Events, Node};
use crate::Style;

/// Streaming `.replace(/\s+/g, " ").trim()`.
struct Collapse<'a> {
    out: &'a mut String,
    pending: bool,
}

impl Collapse<'_> {
    fn push(&mut self, s: &str) {
        let mut run = 0; // start of the current non-space run
        for (i, c) in s.char_indices() {
            if js::is_space(c) {
                if i > run {
                    self.word(&s[run..i]);
                }
                run = i + c.len_utf8();
                self.pending = !self.out.is_empty();
            }
        }
        if s.len() > run {
            self.word(&s[run..]);
        }
    }

    fn word(&mut self, w: &str) {
        if self.pending {
            self.out.push(' ');
            self.pending = false;
        }
        self.out.push_str(w);
    }
}

fn plain_from_events(ev: &Events, src: &str, out: &mut String) {
    let mut c = Collapse { out, pending: false };
    for n in &ev.nodes {
        match *n {
            Node::Text { p0, p1 } => {
                for &(a, b) in &ev.pieces[p0 as usize..p1 as usize] {
                    c.push(&src[a as usize..b as usize]);
                }
            }
            Node::Link { s, e, .. } | Node::Code { s, e } | Node::CodeBlock { s, e, .. } | Node::Math { s, e, .. } => {
                c.push(&src[s as usize..e as usize])
            }
            Node::Open(_) | Node::Close => {}
        }
    }
}

/// richText.ts `richTextToPlain`: markers dropped, emphasis inlined, code / code-block / link
/// text and TeX kept verbatim, every whitespace run one space, trimmed. `:shortcodes:` are left
/// as typed (the preview expands them afterwards).
pub fn rich_text_to_plain(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    with_events(src, |ev| plain_from_events(ev, src, &mut out));
    out
}

/// richText.ts `loneLink` without building a document (the source range of the link).
pub(crate) fn lone_link_range(ev: &Events, src: &str) -> Option<(u32, u32)> {
    let mut depth = 0;
    let mut link = None;
    for n in &ev.nodes {
        match *n {
            Node::Open(_) => {
                if depth == 0 {
                    return None;
                }
                depth += 1;
            }
            Node::Close => depth -= 1,
            _ if depth > 0 => {}
            Node::Link { s, e, embed } => {
                if link.is_some() || !embed {
                    return None;
                }
                link = Some((s, e));
            }
            Node::Text { p0, p1 } => {
                if !ev.pieces[p0 as usize..p1 as usize].iter().all(|&(a, b)| src[a as usize..b as usize].chars().all(js::is_space)) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    link
}

/// The one-line preview of a message (MessageText `preview`): a message that is only a GIF /
/// image link reads "GIF" / "Image"; anything else is [`rich_text_to_plain`]; then
/// `:shortcodes:` expand and emoji split out (drawn at the caller's constant size, never jumbo).
/// Note the renderer expands shortcodes and draws emoji even where the bubble would not
/// (inside code, math and links), because the markers are gone by then.
///
/// The result is a detached document: its spans index [`Document::own_text`].
pub fn preview(src: &str) -> Document {
    let mut text = String::new();
    let label = with_events(src, |ev| {
        let lone = lone_link_range(ev, src);
        let label = lone.and_then(|(a, b)| media_label_for(&src[a as usize..b as usize]));
        if label.is_none() {
            plain_from_events(ev, src, &mut text);
        }
        label
    });
    if let Some(l) = label {
        text = l.as_str().to_owned();
    }
    let text = match emoji::expand_shortcodes(&text) {
        std::borrow::Cow::Borrowed(_) => text,
        std::borrow::Cow::Owned(o) => o,
    };
    let mut spans = Vec::new();
    let mut prev = 0usize;
    let text_span = |a: usize, b: usize| Span::detached(a, b, SpanKind::Text);
    for m in emoji::find_iter(&text) {
        if m.start > prev {
            spans.push(text_span(prev, m.start));
        }
        let asset = emoji::twemoji_asset(&text[m.clone()]);
        spans.push(Span::detached(m.start, m.end, SpanKind::Emoji { asset }));
        prev = m.end;
    }
    if prev < text.len() {
        spans.push(text_span(prev, text.len()));
    }
    Document::detached(text, spans)
}

impl Span {
    pub(crate) fn detached(a: usize, b: usize, kind: SpanKind) -> Span {
        Span::new_src(SrcRange { start: a as u32, end: b as u32 }, Style::NONE, kind)
    }
}

/// A position in a document's displayed text: a span and a byte offset into its text.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub span: usize,
    pub offset: usize,
}

/// The message as displayed, for copying: markup gone (escape backslashes too, except in an
/// unformatted message, where the renderer shows them), emoji as their characters, code without
/// any padding, TeX as its source; blocks on their own lines.
///
/// Electron itself copies less than this: a Twemoji-drawn emoji is an inline SVG with no text,
/// so it is dropped from a copied selection, and KaTeX copies its MathML and HTML renderings.
pub fn as_displayed(doc: &Document, src: &str) -> String {
    let n = doc.spans().len();
    let end = TextPos { span: n, offset: 0 };
    as_displayed_range(doc, src, TextPos { span: 0, offset: 0 }, end)
}

/// [`as_displayed`] over a selection `[from, to)`. Emoji, math and code-block spans are atomic:
/// partly covered counts as covered.
pub fn as_displayed_range(doc: &Document, src: &str, from: TextPos, to: TextPos) -> String {
    let mut out = String::new();
    let mut prev_block = false;
    for (i, sp) in doc.spans().iter().enumerate() {
        if i < from.span {
            continue;
        }
        if i > to.span {
            break;
        }
        let text = doc.text(src, sp);
        let a = if i == from.span { from.offset.min(text.len()) } else { 0 };
        let b = if i == to.span { to.offset.min(text.len()) } else { text.len() };
        let atomic = !matches!(sp.kind, SpanKind::Text | SpanKind::Link { .. });
        let piece = if atomic {
            if a < b { text } else { "" }
        } else {
            let a = floor_char(text, a);
            &text[a..floor_char(text, b.max(a))]
        };
        if piece.is_empty() {
            continue;
        }
        let block = sp.is_block();
        if (block || prev_block) && !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(piece);
        prev_block = block;
    }
    out
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

impl Document {
    /// Source offset of a displayed position (selection → source, caret mapping). Inside text
    /// taken from the source it is exact; inside an emoji or other atomic / expanded span it
    /// snaps to the span's start (offset 0) or end.
    pub fn source_offset(&self, pos: TextPos) -> u32 {
        let spans = self.spans();
        match spans.get(pos.span) {
            None => spans.last().map_or(0, |s| s.src.end),
            Some(s) => {
                if s.text_is_source() && matches!(s.kind, SpanKind::Text | SpanKind::Link { .. }) {
                    (s.src.start + pos.offset as u32).min(s.src.end)
                } else if pos.offset == 0 {
                    s.src.start
                } else {
                    s.src.end
                }
            }
        }
    }

    /// Blocks' displayed text joined the way [`as_displayed`] joins them.
    pub fn block_texts<'a>(&'a self, src: &'a str) -> impl Iterator<Item = (Block<'a>, std::borrow::Cow<'a, str>)> + 'a {
        self.blocks().map(move |b| {
            let t: std::borrow::Cow<'a, str> = match b {
                Block::Paragraph(sp) if sp.len() == 1 => self.text(src, &sp[0]).into(),
                Block::Paragraph(sp) => sp.iter().map(|s| self.text(src, s)).collect::<String>().into(),
                Block::Code(s) | Block::Math(s) => self.text(src, s).into(),
            };
            (b, t)
        })
    }
}
