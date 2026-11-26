//! Deterministic chat fixtures, fed through the model's real router (no network, no core):
//!
//! - **`#general`**: the 10,000-message busy channel of `tools/dev/fixture.py` (itself the port
//!   of `qt-client/tools/gen-fixture.mjs`: same PRNG, same draw order, same contents, replies,
//!   pictures and edits; the senders are the shell fixture's Daily Gaming members). The list
//!   pages through it as it would through a server: [`FixtureSource`] answers `load_older` /
//!   `load_newer` / jumps with `ChannelHistoryReceived` events.
//! - **`#clips`**: a hand-written channel with every row type and body feature.
//!
//! [`json`] exports the same events for the Electron capture pages (the shell fixture's events,
//! then the channel's history page), so both clients draw the same rows.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use decibell_core::events::*;
use decibell_model::chat::MessageSlice;
use decibell_model::{ChannelId, Model, ServerId};
use decibell_shell::fixture as shell;
use gpui::{App, Entity, Task};

use crate::source::{Jump, ListSource};

pub const GENERAL: &str = "general";
pub const FEATURES: &str = "clips";
/// Messages of the big channel.
pub const COUNT: usize = 10_000;
/// The page size the list's requests get (`PAGE_LIMIT`).
const PAGE: usize = 50;
/// A jump's window: the target and 12 rows either side (`limit: 25`).
const AROUND: usize = 25;

/// gen-fixture.mjs's `rnd` (a mulberry32 variant), bit-exact.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        ((t ^ (t >> 14)) as f64) / 4_294_967_296.
    }

    fn pick<'a, T: ?Sized>(&mut self, list: &[&'a T]) -> &'a T {
        list[(self.next() * list.len() as f64).floor() as usize]
    }

    fn between(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() * (hi - lo + 1) as f64).floor() as i64
    }
}

/// Pictures: (width, height); the placeholders are drawn at these sizes.
const IMAGES: [(u32, u32); 18] = [
    (1600, 900), (1280, 960), (900, 1200), (1920, 1080), (800, 800), (2000, 800), (1200, 1600), (1600, 1066), (640, 480),
    (1080, 1350), (1500, 1000), (1000, 1500), (1920, 1200), (1200, 675), (3000, 2000), (720, 1280), (1400, 1400), (1024, 576),
];

const WORDS: &str = "the a stream tonight build new server patch voice channel lag fps frame bitrate encoder works fine \
broken again fixed it now ok yes no maybe later tomorrow today game ranked match lobby queue join leave win lost \
gg nice clip watch this look at that why how when where who what config driver update reboot test release \
linux windows wayland nvidia audio mic headset echo noise loud quiet sorry thanks great cool wow lol \
selam naber tamam evet hayır sonra bugün yarın oyun maç ses kanal güncelleme";
const EMOJI: [&str; 12] = ["😂", "🔥", "👍", "❤️", "🎮", "✨", "😭", "🙏", "👀", "💀", "🎉", "😅"];
const LANGS: [(&str, &str); 4] = [
    ("rust", "fn main() {\n    let rate = 120;\n    println!(\"{rate} fps\");\n}"),
    ("ts", "const fps = await probe();\nif (fps < 60) retry();"),
    ("cpp", "int main() {\n  return decibell::run();\n}"),
    ("sh", "cargo test --lib\nnpx napi build --platform"),
];

fn upper_first(text: String) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

struct ContentGen<'a> {
    rnd: &'a mut Rng,
    words: Vec<&'static str>,
}

