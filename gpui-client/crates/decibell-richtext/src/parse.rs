//! richText.ts `parseRange`, line for line, emitting a flat pre-order event list instead of a
//! node tree. Byte offsets stand in for UTF-16 indices: every decision the parser makes is on an
//! ASCII byte, and the few that look at a neighbouring character (`isSpaceAt`) decode it.

use crate::js;
use crate::links::{is_linkable, starts_with_scheme, url_extent};

/// Emphasis kinds (`bold` | `italic` | `underline` | `strike`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Emph {
    Bold,
    Italic,
    Underline,
    Strike,
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum Node {
    /// One text node (pushText merges consecutive text): `pieces[p0..p1]`.
    Text { p0: u32, p1: u32 },
    /// Autolink; href = src[s..e]. `embed` false for the `<url>` form.
    Link { s: u32, e: u32, embed: bool },
    /// Inline code, text = src[s..e].
    Code { s: u32, e: u32 },
    /// Code block, text = src[s..e]; `lang` = the raw tag's byte range (not lower-cased yet).
    CodeBlock { s: u32, e: u32, lang: Option<(u32, u32)> },
    /// Math, tex = src[s..e].
    Math { s: u32, e: u32, display: bool },
    Open(Emph),
    Close,
}

#[derive(Default)]
pub(crate) struct Events {
    pub nodes: Vec<Node>,
    pub pieces: Vec<(u32, u32)>,
}

impl Events {
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.pieces.clear();
    }

    /// Top-level node count and whether the single top-level node (if one) is text —
    /// `isPlainText(nodes) || nodes.length === 0`.
    pub fn is_plain(&self) -> bool {
        let mut depth = 0i32;
        let mut top = 0usize;
        let mut only_text = true;
        for n in &self.nodes {
            match n {
                Node::Open(_) => {
                    if depth == 0 {
                        top += 1;
                        only_text = false;
                    }
                    depth += 1;
                }
                Node::Close => depth -= 1,
                Node::Text { .. } => {
                    if depth == 0 {
                        top += 1;
                    }
                }
                _ => {
                    if depth == 0 {
                        top += 1;
                        only_text = false;
                    }
                }
            }
        }
        top == 0 || (top == 1 && only_text)
    }

    /// `hasFormatting`: a top-level node other than text or a link.
    pub fn has_formatting(&self) -> bool {
        let mut depth = 0i32;
        for n in &self.nodes {
            match n {
                Node::Open(_) => {
                    if depth == 0 {
                        return true;
                    }
                    depth += 1;
                }
                Node::Close => depth -= 1,
                Node::Text { .. } | Node::Link { .. } => {}
                _ => {
                    if depth == 0 {
                        return true;
                    }
                }
            }
        }
        false
    }
}

const ESCAPABLE: &[u8] = b"*_~`$\\";

/// Bytes that can start anything the parser acts on; every other byte is plain text.
static TRIGGER: [bool; 256] = {
    let mut t = [false; 256];
    let set = b"\\<hHdD`$*_~";
    let mut k = 0;
    while k < set.len() {
        t[set[k] as usize] = true;
        k += 1;
    }
    t
};

struct P<'a> {
    s: &'a str,
    b: &'a [u8],
    ev: &'a mut Events,
}

/// richText.ts `findCloser`: next occurrence of `marker` at or after `from` whose preceding
/// backslash run is even, ending by `end`. Searches the whole string (only the end is bounded)
/// and counts backslashes back past the range start, as the renderer does.
fn find_closer(b: &[u8], from: usize, end: usize, marker: &[u8]) -> Option<usize> {
    let m = marker.len();
    let mut i = from;
    while i + m <= end {
        let at = i + find(&b[i..], marker)?;
        if at + m > end {
            return None;
        }
        let mut bs = 0;
        let mut j = at;
        while j > 0 && b[j - 1] == b'\\' {
            bs += 1;
            j -= 1;
        }
        if bs % 2 == 0 {
            return Some(at);
        }
        i = at + 1;
    }
    None
}

#[inline]
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let first = needle[0];
    let mut k = 0;
    while k + needle.len() <= hay.len() {
        k += hay[k..].iter().position(|&c| c == first)?;
        if k + needle.len() > hay.len() {
            return None;
        }
        if &hay[k..k + needle.len()] == needle {
            return Some(k);
        }
        k += 1;
    }
    None
}

#[inline]
fn starts_with(b: &[u8], i: usize, p: &[u8]) -> bool {
    b.len() >= i + p.len() && &b[i..i + p.len()] == p
}

