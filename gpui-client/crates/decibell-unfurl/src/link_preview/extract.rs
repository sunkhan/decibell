//! HTML metadata extraction, ported from the "HTML metadata" section of `linkPreview.ts`. Same
//! approach: regex scans over the decoded head (no DOM), the same patterns in the same priority
//! order. JS regex classes are spelled out (`\s` → [`js::SPACE_CLASS`], `\w` → `[A-Za-z0-9_]`, `/i`
//! on ASCII letters → `[Mm]…`) so Rust's Unicode-aware classes can't widen a match.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use encoding_rs::{Encoding, REPLACEMENT, UTF_8, WINDOWS_1252};
use regex::Regex;
use url::Url;

use crate::js;

/// Caps (linkPreview.ts "Limits").
pub(crate) const URL_MAX: usize = 2048;
pub(crate) const TITLE_MAX: usize = 200;
pub(crate) const DESCRIPTION_MAX: usize = 350;
pub(crate) const SITE_NAME_MAX: usize = 80;

struct Patterns {
    ct_charset: Regex,
    meta_charset: Regex,
    entity: Regex,
    space_run: Regex,
    attr: Regex,
    meta_tag: Regex,
    title: Regex,
    link_tag: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| {
        let ws = js::SPACE_CLASS;
        let ci = js::ci;
        // `<tag\b([^>]*)>`: `\b` after the name means the next character is not an ASCII word
        // character, so the attribute run is empty (`<meta>`) or starts with a non-word character.
        let tag = |name: &str| format!(r"<{}((?:[^A-Za-z0-9_>][^>]*)?)>", ci(name));
        let re = |p: &str| Regex::new(p).expect("static pattern");
        Patterns {
            // /charset=["']?\s*([\w.:-]+)/i
            ct_charset: re(&format!(
                r#"{}=["']?[{ws}]*([A-Za-z0-9_.:-]+)"#,
                ci("charset")
            )),
            // /<meta[^>]+charset=["']?\s*([\w.:-]+)/i
            meta_charset: re(&format!(
                r#"<{}[^>]+{}=["']?[{ws}]*([A-Za-z0-9_.:-]+)"#,
                ci("meta"),
                ci("charset")
            )),
            // /&(#x[0-9a-f]+|#\d+|[a-z][a-z0-9]*);/gi
            entity: re(r"&(#[xX][0-9a-fA-F]+|#[0-9]+|[A-Za-z][A-Za-z0-9]*);"),
            space_run: re(&format!("[{ws}]+")),
            // /([^\s"'<>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'<>`]+)))?/g
            attr: re(&format!(
                r#"([^{ws}"'<>/=]+)(?:[{ws}]*=[{ws}]*(?:"([^"]*)"|'([^']*)'|([^{ws}"'<>`]+)))?"#
            )),
            meta_tag: re(&tag("meta")),
            // /<title\b[^>]*>([\s\S]*?)<\/title>/i
            title: re(&format!(
                r"<{}(?:[^A-Za-z0-9_>][^>]*)?>((?s:.)*?)</{}>",
                ci("title"),
                ci("title")
            )),
            link_tag: re(&tag("link")),
        }
    })
}

/// `decodeHtml`: the charset from the Content-Type, else from a `<meta … charset=…>` in the first
/// 2048 bytes (read as latin1), else UTF-8; a label `TextDecoder` refuses falls back to UTF-8.
/// Decodes like `TextDecoder.decode`: a BOM of that encoding is dropped, malformed bytes become
/// U+FFFD, and no BOM sniffing switches the encoding.
pub(crate) fn decode_html(bytes: &[u8], content_type: &str) -> String {
    let p = patterns();
    let mut label = p.ct_charset.captures(content_type).map(|c| c[1].to_owned());
    if label.is_none() {
        let head = js::latin1(&bytes[..bytes.len().min(2048)]);
        label = p.meta_charset.captures(&head).map(|c| c[1].to_owned());
    }
    let encoding = match label {
        None => UTF_8,
        // `TextDecoder` throws for an unknown label and for the "replacement" encoding.
        Some(l) => Encoding::for_label(l.as_bytes())
            .filter(|e| *e != REPLACEMENT)
            .unwrap_or(UTF_8),
    };
    if NODE20_LATIN1_DECODER && encoding == WINDOWS_1252 {
        return js::latin1(bytes);
    }
    encoding.decode_with_bom_removal(bytes).0.into_owned()
}

