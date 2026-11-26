//! Emoji segmentation exactly as the renderer does it: `emoji-regex` (run as a code-point program
//! generated from the same package, same backtracking order), Twemoji asset names by
//! `Twemoji.tsx`'s `toCodePoints`, and `:shortcode:` expansion from `@emoji-mart/data`.

use crate::tables::{
    EMOJI_FIRST, EMOJI_KIDS, EMOJI_NODES, EMOJI_RANGES, EMOJI_ROOT, EMOJI_ROOT_FIRST, SHORTCODE_IDS, SHORTCODE_ID_OFF,
    SHORTCODE_NATIVES, SHORTCODE_NATIVE_OFF, TWEMOJI_BLOB, TWEMOJI_OFF,
};
use std::ops::Range;

/// A Twemoji SVG of `@twemoji/svg` 15.0.0 (`gpui-client/assets/twemoji/<name>.svg`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EmojiAsset(pub(crate) u16);

impl EmojiAsset {
    /// The sequence the asset draws, as the renderer keys it (U+FE0F dropped from
    /// multi-code-point sequences).
    pub fn chars(self) -> &'static str {
        let i = self.0 as usize;
        &TWEMOJI_BLOB[TWEMOJI_OFF[i] as usize..TWEMOJI_OFF[i + 1] as usize]
    }

    /// The file name without `.svg`: lower-case hex code points joined by `-` (`1f468-200d-1f469`).
    pub fn name(self) -> String {
        let mut s = String::with_capacity(24);
        for (k, c) in self.chars().chars().enumerate() {
            if k > 0 {
                s.push('-');
            }
            use std::fmt::Write;
            let _ = write!(s, "{:x}", c as u32);
        }
        s
    }

    /// Index into the asset table (stable for one build of the tables).
    pub fn index(self) -> u16 {
        self.0
    }

    /// Number of assets reachable through `toCodePoints`.
    pub fn count() -> usize {
        TWEMOJI_OFF.len() - 1
    }
}

/// `Twemoji.tsx`: the SVG for an emoji match, `None` when the renderer falls back to drawing the
/// text with the OS font. `toCodePoints` drops U+FE0F when the sequence has more than one code
/// point (even inside ZWJ sequences, where Twemoji's files keep it: those then miss).
pub fn twemoji_asset(emoji: &str) -> Option<EmojiAsset> {
    let multi = emoji.chars().nth(1).is_some();
    let mut key = [0u8; 96];
    let mut n = 0;
    for c in emoji.chars() {
        if multi && c == '\u{FE0F}' {
            continue;
        }
        let l = c.len_utf8();
        if n + l > key.len() {
            return None;
        }
        c.encode_utf8(&mut key[n..]);
        n += l;
    }
    if n == 0 {
        return None;
    }
    let key = &key[..n];
    let blob = TWEMOJI_BLOB.as_bytes();
    let (mut lo, mut hi) = (0usize, TWEMOJI_OFF.len() - 1);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let k = &blob[TWEMOJI_OFF[mid] as usize..TWEMOJI_OFF[mid + 1] as usize];
        match k.cmp(key) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Some(EmojiAsset(mid as u16)),
        }
    }
    None
}

/// `@emoji-mart/data`'s `emojis[id].skins[0].native` for a shortcode id (case-insensitive, as
/// MessageText / RichInput lower-case it first).
pub fn shortcode(id: &str) -> Option<&'static str> {
    if id.is_empty() || id.len() > 64 {
        return None;
    }
    let mut buf = [0u8; 64];
    for (k, b) in id.bytes().enumerate() {
        buf[k] = b.to_ascii_lowercase();
    }
    let key = &buf[..id.len()];
    let ids = SHORTCODE_IDS.as_bytes();
    let (mut lo, mut hi) = (0usize, SHORTCODE_ID_OFF.len() - 1);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let k = &ids[SHORTCODE_ID_OFF[mid] as usize..SHORTCODE_ID_OFF[mid + 1] as usize];
        match k.cmp(key) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                return Some(&SHORTCODE_NATIVES[SHORTCODE_NATIVE_OFF[mid] as usize..SHORTCODE_NATIVE_OFF[mid + 1] as usize]);
            }
        }
    }
    None
}

/// One match of `/:([a-z0-9_+-]+):/gi` (global, left to right, non-overlapping): the byte range
/// of the whole `:id:` and of the id. Unknown ids still consume their colons, as in the renderer.
#[derive(Copy, Clone, Debug)]
pub(crate) struct ShortcodeMatch {
    pub start: usize,
    pub end: usize,
}

