//! Autolinks (richText.ts `startsWithScheme` / `urlExtent` / `isLinkable`), invite links
//! (inviteLink.ts), and what the message row does with links: the embed list (LinkEmbeds.tsx),
//! the lone media link (`loneLink`), `mediaLabelFor` and `isGifUrl`.

use crate::js;
use crate::url;
use crate::{Document, SpanKind};

const URL_MAX_LEN: usize = 2048;
const INVITE_SCHEME: &[u8] = b"decibell://invite/";
/// LinkEmbeds.tsx `MAX_EMBEDS`.
pub const MAX_EMBEDS: usize = 3;

#[inline]
fn ci_prefix(b: &[u8], p: &[u8]) -> bool {
    b.len() >= p.len() && b[..p.len()].eq_ignore_ascii_case(p)
}

/// `startsWithScheme(s, i)`: `http://`, `https://` or `decibell://invite/` (ASCII
/// case-insensitive) at byte `i`.
#[inline]
pub(crate) fn starts_with_scheme(b: &[u8], i: usize) -> bool {
    match b.get(i) {
        Some(b'h' | b'H') => ci_prefix(&b[i..], b"https://") || ci_prefix(&b[i..], b"http://"),
        Some(b'd' | b'D') => ci_prefix(&b[i..], INVITE_SCHEME),
        _ => false,
    }
}

#[inline]
fn url_stop(c: char) -> bool {
    js::is_space(c) || matches!(c, '<' | '>' | '"' | '`')
}

/// `urlExtent(src, i, end)`: byte length of the autolink at `i`, 0 when nothing links.
pub(crate) fn url_extent(s: &str, i: usize, end: usize) -> usize {
    let mut j = i;
    for c in s[i..end].chars() {
        if url_stop(c) {
            break;
        }
        j += c.len_utf8();
    }
    let mut cand = &s[i..j];
    // Trim until stable: trailing sentence punctuation, then an unbalanced closing bracket.
    loop {
        let before = cand.len();
        if let Some(&l) = cand.as_bytes().last() {
            if matches!(l, b'.' | b',' | b':' | b';' | b'!' | b'?' | b'\'' | b'"') {
                cand = &cand[..cand.len() - 1];
            }
        }
        if let Some(&last) = cand.as_bytes().last() {
            let opener = match last {
                b')' => Some(b'('),
                b']' => Some(b'['),
                b'}' => Some(b'{'),
                _ => None,
            };
            if let Some(op) = opener {
                let (mut opens, mut closes) = (0usize, 0usize);
                for &c in cand.as_bytes() {
                    if c == op {
                        opens += 1;
                    } else if c == last {
                        closes += 1;
                    }
                }
                if closes > opens {
                    cand = &cand[..cand.len() - 1];
                }
            }
        }
        if cand.len() == before {
            break;
        }
    }
    if is_linkable(cand) {
        cand.len()
    } else {
        0
    }
}

/// `isLinkable`: a parseable http(s) URL with a dotted host (or localhost), or a well-formed
/// invite link; at most 2048 UTF-16 units.
pub fn is_linkable(candidate: &str) -> bool {
    if candidate.len() > URL_MAX_LEN && js::utf16_len(candidate) > URL_MAX_LEN {
        return false;
    }
    if ci_prefix(candidate.as_bytes(), b"decibell:") {
        return parse_invite_link(candidate).is_some();
    }
    url::parse(candidate, false).is_some_and(|p| p.http && p.dotted_or_localhost)
}

/// A parsed `decibell://invite/…` link (inviteLink.ts `parseInviteLink`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invite<'a> {
    /// The invite code (4–64 ASCII letters / digits, as typed; native upper-cases on redeem).
    pub code: &'a str,
    /// The older endpoint-carrying shapes: host (brackets of an IPv6 literal removed) and port.
    pub endpoint: Option<(&'a str, u16)>,
}

fn invite_host_char(c: char) -> bool {
    !(js::is_space(c) || c == '/' || c == ':')
}

/// `[^\s/:]+`, maximal, from byte 0. Returns its byte length (0 = none).
fn run_host(s: &str) -> usize {
    s.char_indices().find(|&(_, c)| !invite_host_char(c)).map_or(s.len(), |(i, _)| i)
}

/// `\d{1,5}` then the next byte must be `/`: returns the digits.
fn port_then_slash(s: &str) -> Option<(&str, &str)> {
    let n = s.bytes().take(5).take_while(u8::is_ascii_digit).count();
    if n == 0 || s.as_bytes().get(n) != Some(&b'/') {
        return None;
    }
    Some((&s[..n], &s[n + 1..]))
}

/// `([A-Za-z0-9]{4,64})\/?$`
fn code_tail(s: &str) -> Option<&str> {
    let n = s.bytes().take_while(u8::is_ascii_alphanumeric).count();
    if !(4..=64).contains(&n) {
        return None;
    }
    match &s[n..] {
        "" | "/" => Some(&s[..n]),
        _ => None,
    }
}

