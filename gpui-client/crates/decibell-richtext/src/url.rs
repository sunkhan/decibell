//! The slice of `new URL(…)` the renderer's link code depends on, as **Electron 33's Blink**
//! computes it (Chromium 130's URL canonicalizer, ICU 74 / Unicode 15.1 IDNA), not as Node or the
//! WHATWG spec do. Two answers are needed:
//!
//! * does it parse, and is its `hostname` dotted or `localhost` (`isLinkable` in richText.ts)
//! * its `pathname` (`mediaLabelFor` / `isGifUrl` test the extension)
//!
//! Blink here is the WHATWG algorithm except (all measured against the renderer, see
//! tests/vectors/host-sweep.json and the url suite):
//! * a space in a host (typed as `%20`, or produced by IDNA mapping such as U+00A8 → " ̈") is
//!   escaped and accepted, not rejected; `*` is escaped too
//! * U+200C / U+200D pass IDNA (no CheckJoiners), still subject to the bidi rule
//! * IDNA data is Unicode 15.1: code points newer than that, and a few whose UTS #46 status
//!   changed in 16.0, fail (table `CHROMIUM_IDNA_REJECT`, derived from the sweep)
//! * a path `%2e` / `%2E` is decoded to `.` (other escapes are kept as typed)
//! * `*` and a space enter IDNA already escaped (`%2A`, `%20`), invalid ASCII too, and the IDNA
//!   output is unescaped again, so a following combining mark can compose with an escape's hex
//!   letter (`a*\u{301}b` → `xn--a+-7na`); `[`, `]` and `:` pass the character table and only the
//!   IP-address check refuses them outside a bracketed IPv6 literal
//! * a non-special (opaque) host is checked on each UTF-16 unit's low byte (🏽 fails like `<`),
//!   and two leading slashes of either kind start its authority

use std::borrow::Cow;

/// What [`parse`] found.
pub(crate) struct Parsed {
    /// `hostname.includes(".") || hostname === "localhost"`.
    pub dotted_or_localhost: bool,
    /// `protocol` is `http:` or `https:`.
    pub http: bool,
    /// `pathname`, when asked for.
    pub path: Option<String>,
}

/// `new URL(input)` (no base). `None` = it throws.
pub(crate) fn parse(input: &str, want_path: bool) -> Option<Parsed> {
    // TrimURL: leading / trailing C0 controls and space; then tab / LF / CR anywhere.
    let t = input.trim_matches(|c: char| c <= ' ');
    let cleaned: Cow<'_, str> = if t.contains(['\t', '\n', '\r']) {
        Cow::Owned(t.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect())
    } else {
        Cow::Borrowed(t)
    };
    let s: &str = &cleaned;
    let b = s.as_bytes();

    // Scheme: ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"
    let colon = s.find(':')?;
    let scheme = &s[..colon];
    if scheme.is_empty() || !b[0].is_ascii_alphabetic() || !scheme.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.')) {
        return None;
    }
    let lower = |x: &str| scheme.eq_ignore_ascii_case(x);
    let special = lower("http") || lower("https") || lower("ws") || lower("wss") || lower("ftp");
    let http = lower("http") || lower("https");
    let rest = &s[colon + 1..];

    if special {
        let rb = rest.as_bytes();
        let mut a = 0;
        while a < rb.len() && (rb[a] == b'/' || rb[a] == b'\\') {
            a += 1;
        }
        let mut e = a;
        while e < rb.len() && !matches!(rb[e], b'/' | b'\\' | b'?' | b'#') {
            e += 1;
        }
        let authority = &rest[a..e];
        let server = match authority.rfind('@') {
            Some(at) => &authority[at + 1..],
            None => authority,
        };
        let (host, port) = split_host_port(server);
        if let Some(p) = port {
            if !port_ok(p) {
                return None;
            }
        }
        let dotted = host_verdict(host)?;
        let path = if want_path { Some(canonical_path(&rest[e..], true)) } else { None };
        return Some(Parsed { dotted_or_localhost: dotted, http, path });
    }
    if lower("file") {
        // Not reached by the client's callers; the path is all they read.
        let r = rest.trim_start_matches(['/', '\\']);
        let r = &r[r.find(['/', '\\', '?', '#']).unwrap_or(r.len())..];
        let path = if want_path { Some(canonical_path(r, true)) } else { None };
        return Some(Parsed { dotted_or_localhost: false, http: false, path });
    }
    // Non-special: `//authority` then a path, or an opaque path.
    let mut r = rest;
    let two_slashes = rest.len() >= 2 && rest.as_bytes()[..2].iter().all(|&c| c == b'/' || c == b'\\');
    if let Some(after) = two_slashes.then(|| &rest[2..]) {
        let e = after.find(['/', '?', '#']).unwrap_or(after.len());
        let authority = &after[..e];
        let server = match authority.rfind('@') {
            Some(at) => &authority[at + 1..],
            None => authority,
        };
        let (host, port) = split_host_port(server);
        if let Some(p) = port {
            if !port_ok(p) {
                return None;
            }
        }
        if host.starts_with('[') {
            if !host.ends_with(']') || !ipv6_ok(&host[1..host.len() - 1]) {
                return None;
            }
        } else if opaque_host_invalid(host) || (host.is_empty() && (port.is_some() || authority.contains('@'))) {
            return None;
        }
        r = &after[e..];
        let path = if want_path { Some(canonical_path(r, false)) } else { None };
        return Some(Parsed { dotted_or_localhost: false, http: false, path });
    }
    let end = r.find(['?', '#']).unwrap_or(r.len());
    let p = &r[..end];
    let path = if want_path {
        Some(if p.starts_with('/') { canonical_path(p, false) } else { escape_opaque(p) })
    } else {
        None
    };
    Some(Parsed { dotted_or_localhost: false, http: false, path })
}