impl P<'_> {
    /// `isSpaceAt(src, i)` for a char boundary `i`: out of range counts as space.
    fn space_at(&self, i: usize) -> bool {
        match js::char_at(self.s, i) {
            None => true,
            Some(c) => js::is_space(c),
        }
    }

    /// `isSpaceAt(src, i - 1)`: the char (UTF-16 unit, in JS) just before byte `i`. An astral
    /// char's low surrogate is never a space, nor is its decoded char.
    fn space_before(&self, i: usize) -> bool {
        match js::char_before(self.s, i) {
            None => true,
            Some(c) => js::is_space(c),
        }
    }

    fn is_word(&self, i: isize) -> bool {
        js::is_word_byte(self.b, i)
    }

    fn push_piece(&mut self, pend: usize, a: usize, b: usize) {
        if b <= a {
            return;
        }
        let pieces = &mut self.ev.pieces;
        if pieces.len() > pend {
            let last = pieces.last_mut().unwrap();
            if last.1 as usize == a {
                last.1 = b as u32;
                return;
            }
        }
        pieces.push((a as u32, b as u32));
    }

    fn emit_text(&mut self, pend: &mut usize) {
        let n = self.ev.pieces.len();
        if n > *pend {
            self.ev.nodes.push(Node::Text { p0: *pend as u32, p1: n as u32 });
        }
        *pend = n;
    }

    fn push_node(&mut self, pend: &mut usize, node: Node) {
        self.emit_text(pend);
        self.ev.nodes.push(node);
        *pend = self.ev.pieces.len();
    }

    /// `trimOneTrailingNewline` on this level's pending text node.
    fn trim_one_trailing_newline(&mut self, pend: usize) {
        let pieces = &mut self.ev.pieces;
        if pieces.len() > pend {
            let last = pieces.last_mut().unwrap();
            if self.b[last.1 as usize - 1] == b'\n' {
                last.1 -= 1;
                if last.0 == last.1 {
                    pieces.pop();
                }
            }
        }
    }

    fn parse_range(&mut self, start: usize, end: usize, depth: u32) {
        let b = self.b;
        let mut i = start;
        let mut plain_from = start;
        let mut pend = self.ev.pieces.len();

        macro_rules! flush {
            ($up_to:expr) => {
                let up = $up_to;
                if up > plain_from {
                    self.push_piece(pend, plain_from, up);
                }
            };
        }

        while i < end {
            // Plain text runs between trigger bytes are the bulk of every message.
            if !TRIGGER[b[i] as usize] {
                i += b[i..end].iter().position(|&x| TRIGGER[x as usize]).unwrap_or(end - i);
                continue;
            }
            let c = b[i];

            // Backslash escape.
            if c == b'\\' && i + 1 < end && ESCAPABLE.contains(&b[i + 1]) {
                flush!(i);
                self.push_piece(pend, i + 1, i + 2);
                i += 2;
                plain_from = i;
                continue;
            }

            // Links before every marker; `<https://…>` is the no-preview form.
            if c == b'<' && starts_with_scheme(b, i + 1) {
                if let Some(off) = b[i + 1..].iter().position(|&x| x == b'>') {
                    let close = i + 1 + off;
                    if close < end {
                        let inner = &self.s[i + 1..close];
                        if !inner.chars().any(js::is_space) && is_linkable(inner) {
                            flush!(i);
                            self.push_node(&mut pend, Node::Link { s: (i + 1) as u32, e: close as u32, embed: false });
                            i = close + 1;
                            plain_from = i;
                            continue;
                        }
                    }
                }
            }
            if starts_with_scheme(b, i) && !self.is_word(i as isize - 1) {
                let len = url_extent(self.s, i, end);
                if len > 0 {
                    flush!(i);
                    self.push_node(&mut pend, Node::Link { s: i as u32, e: (i + len) as u32, embed: true });
                    i += len;
                    plain_from = i;
                    continue;
                }
            }

            // Code.
            if c == b'`' {
                if starts_with(b, i, b"```") {
                    if let Some(close) = find_closer(b, i + 3, end, b"```") {
                        if close > i + 3 {
                            flush!(i);
                            let (mut bs, mut be) = (i + 3, close);
                            let mut lang = None;
                            if let Some(nl) = b[bs..be].iter().position(|&x| x == b'\n') {
                                let tag = &b[bs..bs + nl];
                                if tag.iter().all(|&x| x.is_ascii_alphanumeric() || matches!(x, b'#' | b'+' | b'.' | b'_' | b'-')) {
                                    if nl > 0 {
                                        lang = Some((bs as u32, (bs + nl) as u32));
                                    }
                                    bs += nl + 1;
                                }
                            }
                            if be > bs && b[be - 1] == b'\n' {
                                be -= 1;
                            }
                            if be > bs {
                                self.trim_one_trailing_newline(pend);
                                self.push_node(&mut pend, Node::CodeBlock { s: bs as u32, e: be as u32, lang });
                            }
                            i = close + 3;
                            if b.get(i) == Some(&b'\n') {
                                i += 1;
                            }
                            plain_from = i;
                            continue;
                        }
                    }
                }
                let marker: &[u8] = if starts_with(b, i, b"``") { b"``" } else { b"`" };
                let ml = marker.len();
                if let Some(close) = find_closer(b, i + ml, end, marker) {
                    if close > i + ml {
                        flush!(i);
                        let (ib, ie) = (i + ml, close);
                        if b[ib..ie].contains(&b'\n') {
                            let (mut bs, mut be) = (ib, ie);
                            if b[bs] == b'\n' {
                                bs += 1;
                            }
                            if be > bs && b[be - 1] == b'\n' {
                                be -= 1;
                            }
                            if be > bs {
                                self.trim_one_trailing_newline(pend);
                                self.push_node(&mut pend, Node::CodeBlock { s: bs as u32, e: be as u32, lang: None });
                            }
                            i = close + ml;
                            if b.get(i) == Some(&b'\n') {
                                i += 1;
                            }
                        } else {
                            self.push_node(&mut pend, Node::Code { s: ib as u32, e: ie as u32 });
                            i = close + ml;
                        }
                        plain_from = i;
                        continue;
                    }
                }
                i += 1;
                continue;
            }

            // Math: display first, then Pandoc-style inline rules.
            if c == b'$' {
                if starts_with(b, i, b"$$") {
                    if let Some(close) = find_closer(b, i + 2, end, b"$$") {
                        if close > i + 2 {
                            let raw = &self.s[i + 2..close];
                            let t = js::trim(raw);
                            if !t.is_empty() {
                                let ts = i + 2 + (t.as_ptr() as usize - raw.as_ptr() as usize);
                                flush!(i);
                                self.trim_one_trailing_newline(pend);
                                self.push_node(&mut pend, Node::Math { s: ts as u32, e: (ts + t.len()) as u32, display: true });
                                i = close + 2;
                                if b.get(i) == Some(&b'\n') {
                                    i += 1;
                                }
                                plain_from = i;
                                continue;
                            }
                        }
                    }
                    i += 2;
                    continue;
                }
                if let Some(close) = find_closer(b, i + 1, end, b"$") {
                    if close > i + 1
                        && !self.space_at(i + 1)
                        && !self.space_before(close)
                        && !b.get(close + 1).is_some_and(u8::is_ascii_digit)
                        && !b[i + 1..close].contains(&b'\n')
                    {
                        flush!(i);
                        self.push_node(&mut pend, Node::Math { s: (i + 1) as u32, e: close as u32, display: false });
                        i = close + 1;
                        plain_from = i;
                        continue;
                    }
                }
                i += 1;
                continue;
            }

            // Emphasis. Triple runs first as an explicit compound.
            if depth < 8 && (c == b'*' || c == b'_') {
                let triple: &[u8] = if c == b'*' { b"***" } else { b"___" };
                if starts_with(b, i, triple) {
                    if let Some(close) = find_closer(b, i + 3, end, triple) {
                        if close > i + 3
                            && !self.space_at(i + 3)
                            && !self.space_before(close)
                            && (c != b'_' || (!self.is_word(i as isize - 1) && !self.is_word((close + 3) as isize)))
                        {
                            flush!(i);
                            self.emit_text(&mut pend);
                            self.ev.nodes.push(Node::Open(if c == b'*' { Emph::Bold } else { Emph::Underline }));
                            self.ev.nodes.push(Node::Open(Emph::Italic));
                            self.parse_range(i + 3, close, depth + 1);
                            self.ev.nodes.push(Node::Close);
                            self.ev.nodes.push(Node::Close);
                            pend = self.ev.pieces.len();
                            i = close + 3;
                            plain_from = i;
                            continue;
                        }
                    }
                }
            }
            if depth < 8 && (c == b'*' || c == b'_' || c == b'~') {
                let (marker, kind): (&[u8], Option<Emph>) = if starts_with(b, i, b"**") {
                    (b"**", Some(Emph::Bold))
                } else if starts_with(b, i, b"__") {
                    (b"__", Some(Emph::Underline))
                } else if starts_with(b, i, b"~~") {
                    (b"~~", Some(Emph::Strike))
                } else if c == b'*' {
                    (b"*", Some(Emph::Italic))
                } else if c == b'_' {
                    (b"_", Some(Emph::Italic))
                } else {
                    (b"", None)
                };
                if let Some(kind) = kind {
                    let ml = marker.len();
                    let close = find_closer(b, i + ml, end, marker);
                    let content_ok = close.is_some_and(|cl| cl > i + ml && !self.space_at(i + ml) && !self.space_before(cl));
                    let underscore_ok = marker != b"_"
                        || (!self.is_word(i as isize - 1) && !self.is_word(close.map_or(-1, |cl| cl as isize) + ml as isize));
                    if content_ok && underscore_ok {
                        let close = close.unwrap();
                        flush!(i);
                        self.emit_text(&mut pend);
                        self.ev.nodes.push(Node::Open(kind));
                        self.parse_range(i + ml, close, depth + 1);
                        self.ev.nodes.push(Node::Close);
                        pend = self.ev.pieces.len();
                        i = close + ml;
                        plain_from = i;
                        continue;
                    }
                }
                i += 1;
                continue;
            }

            i += 1;
        }

        flush!(end);
        self.emit_text(&mut pend);
    }
}

/// Parse `src` into `ev` (cleared first).
pub(crate) fn parse_events(src: &str, ev: &mut Events) {
    ev.clear();
    let mut p = P { s: src, b: src.as_bytes(), ev };
    p.parse_range(0, src.len(), 0);
}
