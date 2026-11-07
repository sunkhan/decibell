//! The spike's synthetic busy channel (tools/gen-fixture.mjs), turned into display rows with the renderer's
//! rules: identity colours hashed from the uid (utils/colors.ts), grouping (MessageBubble.tsx `shouldGroup`:
//! same sender, under 7 minutes, never across a day divider), day dividers and time labels
//! (MessageBubble.tsx `formatTimestamp`). Pure Rust, no Qt: the models in chat_model.rs / roster_model.rs read it.

use chrono::{DateTime, Datelike, Local, TimeZone};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Deserialize)]
pub struct Fixture {
    pub now: i64,
    #[serde(rename = "self")]
    pub me: Member,
    pub server: Server,
    pub channels: Vec<Channel>,
    pub members: Vec<Member>,
    pub messages: Vec<Message>,
}

#[derive(Deserialize)]
pub struct Server {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub public: bool,
}

#[derive(Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub encrypted: bool,
}

#[derive(Deserialize, Clone)]
pub struct Member {
    pub uid: i64,
    pub name: String,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub avatar: Option<String>,
}

#[derive(Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(rename = "senderUid")]
    pub sender_uid: i64,
    pub ts: i64,
    pub content: String,
    #[serde(rename = "replyTo")]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

#[derive(Deserialize, Clone)]
pub struct Attachment {
    pub kind: String,
    pub file: String,
    pub width: u32,
    pub height: u32,
}

/// One message as the list draws it.
pub struct Row {
    pub id: String,
    pub sender_uid: i64,
    pub sender_name: String,
    /// 1..=8: `--color-name-N` / `--color-av-N`.
    pub color_index: u8,
    pub avatar: String,
    pub initial: String,
    pub time_label: String,
    /// Non-empty when a day divider opens this row.
    pub day_label: String,
    pub grouped: bool,
    pub content: String,
    pub reply: Option<Reply>,
    pub images: Vec<Attachment>,
}

pub struct Reply {
    pub sender_name: String,
    pub color_index: u8,
    pub snippet: String,
}

/// utils/colors.ts `rampIndex`, bit for bit: `hash = c + ((hash << 5) - hash)` over UTF-16 units, where `<<`
/// first truncates to int32 and the sum may leave the int32 range, then `abs(hash) % 8 + 1`.
pub fn ramp_index(s: &str) -> u8 {
    let mut hash: i64 = 0;
    for unit in s.encode_utf16() {
        let shifted = ((hash as i32).wrapping_shl(5)) as i64;
        hash = unit as i64 + (shifted - hash);
    }
    (hash.abs() % 8 + 1) as u8
}

/// utils/colors.ts `personKey`: the uid when there is one, else the name.
pub fn color_index(uid: i64, name: &str) -> u8 {
    if uid > 0 {
        ramp_index(&format!("uid:{uid}"))
    } else {
        ramp_index(name)
    }
}

fn local(ms: i64) -> DateTime<Local> {
    Local.timestamp_millis_opt(ms).single().unwrap_or_else(|| Local.timestamp_millis_opt(0).unwrap())
}

/// en-US clock, as the renderer prints it under a 12-hour locale ("1:58 PM").
fn clock(t: &DateTime<Local>) -> String {
    t.format("%-I:%M %p").to_string()
}

/// MessageBubble.tsx `formatTimestamp`.
pub fn time_label(ms: i64, now_ms: i64) -> String {
    let t = local(ms);
    let now = local(now_ms);
    if t.date_naive() == now.date_naive() {
        return clock(&t);
    }
    if Some(t.date_naive()) == now.date_naive().pred_opt() {
        return format!("Yesterday, at {}", clock(&t));
    }
    format!("{}/{}/{}, at {}", t.month(), t.day(), t.year(), clock(&t))
}

/// The day divider's label: `{weekday: "long", month: "long", day: "numeric", year: "numeric"}`.
pub fn day_label(ms: i64) -> String {
    local(ms).format("%A, %B %-d, %Y").to_string()
}

const GROUP_WINDOW_MS: i64 = 7 * 60 * 1000;

