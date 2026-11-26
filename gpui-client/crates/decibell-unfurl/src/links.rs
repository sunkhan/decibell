//! Pure link rules the message view needs next to the unfurler:
//!
//! - the one grammar for `decibell://invite/…` links (`src/features/servers/inviteLink.ts`): the
//!   chat autolinker, the invite card, the deep-link receiver and the browse view's paste box all
//!   go through it;
//! - which lone media links read as "GIF" / "Image" (`mediaLabelFor` in `richText.ts`) and which
//!   image URLs are GIFs by their path (`isGifUrl` in `LinkEmbeds.tsx`);
//! - which links may be opened in the OS browser (`decibell:shell:openExternal`).
//!
//! Not here: which URLs in a message get a card (`extractLinks`) and the lone-link rule
//! (`loneLink`). Both walk the rich-text parse (autolinks outside code and math, the `<url>`
//! no-preview form), so they belong with the rich-text parser's port. The UI then hides a lone
//! link's text only once its preview resolved to [`crate::LinkPreview::Image`], with previews on and
//! embeds not suppressed (`MessageBubble.tsx`).

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::js;

/// A parsed invite link. `host` / `port` only come with the older endpoint-carrying shapes;
/// otherwise central resolves the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvite {
    /// As written (native uppercases it before redeeming).
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

fn invite_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let ws = js::SPACE_CLASS;
        // /^decibell:\/\/invite\/(?:(?:(\[[0-9a-f:.]+\]|[^\s/:]+):(\d{1,5})|([^\s/:]+)\/(\d{1,5}))\/)?([A-Za-z0-9]{4,64})\/?$/i
        Regex::new(&format!(
            r"^{}://{}/(?:(?:(\[[0-9a-fA-F:.]+\]|[^{ws}/:]+):([0-9]{{1,5}})|([^{ws}/:]+)/([0-9]{{1,5}}))/)?([A-Za-z0-9]{{4,64}})/?$",
            js::ci("decibell"),
            js::ci("invite"),
        ))
        .expect("static pattern")
    })
}

/// `parseInviteLink`: `decibell://invite/<code>` (what the Invites modal copies), or the older
/// `decibell://invite/<host>:<port>/<code>` / `…/<host>/<port>/<code>` (host may be a bracketed
/// IPv6 literal). Codes are 4–64 letters and digits; the scheme and `invite` are
/// case-insensitive.
pub fn parse_invite_link(url: &str) -> Option<PendingInvite> {
    let m = invite_re().captures(url)?;
    let code = m[5].to_owned();
    let Some(raw_host) = m.get(1).or(m.get(3)) else {
        return Some(PendingInvite {
            code,
            host: None,
            port: None,
        });
    };
    let host = raw_host
        .as_str()
        .strip_prefix('[')
        .unwrap_or(raw_host.as_str());
    let host = host.strip_suffix(']').unwrap_or(host);
    let port: u32 = m.get(2).or(m.get(4))?.as_str().parse().ok()?;
    if host.is_empty() || !(1..=65535).contains(&port) {
        return None;
    }
    Some(PendingInvite {
        code,
        host: Some(host.to_owned()),
        port: Some(port as u16),
    })
}

/// `isInviteLink`.
pub fn is_invite_link(url: &str) -> bool {
    parse_invite_link(url).is_some()
}

/// `buildInviteLink`: the link the Invites modal hands out (code only).
pub fn build_invite_link(code: &str) -> String {
    format!("decibell://invite/{code}")
}

/// What a lone media link reads as in a one-line preview (the DM list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaLabel {
    #[serde(rename = "GIF")]
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

/// The URL's WHATWG `pathname`, or `None` when it doesn't parse.
fn pathname(href: &str) -> Option<String> {
    Url::parse(href).ok().map(|u| u.path().to_owned())
}