pub(crate) fn shortcode_matches(s: &str) -> impl Iterator<Item = ShortcodeMatch> + '_ {
    let b = s.as_bytes();
    let mut p = 0usize;
    std::iter::from_fn(move || {
        while p < b.len() {
            if b[p] == b':' {
                let mut q = p + 1;
                while q < b.len() && (b[q].is_ascii_alphanumeric() || matches!(b[q], b'_' | b'+' | b'-')) {
                    q += 1;
                }
                if q > p + 1 && q < b.len() && b[q] == b':' {
                    let m = ShortcodeMatch { start: p, end: q + 1 };
                    p = q + 1;
                    return Some(m);
                }
            }
            p += 1;
        }
        None
    })
}

impl ShortcodeMatch {
    pub fn id<'a>(&self, s: &'a str) -> &'a str {
        &s[self.start + 1..self.end - 1]
    }
}

// ── the emoji-regex program ──

#[inline]
fn in_ranges(r: &[(u32, u32)], c: u32) -> bool {
    if r.len() <= 4 {
        return r.iter().any(|&(a, b)| a <= c && c <= b);
    }
    let (mut lo, mut hi) = (0usize, r.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        let (a, b) = r[mid];
        if c < a {
            hi = mid;
        } else if c > b {
            lo = mid + 1;
        } else {
            return true;
        }
    }
    false
}

#[inline]
fn decode(s: &str, pos: usize) -> Option<(u32, usize)> {
    let c = s.get(pos..)?.chars().next()?;
    Some((c as u32, c.len_utf8()))
}

type Cont<'a> = dyn FnMut(usize) -> Option<usize> + 'a;

fn run(node: u16, s: &str, pos: usize, k: &mut Cont<'_>) -> Option<usize> {
    let (kind, a, b) = EMOJI_NODES[node as usize];
    let (a, b) = (a as usize, b as usize);
    match kind {
        0 => {
            let (c, l) = decode(s, pos)?;
            if in_ranges(&EMOJI_RANGES[a..a + b], c) {
                k(pos + l)
            } else {
                None
            }
        }
        1 => run_seq(&EMOJI_KIDS[a..a + b], s, pos, k),
        2 => {
            for &child in &EMOJI_KIDS[a..a + b] {
                if let r @ Some(_) = run(child, s, pos, k) {
                    return r;
                }
            }
            None
        }
        _ => {
            if let r @ Some(_) = run(a as u16, s, pos, k) {
                return r;
            }
            k(pos)
        }
    }
}

fn run_seq(items: &[u16], s: &str, pos: usize, k: &mut Cont<'_>) -> Option<usize> {
    match items.split_first() {
        None => k(pos),
        Some((&first, rest)) => run(first, s, pos, &mut |p| run_seq(rest, s, p, k)),
    }
}

/// Bytes at which a match can start: `#`, `*`, digits (keycaps) and every non-ASCII lead byte.
static MAY_START_BYTE: [bool; 256] = {
    let mut t = [false; 256];
    let mut k = 0x80;
    while k < 256 {
        t[k] = true;
        k += 1;
    }
    t[b'#' as usize] = true;
    t[b'*' as usize] = true;
    let mut d = b'0';
    while d <= b'9' {
        t[d as usize] = true;
        d += 1;
    }
    t
};

/// Can an emoji-regex match start with this char?
#[inline]
fn may_start(c: char) -> bool {
    let c = c as u32;
    if c < 0x80 {
        return matches!(c as u8, b'#' | b'*' | b'0'..=b'9');
    }
    in_ranges(&EMOJI_FIRST, c)
}

/// The emoji-regex match starting exactly at `pos`, if any (its end).
pub(crate) fn match_at(s: &str, pos: usize) -> Option<usize> {
    let b = s.as_bytes();
    let x = *b.get(pos)?;
    if x < 0x80 {
        // Only the keycap alternative `[#*0-9]\uFE0F?\u20E3` starts with ASCII (the generator
        // checks), and it backtracks to the same two shapes.
        if !matches!(x, b'#' | b'*' | b'0'..=b'9') {
            return None;
        }
        const VS16: &[u8] = "\u{FE0F}".as_bytes();
        const KEYCAP: &[u8] = "\u{20E3}".as_bytes();
        let rest = &b[pos + 1..];
        if rest.starts_with(VS16) && rest[VS16.len()..].starts_with(KEYCAP) {
            return Some(pos + 1 + VS16.len() + KEYCAP.len());
        }
        return rest.starts_with(KEYCAP).then_some(pos + 1 + KEYCAP.len());
    }
    let c = s[pos..].chars().next()? as u32;
    let (_, a, n) = EMOJI_NODES[EMOJI_ROOT as usize];
    for (k, &child) in EMOJI_KIDS[a as usize..(a + n) as usize].iter().enumerate() {
        if in_ranges(EMOJI_ROOT_FIRST[k], c) {
            if let r @ Some(_) = run(child, s, pos, &mut Some) {
                return r;
            }
        }
    }
    None
}

