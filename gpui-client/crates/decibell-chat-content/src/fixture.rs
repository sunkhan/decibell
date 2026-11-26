//! The content fixtures: channels with every attachment kind and every embed, pictures generated
//! in memory (deterministic: the same picture at every size, so a thumbnail, its ThumbHash and the
//! original agree) and served by [`MemoryFetcher`], link previews and invites answered from
//! memory. No network, no clock, no randomness: the same screen is the same picture.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Cursor;
use std::rc::Rc;
use std::time::Duration;

use bytes::Bytes;
use decibell_attachments::CancellationToken;
use decibell_core::events::*;
use decibell_model::chat::MessageSlice;
use decibell_model::stores::invites::ResolvedInvite;
use decibell_model::{ChannelId, Model, ServerId};
use decibell_shell::fixture as shell;
use decibell_unfurl::{ImagePreview, LinkPreview, LinkPreviewImage, SitePreview};
use gpui::{App, AppContext as _, Entity, SharedString, Task};
use image::{Rgba, RgbaImage};

use crate::pictures::{Fetcher, Source};

pub const ATTACHMENTS: &str = "clips";
pub const EMBEDS: &str = "off-topic";
/// 2026-11-05 21:50 UTC.
const DAY: i64 = 1_793_915_400;

/// What an attachment's file is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Still,
    /// An animated GIF of `frames` frames.
    Gif(u32),
    /// Never answers: the blur stays.
    Hold,
    /// Answers 404: the broken-image look.
    Fail,
}

/// A generated picture: its natural size and kind, by attachment id or URL.
#[derive(Clone, Copy, Debug)]
pub struct Generated {
    pub w: u32,
    pub h: u32,
    pub kind: Kind,
    pub seed: u64,
}

fn hsl(h: f32, s: f32, l: f32) -> [f32; 3] {
    let c = (1. - (2. * l - 1.).abs()) * s;
    let hp = h / 60.;
    let x = c * (1. - (hp % 2. - 1.).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.),
        1 => (x, c, 0.),
        2 => (0., c, x),
        3 => (0., x, c),
        4 => (x, 0., c),
        _ => (c, 0., x),
    };
    let m = l - c / 2.;
    [r + m, g + m, b + m]
}

/// The picture of `seed` at `w` × `h` (resolution independent: drawn in the unit square), frame
/// `frame` of an animation.
pub fn pattern(seed: u64, w: u32, h: u32, frame: u32) -> RgbaImage {
    let a = hsl((seed * 47 % 360) as f32, 0.55, 0.42);
    let b = hsl(((seed * 47 + 150) % 360) as f32, 0.6, 0.6);
    let spot = hsl(((seed * 47 + 60) % 360) as f32, 0.7, 0.75);
    let (cx, cy) = (0.25 + 0.5 * ((seed % 5) as f32 / 4.) + 0.1 * frame as f32 % 0.6, 0.45);
    let aspect = w.max(1) as f32 / h.max(1) as f32;
    RgbaImage::from_fn(w.max(1), h.max(1), |x, y| {
        let u = (x as f32 + 0.5) / w as f32;
        let v = (y as f32 + 0.5) / h as f32;
        let t = ((u + v) / 2.).clamp(0., 1.);
        let mut c = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
        let (dx, dy) = ((u - cx) * aspect, v - cy);
        let d = (dx * dx + dy * dy).sqrt();
        if d < 0.22 {
            let k = ((0.22 - d) / 0.03).min(1.);
            for i in 0..3 {
                c[i] += (spot[i] - c[i]) * k;
            }
        }
        // Bands that show how a picture was scaled.
        if (v * 12.).fract() < 0.08 {
            for v in &mut c {
                *v *= 0.82;
            }
        }
        Rgba([(c[0] * 255.) as u8, (c[1] * 255.) as u8, (c[2] * 255.) as u8, 255])
    })
}

fn jpeg(img: &RgbaImage) -> Vec<u8> {
    decibell_attachments::thumbs::encode_jpeg(img, decibell_attachments::thumbs::THUMB_JPEG_QUALITY).unwrap_or_default()
}

fn png(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png);
    out
}

fn gif(seed: u64, w: u32, h: u32, frames: u32) -> Vec<u8> {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame};
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new_with_speed(&mut out, 30);
        let _ = enc.set_repeat(Repeat::Infinite);
        for i in 0..frames {
            let _ = enc.encode_frame(Frame::from_parts(pattern(seed, w, h, i), 0, 0, Delay::from_numer_denom_ms(80, 1)));
        }
    }
    out
}

