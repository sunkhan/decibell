//! What is taken out of a crash report before it leaves the machine.
//!
//! Electron's `beforeSend` deletes `user.ip_address`, and its Sentry SDK sends no default PII and
//! rewrites stack paths to `app:///` (the install path holds the user's name); the Privacy tab
//! promises "No usernames, no message contents, no server names". A panic message is free text that
//! can quote any of those, so here it keeps its shape and loses its data:
//!
//! - quoted text (`"…"`, `'…'`: how `{:?}` prints strings, where content leaks) → `"<redacted>"`
//! - URLs → `<url>`; e-mail addresses → `<email>`; IPv4 / IPv6 addresses (and ports) → `<ip>`
//! - tokens: JWTs, long base64 / hex runs (keys, session tokens, hashes) → `<token>`
//! - the home directory → `~`; the OS user's name → `<user>`; names the app registered while
//!   running (the signed-in account, the communities joined) → `<name>`
//! - absolute paths that aren't source files → `<path>`; source paths keep only their part from
//!   `src/` or the crate directory on (`crates/decibell-chat/src/list.rs`)
//! - at most [`MAX_LEN`] characters.
//!
//! The report itself carries no IP (`user` is the random install id only), no host name
//! (`server_name` is never set), no breadcrumbs, no request, no session.

use std::sync::OnceLock;

use regex::Regex;

pub const MAX_LEN: usize = 1024;

pub struct Scrubber {
    /// The home directory and the OS user's name (both as found in the environment).
    home: Option<String>,
    user: Option<String>,
    /// Account and community names the app registered.
    names: Vec<String>,
}

struct Patterns {
    quoted: Regex,
    url: Regex,
    email: Regex,
    jwt: Regex,
    token: Regex,
    ipv4: Regex,
    ipv6: Regex,
    unix_path: Regex,
    windows_path: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    // ASCII classes only (`(?-u:\b)`, `[0-9]`): the regex crate is built without its Unicode
    // tables, which a crash message doesn't need.
    PATTERNS.get_or_init(|| Patterns {
        quoted: Regex::new(r#""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.){2,}'"#).unwrap(),
        url: Regex::new(r"(?-u:\b)[a-zA-Z][a-zA-Z0-9+.-]*://[^ \t\r\n\)\]>,;]+").unwrap(),
        email: Regex::new(r"(?-u:\b)[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}(?-u:\b)").unwrap(),
        jwt: Regex::new(r"(?-u:\b)eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*").unwrap(),
        // 24+ characters of base64 / hex with at least one digit: keys, tokens, hashes.
        token: Regex::new(r"(?-u:\b)[A-Za-z0-9+/_=-]{24,}").unwrap(),
        ipv4: Regex::new(r"(?-u:\b)(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?::[0-9]{1,5})?(?-u:\b)").unwrap(),
        ipv6: Regex::new(r"\[?(?:[0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4}(?-u:\b)\]?(?::[0-9]{1,5})?").unwrap(),
        unix_path: Regex::new(r"(?:~|(?-u:\B))/[^ \t\r\n:\)\]'`,;]+").unwrap(),
        windows_path: Regex::new(r"(?-u:\b)[A-Za-z]:\\[^ \t\r\n:\)\]'`,;]+").unwrap(),
    })
}

impl Scrubber {
    pub fn from_env(names: Vec<String>) -> Scrubber {
        let home = std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok()).filter(|h| h.len() > 1);
        let user = std::env::var("USER")
            .ok()
            .or_else(|| std::env::var("USERNAME").ok())
            .or_else(|| home.as_deref().and_then(|h| h.rsplit(['/', '\\']).next()).map(str::to_string))
            .filter(|u| u.len() >= 2);
        Scrubber { home, user, names }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(home: &str, user: &str, names: &[&str]) -> Scrubber {
        Scrubber { home: Some(home.into()), user: Some(user.into()), names: names.iter().map(|n| n.to_string()).collect() }
    }

    /// A message (panic text, a native crash's description).
    pub fn text(&self, input: &str) -> String {
        let p = patterns();
        let mut s = input.to_string();
        if let Some(home) = &self.home {
            s = s.replace(home.as_str(), "~");
        }
        s = p.quoted.replace_all(&s, |c: &regex::Captures| {
            let q = &c[0][..1];
            format!("{q}<redacted>{q}")
        }).into_owned();
        s = p.url.replace_all(&s, "<url>").into_owned();
        s = p.email.replace_all(&s, "<email>").into_owned();
        s = p.jwt.replace_all(&s, "<token>").into_owned();
        s = p.windows_path.replace_all(&s, |c: &regex::Captures| source_path(&c[0])).into_owned();
        s = p.unix_path.replace_all(&s, |c: &regex::Captures| source_path(&c[0])).into_owned();
        s = p.token.replace_all(&s, |c: &regex::Captures| {
            let t = &c[0];
            if t.bytes().any(|b| b.is_ascii_digit()) && !t.starts_with("crates/") { "<token>".to_string() } else { t.to_string() }
        }).into_owned();
        s = p.ipv4.replace_all(&s, "<ip>").into_owned();
        s = p.ipv6.replace_all(&s, |c: &regex::Captures| if is_ipv6(&c[0]) { "<ip>".to_string() } else { c[0].to_string() }).into_owned();
        for name in &self.names {
            if name.chars().count() >= 2 {
                s = replace_word(&s, name, "<name>");
            }
        }
        if let Some(user) = &self.user {
            s = replace_word(&s, user, "<user>");
        }
        truncate(s)
    }