/// Electron 33 ships Node 20, whose `TextDecoder` decodes every label of the WHATWG
/// windows-1252 encoding (`windows-1252`, `iso-8859-1`, `latin1`, `ascii`, `cp1252`, …) as
/// ISO-8859-1: byte 0x80–0x9F become C1 controls (U+0080–U+009F) instead of `€ ‚ ƒ … “ ” –`.
/// That's what the Electron client shows today, so it's mirrored; set this to `false` to decode
/// those pages as browsers do (Node fixed it in later versions).
const NODE20_LATIN1_DECODER: bool = true;

const NAMED_ENTITIES: &[(&str, &str)] = &[
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    ("nbsp", "\u{A0}"),
    ("ndash", "–"),
    ("mdash", "—"),
    ("hellip", "…"),
    ("copy", "©"),
    ("reg", "®"),
    ("trade", "™"),
    ("laquo", "«"),
    ("raquo", "»"),
    ("lsquo", "‘"),
    ("rsquo", "’"),
    ("ldquo", "“"),
    ("rdquo", "”"),
    ("bull", "•"),
    ("middot", "·"),
    ("euro", "€"),
    ("pound", "£"),
    ("yen", "¥"),
    ("deg", "°"),
    ("times", "×"),
];

/// `decodeEntities`: numeric references (decimal / hex, 1..=0x10FFFF) and a short list of named
/// ones (case-insensitive); anything else is left as written. JS builds a UTF-16 string, so a
/// reference to a high surrogate directly followed by one to a low surrogate (`&#xD83D;&#xDE00;`)
/// makes one character there too; a surrogate left alone becomes U+FFFD (what Chromium draws for
/// the lone surrogate JS would hold).
pub(crate) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    // A high surrogate waiting for an adjacent low one, and where its reference ended.
    let mut high: Option<(u32, usize)> = None;
    let flush_high = |out: &mut String, high: &mut Option<(u32, usize)>| {
        if high.take().is_some() {
            out.push('\u{FFFD}');
        }
    };
    for c in patterns().entity.captures_iter(s) {
        let whole = c.get(0).expect("match");
        if let Some((_, end)) = high {
            if end != whole.start() {
                flush_high(&mut out, &mut high);
            }
        }
        out.push_str(&s[last..whole.start()]);
        last = whole.end();
        let e = &c[1];
        let Some(num) = e.strip_prefix('#') else {
            flush_high(&mut out, &mut high);
            let lower = e.to_ascii_lowercase();
            match NAMED_ENTITIES.iter().find(|(name, _)| *name == lower) {
                Some((_, v)) => out.push_str(v),
                None => out.push_str(whole.as_str()),
            }
            continue;
        };
        let (digits, radix) = match num.strip_prefix(['x', 'X']) {
            Some(h) => (h, 16),
            None => (num, 10),
        };
        // An overflow is "> 0x10ffff" (or Infinity in JS): left as written.
        let cp = match u32::from_str_radix(digits, radix) {
            Ok(cp) if cp > 0 && cp <= 0x10FFFF => cp,
            _ => {
                flush_high(&mut out, &mut high);
                out.push_str(whole.as_str());
                continue;
            }
        };
        match cp {
            0xD800..=0xDBFF => {
                flush_high(&mut out, &mut high);
                high = Some((cp, whole.end()));
            }
            0xDC00..=0xDFFF => match high.take() {
                Some((h, _)) => {
                    let joined = 0x10000 + ((h - 0xD800) << 10) + (cp - 0xDC00);
                    out.push(char::from_u32(joined).expect("valid pair"));
                }
                None => out.push('\u{FFFD}'),
            },
            _ => {
                flush_high(&mut out, &mut high);
                out.push(char::from_u32(cp).expect("not a surrogate"));
            }
        }
    }
    flush_high(&mut out, &mut high);
    out.push_str(&s[last..]);
    out
}

