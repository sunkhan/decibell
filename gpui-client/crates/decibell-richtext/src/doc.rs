//! The parsed message: a flat span list the layout crate walks once.

use crate::emoji::{self, EmojiAsset};
use crate::js;
use crate::lang::Lang;
use crate::links::is_invite_link;
use crate::parse::{parse_events, Emph, Events, Node};
use std::cell::RefCell;

/// Text style bits. Inline code is a style (`CODE`) on a [`SpanKind::Text`] span, as in the
/// renderer, where `<code>` sits inside whatever emphasis surrounds it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style(pub u8);

impl Style {
    pub const NONE: Style = Style(0);
    pub const BOLD: Style = Style(1);
    pub const ITALIC: Style = Style(2);
    pub const UNDERLINE: Style = Style(4);
    pub const STRIKE: Style = Style(8);
    /// Inline `<code>` (also a code block or math past the per-message budget).
    pub const CODE: Style = Style(16);

    pub fn contains(self, other: Style) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn is_bold(self) -> bool {
        self.contains(Style::BOLD)
    }
    pub fn is_italic(self) -> bool {
        self.contains(Style::ITALIC)
    }
    pub fn is_underline(self) -> bool {
        self.contains(Style::UNDERLINE)
    }
    pub fn is_strike(self) -> bool {
        self.contains(Style::STRIKE)
    }
    pub fn is_code(self) -> bool {
        self.contains(Style::CODE)
    }
}

impl std::ops::BitOr for Style {
    type Output = Style;
    fn bitor(self, o: Style) -> Style {
        Style(self.0 | o.0)
    }
}

/// What a span is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpanKind {
    /// Running text (inline code when the style has `CODE`). May contain `\n`: the renderer
    /// draws message text `white-space: pre-wrap`.
    Text,
    /// An autolink; its text is the href. `embed: false` for the `<url>` form (no preview);
    /// `invite`: a `decibell://invite/…` link (click opens the join confirmation, not a browser).
    Link { embed: bool, invite: bool },
    /// Inline TeX (`$…$`); text is the TeX source.
    Math,
    /// One emoji-regex match; text is the sequence (from the source, or a `:shortcode:`'s
    /// expansion). `asset: None` = no Twemoji SVG, draw the text with the system emoji font at
    /// the emoji size.
    Emoji { asset: Option<EmojiAsset> },
    /// A fenced (or multi-line backtick) code block: a block of its own. Text is the code.
    /// `lang`: a tag CodeBlock.tsx knows (header shows it, highlighted with its grammar); `None`
    /// = header "code", plain text (the typed tag, if any, is [`Document::code_tag`]).
    CodeBlock { lang: Option<Lang> },
    /// `$$…$$`: a block of its own. Text is the TeX source (trimmed).
    DisplayMath,
}

/// A source byte range.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SrcRange {
    pub start: u32,
    pub end: u32,
}

impl SrcRange {
    pub fn range(self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
}

/// One run of a [`Document`]. 24 bytes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// The source bytes this span displays (markers and escape backslashes fall between spans).
    /// Ordered and non-overlapping across the document. For text taken from the source it is
    /// exactly the displayed text; for an emoji that came from a `:shortcode:` (or spans an
    /// escape) it covers what produced it, and may be empty for the tail of an expansion that
    /// an earlier span already claimed.
    pub src: SrcRange,
    pub style: Style,
    pub kind: SpanKind,
    /// Text in the document's own buffer (start, end); (0, 0) = the text is `src`.
    buf: (u32, u32),
}

impl Span {
    pub(crate) fn new_src(src: SrcRange, style: Style, kind: SpanKind) -> Span {
        Span { src, style, kind, buf: (0, 0) }
    }
    pub fn is_block(&self) -> bool {
        matches!(self.kind, SpanKind::CodeBlock { .. } | SpanKind::DisplayMath)
    }
    /// The displayed text is a slice of the source (`src`), not of the document's buffer.
    pub fn text_is_source(&self) -> bool {
        self.buf.1 == 0
    }
}

/// A parsed message. Holds no copy of the source: pass it back to [`Document::text`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Document {
    spans: Box<[Span]>,
    /// Text that is not a source slice: `:shortcode:` expansions, emoji spanning an escape, and
    /// the whole text of a detached document (previews).
    buf: Box<str>,
    pub(crate) jumbo: Option<u8>,
    pub(crate) lone_link_span: Option<u32>,
    flags: u8,
}