/// The renderer's `INVITE_RE`, which backtracks: the endpoint group is tried first (host:port,
/// bracketed host first, then host/port), then skipped. The first full match decides; its
/// captures are then checked (non-empty host, port 1–65535) without trying the others.
pub fn parse_invite_link(url: &str) -> Option<Invite<'_>> {
    if !ci_prefix(url.as_bytes(), INVITE_SCHEME) {
        return None;
    }
    let rest = &url[INVITE_SCHEME.len()..];
    let found: Option<(&str, Option<(&str, &str)>)> = (|| {
        // (?:(\[[0-9a-f:.]+\]|[^\s/:]+):(\d{1,5})
        let mut hosts: [Option<usize>; 2] = [None, None];
        if let Some(inner) = rest.strip_prefix('[') {
            let n = inner.bytes().take_while(|c| c.is_ascii_hexdigit() || *c == b':' || *c == b'.').count();
            if n > 0 && rest.as_bytes().get(1 + n) == Some(&b']') {
                hosts[0] = Some(n + 2);
            }
        }
        let h = run_host(rest);
        if h > 0 {
            hosts[1] = Some(h);
        }
        for hl in hosts.into_iter().flatten() {
            if rest.as_bytes().get(hl) == Some(&b':') {
                if let Some((port, after)) = port_then_slash(&rest[hl + 1..]) {
                    if let Some(code) = code_tail(after) {
                        return Some((code, Some((&rest[..hl], port))));
                    }
                }
            }
        }
        // |([^\s/:]+)\/(\d{1,5}))\/
        if h > 0 && rest.as_bytes().get(h) == Some(&b'/') {
            if let Some((port, after)) = port_then_slash(&rest[h + 1..]) {
                if let Some(code) = code_tail(after) {
                    return Some((code, Some((&rest[..h], port))));
                }
            }
        }
        // group skipped
        code_tail(rest).map(|code| (code, None))
    })();
    let (code, ep) = found?;
    let Some((raw_host, port)) = ep else {
        return Some(Invite { code, endpoint: None });
    };
    let host = raw_host.strip_prefix('[').unwrap_or(raw_host);
    let host = host.strip_suffix(']').unwrap_or(host);
    let port: u32 = port.parse().ok()?;
    if host.is_empty() || !(1..=65535).contains(&port) {
        return None;
    }
    Some(Invite { code, endpoint: Some((host, port as u16)) })
}

/// inviteLink.ts `isInviteLink`.
pub fn is_invite_link(url: &str) -> bool {
    parse_invite_link(url).is_some()
}

/// How a message link is shown under the message (LinkEmbeds.tsx).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedKind {
    /// An invite card (`InviteEmbed`): resolved against our own central, shown whatever the
    /// link-preview privacy toggle or EMBED_LINKS say.
    Invite,
    /// A link preview (`LinkEmbed`): only with previews on and embeds not suppressed; an image /
    /// GIF preview or a site card once the unfurl is in.
    Web,
}

/// `richTextToPlain`-independent: what `mediaLabelFor` returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaLabel {
    Gif,
    Image,
}

impl MediaLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaLabel::Gif => "GIF",
            MediaLabel::Image => "Image",
        }
    }
}

/// richText.ts `mediaLabelFor`: "GIF" / "Image" by the URL's path extension.
pub fn media_label_for(href: &str) -> Option<MediaLabel> {
    let p = url::parse(href, true)?.path?;
    if url::path_ends_with_ext(&p, "gif") {
        return Some(MediaLabel::Gif);
    }
    for ext in ["png", "jpg", "jpeg", "webp", "avif"] {
        if url::path_ends_with_ext(&p, ext) {
            return Some(MediaLabel::Image);
        }
    }
    None
}

/// LinkEmbeds.tsx `isGifUrl`: a direct image URL that is a GIF by its path.
pub fn is_gif_url(url: &str) -> bool {
    url::parse(url, true).and_then(|p| p.path).is_some_and(|p| url::path_ends_with_ext(&p, "gif"))
}

impl Document {
    /// `extractLinks(content, max)`: the autolinks eligible for a preview card, in order of
    /// appearance, outside code and math, not in the `<url>` form, deduplicated, at most `max`.
    pub fn embed_links<'a>(&self, src: &'a str, max: usize) -> Vec<&'a str> {
        let mut out: Vec<&'a str> = Vec::new();
        for sp in self.spans() {
            if out.len() >= max {
                break;
            }
            if let SpanKind::Link { embed: true, .. } = sp.kind {
                let href = &src[sp.src.start as usize..sp.src.end as usize];
                if !out.contains(&href) {
                    out.push(href);
                }
            }
        }
        out
    }

    /// LinkEmbeds.tsx: the cards under the message, in order (at most [`MAX_EMBEDS`]).
    pub fn embeds<'a>(&self, src: &'a str) -> Vec<(&'a str, EmbedKind)> {
        self.embed_links(src, MAX_EMBEDS)
            .into_iter()
            .map(|u| (u, if is_invite_link(u) { EmbedKind::Invite } else { EmbedKind::Web }))
            .collect()
    }

    /// richText.ts `loneLink`: the href when the whole message is one autolink (whitespace
    /// aside, not the `<url>` form). MessageBubble hides the text once that link's preview
    /// resolves to an image (previews on, embeds not suppressed).
    pub fn lone_link<'a>(&self, src: &'a str) -> Option<&'a str> {
        let i = self.lone_link_span?;
        let sp = &self.spans()[i as usize];
        Some(&src[sp.src.start as usize..sp.src.end as usize])
    }
}