impl ContentGen<'_> {
    fn sentence(&mut self, lo: i64, hi: i64) -> String {
        let n = self.rnd.between(lo, hi);
        let words: Vec<&str> = (0..n).map(|_| self.rnd.pick(&self.words)).collect();
        upper_first(words.join(" "))
    }

    fn content(&mut self) -> String {
        let x = self.rnd.next();
        if x < 0.40 {
            return self.sentence(1, 8);
        }
        if x < 0.58 {
            let sentence = self.sentence(10, 30);
            return sentence + self.rnd.pick(&[".", "?", "!", ""]);
        }
        if x < 0.64 {
            let mut parts = Vec::new();
            let mut i = 0;
            // The bound is redrawn every turn, as in the JS.
            while i < self.rnd.between(2, 4) {
                parts.push(self.sentence(15, 40) + ".");
                i += 1;
            }
            return parts.join(" ");
        }
        if x < 0.71 {
            let sentence = self.sentence(3, 10);
            let mut words: Vec<String> = sentence.split(' ').map(str::to_string).collect();
            let i = self.rnd.between(0, words.len() as i64 - 1) as usize;
            words[i] = format!("`{}`", self.rnd.pick(&["cargo build", "fps=120", "nvenc", "d3d11va", "--release", "config.json", "v0.9.4"]));
            return words.join(" ");
        }
        if x < 0.74 {
            let lang = LANGS[(self.rnd.next() * LANGS.len() as f64).floor() as usize];
            let sentence = self.sentence(2, 8);
            return format!("{sentence}:\n```{}\n{}\n```", lang.0, lang.1);
        }
        if x < 0.80 {
            let sentence = self.sentence(4, 14);
            let mut words: Vec<String> = sentence.split(' ').map(str::to_string).collect();
            let i = self.rnd.between(0, words.len() as i64 - 1) as usize;
            let marks = [("**", "**"), ("*", "*"), ("__", "__"), ("~~", "~~")];
            let mark = marks[(self.rnd.next() * marks.len() as f64).floor() as usize];
            words[i] = format!("{}{}{}", mark.0, words[i], mark.1);
            return words.join(" ");
        }
        if x < 0.88 {
            let sentence = self.sentence(2, 10);
            let emoji = self.rnd.pick(&EMOJI);
            let extra = if self.rnd.next() < 0.3 { self.rnd.pick(&EMOJI) } else { "" };
            return format!("{sentence} {emoji}{extra}");
        }
        if x < 0.90 {
            let n = self.rnd.between(1, 3);
            return (0..n).map(|_| self.rnd.pick(&EMOJI)).collect();
        }
        if x < 0.95 {
            let pre = self.rnd.pick(&["check this ", "link: ", "", "docs here "]);
            // Every candidate is built (and draws) before one is picked, as the list literal does.
            let clips = format!("clips/{}", self.rnd.between(100, 999));
            let issue = format!("issue/{}", self.rnd.between(1, 500));
            let which = (self.rnd.next() * 4.).floor() as usize;
            let tail = ["patch-notes", clips.as_str(), "docs/voice", issue.as_str()][which];
            return format!("{pre}https://example.com/{tail}");
        }
        let a = self.sentence(1, 6);
        let b = self.sentence(2, 8).to_lowercase();
        format!("{a}\nand {b}")
    }
}

/// One generated message (ids are 1-based positions; `reply_to` is an id).
pub struct FixtureMessage {
    pub sender: usize,
    pub timestamp: i64,
    pub content: String,
    pub reply_to: i64,
    pub images: Vec<(u32, u32)>,
    pub edited_at: i64,
}

/// (name, uid): the shell fixture's Daily Gaming members, the first four the heavy talkers.
pub const SENDERS: [(&str, i64); 16] = [
    ("perf2", 7), ("alice", 1), ("bob", 2), ("carol", 3), ("dave", 4), ("erin", 5), ("perf3", 8), ("perf4", 9), ("perf1", 6),
    ("aurora", -1000), ("blaze", -1001), ("cinder", -1002), ("drift", -1003), ("ember", -1004), ("flux", -1005), ("glimmer", -1006),
];