const PLAIN: u8 = 1;
const HAS_FORMATTING: u8 = 2;
const DETACHED: u8 = 4;

/// A layout block.
#[derive(Copy, Clone, Debug)]
pub enum Block<'a> {
    /// Inline spans laid out as one flow (line breaks are `\n` inside text). A paragraph whose
    /// last text ends with `\n` draws no extra empty line after it (CSS `pre-wrap` before a
    /// block), but one that starts with `\n` does open with an empty line.
    Paragraph(&'a [Span]),
    Code(&'a Span),
    Math(&'a Span),
}

/// Iterator over a document's blocks.
pub struct Blocks<'a> {
    spans: &'a [Span],
    i: usize,
}

impl<'a> Iterator for Blocks<'a> {
    type Item = Block<'a>;
    fn next(&mut self) -> Option<Block<'a>> {
        let sp = self.spans.get(self.i)?;
        match sp.kind {
            SpanKind::CodeBlock { .. } => {
                self.i += 1;
                Some(Block::Code(sp))
            }
            SpanKind::DisplayMath => {
                self.i += 1;
                Some(Block::Math(sp))
            }
            _ => {
                let start = self.i;
                while self.i < self.spans.len() && !self.spans[self.i].is_block() {
                    self.i += 1;
                }
                Some(Block::Paragraph(&self.spans[start..self.i]))
            }
        }
    }
}

/// Parse options.
#[derive(Clone, Debug)]
pub struct ParseOptions {
    /// MessageText.tsx's per-message budget of "expensive" nodes (code blocks and math, inline or
    /// display, in document order): past it they render as inline code. Default 10.
    pub expensive_budget: u32,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions { expensive_budget: 10 }
    }
}

impl Document {
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// The displayed text of `span` (`src` = the string this document was parsed from).
    pub fn text<'a>(&'a self, src: &'a str, span: &Span) -> &'a str {
        if span.buf.1 != 0 {
            &self.buf[span.buf.0 as usize..span.buf.1 as usize]
        } else if self.flags & DETACHED != 0 {
            &self.buf[span.src.range()]
        } else {
            &src[span.src.range()]
        }
    }

    pub fn blocks(&self) -> Blocks<'_> {
        Blocks { spans: &self.spans, i: 0 }
    }

    /// The unformatted path of MessageText: the parse was one text node (or nothing), so the
    /// message is drawn from its raw string (escape backslashes stay visible) with only emoji
    /// handled, and may be jumbo.
    pub fn is_plain(&self) -> bool {
        self.flags & PLAIN != 0
    }

    /// richText.ts `hasFormatting` (formatting beyond plain text and autolinks): the composer
    /// shows its live preview only then.
    pub fn has_formatting(&self) -> bool {
        self.flags & HAS_FORMATTING != 0
    }

    /// Jumbo emoji size, see [`crate::jumbo_size`].
    pub fn jumbo_size(&self) -> Option<u8> {
        self.jumbo
    }

    /// For a detached document (a preview line), its own text; spans' `src` index into it.
    pub fn own_text(&self) -> Option<&str> {
        (self.flags & DETACHED != 0).then_some(&*self.buf)
    }

    /// The tag typed after a code fence, as written (the renderer lower-cases it); `None` when
    /// there was none. Only meaningful for a [`SpanKind::CodeBlock`] span.
    pub fn code_tag<'a>(&self, src: &'a str, span: &Span) -> Option<&'a str> {
        let SpanKind::CodeBlock { .. } = span.kind else { return None };
        let b = src.as_bytes();
        let s = span.src.start as usize;
        if s == 0 || b[s - 1] != b'\n' {
            return None;
        }
        let mut k = s - 1;
        while k > 0 && (b[k - 1].is_ascii_alphanumeric() || matches!(b[k - 1], b'#' | b'+' | b'.' | b'_' | b'-')) {
            k -= 1;
        }
        // The tag sits right after the opening ``` fence.
        if k >= 3 && &b[k - 3..k] == b"```" && k < s - 1 {
            Some(&src[k..s - 1])
        } else {
            None
        }
    }

    pub(crate) fn detached(text: String, spans: Vec<Span>) -> Document {
        Document { spans: spans.into(), buf: text.into(), jumbo: None, lone_link_span: None, flags: DETACHED }
    }
}