fn ends_with_ci(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len()
        && s.as_bytes()[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

/// `mediaLabelFor`: "GIF" for a `.gif` path, "Image" for `.png` / `.jpg` / `.jpeg` / `.webp` /
/// `.avif`, else `None` (show the URL).
pub fn media_label_for(href: &str) -> Option<MediaLabel> {
    let p = pathname(href)?;
    if ends_with_ci(&p, ".gif") {
        Some(MediaLabel::Gif)
    } else if [".png", ".jpg", ".jpeg", ".webp", ".avif"]
        .iter()
        .any(|e| ends_with_ci(&p, e))
    {
        Some(MediaLabel::Image)
    } else {
        None
    }
}

/// `isGifUrl`: an image URL that is a GIF by its path (KLIPY's `….gif`, GIPHY's
/// `giphy.gif?cid=…`). The UI uses it beside [`crate::ImagePreview::gif`].
pub fn is_gif_url(url: &str) -> bool {
    pathname(url).is_some_and(|p| ends_with_ci(&p, ".gif"))
}

/// The `decibell:shell:openExternal` check: an http(s) URL of at most 2048 characters, returned
/// normalised (`href`) for the OS browser; anything else is dropped.
pub fn external_link_target(url: &str) -> Option<String> {
    if js::len16(url) > 2048 {
        return None;
    }
    let u = Url::parse(url).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invites() {
        let code_only = PendingInvite {
            code: "ABCDEFGHIJ".into(),
            host: None,
            port: None,
        };
        assert_eq!(
            parse_invite_link("decibell://invite/ABCDEFGHIJ"),
            Some(code_only.clone())
        );
        assert_eq!(
            parse_invite_link("DECIBELL://Invite/ABCDEFGHIJ/"),
            Some(code_only)
        );
        assert_eq!(
            parse_invite_link("decibell://invite/example.com:8082/abcd1234"),
            Some(PendingInvite {
                code: "abcd1234".into(),
                host: Some("example.com".into()),
                port: Some(8082)
            })
        );
        assert_eq!(
            parse_invite_link("decibell://invite/[::1]:8082/abcd1234"),
            Some(PendingInvite {
                code: "abcd1234".into(),
                host: Some("::1".into()),
                port: Some(8082)
            })
        );
        assert_eq!(
            parse_invite_link("decibell://invite/10.0.0.2/8082/abcd1234"),
            Some(PendingInvite {
                code: "abcd1234".into(),
                host: Some("10.0.0.2".into()),
                port: Some(8082)
            })
        );
        assert_eq!(parse_invite_link("decibell://invite/host:0/abcd1234"), None);
        assert_eq!(
            parse_invite_link("decibell://invite/host:70000/abcd1234"),
            None
        );
        assert_eq!(parse_invite_link("decibell://invite/abc"), None);
        assert_eq!(parse_invite_link("decibell://invite/ABCD EFGH"), None);
        assert_eq!(parse_invite_link("https://invite/ABCDEFGHIJ"), None);
        assert!(is_invite_link("decibell://invite/ABCDEFGHIJ"));
        assert_eq!(
            build_invite_link("XYZ123ABCD"),
            "decibell://invite/XYZ123ABCD"
        );
    }

    #[test]
    fn media() {
        assert_eq!(
            media_label_for("https://static.klipy.com/x/abc.GIF"),
            Some(MediaLabel::Gif)
        );
        assert_eq!(
            media_label_for("https://media.giphy.com/media/x/giphy.gif?cid=1"),
            Some(MediaLabel::Gif)
        );
        assert_eq!(
            media_label_for("https://x.y/a.jpeg#f"),
            Some(MediaLabel::Image)
        );
        assert_eq!(media_label_for("https://x.y/a.svg"), None);
        assert_eq!(media_label_for("not a url"), None);
        assert!(is_gif_url("https://x.y/a.gif"));
        assert!(!is_gif_url("https://x.y/a.gif.png"));
    }

    #[test]
    fn external() {
        assert_eq!(
            external_link_target("https://Example.com/a b").as_deref(),
            Some("https://example.com/a%20b")
        );
        assert_eq!(external_link_target("javascript:alert(1)"), None);
        assert_eq!(external_link_target("file:///etc/passwd"), None);
        assert_eq!(external_link_target("decibell://invite/ABCD"), None);
        assert_eq!(
            external_link_target(&format!("https://x.y/{}", "a".repeat(2048))),
            None
        );
    }
}