    /// A source location (`file:line`): only its part from the crate on.
    pub fn location(&self, file: &str, line: u32) -> String {
        let file = file.replace('\\', "/");
        format!("{}:{line}", source_relative(&file).unwrap_or("<path>"))
    }
}

/// `…/crates/<crate>/src/x.rs` → `crates/<crate>/src/x.rs`; `…/registry/src/<index>/<crate-1.0>/src/x.rs`
/// → `<crate-1.0>/src/x.rs`; `/rustc/<hash>/library/std/src/x.rs` → `library/std/src/x.rs`.
fn source_relative(path: &str) -> Option<&str> {
    if !path.ends_with(".rs") {
        return None;
    }
    if !path.starts_with('/') && !path.contains(":/") {
        return Some(path);
    }
    for marker in ["/crates/", "/library/"] {
        if let Some(at) = path.rfind(marker) {
            return Some(&path[at + 1..]);
        }
    }
    // A dependency: from the directory before its `src/`.
    let src = path.rfind("/src/")?;
    let start = path[..src].rfind('/')? + 1;
    Some(&path[start..])
}

fn source_path(path: &str) -> String {
    let unix = path.replace('\\', "/");
    match source_relative(&unix) {
        Some(relative) => relative.to_string(),
        None => "<path>".to_string(),
    }
}

/// An IPv6 address (bracketed, maybe with a port), not a Rust path (`a::b`, `f64::EPSILON`) or a
/// `file:line:col`: it parses, has two groups or more and a decimal digit (or is `::1`).
fn is_ipv6(candidate: &str) -> bool {
    let core = candidate.trim_start_matches('[');
    let core = core.split(']').next().unwrap_or(core);
    let groups = core.split(':').filter(|g| !g.is_empty()).count();
    core.parse::<std::net::Ipv6Addr>().is_ok() && (core == "::1" || (groups >= 2 && core.bytes().any(|b| b.is_ascii_digit())))
}

/// Replaces `word` where it stands as a word (not inside a longer identifier), ASCII case ignored.
fn replace_word(s: &str, word: &str, with: &str) -> String {
    let (hay, needle) = (s.to_ascii_lowercase(), word.to_ascii_lowercase());
    let word_byte = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    while let Some(found) = hay[at..].find(&needle) {
        let start = at + found;
        let end = start + needle.len();
        out.push_str(&s[at..start]);
        if !word_byte(start.checked_sub(1).and_then(|i| hay.as_bytes().get(i))) && !word_byte(hay.as_bytes().get(end)) {
            out.push_str(with);
        } else {
            out.push_str(&s[start..end]);
        }
        at = end;
    }
    out.push_str(&s[at..]);
    out
}

fn truncate(mut s: String) -> String {
    if s.chars().count() > MAX_LEN {
        s = s.chars().take(MAX_LEN - 1).collect();
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrub(s: &str) -> String {
        Scrubber::for_tests("/home/alice", "alice", &["perf1", "Daily Gaming"]).text(s)
    }

    #[test]
    fn takes_out_what_identifies_people_and_content() {
        let cases = [
            (r#"called `Result::unwrap()` on an `Err` value: Parse("hello world, see you at 5")"#, r#"called `Result::unwrap()` on an `Err` value: Parse("<redacted>")"#),
            ("failed to open /home/alice/.config/decibell/config.json: No such file", "failed to open <path>: No such file"),
            ("failed to open C:\\Users\\alice\\AppData\\x.json: denied", "failed to open <path>: denied"),
            ("connect to 178.104.131.247:8080 failed", "connect to <ip> failed"),
            ("peer [2001:db8::1]:443 reset", "peer <ip> reset"),
            ("fetch https://cdn.example.org/a.png?sig=abc failed", "fetch <url> failed"),
            ("mail bob@example.com bounced", "mail <email> bounced"),
            ("token eyJhbGciOiJFZERTQSJ9.eyJ1aWQiOjF9.c2lnbmF0dXJl expired", "token <token> expired"),
            ("key 3q2+7wAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= rejected", "key <token> rejected"),
            ("user perf1 is not a member of Daily Gaming", "user <name> is not a member of <name>"),
            ("alice's home is /home/alice/x", "<user>'s home is <path>"),
            ("index out of bounds: the len is 3 but the index is 5", "index out of bounds: the len is 3 but the index is 5"),
            ("at crates/decibell-chat/src/list.rs:120:9", "at crates/decibell-chat/src/list.rs:120:9"),
            ("in decibell_model::stores::auth::AuthStore", "in decibell_model::stores::auth::AuthStore"),
            ("f64::EPSILON and u8::MAX at a.rs:120:9", "f64::EPSILON and u8::MAX at a.rs:120:9"),
            ("fe80::1 and ::1", "<ip> and <ip>"),
        ];
        for (input, want) in cases {
            assert_eq!(scrub(input), want, "{input}");
        }
        assert!(scrub(&"x".repeat(5000)).chars().count() <= MAX_LEN);
    }

    #[test]
    fn keeps_source_locations_relative() {
        let s = Scrubber::for_tests("/home/runner", "runner", &[]);
        assert_eq!(s.location("crates/decibell-chat/src/list.rs", 12), "crates/decibell-chat/src/list.rs:12");
        assert_eq!(s.location("/home/runner/work/decibell/decibell/gpui-client/crates/decibell-app/src/boot.rs", 3), "crates/decibell-app/src/boot.rs:3");
        assert_eq!(s.location("/home/runner/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/wgpu-26.0.1/src/lib.rs", 7), "wgpu-26.0.1/src/lib.rs:7");
        assert_eq!(s.location("/rustc/0ef1bc386/library/core/src/option.rs", 9), "library/core/src/option.rs:9");
        assert_eq!(s.location("C:\\Users\\x\\.cargo\\registry\\src\\i\\foo-1.0\\src\\a.rs", 1), "foo-1.0/src/a.rs:1");
        assert_eq!(s.location("/tmp/secret.txt", 1), "<path>:1");
    }
}