// ── building ──

#[derive(Default)]
struct Scratch {
    ev: Events,
    spans: Vec<Span>,
    buf: String,
    merged: String,
    logical: String,
    segs: Vec<Seg>,
    msegs: Vec<Seg>,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::default());
}

/// A stretch of the logical (merged, shortcode-expanded) text and where it came from.
#[derive(Copy, Clone, Debug)]
struct Seg {
    log: (u32, u32),
    src: (u32, u32),
    ident: bool,
}

fn map_start(segs: &[Seg], x: u32) -> u32 {
    for s in segs {
        if x >= s.log.0 && x < s.log.1 {
            return if s.ident {
                s.src.0 + (x - s.log.0)
            } else if x == s.log.0 {
                s.src.0
            } else {
                s.src.1
            };
        }
    }
    segs.last().map_or(0, |s| s.src.1)
}

fn map_end(segs: &[Seg], y: u32) -> u32 {
    for s in segs {
        if y > s.log.0 && y <= s.log.1 {
            return if s.ident { s.src.0 + (y - s.log.0) } else { s.src.1 };
        }
    }
    segs.first().map_or(0, |s| s.src.0)
}

pub(crate) fn jumbo_size_for(count: usize) -> u8 {
    match count {
        1 => 56,
        2 => 44,
        3 => 38,
        _ => 32,
    }
}

struct B<'a> {
    src: &'a str,
    sc: &'a mut Scratch,
}