/// `generate(count, seed = 7)` of tools/dev/fixture.py with 16 senders.
pub fn generate(count: usize) -> Vec<FixtureMessage> {
    let weights: Vec<f64> = (0..SENDERS.len()).map(|i| if i < 4 { 12. } else if i < 8 { 4. } else { 1. }).collect();
    let total: f64 = weights.iter().sum();
    let mut rnd = Rng(7);
    let mut edits = Rng(7 ^ 0x5EED);
    let words: Vec<&'static str> = WORDS.split(' ').collect();
    let now = 1_794_072_600_000f64;
    let mut ts = now - 90. * 24. * 3_600e3;
    let mut out: Vec<FixtureMessage> = Vec::with_capacity(count);
    for i in 0..count {
        if rnd.next() < 0.08 {
            ts += rnd.between(1, 10) as f64 * 3_600e3;
        } else if rnd.next() < 0.5 {
            ts += rnd.between(5, 90) as f64 * 1000.;
        } else {
            ts += rnd.between(2, 15) as f64 * 60e3;
        }
        let sender = {
            let mut r = rnd.next() * total;
            weights.iter().position(|weight| {
                r -= weight;
                r < 0.
            })
        }
        .unwrap_or(0);
        let mut message = FixtureMessage {
            sender,
            timestamp: (ts + 0.5).floor() as i64,
            content: ContentGen { rnd: &mut rnd, words: words.clone() }.content(),
            reply_to: 0,
            images: Vec::new(),
            edited_at: 0,
        };
        if i > 5 && rnd.next() < 0.06 {
            message.reply_to = out.len() as i64 - rnd.between(1, 5) + 1;
        }
        if rnd.next() < 0.07 {
            let n = if rnd.next() < 0.7 { 1 } else { rnd.between(2, 3) };
            for _ in 0..n {
                message.images.push(IMAGES[(rnd.next() * IMAGES.len() as f64).floor() as usize]);
            }
            if rnd.next() < 0.4 {
                message.content.clear();
            }
        }
        if !message.content.is_empty() && edits.next() < 0.04 {
            message.edited_at = 15 + (edits.next() * 900.) as i64;
        }
        out.push(message);
    }
    let shift = now - 4. * 60_000. - out.last().map_or(0., |last| last.timestamp as f64);
    for message in &mut out {
        // Milliseconds → the wire's seconds.
        let at = (message.timestamp as f64 + shift) / 1000.;
        message.timestamp = at.floor() as i64;
        if message.edited_at > 0 {
            message.edited_at += message.timestamp;
        }
    }
    out
}

fn image(id: i64, message_id: i64, position: i32, (width, height): (u32, u32), created_at: i64) -> AttachmentPayload {
    attachment(id, message_id, position, "image", &format!("picture-{id}.jpg"), "image/jpeg", width, height, created_at)
}

#[allow(clippy::too_many_arguments)]
fn attachment(id: i64, message_id: i64, position: i32, kind: &str, filename: &str, mime: &str, width: u32, height: u32, created_at: i64) -> AttachmentPayload {
    AttachmentPayload {
        id,
        message_id,
        kind: kind.into(),
        filename: filename.into(),
        mime: mime.into(),
        size_bytes: 240_000,
        url: String::new(),
        position,
        created_at,
        purged_at: 0,
        width,
        height,
        thumbnail_size_bytes: 0,
        thumbnail_sizes_mask: 0,
        duration_ms: if kind == "audio" || kind == "video" { 61_000 } else { 0 },
        placeholder: String::new(),
        encrypted: false,
    }
}

fn payload(id: i64, sender: (&str, i64), channel: &str, content: &str, timestamp: i64) -> ChannelMessagePayload {
    ChannelMessagePayload {
        id,
        sender: sender.0.into(),
        sender_uid: sender.1,
        channel_id: channel.into(),
        content: content.into(),
        timestamp,
        attachments: Vec::new(),
        nonce: String::new(),
        edited_at: 0,
        reply_to: 0,
        reply_to_sender: String::new(),
        reply_to_sender_uid: 0,
        reply_to_content: String::new(),
        reply_to_attachment_kinds: Vec::new(),
        encrypted: false,
        decrypt_error: String::new(),
        encrypted_attachments: Vec::new(),
        suppress_embeds: false,
    }
}

/// The big channel as wire payloads, oldest first (ids 1..=COUNT).
pub fn general_payloads(messages: &[FixtureMessage]) -> Vec<ChannelMessagePayload> {
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let id = index as i64 + 1;
            let mut p = payload(id, SENDERS[message.sender], GENERAL, &message.content, message.timestamp);
            p.edited_at = message.edited_at;
            for (position, size) in message.images.iter().enumerate() {
                p.attachments.push(image(id * 10 + position as i64, id, position as i32, *size, message.timestamp));
            }
            if message.reply_to > 0 {
                let parent = &messages[message.reply_to as usize - 1];
                p.reply_to = message.reply_to;
                p.reply_to_sender = SENDERS[parent.sender].0.into();
                p.reply_to_sender_uid = SENDERS[parent.sender].1;
                p.reply_to_content = parent.content.clone();
                p.reply_to_attachment_kinds = vec![1; parent.images.len()];
            }
            p
        })
        .collect()
}