pub fn load(path: &Path) -> Result<Fixture, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Display rows for every message, oldest first.
pub fn rows(f: &Fixture, asset_dir: &Path) -> Vec<Row> {
    let by_uid: HashMap<i64, &Member> = f.members.iter().map(|m| (m.uid, m)).collect();
    let by_id: HashMap<&str, &Message> = f.messages.iter().map(|m| (m.id.as_str(), m)).collect();
    let url = |rel: &str| format!("file:///{}", asset_dir.join(rel).to_string_lossy().replace('\\', "/"));
    let mut out = Vec::with_capacity(f.messages.len());
    let mut prev: Option<&Message> = None;
    for m in &f.messages {
        let member = by_uid.get(&m.sender_uid);
        let name = member.map(|x| x.name.clone()).unwrap_or_else(|| "Unknown user".into());
        let new_day = prev.map_or(true, |p| local(p.ts).date_naive() != local(m.ts).date_naive());
        let grouped = !new_day
            && prev.is_some_and(|p| p.sender_uid == m.sender_uid && (m.ts - p.ts).abs() < GROUP_WINDOW_MS);
        let reply = m.reply_to.as_deref().and_then(|id| by_id.get(id)).map(|r| {
            let rn = by_uid.get(&r.sender_uid).map(|x| x.name.clone()).unwrap_or_else(|| "Unknown user".into());
            Reply {
                color_index: color_index(r.sender_uid, &rn),
                sender_name: rn,
                snippet: r.content.lines().next().unwrap_or("").chars().take(120).collect(),
            }
        });
        out.push(Row {
            id: m.id.clone(),
            sender_uid: m.sender_uid,
            color_index: color_index(m.sender_uid, &name),
            avatar: member.and_then(|x| x.avatar.as_deref()).map(url).unwrap_or_default(),
            initial: name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default(),
            sender_name: name,
            time_label: time_label(m.ts, f.now),
            day_label: if new_day { day_label(m.ts) } else { String::new() },
            grouped,
            content: m.content.clone(),
            reply,
            images: m
                .attachments
                .iter()
                .filter(|a| a.kind == "image")
                .map(|a| Attachment { file: url(&a.file), ..a.clone() })
                .collect(),
        });
        prev = Some(m);
    }
    out
}

/// Spike v1 body: the markdown subset as Qt StyledText (bold, italic, underline, strike, inline code, links,
/// line breaks). The MessageText item replaces this with real runs (code chips, twemoji, selection).
pub fn styled_body(src: &str) -> (String, Option<(String, String)>) {
    // Pull the first fenced block out; it draws as its own box.
    let mut code = None;
    let mut text = src.to_string();
    if let Some(start) = text.find("```") {
        if let Some(len) = text[start + 3..].find("```") {
            let inner = &text[start + 3..start + 3 + len];
            let (lang, body) = inner.split_once('\n').unwrap_or(("", inner));
            code = Some((lang.trim().to_string(), body.trim_end().to_string()));
            text = format!("{}{}", &text[..start], &text[start + 3 + len + 3..]);
        }
    }
    let esc = text.trim_end().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut html = String::with_capacity(esc.len() + 32);
    let mut rest = esc.as_str();
    while !rest.is_empty() {
        let (tag, open, close) = if rest.starts_with('`') {
            ("code", "<font face=\"JetBrains Mono\">", "</font>")
        } else if rest.starts_with("**") {
            ("**", "<b>", "</b>")
        } else if rest.starts_with("__") {
            ("__", "<u>", "</u>")
        } else if rest.starts_with("~~") {
            ("~~", "<s>", "</s>")
        } else if rest.starts_with('*') {
            ("*", "<i>", "</i>")
        } else if rest.starts_with("https://") || rest.starts_with("http://") {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let u = &rest[..end];
            html.push_str(&format!("<a href=\"{u}\">{u}</a>"));
            rest = &rest[end..];
            continue;
        } else {
            let c = rest.chars().next().unwrap();
            if c == '\n' {
                html.push_str("<br>");
            } else {
                html.push(c);
            }
            rest = &rest[c.len_utf8()..];
            continue;
        };
        let delim = if tag == "code" { "`" } else { tag };
        if let Some(end) = rest[delim.len()..].find(delim) {
            html.push_str(open);
            html.push_str(&rest[delim.len()..delim.len() + end]);
            html.push_str(close);
            rest = &rest[delim.len() * 2 + end..];
        } else {
            html.push_str(&rest[..delim.len()]);
            rest = &rest[delim.len()..];
        }
    }
    (html, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_index_matches_the_renderer() {
        // Reference values from utils/colors.ts rampIndex run in Node; the long strings leave the int32 range
        // mid-hash, the last one has non-ASCII and a surrogate pair.
        for (s, want) in [
            ("uid:1001", 7),
            ("uid:1002", 6),
            ("johnny brawO", 2),
            ("mert.the.long.name.example", 1),
            ("uid:4611686018427387904", 6),
            ("çğışöü emoji 😂 name", 1),
        ] {
            assert_eq!(ramp_index(s), want, "{s}");
        }
    }

    #[test]
    fn styled_body_survives_several_code_spans() {
        let (h, _) = styled_body("Inline code: run `cargo test --lib` then `npx napi build --platform` and check `config.json` again.");
        assert!(h.contains("config.json"));
    }

    #[test]
    fn styled_body_handles_the_subset() {
        let (h, code) = styled_body("a **b** *i* `x<y` https://e.com/z\nnext");
        assert_eq!(h, "a <b>b</b> <i>i</i> <font face=\"JetBrains Mono\">x&lt;y</font> <a href=\"https://e.com/z\">https://e.com/z</a><br>next");
        assert!(code.is_none());
        let (h, code) = styled_body("see:\n```rust\nfn x() {}\n```");
        assert_eq!(h, "see:");
        assert_eq!(code, Some(("rust".into(), "fn x() {}".into())));
    }
}
