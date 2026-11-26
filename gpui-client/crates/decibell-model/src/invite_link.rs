//! One grammar for `decibell://invite/…` links (`features/servers/inviteLink.ts`),
//! shared by the deep-link receiver, the browse view's paste box and a clicked link.
//! The link is the code alone — central resolves it to the community's host:port, so a
//! link never has to expose an address:
//!   `decibell://invite/<code>`
//! The endpoint-carrying shapes older links used still parse:
//!   `decibell://invite/<host>:<port>/<code>`
//!   `decibell://invite/<host>/<port>/<code>`
//! Host may be a bracketed IPv6 literal. Codes are 4–64 ASCII letters and digits (the
//! server's are 10 base32 characters; the core uppercases before redeeming).

use crate::stores::servers::PendingInvite;

fn is_code(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn port_of(s: &str) -> Option<u16> {
    if !(1..=5).contains(&s.len()) || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok().filter(|p| (1..=65535).contains(p)).map(|p| p as u16)
}

/// A host segment without the characters the grammar excludes (`[^\s/:]+`).
fn is_plain_host(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c == '/' || c == ':')
}

pub fn parse_invite_link(url: &str) -> Option<PendingInvite> {
    const SCHEME: &str = "decibell://invite/";
    if !url.get(..SCHEME.len())?.eq_ignore_ascii_case(SCHEME) {
        return None;
    }
    let body = &url[SCHEME.len()..];
    let body = body.strip_suffix('/').unwrap_or(body);
    let parts: Vec<&str> = body.split('/').collect();
    let (host, port, code) = match parts.as_slice() {
        [code] => return is_code(code).then(|| PendingInvite { code: (*code).to_string().into(), host: None, port: None }),
        // <host>:<port>/<code>, the host possibly a bracketed IPv6 literal.
        [endpoint, code] => {
            let (host, port) = endpoint.rsplit_once(':')?;
            let host = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
                Some(v6) => {
                    if v6.is_empty() || !v6.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.') {
                        return None;
                    }
                    v6
                }
                None => {
                    if !is_plain_host(host) {
                        return None;
                    }
                    host
                }
            };
            (host, port, code)
        }
        // <host>/<port>/<code>
        [host, port, code] => {
            if !is_plain_host(host) {
                return None;
            }
            (*host, *port, code)
        }
        _ => return None,
    };
    if !is_code(code) {
        return None;
    }
    Some(PendingInvite {
        code: (*code).to_string().into(),
        host: Some(host.to_string().into()),
        port: Some(port_of(port)?),
    })
}

/// The link the Invites modal hands out: code only.
pub fn build_invite_link(code: &str) -> String {
    format!("decibell://invite/{code}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(url: &str) -> Option<(String, Option<String>, Option<u16>)> {
        parse_invite_link(url).map(|p| (p.code.to_string(), p.host.map(|h| h.to_string()), p.port))
    }

    #[test]
    fn the_three_shapes_parse() {
        assert_eq!(parts("decibell://invite/ABCD1234EF"), Some(("ABCD1234EF".into(), None, None)));
        assert_eq!(parts("DECIBELL://Invite/abcd/"), Some(("abcd".into(), None, None)));
        assert_eq!(
            parts("decibell://invite/127.0.0.11:8082/ABCD1234EF"),
            Some(("ABCD1234EF".into(), Some("127.0.0.11".into()), Some(8082)))
        );
        assert_eq!(
            parts("decibell://invite/example.org/8082/ABCD1234EF/"),
            Some(("ABCD1234EF".into(), Some("example.org".into()), Some(8082)))
        );
        assert_eq!(
            parts("decibell://invite/[2001:db8::1]:8082/ABCD1234EF"),
            Some(("ABCD1234EF".into(), Some("2001:db8::1".into()), Some(8082)))
        );
        assert_eq!(build_invite_link("ABCD"), "decibell://invite/ABCD");
    }

    #[test]
    fn everything_else_is_refused() {
        for url in [
            "https://invite/ABCD1234EF",
            "decibell://invite/",
            "decibell://invite/abc",                     // too short
            "decibell://invite/ab-cd",                   // not alphanumeric
            "decibell://invite/host:0/ABCD1234EF",       // port 0
            "decibell://invite/host:70000/ABCD1234EF",   // port too large
            "decibell://invite/host/ABCD1234EF",         // no port
            "decibell://invite/host:port/ABCD1234EF",
            "decibell://invite/a/b/8082/ABCD1234EF",
            "decibell://invite/ABCD1234EF//",
            "decibell://invite/ho st:8082/ABCD1234EF",
            "decibell://invite/[zz]:8082/ABCD1234EF",
            " decibell://invite/ABCD1234EF",
        ] {
            assert_eq!(parts(url), None, "{url}");
        }
    }
}