/// The hand-written channel: every row type and body feature, oldest first.
pub fn features_payloads() -> Vec<ChannelMessagePayload> {
    let me = (shell::ME_NAME, shell::ME);
    let (alice, bob, carol, dave) = (("alice", 1), ("bob", 2), ("carol", 3), ("dave", 4));
    // 2026-11-05 21:50 UTC onwards; a second day starts at row 9.
    let day_one = 1_793_915_400;
    let day_two = day_one + 86_400 - 3_000;
    let mut rows: Vec<ChannelMessagePayload> = Vec::new();
    let mut next_id = 0i64;
    let mut say = |sender: (&str, i64), at: i64, content: &str| -> ChannelMessagePayload {
        next_id += 1;
        payload(next_id, sender, FEATURES, content, at)
    };
    let long_code: String = (1..=22).map(|n| format!("    step_{n:02}(frame, &mut encoder)?; // stage {n}")).collect::<Vec<_>>().join("\n");

    rows.push(say(alice, day_one, "Plain text with **bold**, *italic*, __underline__, ~~strike~~ and ***bold italic***, then `inline code` and a link https://example.com/docs/voice?channel=general to follow."));
    rows.push(say(alice, day_one + 30, "A second message from the same sender within seven minutes: grouped, the time shows on hover."));
    rows.push(say(alice, day_one + 60, "And a third, long enough to wrap onto a second line at the pane's width so that the grouped row's height is more than one line box of the body text, which is what most rows of a busy channel look like."));
    let mut edited = say(bob, day_one + 120, "An edited message keeps its text and gains the marker.");
    edited.edited_at = day_one + 400;
    rows.push(edited);
    let mut reply = say(carol, day_one + 200, "A reply to someone else's message, with the spine up to the quoted line.");
    reply.reply_to = 1;
    reply.reply_to_sender = "alice".into();
    reply.reply_to_sender_uid = 1;
    reply.reply_to_content = "Plain text with **bold**, *italic*, __underline__".into();
    rows.push(reply);
    rows.push(say(me, day_one + 260, "My own message: the hover actions offer reply, edit and delete."));
    let mut to_me = say(dave, day_one + 300, "A reply to my message: the row carries the accent rail.");
    to_me.reply_to = 6;
    to_me.reply_to_sender = shell::ME_NAME.into();
    to_me.reply_to_sender_uid = shell::ME;
    to_me.reply_to_content = "My own message: the hover actions offer reply, edit and delete.".into();
    rows.push(to_me);
    let mut orphan = say(bob, day_one + 360, "A reply whose parent was deleted.");
    orphan.reply_to = 9_999;
    rows.push(orphan);
    // A new day: the divider.
    rows.push(say(alice, day_two, "😀"));
    rows.push(say(alice, day_two + 20, "🔥🎉"));
    rows.push(say(alice, day_two + 40, "Emoji inline 😀 in running text 🔥 keep the line box in step 🎉 and a :wave: shortcode."));
    rows.push(say(bob, day_two + 100, &format!("A long code block, collapsed past 14 lines:\n```rust\nfn encode(frame: &Frame) -> Result<()> {{\n{long_code}\n    Ok(())\n}}\n```")));
    rows.push(say(bob, day_two + 130, "A short one:\n```sh\ncargo test --lib\nnpx napi build --platform\n```\nand text after it."));
    rows.push(say(carol, day_two + 200, "Inline math $x^2+\\frac{a}{b}$ sits on the baseline. Display math:\n$$\\int_0^\\infty e^{-x^2}\\,dx = \\frac{\\sqrt{\\pi}}{2}$$\nand the text goes on."));
    rows.push(say(dave, day_two + 260, "https://example.com/clips/funny.gif"));
    let mut sealed = say(carol, day_two + 320, "[encrypted]");
    sealed.encrypted = true;
    sealed.decrypt_error = "locked".into();
    rows.push(sealed);
    let mut lost = say(carol, day_two + 340, "[encrypted]");
    lost.encrypted = true;
    lost.decrypt_error = "no_key".into();
    rows.push(lost);
    // Attachments in each grid shape.
    let shapes: [&[(u32, u32)]; 6] = [
        &[(1600, 900)],
        &[(900, 1200)],
        &[(1600, 900), (1280, 960)],
        &[(800, 800), (1920, 1080), (640, 480)],
        &[(800, 800), (1920, 1080), (640, 480), (1200, 1600)],
        &[(800, 800), (1920, 1080), (640, 480), (1200, 1600), (1024, 576)],
    ];
    for (n, shape) in shapes.iter().enumerate() {
        let at = day_two + 400 + n as i64 * 600;
        let text = if n == 0 { "A picture under a caption." } else { "" };
        let mut row = say(if n % 2 == 0 { dave } else { alice }, at, text);
        for (position, size) in shape.iter().enumerate() {
            row.attachments.push(image(row.id * 10 + position as i64, row.id, position as i32, *size, at));
        }
        rows.push(row);
    }
    let at = day_two + 4_200;
    let mut gif = say(bob, at, "");
    gif.attachments.push(attachment(gif.id * 10, gif.id, 0, "image", "loop.gif", "image/gif", 480, 270, at));
    rows.push(gif);
    let mut files = say(bob, at + 30, "A document, a voice note and one the retention already purged:");
    files.attachments.push(attachment(files.id * 10, files.id, 0, "document", "patch-notes.pdf", "application/pdf", 0, 0, at));
    files.attachments.push(attachment(files.id * 10 + 1, files.id, 1, "audio", "voice-note.ogg", "audio/ogg", 0, 0, at));
    let mut purged = attachment(files.id * 10 + 2, files.id, 2, "document", "old.zip", "application/zip", 0, 0, at);
    purged.purged_at = at + 10;
    files.attachments.push(purged);
    rows.push(files);
    rows.push(say(me, at + 600, "Supercalifragilisticexpialidociouspneumonoultramicroscopicsilicovolcanoconiosisantidisestablishmentarianism-floccinaucinihilipilification-and-then-some-more-so-that-it-must-break"));
    rows.push(say(alice, at + 660, "line one\nline two\n\nline four after an empty line"));
    // Where CSS collapses margins: a block first or last in a body, two blocks in a row, a
    // block followed by an attachment.
    rows.push(say(carol, at + 1_200, "```rust\nfn first() {}\n```"));
    rows.push(say(carol, at + 1_230, "```sh\ngrouped --and --first\n```"));
    rows.push(say(dave, at + 1_800, "Two blocks in a row:\n```rust\nfn a() {}\n```\n```ts\nconst b = 1;\n```"));
    rows.push(say(dave, at + 1_830, "$$a^2 + b^2 = c^2$$"));
    rows.push(say(dave, at + 1_860, "A block, then a formula:\n```cpp\nint main() {}\n```\n$$e^{i\\pi} + 1 = 0$$"));
    let mut tail = say(alice, at + 2_400, "A formula last, then a picture:\n$$\\sum_{k=1}^{n} k = \\frac{n(n+1)}{2}$$");
    tail.attachments.push(image(tail.id * 10, tail.id, 0, (640, 480), at + 2_400));
    rows.push(tail);
    let mut code_then_picture = say(bob, at + 3_000, "```sh\nmake release\n```");
    code_then_picture.attachments.push(image(code_then_picture.id * 10, code_then_picture.id, 0, (1024, 576), at + 3_000));
    rows.push(code_then_picture);
    rows.push(say(bob, at + 3_030, "The last row, plain."));
    rows
}