/// A non-special URL's (opaque) host: Chromium 130 tests each UTF-16 unit's **low byte** against
/// the forbidden host code points (it truncates to `char`), so U+0120 fails like a space and an
/// astral char fails when a surrogate ends in 0x3C (`<`), e.g. 🏽 D83C DFFD.
fn opaque_host_invalid(host: &str) -> bool {
    let bad = |u: u32| matches!((u & 0xFF) as u8, 0x00 | 0x09 | 0x0A | 0x0D | 0x20 | b'#' | b'/' | b':' | b'<' | b'>' | b'?' | b'@' | b'[' | b'\\' | b']' | b'^' | b'|');
    host.chars().any(|c| {
        let mut buf = [0u16; 2];
        c.encode_utf16(&mut buf).iter().any(|&u| bad(u as u32))
    })
}

/// ParseServerInfo: a `[` start makes the host an IPv6 literal up to the last `]`; the port is
/// after the last `:` that follows it.
fn split_host_port(server: &str) -> (&str, Option<&str>) {
    let b = server.as_bytes();
    let mut v6_end: isize = if b.first() == Some(&b'[') { b.len() as isize } else { -1 };
    let mut colon: isize = -1;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b']' => v6_end = i as isize,
            b':' => colon = i as isize,
            _ => {}
        }
    }
    if colon > v6_end {
        let c = colon as usize;
        (&server[..c], Some(&server[c + 1..]))
    } else {
        (server, None)
    }
}

/// Empty, or digits (leading zeros free) up to 65535.
fn port_ok(p: &str) -> bool {
    if p.is_empty() {
        return true;
    }
    if !p.bytes().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let t = p.trim_start_matches('0');
    t.len() <= 5 && t.parse::<u32>().map_or(true, |v| v <= 65535)
}

/// Chromium's host character table (`kHostCharLookup`) for one ASCII byte: `Some(out)` with the
/// canonical form, `None` when the host is invalid.
fn host_char(c: u8) -> Option<&'static str> {
    const LIT: &str = "\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0 !\"#$%&'()*+,-./0123456789:;<=>?@abcdefghijklmnopqrstuvwxyz[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
    match c {
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'!' | b'"' | b'$' | b'&' | b'\'' | b'(' | b')' | b'+'
        | b',' | b';' | b'=' | b'`' | b'{' | b'}' | b'~' | b'[' | b']' | b':' => Some(&LIT[c as usize..c as usize + 1]),
        b' ' => Some("%20"),
        b'*' => Some("%2A"),
        _ => None,
    }
}