/// The ThumbHash of a generated picture (what the uploader would have computed).
pub fn placeholder(g: &Generated) -> String {
    let (tw, th) = decibell_attachments::thumbs::thumbhash_dims(g.w, g.h);
    decibell_attachments::thumbhash::encode_b64(tw as usize, th as usize, pattern(g.seed, tw, th, 0).as_raw())
}

/// The bytes a source answers with.
pub fn file(g: &Generated, thumb: u32) -> Vec<u8> {
    if thumb > 0 {
        let (w, h) = decibell_attachments::thumbs::thumbnail_dims(g.w, g.h, thumb).unwrap_or((g.w, g.h));
        return jpeg(&pattern(g.seed, w, h, 0));
    }
    match g.kind {
        Kind::Gif(frames) => gif(g.seed, g.w, g.h, frames),
        _ => png(&pattern(g.seed, g.w, g.h, 0)),
    }
}

/// Pictures from memory. With `sync`, answers within the call (captures render one frame).
#[derive(Default)]
pub struct MemoryFetcher {
    pub attachments: RefCell<HashMap<i64, Generated>>,
    pub remote: RefCell<HashMap<String, Generated>>,
    pub served: RefCell<HashMap<Source, Bytes>>,
    /// Requests seen, for the self-tests.
    pub asked: Cell<usize>,
    pub latency: Cell<Duration>,
}

impl MemoryFetcher {
    fn generated(&self, source: &Source) -> Option<Generated> {
        match source {
            Source::Attachment { id, .. } => self.attachments.borrow().get(id).copied(),
            Source::Remote(url) => self.remote.borrow().get(url.as_ref()).copied(),
            Source::Local(_) => None,
        }
    }

    /// The answer for `source` now: `None` = never (held).
    pub fn answer(&self, source: &Source) -> Option<Result<Bytes, SharedString>> {
        let Some(g) = self.generated(source) else { return Some(Err("HTTP 404".into())) };
        match g.kind {
            Kind::Hold => return None,
            Kind::Fail => return Some(Err("HTTP 404".into())),
            _ => {}
        }
        if let Some(bytes) = self.served.borrow().get(source) {
            return Some(Ok(bytes.clone()));
        }
        let thumb = match source {
            Source::Attachment { thumb, .. } => *thumb,
            _ => 0,
        };
        let bytes = Bytes::from(file(&g, thumb));
        self.served.borrow_mut().insert(source.clone(), bytes.clone());
        Some(Ok(bytes))
    }
}

/// [`MemoryFetcher`] as the pipeline's fetcher (shared with the screen that fills it).
pub struct SharedFetcher(pub Rc<MemoryFetcher>);

impl Fetcher for SharedFetcher {
    fn fetch(&self, source: &Source, cancel: CancellationToken, cx: &mut App) -> Task<Result<Bytes, SharedString>> {
        self.0.asked.set(self.0.asked.get() + 1);
        let answer = self.0.answer(source);
        let latency = self.0.latency.get();
        match answer {
            None => cx.background_spawn(async move {
                cancel.cancelled().await;
                Err(SharedString::new_static("cancelled"))
            }),
            Some(result) if latency.is_zero() => Task::ready(result),
            Some(result) => cx.spawn(async move |cx| {
                cx.background_executor().timer(latency).await;
                result
            }),
        }
    }

    fn fetch_sync(&self, source: &Source) -> Option<Result<Bytes, SharedString>> {
        if !self.0.latency.get().is_zero() {
            return None;
        }
        self.0.answer(source)
    }
}

// ── The channels ────────────────────────────────────────────────────────────────────────────

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

#[allow(clippy::too_many_arguments)]
fn attachment(id: i64, message_id: i64, position: i32, kind: &str, filename: &str, mime: &str, size: i64, g: Option<&Generated>, created_at: i64) -> AttachmentPayload {
    let pictured = g.is_some_and(|g| !matches!(g.kind, Kind::Gif(_)) || kind == "video");
    AttachmentPayload {
        id,
        message_id,
        kind: kind.into(),
        filename: filename.into(),
        mime: mime.into(),
        size_bytes: size,
        url: String::new(),
        position,
        created_at,
        purged_at: 0,
        width: g.map_or(0, |g| g.w),
        height: g.map_or(0, |g| g.h),
        thumbnail_size_bytes: if pictured { 48_000 } else { 0 },
        thumbnail_sizes_mask: if pictured { 0b111 } else { 0 },
        duration_ms: match kind {
            "audio" => 185_000,
            "video" => 61_000,
            _ => 0,
        },
        placeholder: g.map(placeholder).unwrap_or_default(),
        encrypted: false,
    }
}