impl B<'_> {
    fn push_src(&mut self, start: u32, end: u32, style: Style, kind: SpanKind) {
        self.sc.spans.push(Span { src: SrcRange { start, end }, style, kind, buf: (0, 0) });
    }

    fn push_buf(&mut self, text: &str, src: (u32, u32), style: Style, kind: SpanKind) {
        let a = self.sc.buf.len() as u32;
        self.sc.buf.push_str(text);
        let b = self.sc.buf.len() as u32;
        self.sc.spans.push(Span { src: SrcRange { start: src.0, end: src.1 }, style, kind, buf: (a, b) });
    }

    /// MessageText `emojiTokens` over one text node (its pieces): shortcode expansion, then
    /// emoji-regex. Returns (match count, every gap is JS whitespace).
    fn tokenize(&mut self, pieces: &[(u32, u32)], style: Style) -> (usize, bool) {
        let src = self.src;
        let single = pieces.len() == 1;
        if single && !src.as_bytes()[pieces[0].0 as usize..pieces[0].1 as usize].contains(&b':') {
            let (a, b) = pieces[0];
            return self.tokenize_source(a, b, style);
        }
        // Merged text of the node (+ where each part came from).
        let mut merged = std::mem::take(&mut self.sc.merged);
        let mut msegs = std::mem::take(&mut self.sc.msegs);
        merged.clear();
        msegs.clear();
        for &(a, b) in pieces {
            let l = merged.len() as u32;
            merged.push_str(&src[a as usize..b as usize]);
            msegs.push(Seg { log: (l, merged.len() as u32), src: (a, b), ident: true });
        }
        // Shortcode expansion into the logical text.
        let mut logical = std::mem::take(&mut self.sc.logical);
        let mut segs = std::mem::take(&mut self.sc.segs);
        logical.clear();
        segs.clear();
        let mut last = 0usize;
        let copy_ident = |logical: &mut String, segs: &mut Vec<Seg>, from: usize, to: usize| {
            for s in &msegs {
                let (a, b) = (s.log.0 as usize, s.log.1 as usize);
                let (x, y) = (a.max(from), b.min(to));
                if x < y {
                    let l = logical.len() as u32;
                    logical.push_str(&merged[x..y]);
                    let off = s.src.0 + (x - a) as u32;
                    segs.push(Seg { log: (l, logical.len() as u32), src: (off, off + (y - x) as u32), ident: true });
                }
            }
        };
        for m in emoji::shortcode_matches(&merged) {
            if let Some(native) = emoji::shortcode(m.id(&merged)) {
                copy_ident(&mut logical, &mut segs, last, m.start);
                let l = logical.len() as u32;
                logical.push_str(native);
                let s0 = map_start(&msegs, m.start as u32);
                let s1 = map_end(&msegs, m.end as u32);
                segs.push(Seg { log: (l, logical.len() as u32), src: (s0, s1), ident: false });
                last = m.end;
            }
        }
        copy_ident(&mut logical, &mut segs, last, merged.len());

        let mut count = 0;
        let mut gaps_space = true;
        let mut prev = 0usize;
        for m in emoji::find_iter(&logical) {
            if m.start > prev {
                gaps_space &= logical[prev..m.start].chars().all(js::is_space);
                self.emit_logical_text(&logical, &segs, prev, m.start, style);
            }
            count += 1;
            let text = &logical[m.clone()];
            let asset = emoji::twemoji_asset(text);
            let kind = SpanKind::Emoji { asset };
            let (a, b) = (m.start as u32, m.end as u32);
            match segs.iter().find(|s| s.ident && a >= s.log.0 && b <= s.log.1) {
                Some(s) => self.push_src(s.src.0 + (a - s.log.0), s.src.0 + (b - s.log.0), style, kind),
                None => self.push_buf(text, (map_start(&segs, a), map_end(&segs, b)), style, kind),
            }
            prev = m.end;
        }
        if prev < logical.len() {
            gaps_space &= logical[prev..].chars().all(js::is_space);
            self.emit_logical_text(&logical, &segs, prev, logical.len(), style);
        }
        self.sc.merged = merged;
        self.sc.msegs = msegs;
        self.sc.logical = logical;
        self.sc.segs = segs;
        (count, gaps_space)
    }

    fn emit_logical_text(&mut self, logical: &str, segs: &[Seg], from: usize, to: usize, style: Style) {
        for s in segs {
            let (a, b) = (s.log.0 as usize, s.log.1 as usize);
            let (x, y) = (a.max(from), b.min(to));
            if x >= y {
                continue;
            }
            if s.ident {
                let off = s.src.0 + (x - a) as u32;
                self.push_src(off, off + (y - x) as u32, style, SpanKind::Text);
            } else {
                let s0 = if x == a { s.src.0 } else { s.src.1 };
                self.push_buf(&logical[x..y], (s0, s.src.1), style, SpanKind::Text);
            }
        }
    }

    /// The common case: one source slice, no colon (so no shortcode can expand).
    fn tokenize_source(&mut self, a: u32, b: u32, style: Style) -> (usize, bool) {
        let text = &self.src[a as usize..b as usize];
        let mut count = 0;
        let mut gaps_space = true;
        let mut prev = 0usize;
        for m in emoji::find_iter(text) {
            if m.start > prev {
                gaps_space &= text[prev..m.start].chars().all(js::is_space);
                self.push_src(a + prev as u32, a + m.start as u32, style, SpanKind::Text);
            }
            count += 1;
            let asset = emoji::twemoji_asset(&text[m.clone()]);
            self.push_src(a + m.start as u32, a + m.end as u32, style, SpanKind::Emoji { asset });
            prev = m.end;
        }
        if prev < text.len() {
            gaps_space &= text[prev..].chars().all(js::is_space);
            self.push_src(a + prev as u32, b, style, SpanKind::Text);
        }
        (count, gaps_space)
    }
}

fn emph_style(e: Emph) -> Style {
    match e {
        Emph::Bold => Style::BOLD,
        Emph::Italic => Style::ITALIC,
        Emph::Underline => Style::UNDERLINE,
        Emph::Strike => Style::STRIKE,
    }
}

fn lang_of(src: &str, tag: Option<(u32, u32)>) -> Option<Lang> {
    let (a, b) = tag?;
    let t = &src.as_bytes()[a as usize..b as usize];
    if t.len() > 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    for (k, c) in t.iter().enumerate() {
        buf[k] = c.to_ascii_lowercase();
    }
    Lang::from_tag(std::str::from_utf8(&buf[..t.len()]).ok()?)
}