/// Chromium `DoSimpleHost`: unescape `%XX` (an invalid escape fails), then the character table.
/// `Err(())` = invalid; a non-ASCII byte (raw or unescaped) is reported as `Ok(None)`.
fn simple_host(s: &str, out: &mut String) -> Result<bool, ()> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut non_ascii = false;
    while i < b.len() {
        let mut c = b[i];
        if c == b'%' {
            let h = |x: u8| (x as char).to_digit(16);
            match (b.get(i + 1).and_then(|&x| h(x)), b.get(i + 2).and_then(|&x| h(x))) {
                (Some(x), Some(y)) => {
                    c = (x * 16 + y) as u8;
                    i += 2;
                }
                _ => return Err(()),
            }
        }
        i += 1;
        if c >= 0x80 {
            non_ascii = true;
            out.push(c as char); // placeholder: only ASCII-ness matters to the caller
            continue;
        }
        out.push_str(host_char(c).ok_or(())?);
    }
    Ok(non_ascii)
}

/// Blink's host canonicalization for a special URL. `None` = invalid; `Some(dotted)` otherwise.
///
/// Chromium (`url_canon_host.cc`): an ASCII host goes through the table; one with escapes is
/// unescaped (and tabled) first; one with non-ASCII is IDNA-converted after its ASCII part went
/// through the table — so `*` and a space enter IDNA as `%2A` / `%20` (where a following
/// combining mark can compose with the `A`) — and the IDNA output is unescaped and tabled again.
fn host_verdict(host: &str) -> Option<bool> {
    if host.is_empty() {
        return None;
    }
    if host.is_ascii() && !host.contains('%') {
        // The common case, judged in place: lower-casing and escaping ' ' / '*' change neither
        // the dots, nor "localhost", nor whether the last label is a number or the host has
        // brackets / colons.
        if !host.bytes().all(|c| host_char(c).is_some()) {
            return None;
        }
        return ip_and_dots(host);
    }
    let mut canon = String::with_capacity(host.len() + 8);
    if host.is_ascii() || host.contains('%') {
        // The table (and unescape) pass; with non-ASCII left over, IDNA on what it spells.
        if simple_host(host, &mut canon).ok()? {
            let decoded = String::from_utf8(percent_decode(host)?).ok()?;
            canon.clear();
            idn_host(&decoded, &mut canon)?;
        }
    } else {
        idn_host(host, &mut canon)?;
    }
    ip_and_dots(&canon)
}

/// CanonicalizeIPAddress over the canonical host, then the `isLinkable` question: IPv4 when the
/// last label is a number (WHATWG "ends in a number"), else IPv6 when bracketed; `[`, `]` or `:`
/// anywhere else make the host BROKEN.
fn ip_and_dots(canon: &str) -> Option<bool> {
    if canon.is_empty() {
        return None;
    }
    if ends_in_number(canon) {
        return if ipv4_ok(canon) { Some(true) } else { None };
    }
    if canon.contains(['[', ']', ':']) {
        let ok = canon.len() >= 2 && canon.starts_with('[') && canon.ends_with(']') && ipv6_ok(&canon[1..canon.len() - 1]);
        return if ok { Some(false) } else { None };
    }
    Some(canon.contains('.') || canon.eq_ignore_ascii_case("localhost"))
}

fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = |x: u8| (x as char).to_digit(16);
            match (b.get(i + 1).and_then(|&x| h(x)), b.get(i + 2).and_then(|&x| h(x))) {
                (Some(x), Some(y)) => {
                    out.push((x * 16 + y) as u8);
                    i += 3;
                }
                _ => return None,
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

/// `DoIDNHost`: table pass (escaping ' ', '*' and invalid ASCII), IDNA, then
/// `DoSimpleHost` over the result (whose own escapes are decoded; non-ASCII there fails).
fn idn_host(decoded: &str, canon: &mut String) -> Option<()> {
    // This pass's own verdict is ignored: an invalid ASCII char enters IDNA escaped ("%5E").
    let mut pre = String::with_capacity(decoded.len() + 8);
    for c in decoded.chars() {
        if c.is_ascii() {
            match host_char(c as u8) {
                Some(t) => pre.push_str(t),
                None => push_escaped(&mut pre, c as u8),
            }
        } else {
            pre.push(c);
        }
    }
    let ascii = idna_to_ascii(&pre)?;
    if simple_host(&ascii, canon).ok()? {
        return None;
    }
    Some(())
}

/// UTS #46 ToASCII as Chromium 130 calls ICU: nontransitional, CheckBidi, no CheckJoiners, no
/// STD3 rules, hyphen / length / empty-label errors allowed.
fn idna_to_ascii(domain: &str) -> Option<String> {
    use idna::uts46::{AsciiDenyList, DnsLength, Hyphens, Uts46};
    if domain.chars().any(chromium_idna_rejects) {
        return None;
    }
    // ZWNJ / ZWJ: the idna crate always applies CheckJoiners. Swap them for '_' (bidi class ON
    // like BN in every rule of RFC 5893 that can tell: allowed mid-label, refused at a label's
    // start or end of a bidi domain), which keeps every other verdict.
    let input: Cow<'_, str> = if domain.contains(['\u{200C}', '\u{200D}']) {
        Cow::Owned(domain.replace(['\u{200C}', '\u{200D}'], "_"))
    } else {
        Cow::Borrowed(domain)
    };
    Uts46::new()
        .to_ascii(input.as_bytes(), AsciiDenyList::EMPTY, Hyphens::Allow, DnsLength::Ignore)
        .ok()
        .map(Cow::into_owned)
}