fn history(channel: &str, messages: Vec<ChannelMessagePayload>, has_more: bool, has_more_after: bool, cursors: (i64, i64, i64)) -> CoreEvent {
    CoreEvent::ChannelHistoryReceived(ChannelHistoryReceivedPayload {
        server_id: shell::DAILY_GAMING.to_string(),
        channel_id: channel.into(),
        messages,
        has_more,
        has_more_after,
        around_id: cursors.0,
        after_id: cursors.1,
        before_id: cursors.2,
    })
}

/// The fixture's channel as a list source: the model's slice, pages served from memory.
pub struct FixtureSource {
    pub server: ServerId,
    pub channel: ChannelId,
    all: Rc<Vec<ChannelMessagePayload>>,
    /// How long a page takes to "arrive" (zero: within the same turn).
    pub latency: Cell<Duration>,
    /// Pages served, for the self-tests.
    pub pages: Cell<usize>,
}

impl FixtureSource {
    fn slice_now(&self, cx: &mut App) -> Entity<MessageSlice> {
        let model = Model::global(cx).clone();
        let own = model.auth.read(cx).uid;
        model.chat.channels.update(cx, |cache, cx| cache.ensure(self.server, &self.channel, own, false, cx))
    }

    fn route(&self, event: CoreEvent, cx: &mut App) -> Task<Option<bool>> {
        self.pages.set(self.pages.get() + 1);
        let latency = self.latency.get();
        if latency.is_zero() {
            let model = Model::global(cx).clone();
            decibell_model::events::route(&model, event, cx);
            return Task::ready(Some(true));
        }
        cx.spawn(async move |cx| {
            cx.background_executor().timer(latency).await;
            cx.update(|cx| {
                let model = Model::global(cx).clone();
                decibell_model::events::route(&model, event, cx);
            });
            Some(true)
        })
    }