/// `#clips`: every attachment kind and grid shape, and their states.
pub fn attachment_payloads(fetcher: &MemoryFetcher) -> Vec<ChannelMessagePayload> {
    let (alice, bob, carol, dave) = (("alice", 1), ("bob", 2), ("carol", 3), ("dave", 4));
    let mut rows = Vec::new();
    let mut next = 0;
    let mut at = DAY;
    let mut add = |sender: (&str, i64), text: &str, files: &[(&str, &str, &str, i64, Option<Generated>)]| {
        next += 1;
        at += 600;
        let mut p = payload(next, sender, ATTACHMENTS, text, at);
        for (i, (kind, name, mime, size, g)) in files.iter().enumerate() {
            let id = next * 10 + i as i64;
            if let Some(g) = g {
                fetcher.attachments.borrow_mut().insert(id, *g);
            }
            p.attachments.push(attachment(id, next, i as i32, kind, name, mime, *size, g.as_ref(), at));
        }
        rows.push(p);
    };
    let still = |w, h, seed| Some(Generated { w, h, kind: Kind::Still, seed });
    add(dave, "A picture under a caption.", &[("image", "sunset.jpg", "image/jpeg", 812_000, still(1600, 900, 1))]);
    add(alice, "", &[("image", "portrait.jpg", "image/jpeg", 640_000, still(900, 1200, 2))]);
    add(bob, "Two side by side:", &[("image", "a.jpg", "image/jpeg", 300_000, still(1600, 900, 3)), ("image", "b.jpg", "image/jpeg", 300_000, still(1280, 960, 4))]);
    add(
        carol,
        "",
        &[
            ("image", "c1.jpg", "image/jpeg", 100_000, still(800, 800, 5)),
            ("image", "c2.jpg", "image/jpeg", 100_000, still(1920, 1080, 6)),
            ("video", "c3.mp4", "video/mp4", 4_000_000, still(1280, 720, 7)),
        ],
    );
    add(
        dave,
        "Five:",
        &[
            ("image", "d1.jpg", "image/jpeg", 1, still(800, 800, 8)),
            ("image", "d2.jpg", "image/jpeg", 1, still(1920, 1080, 9)),
            ("image", "d3.jpg", "image/jpeg", 1, still(640, 480, 10)),
            ("image", "d4.jpg", "image/jpeg", 1, still(1200, 1600, 11)),
            ("image", "d5.jpg", "image/jpeg", 1, still(1024, 576, 12)),
        ],
    );
    add(bob, "", &[("image", "loop.gif", "image/gif", 900_000, Some(Generated { w: 480, h: 270, kind: Kind::Gif(12), seed: 13 }))]);
    add(alice, "A clip:", &[("video", "match-point.mp4", "video/mp4", 24_600_000, still(1920, 1080, 14))]);
    add(carol, "Still loading:", &[("image", "slow.jpg", "image/jpeg", 500_000, Some(Generated { w: 1200, h: 800, kind: Kind::Hold, seed: 15 }))]);
    add(dave, "This one is gone:", &[("image", "missing.png", "image/png", 2_000, Some(Generated { w: 1000, h: 600, kind: Kind::Fail, seed: 16 }))]);
    add(
        bob,
        "A document, a voice note and one the retention already purged:",
        &[
            ("document", "patch-notes.pdf", "application/pdf", 245_760, None),
            ("audio", "voice-note.ogg", "audio/ogg", 1_480_000, None),
            ("document", "old.zip", "application/zip", 52_428_800, None),
        ],
    );
    if let Some(last) = rows.last_mut()
        && let Some(purged) = last.attachments.last_mut()
    {
        purged.purged_at = at + 10;
    }
    rows
}