/// richText.ts `parseRichText` + what MessageText.tsx draws from it.
pub fn parse(src: &str, opts: &ParseOptions) -> Document {
    assert!(src.len() < (u32::MAX >> 1) as usize, "message too long");
    SCRATCH.with(|cell| {
        let mut guard = cell.borrow_mut();
        let sc: &mut Scratch = &mut guard;
        parse_events(src, &mut sc.ev);
        sc.spans.clear();
        sc.buf.clear();
        let ev = std::mem::take(&mut sc.ev);
        let plain = ev.is_plain();
        let mut flags = 0u8;
        if ev.has_formatting() {
            flags |= HAS_FORMATTING;
        }
        let mut jumbo = None;
        let mut lone: Option<u32> = None;
        {
            let mut b = B { src, sc: &mut *sc };
            if plain {
                flags |= PLAIN;
                if !src.is_empty() {
                    let (count, gaps_space) = b.tokenize(&[(0, src.len() as u32)], Style::NONE);
                    if count > 0 && gaps_space {
                        jumbo = Some(jumbo_size_for(count));
                    }
                }
            } else {
                let mut style = Style::NONE;
                // Emphasis nests at most 8 parse levels, two Opens each (`***`): 16 deep.
                let mut stack = [Style::NONE; 16];
                let mut depth = 0usize;
                let mut budget: i64 = opts.expensive_budget as i64;
                // loneLink: one embeddable link at the top level, whitespace-only text beside it.
                let mut lone_ok = true;
                let mut lone_seen: Option<u32> = None;
                for n in &ev.nodes {
                    let top = depth == 0;
                    match *n {
                        Node::Text { p0, p1 } => {
                            let pieces = &ev.pieces[p0 as usize..p1 as usize];
                            if top && lone_ok {
                                let blank = pieces.iter().all(|&(a, z)| src[a as usize..z as usize].chars().all(js::is_space));
                                if !blank {
                                    lone_ok = false;
                                }
                            }
                            b.tokenize(pieces, style);
                        }
                        Node::Link { s, e, embed } => {
                            if top {
                                if lone_seen.is_some() || !embed {
                                    lone_ok = false;
                                }
                                lone_seen = Some(b.sc.spans.len() as u32);
                            }
                            let invite = is_invite_link(&src[s as usize..e as usize]);
                            b.push_src(s, e, style, SpanKind::Link { embed, invite });
                        }
                        Node::Code { s, e } => {
                            lone_ok &= !top;
                            b.push_src(s, e, style | Style::CODE, SpanKind::Text);
                        }
                        Node::CodeBlock { s, e, lang } => {
                            lone_ok &= !top;
                            if budget > 0 {
                                b.push_src(s, e, style, SpanKind::CodeBlock { lang: lang_of(src, lang) });
                            } else {
                                b.push_src(s, e, style | Style::CODE, SpanKind::Text);
                            }
                            budget -= 1;
                        }
                        Node::Math { s, e, display } => {
                            lone_ok &= !top;
                            if budget > 0 {
                                b.push_src(s, e, style, if display { SpanKind::DisplayMath } else { SpanKind::Math });
                            } else {
                                b.push_src(s, e, style | Style::CODE, SpanKind::Text);
                            }
                            budget -= 1;
                        }
                        Node::Open(k) => {
                            lone_ok &= !top;
                            stack[depth] = style;
                            depth += 1;
                            style = style | emph_style(k);
                        }
                        Node::Close => {
                            depth -= 1;
                            style = stack[depth];
                        }
                    }
                }
                if lone_ok {
                    lone = lone_seen;
                }
            }
        }
        sc.ev = ev;
        let doc = Document { spans: sc.spans.as_slice().into(), buf: sc.buf.as_str().into(), jumbo, lone_link_span: lone, flags };
        sc.trim();
        doc
    })
}

impl Scratch {
    /// Keep the per-thread buffers small after an outsized message.
    fn trim(&mut self) {
        const KEEP: usize = 1024;
        if self.spans.capacity() > KEEP {
            self.spans = Vec::new();
        }
        if self.ev.nodes.capacity() > KEEP || self.ev.pieces.capacity() > KEEP {
            self.ev = Events::default();
        }
        for s in [&mut self.buf, &mut self.merged, &mut self.logical] {
            if s.capacity() > 16 * KEEP {
                *s = String::new();
            }
        }
        if self.segs.capacity() > KEEP || self.msegs.capacity() > KEEP {
            self.segs = Vec::new();
            self.msegs = Vec::new();
        }
    }
}

/// `richTextToPlain`'s walk: every node's text in order (text with escapes resolved, code and
/// code-block text, link hrefs, TeX), before whitespace collapsing.
pub(crate) fn with_events<R>(src: &str, f: impl FnOnce(&Events) -> R) -> R {
    SCRATCH.with(|cell| {
        let mut guard = cell.borrow_mut();
        let sc: &mut Scratch = &mut guard;
        parse_events(src, &mut sc.ev);
        f(&sc.ev)
    })
}