/// `clean`: whitespace runs collapsed to one space, trimmed, and cut to `max` UTF-16 units with an
/// ellipsis (the cut keeps `max - 1` units, trimmed at the end).
pub(crate) fn clean(s: &str, max: usize) -> String {
    let collapsed = patterns().space_run.replace_all(s, " ");
    let t = js::trim(&collapsed);
    if js::len16(t) > max {
        let cut = js::slice16(t, max - 1);
        format!("{}…", js::trim_end(&cut))
    } else {
        t.to_owned()
    }
}

/// `parseAttrs`: attribute names lowercased, first occurrence wins, a bare attribute is `""`.
pub(crate) fn parse_attrs(s: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for a in patterns().attr.captures_iter(s) {
        let v = a
            .get(2)
            .or(a.get(3))
            .or(a.get(4))
            .map_or("", |m| m.as_str());
        out.entry(a[1].to_lowercase())
            .or_insert_with(|| v.to_owned());
    }
    out
}

/// What `parsePage` finds in a document head.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct PageMeta {
    /// `<meta>` contents by lowercased `property` / `name` / `itemprop` (first wins), entities
    /// decoded, untrimmed.
    pub meta: BTreeMap<String, String>,
    /// The first `<title>`, entities decoded and cleaned; `None` when absent or blank.
    pub title: Option<String>,
    /// The first `<link type="application/json+oembed" href=…>`'s href, entities decoded.
    pub oembed: Option<String>,
}

/// `parsePage`.
pub(crate) fn parse_page(html: &str) -> PageMeta {
    let p = patterns();
    let mut meta = BTreeMap::new();
    for m in p.meta_tag.captures_iter(html) {
        let attrs = parse_attrs(&m[1]);
        // `??`, not `||`: an empty `property=""` is a key ("") and is skipped, not a fallback.
        let Some(key) = attrs
            .get("property")
            .or(attrs.get("name"))
            .or(attrs.get("itemprop"))
        else {
            continue;
        };
        let key = js::trim(key).to_lowercase();
        let Some(content) = attrs.get("content") else {
            continue;
        };
        if key.is_empty() || meta.contains_key(&key) {
            continue;
        }
        meta.insert(key, decode_entities(content));
    }
    let title = p
        .title
        .captures(html)
        .map(|t| clean(&decode_entities(&t[1]), TITLE_MAX))
        .filter(|t| !t.is_empty());
    let mut oembed = None;
    for m in p.link_tag.captures_iter(html) {
        let attrs = parse_attrs(&m[1]);
        let ty = attrs
            .get("type")
            .map(|t| t.to_lowercase())
            .unwrap_or_default();
        if let Some(href) = attrs.get("href").filter(|h| !h.is_empty()) {
            if ty == "application/json+oembed" {
                oembed = Some(decode_entities(href));
                break;
            }
        }
    }
    PageMeta {
        meta,
        title,
        oembed,
    }
}

/// `pick`: the first of `keys` whose value is non-blank, trimmed.
pub(crate) fn pick<'a>(meta: &'a BTreeMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| meta.get(*k).map(|v| js::trim(v)).filter(|v| !v.is_empty()))
}

/// `toInt`: `parseInt(s, 10)` when it lands in 1..=99_999, else 0.
pub(crate) fn to_int(s: Option<&str>) -> u32 {
    match s.filter(|s| !s.is_empty()).and_then(js::parse_int) {
        Some(n) if n > 0.0 && n < 100_000.0 => n as u32,
        _ => 0,
    }
}

/// `toInt(String(n))` for a JSON number (`thumbnail_width`): JS stringifies first, so `1e21`
/// becomes `"1e+21"` and parses as 1.
pub(crate) fn to_int_number(n: f64) -> u32 {
    to_int(Some(&js::number_to_string(n)))
}

/// `resolveImage`: a page-relative image reference resolved against `base`, `http:` upgraded to
/// `https:` (the renderer only loads https images), anything else, userinfo, or an href over
/// 2048 characters refused.
pub(crate) fn resolve_image(raw: Option<&str>, base: &str) -> Option<String> {
    let raw = raw.filter(|r| !r.is_empty())?;
    let base = Url::parse(base).ok()?;
    let mut u = base.join(js::trim(raw)).ok()?;
    if u.scheme() == "http" {
        u.set_scheme("https").ok()?;
    }
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some_and(|p| !p.is_empty())
    {
        return None;
    }
    let href: String = u.into();
    (href.len() <= URL_MAX).then_some(href)
}