    /// Rows `[from, to)` (0-based positions) as a page.
    fn page(&self, from: usize, to: usize) -> Vec<ChannelMessagePayload> {
        self.all[from.min(self.all.len())..to.min(self.all.len())].to_vec()
    }

    /// The newest page, as a fresh channel entry gets it.
    pub fn load_latest(&self, cx: &mut App) {
        let len = self.all.len();
        let from = len.saturating_sub(PAGE);
        drop(self.route(history(&self.channel, self.page(from, len), from > 0, false, (0, 0, 0)), cx));
    }

    /// A message nobody asked for: what a live send by someone else looks like (id above all).
    pub fn live_message(&self, id: i64, content: &str, cx: &mut App) {
        let last = self.all.last().map_or(0, |last| last.timestamp);
        let event = CoreEvent::MessageReceived(MessageReceivedPayload {
            context: self.channel.to_string(),
            server_id: shell::DAILY_GAMING.to_string(),
            sender: "carol".into(),
            sender_uid: 3,
            recipient: String::new(),
            recipient_uid: 0,
            content: content.into(),
            timestamp: (last + 3_600 + id).to_string(),
            id,
            attachments: Vec::new(),
            nonce: String::new(),
            edited_at: 0,
            reply_to: 0,
            reply_to_sender: String::new(),
            reply_to_sender_uid: 0,
            reply_to_content: String::new(),
            reply_to_attachment_kinds: Vec::new(),
            encrypted: false,
            decrypt_error: String::new(),
            encrypted_attachments: Vec::new(),
            suppress_embeds: false,
        });
        let model = Model::global(cx).clone();
        decibell_model::events::route(&model, event, cx);
    }
}

impl ListSource for FixtureSource {
    fn slice(&self, cx: &mut App) -> Entity<MessageSlice> {
        self.slice_now(cx)
    }

    fn server(&self) -> Option<ServerId> {
        Some(self.server)
    }

    fn load_older(&self, cx: &mut App) -> Option<Task<Option<bool>>> {
        let oldest = self.slice_now(cx).read(cx).oldest_stored_id();
        if oldest <= 1 {
            return None;
        }
        let to = oldest as usize - 1;
        let from = to.saturating_sub(PAGE);
        Some(self.route(history(&self.channel, self.page(from, to), from > 0, false, (0, 0, oldest)), cx))
    }

    fn load_newer(&self, cx: &mut App) -> Option<Task<Option<bool>>> {
        let newest = self.slice_now(cx).read(cx).newest_stored_id();
        let from = newest as usize;
        if from >= self.all.len() {
            return None;
        }
        let to = (from + PAGE).min(self.all.len());
        Some(self.route(history(&self.channel, self.page(from, to), false, to < self.all.len(), (0, newest, 0)), cx))
    }

    fn jump_to_present(&self, cx: &mut App) {
        self.slice_now(cx).update(cx, |slice, cx| slice.reset_for_jump(cx));
        self.load_latest(cx);
    }

    fn jump_to_message(&self, id: i64, cx: &mut App) -> Jump {
        if self.slice_now(cx).read(cx).find(id).is_some() {
            return Jump::Loaded;
        }
        if id < 1 || id as usize > self.all.len() {
            return Jump::Refused;
        }
        let from = (id as usize - 1).saturating_sub(AROUND / 2);
        let to = (from + AROUND).min(self.all.len());
        Jump::Fetching(self.route(history(&self.channel, self.page(from, to), from > 0, to < self.all.len(), (id, 0, 0)), cx))
    }

    fn jump_landed(&self, _: &mut App) {}

    fn can_delete_others(&self, _: &App) -> bool {
        false
    }

    fn edit(&self, id: i64, content: &str, _: &mut App) {
        log::info!("[chat fixture] edit {id}: {content:?}");
    }

    fn delete(&self, id: i64, cx: &mut App) {
        let event = CoreEvent::ChannelMessageDeleted(ChannelMessageDeletedPayload {
            server_id: shell::DAILY_GAMING.to_string(),
            channel_id: self.channel.to_string(),
            message_id: id,
            deleted_at: 0,
            deleted_by: shell::ME_NAME.into(),
            deleted_by_uid: shell::ME,
        });
        let model = Model::global(cx).clone();
        decibell_model::events::route(&model, event, cx);
    }
}