/// `s.matchAll(emojiRegex())`: byte ranges of every match, left to right.
pub fn find_iter(s: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut p = 0usize;
    std::iter::from_fn(move || {
        let b = s.as_bytes();
        while p < b.len() {
            let x = b[p];
            if !MAY_START_BYTE[x as usize] {
                p += b[p..].iter().position(|&x| MAY_START_BYTE[x as usize]).unwrap_or(b.len() - p);
                continue;
            }
            let c = s[p..].chars().next()?;
            if may_start(c) {
                if let Some(e) = match_at(s, p) {
                    let m = p..e;
                    p = e;
                    return Some(m);
                }
            }
            p += c.len_utf8();
        }
        None
    })
}

/// `emojiRegex().test(s)`.
pub fn contains_emoji(s: &str) -> bool {
    find_iter(s).next().is_some()
}

/// A piece of a text run split the way `MessageText`'s `emojiTokens` splits it (after its
/// `:shortcode:` expansion, which [`segment`] does when asked).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(std::borrow::Cow<'a, str>),
    Emoji { text: std::borrow::Cow<'a, str>, asset: Option<EmojiAsset> },
}

/// Split plain text into text and emoji the renderer's way. With `shortcodes`, known
/// `:shortcode:`s are expanded first (MessageText); without, the text is matched as typed
/// (StatusMessageText). Allocates only where an expansion changed the text.
pub fn segment(text: &str, shortcodes: bool) -> Vec<Segment<'_>> {
    use std::borrow::Cow;
    let expanded: Cow<'_, str> = if shortcodes { expand_shortcodes(text) } else { Cow::Borrowed(text) };
    let mut out = Vec::new();
    let mut last = 0;
    let piece = |a: usize, b: usize| -> Cow<'_, str> {
        match &expanded {
            Cow::Borrowed(s) => Cow::Borrowed(&s[a..b]),
            Cow::Owned(s) => Cow::Owned(s[a..b].to_owned()),
        }
    };
    for m in find_iter(&expanded) {
        if m.start > last {
            out.push(Segment::Text(piece(last, m.start)));
        }
        let asset = twemoji_asset(&expanded[m.clone()]);
        out.push(Segment::Emoji { text: piece(m.start, m.end), asset });
        last = m.end;
    }
    if last < expanded.len() {
        out.push(Segment::Text(piece(last, expanded.len())));
    }
    out
}

/// `text.replace(/:([a-z0-9_+-]+):/gi, …)` with the picker dataset (MessageText's
/// `expandShortcodes`).
pub fn expand_shortcodes(text: &str) -> std::borrow::Cow<'_, str> {
    let mut out: Option<String> = None;
    let mut last = 0;
    for m in shortcode_matches(text) {
        if let Some(native) = shortcode(m.id(text)) {
            let o = out.get_or_insert_with(|| String::with_capacity(text.len()));
            o.push_str(&text[last..m.start]);
            o.push_str(native);
            last = m.end;
        }
    }
    match out {
        None => std::borrow::Cow::Borrowed(text),
        Some(mut o) => {
            o.push_str(&text[last..]);
            std::borrow::Cow::Owned(o)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(find_iter("a😂b").collect::<Vec<_>>(), vec![1..5]);
        assert_eq!(find_iter("#️⃣").count(), 1);
        assert_eq!(find_iter("#").count(), 0);
        assert!(twemoji_asset("😂").is_some());
        assert_eq!(twemoji_asset("😂").unwrap().name(), "1f602");
        assert_eq!(twemoji_asset("❤️").unwrap().name(), "2764");
        assert_eq!(shortcode("SMILE"), Some("😄"));
        assert_eq!(shortcode("nope"), None);
        assert_eq!(expand_shortcodes(":nope:smile:"), ":nope:smile:"); // an unknown id still eats its colons
        assert_eq!(expand_shortcodes(":a:smile:"), "🅰\u{fe0f}smile:");
        assert_eq!(expand_shortcodes("x:smile:y"), "x😄y");
    }
}