/// What the link fixtures unfurl to.
pub fn previews() -> Vec<(String, LinkPreview)> {
    vec![
        (
            "https://blog.example.com/posts/vulkan-video".into(),
            LinkPreview::Site(SitePreview {
                url: "https://blog.example.com/posts/vulkan-video".into(),
                site_name: Some("Example Engineering".into()),
                title: Some("Decoding video on the GPU you already render with".into()),
                description: Some("How a chat client can hand compressed frames straight to the graphics queue, why it matters for battery life, and the three driver bugs we met on the way.".into()),
                image: Some(LinkPreviewImage { url: "https://blog.example.com/cover.png".into(), width: 1200, height: 630 }),
                large_image: false,
                color: Some("#5865f2".into()),
            }),
        ),
        (
            "https://video.example.com/watch/abc123".into(),
            LinkPreview::Site(SitePreview {
                url: "https://video.example.com/watch/abc123".into(),
                site_name: Some("ExampleTube".into()),
                title: Some("Ranked match highlights — season finale".into()),
                description: Some("The last five minutes.".into()),
                image: Some(LinkPreviewImage { url: "https://video.example.com/thumb.jpg".into(), width: 1280, height: 720 }),
                large_image: true,
                color: None,
            }),
        ),
        (
            "https://docs.example.org/changelog".into(),
            LinkPreview::Site(SitePreview {
                url: "https://docs.example.org/changelog".into(),
                site_name: Some("docs.example.org".into()),
                title: Some("Changelog".into()),
                description: None,
                image: None,
                large_image: false,
                color: None,
            }),
        ),
        (
            "https://media.klipy.com/gifs/party/hd.gif".into(),
            LinkPreview::Image(ImagePreview { url: "https://media.klipy.com/gifs/party/hd.gif".into(), width: 498, height: 280, gif: true }),
        ),
        (
            "https://images.example.net/screenshot.png".into(),
            LinkPreview::Image(ImagePreview { url: "https://images.example.net/screenshot.png".into(), width: 1920, height: 1080, gif: false }),
        ),
    ]
}

pub fn remote_pictures() -> Vec<(String, Generated)> {
    vec![
        ("https://blog.example.com/cover.png".into(), Generated { w: 1200, h: 630, kind: Kind::Still, seed: 21 }),
        ("https://video.example.com/thumb.jpg".into(), Generated { w: 1280, h: 720, kind: Kind::Still, seed: 22 }),
        ("https://media.klipy.com/gifs/party/hd.gif".into(), Generated { w: 498, h: 280, kind: Kind::Gif(10), seed: 23 }),
        ("https://images.example.net/screenshot.png".into(), Generated { w: 1920, h: 1080, kind: Kind::Still, seed: 24 }),
    ]
}

/// Invite codes and how central answers them.
pub fn invites() -> Vec<(&'static str, Result<ResolvedInvite, &'static str>)> {
    let resolved = |code: &str, id: i64, name: &str, description: &str, members: i32| ResolvedInvite {
        host: "127.0.0.13".into(),
        port: 8082,
        code: code.to_string().into(),
        server_id: id,
        server_name: name.to_string().into(),
        server_description: description.to_string().into(),
        member_count: members,
        picture_version: SharedString::default(),
    };
    vec![
        ("DESIGNLAB", Ok(resolved("DESIGNLAB", 3, "Design Lab", "Critique, mockups and type nerdery.", 42))),
        ("DAILYGG", Ok(resolved("DAILYGG", shell::DAILY_GAMING as i64, "Daily Gaming", "", 298))),
        ("EXPIRED1", Err("Invite expired")),
    ]
}

/// `#off-topic`: link previews, picture links, invite cards, a lone GIF link.
pub fn embed_payloads() -> Vec<ChannelMessagePayload> {
    let (alice, bob, carol, dave) = (("alice", 1), ("bob", 2), ("carol", 3), ("dave", 4));
    let lines: [((&str, i64), &str); 9] = [
        (alice, "Good write-up: https://blog.example.com/posts/vulkan-video"),
        (bob, "https://video.example.com/watch/abc123 the last five minutes are unreal"),
        (carol, "Release notes are up https://docs.example.org/changelog"),
        (dave, "https://media.klipy.com/gifs/party/hd.gif"),
        (alice, "Look at this https://images.example.net/screenshot.png"),
        (bob, "Come hang out decibell://invite/DESIGNLAB"),
        (carol, "decibell://invite/DAILYGG"),
        (dave, "This one is old decibell://invite/EXPIRED1"),
        (alice, "Plain link with nothing to show https://nothing.example.com/"),
    ];
    lines.iter().enumerate().map(|(i, (sender, text))| payload(i as i64 + 1, *sender, EMBEDS, text, DAY + 600 * (i as i64 + 1))).collect()
}