/// Which fixture channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// `#general`: the 10k channel, its newest page loaded.
    General,
    /// `#clips`: the hand-written channel, whole.
    Features,
}

/// Signs the model in to the shell's fixture, opens `which`'s channel with its newest page
/// routed (from a fresh slice: nothing an earlier opening left carries over), and returns the
/// source a [`MessageList`](crate::MessageList) shows it through.
pub fn open(which: Which, cx: &mut App) -> Rc<FixtureSource> {
    open_pages(which, 1, cx)
}

/// [`open`] with `#general`'s `pages` newest pages loaded at once (what [`json`] exports).
pub fn open_pages(which: Which, pages: usize, cx: &mut App) -> Rc<FixtureSource> {
    // The shell's fixture once per process: signing in again would re-sync (and drop) the
    // channels already open.
    struct Applied;
    impl gpui::Global for Applied {}
    let model = if cx.has_global::<Applied>() {
        Model::global(cx).clone()
    } else {
        cx.set_global(Applied);
        shell::apply(shell::Scene::Server, cx)
    };
    let server = ServerId::from_central(shell::DAILY_GAMING);
    let (channel, all) = match which {
        Which::General => (GENERAL, general_payloads(&generate(COUNT))),
        Which::Features => (FEATURES, features_payloads()),
    };
    let channel: ChannelId = channel.into();
    // Each opening starts from the fixture's own seeded state: what an earlier screen or self-test
    // of this process left in the channel's slice (more pages, a saved position) goes first.
    model.chat.channels.update(cx, |cache, cx| cache.purge(server, &channel, cx));
    model.nav.update(cx, |nav, cx| nav.set_active_channel(Some(channel.clone()), cx));
    let source = Rc::new(FixtureSource { server, channel, all: Rc::new(all), latency: Cell::new(Duration::ZERO), pages: Cell::new(0) });
    match which {
        Which::General if pages > 1 => {
            let len = source.all.len();
            let from = len.saturating_sub(PAGE * pages);
            drop(source.route(history(&source.channel, source.page(from, len), from > 0, false, (0, 0, 0)), cx));
        }
        Which::General => source.load_latest(cx),
        Which::Features => {
            let all = source.all.to_vec();
            drop(source.route(history(&source.channel, all, false, false, (0, 0, 0)), cx));
        }
    }
    source
}

/// The fixture as the Electron pages replay it: the shell's events, then the channel's
/// history page(s) (`pages` newest pages of `#general`; all of `#clips`), and the navigation.
pub fn json(which: Which, pages: usize) -> serde_json::Value {
    let mut value = shell::json(shell::Scene::Server);
    let (channel, messages) = match which {
        Which::General => {
            let all = general_payloads(&generate(COUNT));
            let from = all.len().saturating_sub(PAGE * pages.max(1));
            (GENERAL, all[from..].to_vec())
        }
        Which::Features => (FEATURES, features_payloads()),
    };
    let more = which == Which::General;
    let (name, payload) = history(channel, messages, more, false, (0, 0, 0)).to_json();
    if let Some(events) = value.get_mut("events").and_then(|events| events.as_array_mut()) {
        events.push(serde_json::json!({ "event": name, "payload": payload }));
    }
    value["nav"]["channel"] = serde_json::json!(channel);
    value["nav"]["voice"] = serde_json::Value::Null;
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first rows of tools/dev/fixture.py's `generate(seed=7)` (contents and spacing are
    /// the generator's own; checked against the Python once, kept here so a port slip shows).
    #[test]
    fn the_generator_is_deterministic() {
        let a = generate(400);
        let b = generate(400);
        assert_eq!(a.len(), 400);
        for (x, y) in a.iter().zip(&b) {
            assert_eq!((x.sender, x.timestamp, &x.content, x.reply_to, &x.images), (y.sender, y.timestamp, &y.content, y.reply_to, &y.images));
        }
        assert!(a.windows(2).all(|pair| pair[0].timestamp <= pair[1].timestamp));
        assert!(a.iter().any(|message| message.reply_to > 0) && a.iter().any(|message| !message.images.is_empty()));
        assert!(a.iter().any(|message| message.content.contains("```")));
    }
}