/// `validColor`: `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`, lowercased.
pub(crate) fn valid_color(raw: Option<&str>) -> Option<String> {
    let v = js::trim(raw.unwrap_or(""));
    let hex = v.strip_prefix('#')?;
    (matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| v.to_ascii_lowercase())
}

/// `/^(text\/html|application\/xhtml\+xml)\b/` on the lowercased Content-Type.
pub(crate) fn is_html(content_type: &str) -> bool {
    ["text/html", "application/xhtml+xml"].iter().any(|t| {
        content_type.strip_prefix(t).is_some_and(|rest| {
            !rest
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities() {
        assert_eq!(decode_entities("a &amp; b &AMP; &Amp;"), "a & b & &");
        assert_eq!(
            decode_entities("&#39;&#x27;&#X27;&#0;&#x110000;&bogus;&amp"),
            "'''&#0;&#x110000;&bogus;&amp"
        );
        assert_eq!(decode_entities("&#xD800;"), "\u{FFFD}");
        assert_eq!(decode_entities("&#xD83D;&#xDE00;"), "😀");
        assert_eq!(decode_entities("&#xD83D; &#xDE00;"), "\u{FFFD} \u{FFFD}");
        assert_eq!(decode_entities("&#xDE00;&#xD83D;x"), "\u{FFFD}\u{FFFD}x");
        assert_eq!(decode_entities("&nbsp;&hellip;&#128512;"), "\u{A0}…😀");
        assert_eq!(
            decode_entities("&#99999999999999999999999;"),
            "&#99999999999999999999999;"
        );
    }

    #[test]
    fn cleaning() {
        assert_eq!(clean("  a \n\t b\u{3000}", 10), "a b");
        assert_eq!(clean("abcdefghijk", 5), "abcd…");
        assert_eq!(clean("abc defghijk", 5), "abc…");
        assert_eq!(clean("ab😀cd", 4), "ab\u{FFFD}…");
    }

    #[test]
    fn attributes() {
        let a = parse_attrs(r#" property="og:title" CONTENT='x "y"' data-x=1 bare name=a/b"#);
        assert_eq!(a["property"], "og:title");
        assert_eq!(a["content"], "x \"y\"");
        assert_eq!(a["data-x"], "1");
        assert_eq!(a["bare"], "");
        assert_eq!(a["name"], "a/b"); // `/` is allowed in an unquoted value
    }

    #[test]
    fn tags_need_a_word_boundary() {
        let p = parse_page(
            r#"<metadata property="a" content="x"><meta/property="b" content="y"><META property="c" content="z">"#,
        );
        assert!(!p.meta.contains_key("a"));
        assert_eq!(p.meta["b"], "y");
        assert_eq!(p.meta["c"], "z");
        assert_eq!(parse_page("<titles>x</title>").title, None);
        assert_eq!(
            parse_page("<title lang=en>x</TITLE>").title.as_deref(),
            Some("x")
        );
    }

    #[test]
    fn colors_and_types() {
        assert_eq!(valid_color(Some(" #ABCDEF ")).as_deref(), Some("#abcdef"));
        assert_eq!(valid_color(Some("#abcde")), None);
        assert_eq!(valid_color(Some("red")), None);
        assert!(is_html("text/html; charset=utf-8"));
        assert!(is_html("application/xhtml+xml"));
        assert!(!is_html("text/html5"));
        assert!(!is_html("text/plain"));
    }

    #[test]
    fn to_int_cases() {
        assert_eq!(to_int(Some("1200")), 1200);
        assert_eq!(to_int(Some(" 630px")), 630);
        assert_eq!(to_int(Some("0")), 0);
        assert_eq!(to_int(Some("100000")), 0);
        assert_eq!(to_int(Some("-5")), 0);
        assert_eq!(to_int(Some("")), 0);
        assert_eq!(to_int(None), 0);
        assert_eq!(to_int_number(1e21), 1);
        assert_eq!(to_int_number(480.9), 480);
        assert_eq!(to_int_number(5e-7), 5);
    }
}