/// A channel as a list source: every row at once, no paging.
pub struct ContentSource {
    pub server: ServerId,
    pub channel: ChannelId,
}

impl decibell_chat::ListSource for ContentSource {
    fn slice(&self, cx: &mut App) -> Entity<MessageSlice> {
        let model = Model::global(cx).clone();
        let own = model.auth.read(cx).uid;
        model.chat.channels.update(cx, |cache, cx| cache.ensure(self.server, &self.channel, own, false, cx))
    }
    fn server(&self) -> Option<ServerId> {
        Some(self.server)
    }
    fn load_older(&self, _: &mut App) -> Option<Task<Option<bool>>> {
        None
    }
    fn load_newer(&self, _: &mut App) -> Option<Task<Option<bool>>> {
        None
    }
    fn jump_to_present(&self, _: &mut App) {}
    fn jump_to_message(&self, _: i64, _: &mut App) -> decibell_chat::Jump {
        decibell_chat::Jump::Loaded
    }
    fn jump_landed(&self, _: &mut App) {}
    fn can_delete_others(&self, _: &App) -> bool {
        false
    }
    fn edit(&self, _: i64, _: &str, _: &mut App) {}
    fn delete(&self, _: i64, _: &mut App) {}
}

/// The shared memory fetcher of this process (created with the model).
pub fn fetcher(cx: &mut App) -> Rc<MemoryFetcher> {
    struct Held(Rc<MemoryFetcher>);
    impl gpui::Global for Held {}
    if let Some(held) = cx.try_global::<Held>() {
        return held.0.clone();
    }
    let fetcher = Rc::new(MemoryFetcher::default());
    for (url, g) in remote_pictures() {
        fetcher.remote.borrow_mut().insert(url, g);
    }
    cx.set_global(Held(fetcher.clone()));
    fetcher
}

/// Signs the model in to the shell's fixture (once per process), installs the pipeline on the
/// memory fetcher, answers the link previews and invites from memory, and routes `channel`'s rows.
pub fn open(channel: &str, payloads: Vec<ChannelMessagePayload>, cx: &mut App) -> Rc<ContentSource> {
    struct Applied;
    impl gpui::Global for Applied {}
    let model = if cx.has_global::<Applied>() {
        Model::global(cx).clone()
    } else {
        cx.set_global(Applied);
        let model = shell::apply(shell::Scene::Server, cx);
        let fetcher = fetcher(cx);
        crate::install_fixture(Rc::new(SharedFetcher(fetcher)), cx);
        let answers: Rc<HashMap<SharedString, LinkPreview>> = Rc::new(previews().into_iter().map(|(url, p)| (url.into(), p)).collect());
        model.chat.link_previews.update(cx, |memo, _| memo.set_unfurler(Rc::new(move |url, _| Task::ready(answers.get(&url).cloned()))));
        for (code, answer) in invites() {
            let result = answer.map_err(|text| decibell_model::UiError {
                kind: decibell_core::api::ErrorKind::Refused,
                text: text.into(),
                cert: None,
            });
            model.invites.update(cx, |invites, cx| invites.finish(code.into(), result, cx));
        }
        model
    };
    let server = ServerId::from_central(shell::DAILY_GAMING);
    let channel: ChannelId = channel.to_string().into();
    model.nav.update(cx, |nav, cx| nav.set_active_channel(Some(channel.clone()), cx));
    let source = Rc::new(ContentSource { server, channel: channel.clone() });
    // Previews resolve before the first frame, as when a channel was visited before.
    for p in &payloads {
        let document = decibell_richtext::parse(&p.content, &decibell_richtext::ParseOptions::default());
        for (url, kind) in document.embeds(&p.content) {
            if kind == decibell_richtext::EmbedKind::Web {
                let url: SharedString = url.to_string().into();
                model.chat.link_previews.update(cx, |memo, cx| memo.request(url, cx));
            }
        }
    }
    let event = CoreEvent::ChannelHistoryReceived(ChannelHistoryReceivedPayload {
        server_id: shell::DAILY_GAMING.to_string(),
        channel_id: channel.to_string(),
        messages: payloads,
        has_more: false,
        has_more_after: false,
        around_id: 0,
        after_id: 0,
        before_id: 0,
    });
    decibell_model::events::route(&model, event, cx);
    source
}