fn chromium_idna_rejects(c: char) -> bool {
    let c = c as u32;
    let t = &crate::url_tables::CHROMIUM_IDNA_REJECT;
    let (mut lo, mut hi) = (0usize, t.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        let (a, b) = t[mid];
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

/// WHATWG "ends in a number".
fn ends_in_number(host: &str) -> bool {
    let h = match host.strip_suffix('.') {
        Some("") => return false,
        Some(h) => h,
        None => host,
    };
    let last = h.rsplit('.').next().unwrap_or("");
    if !last.is_empty() && last.bytes().all(|c| c.is_ascii_digit()) {
        return true;
    }
    ipv4_number(last).is_some()
}

fn ipv4_number(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let (digits, radix) = if s.len() >= 2 && (s.starts_with("0x") || s.starts_with("0X")) {
        (&s[2..], 16)
    } else if s.len() >= 2 && s.starts_with('0') {
        (&s[1..], 8)
    } else {
        (s, 10)
    };
    if digits.is_empty() {
        return Some(0);
    }
    if !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    // Saturate: anything past 2^32 fails the range checks the same way.
    let mut v: u64 = 0;
    for c in digits.chars() {
        v = v.saturating_mul(radix as u64).saturating_add(c.to_digit(radix).unwrap() as u64);
        if v > 1 << 40 {
            v = 1 << 40;
        }
    }
    Some(v)
}

/// WHATWG IPv4 parser (validity only).
fn ipv4_ok(host: &str) -> bool {
    let h = match host.strip_suffix('.') {
        Some(h) if !h.is_empty() => h,
        _ => host,
    };
    let n = h.split('.').count();
    if n > 4 {
        return false;
    }
    for (i, p) in h.split('.').enumerate() {
        let Some(v) = ipv4_number(p) else { return false };
        if i + 1 < n {
            if v > 255 {
                return false;
            }
        } else if v >= 256u64.pow(5 - n as u32) {
            return false;
        }
    }
    true
}

/// WHATWG IPv6 parser (validity only).
fn ipv6_ok(s: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    let at = |p: usize| c.get(p).copied();
    let mut piece = 0usize;
    let mut compress: Option<usize> = None;
    let mut p = 0usize;
    if at(p) == Some(':') {
        if at(p + 1) != Some(':') {
            return false;
        }
        p += 2;
        piece += 1;
        compress = Some(piece);
    }
    while at(p).is_some() {
        if piece == 8 {
            return false;
        }
        if at(p) == Some(':') {
            if compress.is_some() {
                return false;
            }
            p += 1;
            piece += 1;
            compress = Some(piece);
            continue;
        }
        let mut len = 0;
        while len < 4 && at(p).is_some_and(|x| x.is_ascii_hexdigit()) {
            p += 1;
            len += 1;
        }
        if at(p) == Some('.') {
            if len == 0 {
                return false;
            }
            p -= len;
            if piece > 6 {
                return false;
            }
            let mut seen = 0;
            while at(p).is_some() {
                let mut v: Option<u32> = None;
                if seen > 0 {
                    if at(p) == Some('.') && seen < 4 {
                        p += 1;
                    } else {
                        return false;
                    }
                }
                if !at(p).is_some_and(|x| x.is_ascii_digit()) {
                    return false;
                }
                while let Some(d) = at(p).and_then(|x| x.to_digit(10)) {
                    v = match v {
                        None => Some(d),
                        Some(0) => return false,
                        Some(x) => Some(x * 10 + d),
                    };
                    if v.unwrap() > 255 {
                        return false;
                    }
                    p += 1;
                }
                seen += 1;
                if seen == 2 || seen == 4 {
                    piece += 1;
                }
            }
            if seen != 4 {
                return false;
            }
            break;
        } else if at(p) == Some(':') {
            p += 1;
            if at(p).is_none() {
                return false;
            }
        } else if at(p).is_some() {
            return false;
        }
        piece += 1;
    }
    if compress.is_some() {
        piece <= 8
    } else {
        piece == 8
    }
}

/// Blink's path canonicalization, up to the query / fragment. `special`: `\` is a slash.
fn canonical_path(raw: &str, special: bool) -> String {
    let end = raw.find(['?', '#']).unwrap_or(raw.len());
    let raw = &raw[..end];
    // Escape / unescape char by char, then resolve dot segments.
    let mut flat = String::with_capacity(raw.len() + 1);
    if !raw.starts_with('/') && !(special && raw.starts_with('\\')) {
        flat.push('/');
    }
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\\' if special => {
                flat.push('/');
                i += 1;
            }
            b'%' => {
                if i + 2 < b.len() && b[i + 1] == b'2' && (b[i + 2] == b'e' || b[i + 2] == b'E') {
                    flat.push('.');
                    i += 3;
                } else {
                    flat.push('%');
                    i += 1;
                }
            }
            0..=0x20 | b'"' | b'<' | b'>' | b'^' | b'`' | b'{' | b'|' | b'}' | 0x7f => {
                push_escaped(&mut flat, c);
                i += 1;
            }
            0x80..=0xff => {
                push_escaped(&mut flat, c);
                i += 1;
            }
            _ => {
                flat.push(c as char);
                i += 1;
            }
        }
    }
    // Dot segments.
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = flat[1..].split('/').collect();
    let n = segs.len();
    for (k, seg) in segs.iter().enumerate() {
        let last = k + 1 == n;
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                out.pop();
                if last {
                    out.push("");
                }
            }
            _ => out.push(seg),
        }
    }
    let mut p = String::with_capacity(flat.len());
    for seg in &out {
        p.push('/');
        p.push_str(seg);
    }
    if p.is_empty() {
        p.push('/');
    }
    p
}

fn escape_opaque(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for &c in p.as_bytes() {
        if !(0x20..0x7f).contains(&c) {
            push_escaped(&mut out, c);
        } else {
            out.push(c as char);
        }
    }
    out
}

fn push_escaped(out: &mut String, c: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    out.push(HEX[(c >> 4) as usize] as char);
    out.push(HEX[(c & 15) as usize] as char);
}

/// `pathname` ends with `.<ext>` (ASCII case-insensitive), for `mediaLabelFor` / `isGifUrl`.
pub(crate) fn path_ends_with_ext(path: &str, ext: &str) -> bool {
    let (pb, eb) = (path.as_bytes(), ext.as_bytes());
    pb.len() > eb.len() && pb[pb.len() - eb.len() - 1] == b'.' && pb[pb.len() - eb.len()..].eq_ignore_ascii_case(eb)
}
