//! The image viewer (`ImageViewer.tsx` + `imageViewer/*`): an overlay over the main layout that
//! opens out of the clicked thumbnail and closes back into it, on the theme's stage (the viewer's
//! own scope: light in the light themes) washed with the picture's colours; real-pixel zoom with
//! the minimap, drag to pan with a glide, wheel zoom toward the pointer, the message's pictures
//! only (owner decision 2026-10-28), arrows / keys, Copy, Save, Esc / a click outside.
//!
//! **Pictures.** What the chat shows is painted at once (the chat's decode, scaled); the picture
//! decoded for the stage replaces it (the "preview" for the fit box from the chat's thumbnail, then
//! the original: both through the pipeline, decoded off the UI thread at the fit box's device
//! pixels and never larger than the file). Zoomed past that, **detail** renders the visible part
//! of the original at the zoom's device pixels in the background (nearest-neighbour from 300 %,
//! as the renderer's `image-rendering: pixelated`). Policy for large originals: the GPU never holds
//! more than the stage's area twice (the fit decode and one detail); the full-resolution decode
//! lives in CPU memory only while the picture is zoomed past its fit decode, and is dropped when it
//! zooms back, steps to another picture or closes (an 8000 × 6000 JPEG costs its 144 MB RGB decode
//! then, never 192 MB of RGBA on the GPU; Chromium keeps the RGBA decode for as long as it shows).
//!
//! **Motion**: the flights (300 ms, `cubic-bezier(0.2, 0.8, 0.2, 1)`), zoom tweens and the glide
//! draw frames only while they run; reduced motion fades instead of flying. GPUI has no transforms:
//! the flight is the picture painted in the interpolated box with the thumbnail's crop as a clip,
//! and a quarter turn swaps to a rotated decode at once (no turning animation).

pub mod geometry;
mod stage;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use decibell_common::{UserAvatar, Watch, display_name};
use decibell_model::chat::Message;
use decibell_model::chat::viewers::{ViewerImage, ViewerOrigin};
use decibell_model::{Model, ServerId};
use decibell_text::Wrapping;
use decibell_ui::{ActiveTheme as _, Icon, IconName, Spinner, title_tooltip};
use gpui::{
    AnyView, App, AppContext as _, Bounds, Context, Corners, EntityId, FocusHandle, Global, Hsla, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, RenderImage, ScrollDelta, ScrollEvent, SharedString, Task, Window, canvas, div,
    point, prelude::*, px, size,
};

use self::geometry::{Area, MAX_SCALE, Size, View, clamp_scale, clamp_view, ease, fit_area, fit_scale, mix, overflows, shown, step_scale, zoom_at};
use crate::actions::{self, Target};
use crate::attachments::OpenRequest;
use crate::pictures::{Fit, Look, PicKey, Pictures, Source, Spec, Want};
use crate::style::{ContentStyle, label, label_in};

const FLY: Duration = Duration::from_millis(300);
const IDLE: Duration = Duration::from_millis(2500);
const SLOW: Duration = Duration::from_millis(400);

/// A link picture to open (`ImageEmbed`'s click).
pub struct LinkOpen {
    pub key: SharedString,
    pub url: SharedString,
    pub page_url: SharedString,
    pub filename: SharedString,
    pub size: (u32, u32),
    pub server: Option<ServerId>,
    pub message: Arc<Message>,
    pub bounds: Bounds<Pixels>,
    pub list: EntityId,
}

/// What the viewer shares with the tiles: the thumbnail it hides while flying, the list to redraw
/// when that changes, and where each thumbnail was painted last (to fly home to).
#[derive(Default)]
struct Shared {
    /// A screen's zoom for the next session (the zoomed capture).
    initial_scale: Option<f32>,
    hidden: Option<SharedString>,
    list: Option<EntityId>,
    targets: RefCell<HashMap<SharedString, Bounds<Pixels>>>,
}

impl Global for Shared {}

fn shared(cx: &mut App) -> &mut Shared {
    cx.default_global::<Shared>()
}

/// Whether the viewer hides attachment `id`'s thumbnail now (it flies out of / back into it).
pub fn hides(server: Option<ServerId>, id: i64, cx: &App) -> bool {
    server.is_some_and(|s| hides_key(&attachment_key(s, id), cx))
}

pub fn hides_key(key: &str, cx: &App) -> bool {
    cx.try_global::<Shared>().and_then(|s| s.hidden.as_deref()).is_some_and(|hidden| hidden == key)
}

/// A thumbnail was painted at `bounds` (the flight home goes there).
pub fn target_drawn(key: SharedString, bounds: Bounds<Pixels>, cx: &mut App) {
    if cx.try_global::<Model>().is_some_and(|m| m.chat.viewer.read(cx).is_open()) || cx.try_global::<Shared>().is_some_and(|s| s.hidden.is_some()) {
        shared(cx).targets.borrow_mut().insert(key, bounds);
    }
}

pub fn attachment_key(server: ServerId, id: i64) -> SharedString {
    format!("a:{}:{id}", server.wire()).into()
}

/// `att:<server>:<id>:<thumb>` → the pipeline's source; anything else is a remote URL.
fn source_of(url: &str) -> Source {
    if let Some(rest) = url.strip_prefix("att:") {
        let mut parts = rest.rsplitn(3, ':');
        let thumb = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        let id = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        let server = parts.next().unwrap_or_default();
        return Source::Attachment { server: server.to_string().into(), id, thumb };
    }
    Source::Remote(url.to_string().into())
}

fn att_url(server: ServerId, id: i64, thumb: u32) -> SharedString {
    format!("att:{}:{id}:{thumb}", server.wire()).into()
}

/// A click on an attachment's picture: the message's pictures (`galleryFor`), from its box.
pub fn open_attachment(req: OpenRequest, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let mut images: Vec<&decibell_model::chat::Attachment> =
        req.message.attachments.iter().filter(|a| a.kind == decibell_attachments::AttachmentKind::Image && a.purged_at == 0).collect();
    images.sort_by_key(|a| a.position);
    let scale = cx.try_global::<Shared>().map_or(1., |_| 1.);
    let _ = scale;
    let mut items = Vec::new();
    let mut index = 0;
    for a in images {
        if a.id == req.attachment_id {
            index = items.len();
        }
        let preview = crate::sizing::preview_thumb(a.thumbnail_sizes_mask, a.thumbnail_size_bytes, &a.mime, a.width.min(550) as f32, a.height.min(350) as f32, 1.).unwrap_or(0);
        let small = if a.mime.as_ref() != "image/gif" && a.thumbnail_size_bytes > 0 {
            decibell_attachments::thumbs::pick_thumbnail_size(a.thumbnail_sizes_mask, 160).unwrap_or(0)
        } else {
            0
        };
        items.push(ViewerImage {
            key: attachment_key(req.server, a.id),
            url: att_url(req.server, a.id, 0),
            preview_url: Some(att_url(req.server, a.id, preview)),
            thumb_url: Some(att_url(req.server, a.id, small)),
            placeholder: (!a.placeholder.is_empty()).then(|| a.placeholder.clone()),
            filename: a.filename.clone(),
            width: a.width,
            height: a.height,
            size_bytes: Some(a.size_bytes),
            mime: Some(a.mime.clone()),
            server: Some(req.server),
            attachment_id: Some(a.id),
            channel: Some(req.message.channel.clone()),
            sender_uid: Some(req.message.sender_uid),
            sender: Some(req.message.sender.clone()),
            timestamp: req.message.timestamp,
            page_url: None,
        });
    }
    if items.is_empty() {
        return;
    }
    let origin = ViewerOrigin {
        left: f32::from(req.bounds.left()),
        top: f32::from(req.bounds.top()),
        width: f32::from(req.bounds.size.width),
        height: f32::from(req.bounds.size.height),
        cover: req.cover,
    };
    shared(cx).list = Some(req.list);
    model.chat.viewer.update(cx, |v, cx| v.open(items, index, Some(origin), cx));
}

/// A click on a link picture: that picture alone.
pub fn open_link(req: LinkOpen, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let item = ViewerImage {
        key: req.key.clone(),
        url: req.url.clone(),
        preview_url: None,
        thumb_url: None,
        placeholder: None,
        filename: req.filename.clone(),
        width: req.size.0,
        height: req.size.1,
        size_bytes: None,
        mime: None,
        server: req.server,
        attachment_id: None,
        channel: Some(req.message.channel.clone()),
        sender_uid: Some(req.message.sender_uid),
        sender: Some(req.message.sender.clone()),
        timestamp: req.message.timestamp,
        page_url: Some(req.page_url.clone()),
    };
    let origin = ViewerOrigin {
        left: f32::from(req.bounds.left()),
        top: f32::from(req.bounds.top()),
        width: f32::from(req.bounds.size.width),
        height: f32::from(req.bounds.size.height),
        cover: false,
    };
    shared(cx).list = Some(req.list);
    model.chat.viewer.update(cx, |v, cx| v.open(vec![item], 0, Some(origin), cx));
}

pub fn init(cx: &mut App) {
    decibell_shell::register_overlay(cx, |window, cx| {
        let view = cx.new(ImageViewer::new);
        cx.set_global(Live(view.downgrade()));
        let _ = window;
        view.into()
    });
}

/// The viewer the shell draws (for the self-tests).
struct Live(gpui::WeakEntity<ImageViewer>);

impl Global for Live {}

fn live(cx: &App) -> Option<gpui::Entity<ImageViewer>> {
    cx.try_global::<Live>().and_then(|l| l.0.upgrade())
}

/// The open viewer's zoom (self-tests).
pub fn current_scale(cx: &App) -> Option<f32> {
    let v = live(cx)?;
    let v = v.read(cx);
    v.session.as_ref().map(|s| s.view.scale)
}

/// The open viewer's pan (self-tests).
pub fn current_pan(cx: &App) -> Option<(f32, f32)> {
    let v = live(cx)?;
    let v = v.read(cx);
    v.session.as_ref().map(|s| (s.view.x, s.view.y))
}

/// The viewer's overlay view.
pub fn overlay(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(ImageViewer::new).into()
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Enter(Instant),
    Open,
    /// Flying home to the thumbnail (`Some`) or fading out (`None`).
    Exit(Instant, Option<Bounds<Pixels>>),
}

struct Tween {
    from: View,
    to: View,
    start: Instant,
    length: Duration,
}

struct Drag {
    start: Point<Pixels>,
    view: View,
    on_picture: bool,
    moved: bool,
    samples: Vec<(Instant, Point<Pixels>)>,
}

/// A render of the visible part of the original at the zoom's device pixels.
struct Detail {
    image: Arc<RenderImage>,
    /// The device size it was rendered at (with `region` and `rot`: what a new request compares).
    out: (u32, u32),
    /// The part of the picture it shows, in natural pixels (unrotated): x, y, w, h.
    region: [f32; 4],
    rot: i32,
}

struct Session {
    items: Vec<ViewerImage>,
    index: usize,
    origin: Option<ViewerOrigin>,
    phase: Phase,
    view: View,
    tween: Option<Tween>,
    stage: Bounds<Pixels>,
    idle: bool,
    idle_timer: Option<Task<()>>,
    holding: bool,
    drag: Option<Drag>,
    glide: Option<((f32, f32), Instant)>,
    slow: bool,
    slow_timer: Option<Task<()>>,
    shown_key: SharedString,
    step_dir: i32,
    /// The original decoded at full size (CPU only), while zoomed past the fit decode.
    full: Option<(SharedString, Arc<image::DynamicImage>)>,
    detail: Option<Detail>,
    detail_job: Option<Task<()>>,
    detail_wanted: Option<(View, Bounds<Pixels>)>,
    /// The fit decode turned a quarter (or more), keyed by the decode's image id and the turn.
    turned: Option<(u64, i32, Arc<RenderImage>)>,
    turn_job: Option<Task<()>>,
    wash: Option<(SharedString, Arc<RenderImage>)>,
    focused: bool,
}

/// See the module doc.
pub struct ImageViewer {
    session: Option<Session>,
    focus: FocusHandle,
    _watch: Watch,
}

impl ImageViewer {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut watch = Watch::new();
        if let Some(model) = cx.try_global::<Model>().cloned() {
            watch.store(&model.chat.viewer, cx);
            watch.store(&model.settings, cx);
        }
        if let Some(pictures) = Pictures::global(cx) {
            // A picture of the session arrived: redraw (the pipeline notifies its owners; this
            // view is one, through `set_wants`).
            let _ = pictures;
        }
        Self { session: None, focus: cx.focus_handle(), _watch: watch }
    }

    fn item(&self) -> Option<&ViewerImage> {
        let s = self.session.as_ref()?;
        s.items.get(s.index.min(s.items.len().saturating_sub(1)))
    }

    /// Follows the store: a session starts, steps, or starts closing.
    fn follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        let store = model.chat.viewer.read(cx);
        let open = store.is_open();
        let (items, index, origin) = (store.items.clone(), store.index, store.origin);
        let reduce = cx.reduce_motion();
        match (&mut self.session, open) {
            (None, true) => {
                let now = Instant::now();
                let key = items.get(index).map(|i| i.key.clone()).unwrap_or_default();
                let flies = origin.is_some() && !reduce;
                self.session = Some(Session {
                    items,
                    index,
                    origin,
                    phase: if flies { Phase::Enter(now) } else { Phase::Open },
                    view: View { scale: 1., x: 0., y: 0., rot: 0 },
                    tween: None,
                    stage: Bounds::default(),
                    idle: false,
                    idle_timer: None,
                    holding: false,
                    drag: None,
                    glide: None,
                    slow: false,
                    slow_timer: None,
                    shown_key: SharedString::default(),
                    step_dir: 0,
                    full: None,
                    detail: None,
                    detail_job: None,
                    detail_wanted: None,
                    turned: None,
                    turn_job: None,
                    wash: None,
                    focused: false,
                });
                if flies {
                    set_hidden(Some(key), cx);
                }
                self.wake(cx);
            }
            (Some(session), true) => {
                if session.index != index || session.items.len() != items.len() {
                    session.step_dir = (index as i32 - session.index as i32).signum();
                }
                session.items = items;
                session.index = index;
            }
            (Some(session), false) => {
                if !matches!(session.phase, Phase::Exit(..)) {
                    let key = session.shown_key.clone();
                    let target = cx.try_global::<Shared>().and_then(|s| s.targets.borrow().get(&key).copied());
                    let window_bounds = window.viewport_size();
                    let visible = target.filter(|b| {
                        f32::from(b.size.width) > 0.
                            && f32::from(b.bottom()) > f32::from(session.stage.top())
                            && f32::from(b.top()) < f32::from(window_bounds.height)
                            && f32::from(b.right()) > 0.
                            && f32::from(b.left()) < f32::from(window_bounds.width)
                    });
                    let fly = visible.filter(|_| !reduce);
                    if fly.is_some() {
                        // The picture unwinds (zoom, pan) while it flies home.
                        let rot = (session.view.rot as f32 / 360.).round() as i32 * 360;
                        session.tween = None;
                        session.view = View { x: 0., y: 0., rot, ..session.view };
                        set_hidden(Some(key), cx);
                    }
                    session.phase = Phase::Exit(Instant::now(), fly);
                    session.drag = None;
                    session.glide = None;
                }
            }
            (None, false) => {}
        }
        let _ = window;
    }

    fn wake(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &mut self.session else { return };
        let was_idle = std::mem::replace(&mut session.idle, false);
        session.idle_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(IDLE).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(s) = &mut this.session
                    && !s.holding
                {
                    s.idle = true;
                    cx.notify();
                }
            });
        }));
        if was_idle {
            cx.notify();
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(model) = cx.try_global::<Model>().cloned() {
            model.chat.viewer.update(cx, |v, cx| v.close(cx));
        }
    }

    fn go(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(model) = cx.try_global::<Model>().cloned() {
            model.chat.viewer.update(cx, |v, cx| v.go(index, cx));
        }
    }

    // ── Geometry of the current item ─────────────────────────────────────────────────────────

    fn natural(&self, cx: &App) -> Size {
        let Some(item) = self.item() else { return Size { w: 640., h: 480. } };
        if item.width > 0 && item.height > 0 {
            return Size { w: item.width as f32, h: item.height as f32 };
        }
        // Measured from the decode, else the thumbnail's box, else 640 × 480.
        if let Some(pictures) = Pictures::global(cx)
            && let Some(picture) = self.level_zero(cx).or_else(|| match pictures.read(cx).look(&self.keys(cx).0) {
                Look::Ready(p) => Some(p),
                _ => None,
            })
        {
            return Size { w: picture.natural.0 as f32, h: picture.natural.1 as f32 };
        }
        if let Some(origin) = self.session.as_ref().and_then(|s| s.origin) {
            return Size { w: origin.width, h: origin.height };
        }
        Size { w: 640., h: 480. }
    }

    fn stage_size(&self) -> Size {
        let b = self.session.as_ref().map_or(Bounds::default(), |s| s.stage);
        Size { w: f32::from(b.size.width).max(1.), h: f32::from(b.size.height).max(1.) }
    }

    fn area(&self) -> Area {
        let strip = self.session.as_ref().is_some_and(|s| s.items.len() > 1);
        fit_area(self.stage_size(), strip)
    }

    /// The fit box (rotation 0): the decode's box.
    fn fit_box(&self, nat: Size) -> (f32, f32) {
        let base = fit_scale(nat, 0, self.area());
        ((nat.w * base).max(1.), (nat.h * base).max(1.))
    }

    /// (the original's key, the preview's key) for the fit box.
    fn keys(&self, cx: &App) -> (PicKey, Option<PicKey>) {
        let item = self.item().cloned().unwrap_or_else(|| ViewerImage {
            key: SharedString::default(),
            url: SharedString::default(),
            preview_url: None,
            thumb_url: None,
            placeholder: None,
            filename: SharedString::default(),
            width: 0,
            height: 0,
            size_bytes: None,
            mime: None,
            server: None,
            attachment_id: None,
            channel: None,
            sender_uid: None,
            sender: None,
            timestamp: None,
            page_url: None,
        });
        let nat = if item.width > 0 { Size { w: item.width as f32, h: item.height as f32 } } else { self.stage_size() };
        let (w, h) = self.fit_box(nat);
        let scale = self.session.as_ref().map_or(1., |_| cx.try_global::<ScaleHint>().map_or(1., |s| s.0));
        let spec = Spec { width: w, height: h, scale, fit: Fit::Contain, upscale: false };
        let full = PicKey::new(source_of(&item.url), spec);
        let preview = item.preview_url.as_ref().filter(|p| *p != &item.url).map(|p| PicKey::new(source_of(p), spec));
        (full, preview)
    }

    /// The best decode of the current item for the fit box: the original, else the preview.
    fn level_zero(&self, cx: &App) -> Option<crate::pictures::Picture> {
        let pictures = Pictures::global(cx)?;
        let (full, preview) = self.keys(cx);
        let p = pictures.read(cx);
        match p.look(&full) {
            Look::Ready(picture) => Some(picture),
            _ => preview.and_then(|k| match p.look(&k) {
                Look::Ready(picture) => Some(picture),
                _ => p.stand_in(&k),
            }),
        }
    }

    fn ready(&self, cx: &App) -> bool {
        Pictures::global(cx).is_some_and(|p| matches!(p.read(cx).look(&self.keys(cx).0), Look::Ready(_)))
    }

    fn failed(&self, cx: &App) -> bool {
        Pictures::global(cx).is_some_and(|p| matches!(p.read(cx).look(&self.keys(cx).0), Look::Failed(_)))
    }

    /// Wants this item's decodes (and its neighbours' originals, so stepping is instant).
    fn want(&self, cx: &mut Context<Self>) {
        let Some(pictures) = Pictures::global(cx) else { return };
        let Some(session) = &self.session else { return };
        let (full, preview) = self.keys(cx);
        let mut wants = vec![Want { key: full.clone(), visible: true }];
        wants.extend(preview.map(|key| Want { key, visible: true }));
        let spec = full.spec();
        for i in [session.index + 1, session.index.wrapping_sub(1)] {
            if let Some(item) = session.items.get(i) {
                let nat = if item.width > 0 { Size { w: item.width as f32, h: item.height as f32 } } else { self.stage_size() };
                let (w, h) = self.fit_box(nat);
                wants.push(Want { key: PicKey::new(source_of(&item.url), Spec { width: w, height: h, ..spec }), visible: false });
            }
        }
        let owner = (cx.entity_id(), 9);
        pictures.update(cx, |p, cx| p.set_wants(owner, wants, cx));
    }

    // ── View changes ────────────────────────────────────────────────────────────────────────

    fn current_view(&self, now: Instant) -> View {
        let Some(session) = &self.session else { return View { scale: 1., x: 0., y: 0., rot: 0 } };
        match &session.tween {
            Some(t) => {
                let k = (now.saturating_duration_since(t.start).as_secs_f32() / t.length.as_secs_f32().max(0.001)).min(1.);
                mix(t.from, t.to, ease(k))
            }
            None => session.view,
        }
    }

    fn apply(&mut self, next: View, anim: Duration, cx: &mut Context<Self>) {
        let nat = self.natural(cx);
        let (area, stage) = (self.area(), self.stage_size());
        let now = Instant::now();
        let from = self.current_view(now);
        let s = clamp_scale(next.scale, nat, next.rot, area);
        let to = clamp_view(View { scale: s, ..next }, nat, stage, area);
        let reduce = cx.reduce_motion();
        let Some(session) = &mut self.session else { return };
        session.tween = (!anim.is_zero() && !reduce).then_some(Tween { from, to, start: now, length: anim });
        session.view = to;
        cx.notify();
    }

    fn zoom_toward(&mut self, scale: f32, p: (f32, f32), anim: Duration, cx: &mut Context<Self>) {
        let nat = self.natural(cx);
        let v = self.current_view(Instant::now());
        let s = clamp_scale(scale, nat, v.rot, self.area());
        if (s - v.scale).abs() < 1e-6 {
            return;
        }
        self.apply(zoom_at(v, s, p), anim, cx);
    }

    fn fit(&self, cx: &App) -> f32 {
        let v = self.session.as_ref().map_or(View { scale: 1., x: 0., y: 0., rot: 0 }, |s| s.view);
        fit_scale(self.natural(cx), v.rot, self.area())
    }

    fn toggle_zoom(&mut self, p: (f32, f32), cx: &mut Context<Self>) {
        let fit = self.fit(cx);
        let v = self.current_view(Instant::now());
        if (v.scale - fit).abs() >= 1e-4 {
            self.apply(View { scale: fit, x: 0., y: 0., ..v }, Duration::from_millis(260), cx);
        } else {
            // A big picture goes to its actual pixels; one at (or near) actual size doubles.
            let to = if fit < 0.9 { 1. } else { MAX_SCALE.min(fit * 2.) };
            self.zoom_toward(to, p, Duration::from_millis(220), cx);
        }
    }

    fn rotate(&mut self, cx: &mut Context<Self>) {
        let v = self.current_view(Instant::now());
        let rot = v.rot + 90;
        let scale = fit_scale(self.natural(cx), rot, self.area());
        self.apply(View { rot, scale, x: 0., y: 0. }, Duration::ZERO, cx);
    }

    fn target(&self) -> Option<Target> {
        let item = self.item()?;
        Some(Target {
            server: item.server,
            attachment_id: item.attachment_id,
            filename: item.filename.clone(),
            mime: item.mime.clone(),
            url: (item.attachment_id.is_none()).then(|| item.url.clone()),
            page_url: item.page_url.clone(),
        })
    }

    // ── Input ───────────────────────────────────────────────────────────────────────────────

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        if matches!(session.phase, Phase::Exit(..)) {
            return;
        }
        let (index, len) = (session.index, session.items.len());
        // A right-click menu over the picture closes on its own Esc.
        if cx.try_global::<Model>().is_some_and(|m| m.chat.media_menu.read(cx).open.is_some()) {
            return;
        }
        cx.stop_propagation();
        self.wake(cx);
        let k = &event.keystroke;
        let modifier = k.modifiers.control || k.modifiers.platform;
        let fit = self.fit(cx);
        let v = self.current_view(Instant::now());
        match k.key.as_str() {
            "escape" => self.close(cx),
            "left" => {
                if index > 0 {
                    self.go(index - 1, cx);
                }
            }
            "right" => self.go(index + 1, cx),
            "home" => self.go(0, cx),
            "end" => self.go(len.saturating_sub(1), cx),
            "+" | "=" => self.zoom_toward(step_scale(v.scale, fit, 1), (0., 0.), Duration::from_millis(220), cx),
            "-" => self.zoom_toward(step_scale(v.scale, fit, -1), (0., 0.), Duration::from_millis(220), cx),
            "0" => self.apply(View { scale: fit, x: 0., y: 0., ..v }, Duration::from_millis(220), cx),
            "1" => self.zoom_toward(1., (0., 0.), Duration::from_millis(220), cx),
            "r" if !modifier => self.rotate(cx),
            "c" if modifier => {
                if let Some(t) = self.target() {
                    actions::copy_image(t, cx);
                }
            }
            "s" if modifier => {
                if let Some(t) = self.target().filter(actions::can_save) {
                    actions::save(&t, decibell_attachments::AttachmentKind::Image, false, cx);
                }
            }
            _ => {}
        }
    }

    /// A stage point relative to the fit area's centre.
    fn to_area(&self, p: Point<Pixels>) -> (f32, f32) {
        let s = self.session.as_ref().map_or(Bounds::default(), |s| s.stage);
        let a = self.area();
        (f32::from(p.x - s.left()) - (a.left + a.w / 2.), f32::from(p.y - s.top()) - (a.top + a.h / 2.))
    }

    /// The displayed picture's box in window coordinates for `view`.
    fn picture_bounds(&self, view: View, nat: Size) -> Bounds<Pixels> {
        let s = self.session.as_ref().map_or(Bounds::default(), |s| s.stage);
        let a = self.area();
        let d = shown(nat, view.rot);
        let (w, h) = (d.w * view.scale, d.h * view.scale);
        let cx = f32::from(s.left()) + a.left + a.w / 2. + view.x;
        let cy = f32::from(s.top()) + a.top + a.h / 2. + view.y;
        Bounds::new(point(px(cx - w / 2.), px(cy - h / 2.)), size(px(w), px(h)))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        if event.button != MouseButton::Left || matches!(session.phase, Phase::Exit(..)) {
            return;
        }
        let nat = self.natural(cx);
        let view = self.current_view(Instant::now());
        let on_picture = self.picture_bounds(view, nat).contains(&event.position);
        let session = self.session.as_mut().expect("checked");
        session.glide = None;
        session.drag = Some(Drag { start: event.position, view, on_picture, moved: false, samples: vec![(Instant::now(), event.position)] });
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.wake(cx);
        let nat = self.natural(cx);
        let stage = self.stage_size();
        let Some(session) = &mut self.session else { return };
        let Some(drag) = &mut session.drag else { return };
        let (dx, dy) = (f32::from(event.position.x - drag.start.x), f32::from(event.position.y - drag.start.y));
        if !drag.moved && dx.hypot(dy) < 4. {
            return;
        }
        drag.moved = true;
        drag.samples.push((Instant::now(), event.position));
        if drag.samples.len() > 6 {
            drag.samples.remove(0);
        }
        let base = drag.view;
        if overflows(base, nat, stage) {
            self.apply(View { x: base.x + dx, y: base.y + dy, ..base }, Duration::ZERO, cx);
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let nat = self.natural(cx);
        let stage = self.stage_size();
        let Some(session) = &mut self.session else { return };
        let Some(drag) = session.drag.take() else { return };
        if !drag.moved {
            if drag.on_picture {
                let p = self.to_area(event.position);
                self.toggle_zoom(p, cx);
            } else {
                self.close(cx);
            }
            return;
        }
        // A flick glides to a stop.
        let (a, b) = (drag.samples[0], *drag.samples.last().expect("one sample"));
        let dt = b.0.saturating_duration_since(a.0).as_secs_f32() * 1000.;
        if dt <= 0. || b.0.elapsed() > Duration::from_millis(60) || !overflows(session.view, nat, stage) {
            return;
        }
        let v = (f32::from(b.1.x - a.1.x) / dt, f32::from(b.1.y - a.1.y) / dt);
        session.glide = Some((v, Instant::now()));
        cx.notify();
    }

    fn scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.wake(cx);
        let dy = match event.delta {
            ScrollDelta::Lines(lines) => -f32::from(lines.y) * 16. * 3.,
            ScrollDelta::Pixels(p) => -f32::from(p.y),
        };
        if dy == 0. {
            return;
        }
        let k = (-dy * if event.modifiers.control { 0.01 } else { 0.0022 }).exp();
        let v = self.current_view(Instant::now());
        let p = self.to_area(event.position);
        let anim = if dy.abs() >= 40. { Duration::from_millis(120) } else { Duration::ZERO };
        self.zoom_toward(v.scale * k, p, anim, cx);
        cx.stop_propagation();
    }

    /// Advances what moves (flight, tween, glide); asks for the next frame while something does.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        let nat = self.natural(cx);
        let (area, stage) = (self.area(), self.stage_size());
        let Some(session) = &mut self.session else { return false };
        let mut moving = false;
        if let Some(t) = &session.tween {
            if now.saturating_duration_since(t.start) >= t.length {
                session.tween = None;
            } else {
                moving = true;
            }
        }
        if let Some(((vx, vy), last)) = session.glide {
            let step = now.saturating_duration_since(last).as_secs_f32() * 1000.;
            let v = clamp_view(View { x: session.view.x + vx * step, y: session.view.y + vy * step, ..session.view }, nat, stage, area);
            session.view = v;
            let decay = (-step / 160.).exp();
            let (vx, vy) = (vx * decay, vy * decay);
            session.glide = (vx.hypot(vy) > 0.02).then_some(((vx, vy), now));
            moving |= session.glide.is_some();
        }
        match session.phase {
            Phase::Enter(start) => {
                if now.saturating_duration_since(start) >= FLY {
                    session.phase = Phase::Open;
                    set_hidden(None, cx);
                } else {
                    moving = true;
                }
            }
            Phase::Exit(start, fly) => {
                let length = if fly.is_some() { FLY } else { Duration::from_millis(200) };
                if now.saturating_duration_since(start) >= length {
                    self.session = None;
                    set_hidden(None, cx);
                    if let Some(pictures) = Pictures::global(cx) {
                        let owner = (cx.entity_id(), 9);
                        pictures.update(cx, |p, cx| p.set_wants(owner, Vec::new(), cx));
                    }
                    return false;
                }
                moving = true;
            }
            Phase::Open => {}
        }
        if moving {
            window.request_animation_frame();
        }
        moving
    }
}

/// Where the minimap was painted (its presses move the view).
struct MinimapAt(Point<Pixels>);

impl Global for MinimapAt {}

/// The window's scale for the keys (the overlay learns it when it paints).
struct ScaleHint(f32);

impl Global for ScaleHint {}

fn set_hidden(key: Option<SharedString>, cx: &mut App) {
    let s = shared(cx);
    if s.hidden == key {
        return;
    }
    s.hidden = key;
    if s.hidden.is_none() {
        s.targets.borrow_mut().clear();
    }
    if let Some(list) = s.list {
        cx.notify(list);
    }
}

fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string())).unwrap_or_else(|| url.to_string())
}

impl Render for ImageViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.follow(window, cx);
        cx.set_global(ScaleHint(window.scale_factor()));
        // Before the first layout: the content area under the title bar, as the window has it.
        if let Some(s) = &mut self.session
            && s.stage.size.width <= px(0.)
        {
            let viewport = window.viewport_size();
            let margin = decibell_ui::outer_window_size(size(px(0.), px(0.)));
            let (w, h) = (viewport.width - margin.width, viewport.height - margin.height - decibell_ui::TITLEBAR_HEIGHT);
            s.stage = Bounds::new(point(margin.width / 2., margin.height / 2. + decibell_ui::TITLEBAR_HEIGHT), size(w, h));
        }
        let Some(session) = &self.session else { return div().into_any_element() };
        // The current item changed: start at fit, show "Loading original" after 400 ms.
        let key = self.item().map(|i| i.key.clone()).unwrap_or_default();
        if session.shown_key != key {
            let nat = self.natural(cx);
            let fit = fit_scale(nat, 0, self.area());
            let initial = cx.try_global::<Shared>().and_then(|s| s.initial_scale);
            if initial.is_some() {
                shared(cx).initial_scale = None;
            }
            let s = self.session.as_mut().expect("checked");
            s.shown_key = key.clone();
            s.view = View { scale: initial.unwrap_or(fit), x: 0., y: 0., rot: 0 };
            s.tween = None;
            s.full = None;
            s.detail = None;
            s.turned = None;
            s.slow = false;
            s.slow_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SLOW).await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(s) = &mut this.session {
                        s.slow = true;
                        cx.notify();
                    }
                });
            }));
            self.want(cx);
        }
        let moving = self.tick(window, cx);
        let Some(session) = &self.session else { return div().into_any_element() };
        if !session.focused {
            self.focus.focus(window, cx);
            if let Some(s) = &mut self.session {
                s.focused = true;
            }
        }
        let session = self.session.as_ref().expect("checked");
        let now = Instant::now();
        let view = self.current_view(now);
        let nat = self.natural(cx);
        let fit = fit_scale(nat, view.rot, self.area());
        let at_fit = (view.scale - fit).abs() < 1e-4;
        let phase = session.phase;
        let chrome_on = phase == Phase::Open && !session.idle;
        let theme = cx.theme();
        let stage_color = stage::stage_color(cx);
        let item = self.item().cloned();
        let strip = session.items.len() > 1;
        let (index, len) = (session.index, session.items.len());
        let style = ContentStyle::get(cx, window.scale_factor());
        let pictures = Pictures::global(cx);
        let level0 = self.level_zero(cx);
        let ready = self.ready(cx);
        let failed = self.failed(cx);
        let slow = session.slow;
        let pannable = overflows(view, nat, self.stage_size());
        let origin = session.origin;
        let progress = |start: Instant, length: Duration| ease((now.saturating_duration_since(start).as_secs_f32() / length.as_secs_f32()).min(1.));
        // How much of the stage shows: 0 → 1 entering, 1 → 0 leaving.
        let shown_amount = match phase {
            Phase::Enter(start) => progress(start, FLY),
            Phase::Open => 1.,
            Phase::Exit(start, fly) => 1. - progress(start, if fly.is_some() { FLY } else { Duration::from_millis(200) }),
        };
        let flight: Option<(Bounds<Pixels>, bool, f32)> = match phase {
            Phase::Enter(start) => origin.map(|o| (Bounds::new(point(px(o.left), px(o.top)), size(px(o.width), px(o.height))), o.cover, 1. - progress(start, FLY))),
            Phase::Exit(start, Some(to)) => Some((to, origin.is_some_and(|o| o.cover), progress(start, FLY))),
            _ => None,
        };
        let picture_box = self.picture_bounds(view, nat);
        let wash = stage::wash(&item, &self.session.as_ref().expect("checked").wash, cx);
        if let Some((k, image)) = &wash
            && let Some(s) = &mut self.session
        {
            s.wash = Some((k.clone(), image.clone()));
        }
        self.request_turn(&level0, view.rot, cx);
        let detail_image = self.session.as_ref().and_then(|s| s.detail.as_ref()).map(|d| (d.image.clone(), d.region, d.rot));
        self.request_detail(view, nat, window, cx);
        let theme = cx.theme();
        let card_bg = Hsla { a: 0.9, ..theme.chrome.color_bg_light };
        let (card_border, shadow) = (theme.chrome.color_border, theme.shadow_float.clone());
        let entity = cx.entity();
        let wash_opacity = stage::wash_opacity(cx) * shown_amount;
        let wash_layer = wash.clone().map(|(_, image)| {
            div()
                .absolute()
                .size_full()
                .opacity(wash_opacity)
                .child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            let sz = image.size(0);
                            let inner = (sz.width.0 as u32 - 2, sz.height.0 as u32 - 2);
                            let grow = (f32::from(bounds.size.width) * 0.1, f32::from(bounds.size.height) * 0.1);
                            let over = Bounds::new(
                                bounds.origin - point(px(grow.0), px(grow.1)),
                                size(bounds.size.width + px(grow.0 * 2.), bounds.size.height + px(grow.1 * 2.)),
                            );
                            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                                crate::tile::paint_cover(&image, inner, over, Corners::default(), window);
                            });
                        },
                    )
                    .size_full(),
                )
        });
        let vignette_layer = div().absolute().size_full().opacity(shown_amount).child(
            canvas(|_, _, _| {}, move |bounds, _, window, cx| stage::paint_vignette(bounds, stage_color, 1., window, cx)).size_full(),
        );
        let stage_layer = canvas(
            {
                let entity = entity.clone();
                move |bounds, _, cx| {
                    entity.update(cx, |this, cx| {
                        let Some(s) = &this.session else { return };
                        if s.stage == bounds {
                            return;
                        }
                        // A resize keeps fit at fit and re-clamps anything zoomed.
                        let nat = this.natural(cx);
                        let old_fit = fit_scale(nat, s.view.rot, this.area());
                        let was_fit = (s.view.scale - old_fit).abs() < 1e-4;
                        if let Some(s) = &mut this.session {
                            s.stage = bounds;
                        }
                        let (area, stage) = (this.area(), this.stage_size());
                        if let Some(s) = &mut this.session {
                            let fit = fit_scale(nat, s.view.rot, area);
                            let next = if was_fit { View { scale: fit, x: 0., y: 0., ..s.view } } else { s.view };
                            s.view = clamp_view(next, nat, stage, area);
                        }
                        this.want(cx);
                        cx.notify();
                    });
                }
            },
            move |bounds, _, window, cx| {
                window.paint_quad(gpui::fill(bounds, Hsla { a: stage_color.a * shown_amount, ..stage_color }));
            },
        )
        .absolute()
        .size_full();
        let picture_layer = canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                // The picture: in its box, or flying between the thumbnail and its box.
                let mut target = picture_box;
                let mut clip: Option<Bounds<Pixels>> = None;
                if let Some((thumb, cover, t)) = flight {
                    // `t` = 1 at the thumbnail, 0 at the box.
                    let (tw, th) = (f32::from(thumb.size.width), f32::from(thumb.size.height));
                    let (pw, ph) = (f32::from(picture_box.size.width).max(1.), f32::from(picture_box.size.height).max(1.));
                    let s = if cover { (tw / pw).max(th / ph) } else { (tw / pw).min(th / ph) };
                    let at_thumb = centered(thumb.center(), pw * s, ph * s);
                    target = lerp_bounds(picture_box, at_thumb, t);
                    if cover {
                        clip = Some(lerp_bounds(picture_box, thumb, t));
                    }
                } else if let Phase::Exit(start, None) = phase {
                    // Fades out, shrinking to 96 %.
                    let k = 1. - 0.04 * ease((Instant::now().saturating_duration_since(start).as_secs_f32() / 0.2).min(1.));
                    target = centered(picture_box.center(), f32::from(picture_box.size.width) * k, f32::from(picture_box.size.height) * k);
                }
                let Some(level0) = &level0 else { return };
                let turned = entity.read(cx).session.as_ref().and_then(|s| s.turned.clone());
                let (image, out) = match turned.filter(|(id, rot, _)| *id == level0.image.id.0 as u64 && *rot == view.rot.rem_euclid(360)) {
                    Some((_, _, img)) => {
                        let sz = img.size(0);
                        (img, (sz.width.0 as u32 - 2, sz.height.0 as u32 - 2))
                    }
                    None => (level0.image.clone(), level0.placement.out),
                };
                window.with_content_mask(Some(gpui::ContentMask { bounds: clip.unwrap_or(bounds) }), |window| {
                    window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                        crate::tile::paint_fill(&image, out, target, Corners::default(), window);
                        if let Some((image, region, rot)) = &detail_image
                            && *rot == view.rot
                            && flight.is_none()
                        {
                            let d = detail_bounds(target, nat, view.rot, *region);
                            let sz = image.size(0);
                            crate::tile::paint_fill(image, (sz.width.0 as u32 - 2, sz.height.0 as u32 - 2), d, Corners::default(), window);
                        }
                    });
                });
            },
        )
        .absolute()
        .size_full();
        let picture_layer = div().absolute().size_full().when(matches!(phase, Phase::Exit(_, None)), |d| d.opacity(shown_amount)).child(picture_layer);

        // ── Chrome ──
        let chrome_opacity = if chrome_on { 1. } else { 0. };
        let header = item.as_ref().map(|item| self.header(item, slow && !ready, failed, &style, cx));
        let filmstrip = strip.then(|| self.filmstrip(window, cx));
        let minimap = pannable.then(|| self.minimap(view, nat, cx));
        let theme = cx.theme();
        let tool = |id: &'static str, icon: IconName, title: &'static str, disabled: bool| {
            div()
                .id(id)
                .size_8()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .when(!disabled, |d| d.cursor_pointer().hover(|s| s.bg(theme.chrome.color_surface_hover)))
                .when(disabled, |d| d.opacity(0.35))
                .tooltip(title_tooltip(title))
                .child(Icon::new(icon).size(px(18.)).color(theme.chrome.color_text_secondary))
        };
        let card = |d: gpui::Div| d.rounded(theme.radius_lg).border_1().border_color(card_border).bg(card_bg).shadow(shadow.clone());
        let percent = format!("{}%", (view.scale * 100.).round());
        let can_save = self.target().is_some_and(|t| actions::can_save(&t));
        let toolbar = card(div().flex().items_center().gap(px(2.)).p_1())
            .child(tool("viewer-zoom-out", IconName::ViewerZoomOut, "Zoom out (−)", at_fit).on_click(cx.listener(move |this, _, _, cx| {
                let v = this.current_view(Instant::now());
                let fit = this.fit(cx);
                this.zoom_toward(step_scale(v.scale, fit, -1), (0., 0.), Duration::from_millis(220), cx);
            })))
            .child(
                div()
                    .id("viewer-percent")
                    .h_8()
                    .min_w(px(56.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_md)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.chrome.color_surface_hover))
                    .tooltip(title_tooltip(if at_fit { "Actual size (1)" } else { "Fit to screen (0)" }))
                    .child(label_in(percent, &style.invite_meta, Wrapping::NOWRAP, theme.chrome.color_text_secondary))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_zoom((0., 0.), cx))),
            )
            .child(tool("viewer-zoom-in", IconName::ViewerZoomIn, "Zoom in (+)", view.scale >= MAX_SCALE).on_click(cx.listener(move |this, _, _, cx| {
                let v = this.current_view(Instant::now());
                let fit = this.fit(cx);
                this.zoom_toward(step_scale(v.scale, fit, 1), (0., 0.), Duration::from_millis(220), cx);
            })))
            .child(div().mx_1().h_5().w(px(1.)).bg(card_border))
            .child(tool("viewer-rotate", IconName::ViewerRotate, "Rotate (R)", false).on_click(cx.listener(|this, _, _, cx| this.rotate(cx))))
            .child(div().mx_1().h_5().w(px(1.)).bg(card_border))
            .child(tool("viewer-copy", IconName::ViewerCopy, "Copy image (Ctrl+C)", false).on_click(cx.listener(|this, _, _, cx| {
                if let Some(t) = this.target() {
                    actions::copy_image(t, cx);
                }
            })))
            .child(if can_save {
                tool("viewer-save", IconName::ViewerDownload, "Save (Ctrl+S)", false)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(t) = this.target() {
                            actions::save(&t, decibell_attachments::AttachmentKind::Image, false, cx);
                        }
                    }))
                    .into_any_element()
            } else {
                tool("viewer-open", IconName::ViewerExternal, "Open in browser", false)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(t) = this.target() {
                            actions::open_in_browser(&t, cx);
                        }
                    }))
                    .into_any_element()
            });
        let close_button = card(div().size_10().flex().items_center().justify_center())
            .id("viewer-close")
            .cursor_pointer()
            .tooltip(title_tooltip("Close (Esc)"))
            .child(Icon::new(IconName::ViewerClose).size(px(18.)).color(theme.chrome.color_text_secondary))
            .on_click(cx.listener(|this, _, _, cx| this.close(cx)));
        let stage_bounds = self.session.as_ref().map_or(Bounds::default(), |s| s.stage);
        let center_y = self.area().top + self.area().h / 2.;
        let arrows = strip.then(|| {
            let mut out = Vec::new();
            for (side, d) in [("left", -1i32), ("right", 1)] {
                let target = index as i32 + d;
                if target < 0 || target >= len as i32 {
                    continue;
                }
                let theme = cx.theme();
                let button = card(div().size_11().flex().items_center().justify_center())
                    .id(SharedString::from(format!("viewer-{side}")))
                    .absolute()
                    .top(px(center_y - 22.))
                    .when(side == "left", |b| b.left_4())
                    .when(side == "right", |b| b.right_4())
                    .cursor_pointer()
                    .tooltip(title_tooltip(if side == "left" { "Previous (←)" } else { "Next (→)" }))
                    .child(Icon::new(if side == "left" { IconName::ViewerChevronLeft } else { IconName::ViewerChevronRight }).size(px(20.)).color(theme.chrome.color_text_secondary))
                    .on_click(cx.listener(move |this, _, _, cx| this.go(target as usize, cx)));
                out.push(button);
            }
            out
        });
        let _ = (pictures, stage_bounds, moving);
        div()
            .id("image-viewer")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if let Some(t) = this.target() {
                    match (t.server, t.attachment_id) {
                        (Some(server), Some(id)) => {
                            let a = decibell_model::chat::Attachment {
                                id,
                                filename: t.filename.clone(),
                                mime: t.mime.clone().unwrap_or_default(),
                                ..empty_attachment()
                            };
                            crate::menu::show_for_attachment(server, &a, event.position, decibell_model::chat::viewers::MediaMenuKind::Image, cx);
                        }
                        _ => {
                            if let Some(url) = t.url.clone() {
                                crate::menu::show_for_link(url, t.page_url.clone(), t.filename.clone(), event.position, cx);
                            }
                        }
                    }
                }
            }))
            .child(stage_layer)
            .children(wash_layer)
            .child(vignette_layer)
            .child(picture_layer)
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top_0()
                    .flex()
                    .items_start()
                    .gap_3()
                    .px_4()
                    .pt_3()
                    .pb_6()
                    .opacity(chrome_opacity)
                    .bg(gpui::linear_gradient(180., gpui::linear_color_stop(Hsla { a: 0.8 * stage_color.a, ..stage_color }, 0.), gpui::linear_color_stop(Hsla { a: 0., ..stage_color }, 1.)))
                    .child(div().min_w_0().flex_1().pt(px(2.)).children(header))
                    .child(toolbar)
                    .child(close_button),
            )
            .children(arrows.into_iter().flatten().map(|b| b.opacity(chrome_opacity)))
            .children(filmstrip.map(|f| div().absolute().left_0().right_0().bottom_4().flex().justify_center().px_4().opacity(chrome_opacity).child(f)))
            .children(minimap.map(|m| div().absolute().right_4().when(strip, |d| d.bottom(px(96.))).when(!strip, |d| d.bottom_4()).opacity(chrome_opacity).child(m)))
            .into_any_element()
    }
}

/// The chrome colours and radii the viewer's parts use (a `Copy` snapshot).
#[derive(Clone, Copy)]
struct Ink {
    color_bg_lighter: Hsla,
    color_bg_light: Hsla,
    color_text_secondary: Hsla,
    color_text_primary: Hsla,
    color_border: Hsla,
    radius_md: Pixels,
    radius_lg: Pixels,
}

impl Ink {
    fn of(cx: &App) -> Ink {
        let theme = cx.theme();
        let c = &theme.chrome;
        Ink {
            color_bg_lighter: c.color_bg_lighter,
            color_bg_light: c.color_bg_light,
            color_text_secondary: c.color_text_secondary,
            color_text_primary: c.color_text_primary,
            color_border: c.color_border,
            radius_md: theme.radius_md,
            radius_lg: theme.radius_lg,
        }
    }
}

fn empty_attachment() -> decibell_model::chat::Attachment {
    decibell_model::chat::Attachment {
        id: 0,
        message_id: 0,
        kind: decibell_attachments::AttachmentKind::Image,
        filename: SharedString::default(),
        mime: SharedString::default(),
        size_bytes: 0,
        url: SharedString::default(),
        position: 0,
        created_at: 0,
        purged_at: 0,
        width: 0,
        height: 0,
        thumbnail_size_bytes: 0,
        thumbnail_sizes_mask: 0,
        duration_ms: 0,
        placeholder: SharedString::default(),
        encrypted: false,
        key_b64: None,
        chunk_bytes: 0,
    }
}

fn centered(center: Point<Pixels>, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(point(center.x - px(w / 2.), center.y - px(h / 2.)), size(px(w), px(h)))
}

fn lerp_bounds(a: Bounds<Pixels>, b: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    let l = |x: Pixels, y: Pixels| x + (y - x) * t;
    Bounds::new(point(l(a.left(), b.left()), l(a.top(), b.top())), size(l(a.size.width, b.size.width), l(a.size.height, b.size.height)))
}

/// Where a region (natural px, unrotated) of the picture lands when the picture fills `target`.
fn detail_bounds(target: Bounds<Pixels>, nat: Size, rot: i32, region: [f32; 4]) -> Bounds<Pixels> {
    let d = shown(nat, rot);
    let k = f32::from(target.size.width) / d.w;
    // Rotations map the region's corners; quarter turns swap the axes.
    let [x, y, w, h] = region;
    let (rx, ry, rw, rh) = match rot.rem_euclid(360) {
        90 => (nat.h - (y + h), x, h, w),
        180 => (nat.w - (x + w), nat.h - (y + h), w, h),
        270 => (y, nat.w - (x + w), h, w),
        _ => (x, y, w, h),
    };
    Bounds::new(point(target.left() + px(rx * k), target.top() + px(ry * k)), size(px(rw * k), px(rh * k)))
}

impl ImageViewer {
    /// Who posted it, when, where; then the file (`ViewerHeader`).
    fn header(&self, item: &ViewerImage, loading: bool, failed: bool, style: &Rc<ContentStyle>, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let has_sender = item.sender.is_some();
        let uid = item.sender_uid.unwrap_or(0);
        let name: SharedString = if has_sender {
            display_name(item.server, uid, item.sender.as_ref().unwrap_or(&SharedString::default()), cx)
        } else {
            host_of(item.page_url.as_deref().unwrap_or(&item.url)).into()
        };
        let format = cx.try_global::<Model>().map_or(decibell_model::stores::settings::TimeFormat::Auto, |m| m.settings.read(cx).time_format());
        let channel = item
            .server
            .zip(item.channel.clone())
            .and_then(|(server, channel)| cx.try_global::<Model>().and_then(|m| m.community(server, cx)).and_then(|c| c.channels.read(cx).get(&channel).map(|c| c.name.clone())));
        let theme = Ink::of(cx);
        let dims = (item.width > 0 && item.height > 0).then(|| format!("{} × {}", item.width, item.height));
        let size_text = item.size_bytes.filter(|b| *b > 0).map(crate::attachments::format_file_size);
        let meta = [dims, size_text].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let avatar: gpui::AnyElement = if has_sender {
            UserAvatar::new(uid, item.sender.clone().unwrap_or_default(), 36.).into_any_element()
        } else {
            div()
                .size_9()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .bg(theme.color_bg_lighter)
                .child(Icon::new(IconName::ViewerLink).size(px(16.)).color(theme.color_text_secondary))
                .into_any_element()
        };
        let mut top = div().flex().min_w_0().items_baseline().gap_2().child(div().min_w_0().child(label_in(name, &style.invite_title.color(theme.color_text_primary), Wrapping::TRUNCATE, theme.color_text_primary)));
        if has_sender {
            let mut when = crate::fmt::posted_at(item.timestamp, format);
            if let Some(channel) = channel {
                when.push_str(&format!(" · #{channel}"));
            }
            top = top.child(div().flex_none().child(label(when, &style.invite_meta, Wrapping::NOWRAP)));
        }
        let mut bottom = div().flex().min_w_0().items_center().gap(px(6.)).child(div().min_w_0().child(label_in(item.filename.clone(), &style.invite_meta, Wrapping::TRUNCATE, theme.color_text_secondary)));
        if !meta.is_empty() {
            bottom = bottom.child(div().flex_none().child(label(meta, &style.invite_meta, Wrapping::NOWRAP)));
        }
        if loading && !failed {
            bottom = bottom.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .child(label("·", &style.invite_meta, Wrapping::NOWRAP))
                    .child(Spinner::new().size(px(12.)))
                    .child(label("Loading original", &style.invite_meta, Wrapping::NOWRAP)),
            );
        }
        if failed {
            let error = cx.theme().color_error;
            let accent = cx.theme().color_accent;
            bottom = bottom.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .child(label("·", &style.invite_meta, Wrapping::NOWRAP))
                    .child(label_in("Couldn't load the original", &style.invite_meta, Wrapping::NOWRAP, error))
                    .child(
                        div()
                            .id("viewer-retry")
                            .cursor_pointer()
                            .child(label_in("Retry", &style.invite_meta, Wrapping::NOWRAP, accent))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let key = this.keys(cx).0;
                                let view = cx.entity_id();
                                if let Some(p) = Pictures::global(cx) {
                                    p.update(cx, |p, cx| p.retry(&key, Some(view), cx));
                                }
                            })),
                    ),
            );
        }
        div().flex().min_w_0().items_center().gap_3().child(avatar).child(div().min_w_0().child(top).child(bottom)).into_any_element()
    }

    /// The message's pictures, the current one ringed in the accent (`Filmstrip`).
    fn filmstrip(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(session) = &self.session else { return div().into_any_element() };
        let theme = Ink::of(cx);
        let card_bg = Hsla { a: 0.9, ..theme.color_bg_light };
        let style = ContentStyle::get(cx, window.scale_factor());
        let mut row = div().flex().items_center().gap(px(6.)).p_1();
        let mut wants = Vec::new();
        for (i, item) in session.items.iter().enumerate() {
            let current = i == session.index;
            let source = source_of(item.thumb_url.as_ref().or(item.preview_url.as_ref()).unwrap_or(&item.url));
            let key = PicKey::new(source, Spec { width: 44., height: 44., scale: window.scale_factor(), fit: Fit::Cover, upscale: true });
            wants.push(Want { key: key.clone(), visible: false });
            let tile = crate::tile::Tile {
                key: Some(key),
                placeholder: item.placeholder.clone().unwrap_or_default(),
                corners: Corners::all(theme.radius_md),
                background: Some(theme.color_bg_lighter),
                alt: SharedString::default(),
                veil: None,
            };
            row = row.child(
                div()
                    .id(SharedString::from(format!("viewer-strip-{i}")))
                    .size_11()
                    .flex_none()
                    .rounded(theme.radius_md)
                    .when(current, |d| d.border_2().border_color(cx.theme().color_accent))
                    .when(!current, |d| d.opacity(0.55).hover(|s| s.opacity(1.)))
                    .cursor_pointer()
                    .tooltip(title_tooltip(item.filename.clone()))
                    .child(tile.element())
                    .on_click(cx.listener(move |this, _, _, cx| this.go(i, cx))),
            );
        }
        if let Some(pictures) = Pictures::global(cx) {
            let owner = (cx.entity_id(), 10);
            cx.defer(move |cx| pictures.update(cx, |p, cx| p.set_wants(owner, wants, cx)));
        }
        div()
            .flex()
            .items_center()
            .gap_1()
            .p(px(6.))
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.color_border)
            .bg(card_bg)
            .shadow(cx.theme().shadow_float.clone())
            .child(row)
            .child(div().flex_none().px_2().child(label(format!("{} / {}", session.index + 1, session.items.len()), &style.invite_meta, Wrapping::NOWRAP)))
            .into_any_element()
    }

    /// Where you are in a zoomed picture; drag or click in it to move (`Minimap`).
    fn minimap(&self, view: View, nat: Size, cx: &mut Context<Self>) -> gpui::AnyElement {
        let d = shown(nat, view.rot);
        let k = (176. / d.w).min(124. / d.h);
        let (w, h) = ((d.w * k).round(), (d.h * k).round());
        let stage = self.stage_size();
        let area = self.area();
        let (dw, dh) = (d.w * view.scale, d.h * view.scale);
        let left = area.left + area.w / 2. + view.x - dw / 2.;
        let top = area.top + area.h / 2. + view.y - dh / 2.;
        let frac = [(-left / dw).max(0.), (-top / dh).max(0.), ((stage.w - left) / dw).min(1.), ((stage.h - top) / dh).min(1.)];
        let level0 = self.level_zero(cx);
        let theme = Ink::of(cx);
        let accent = cx.theme().color_accent;
        let card_bg = Hsla { a: 0.9, ..theme.color_bg_light };
        let mover = cx.listener(move |this: &mut Self, event: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            this.minimap_move(event.position, (w, h), cx);
        });
        div()
            .p(px(6.))
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.color_border)
            .bg(card_bg)
            .child(
                div()
                    .id("viewer-minimap")
                    .relative()
                    .w(px(w))
                    .h(px(h))
                    .cursor_grab()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, cx| {
                                cx.set_global(MinimapAt(bounds.origin));
                                if let Some(p) = &level0 {
                                    let placement = crate::pictures::decode::Placement { x: 0., y: 0., width: w, height: h, crop: (0, 0, p.placement.out.0, p.placement.out.1), out: p.placement.out };
                                    let _ = window.paint_image(placement.bounds(bounds.origin), placement.image_bounds(bounds.origin), Corners::all(px(2.)), p.image.clone(), 0, false);
                                }
                                let r = Bounds::new(bounds.origin + point(px(frac[0] * w), px(frac[1] * h)), size(px((frac[2] - frac[0]) * w), px((frac[3] - frac[1]) * h)));
                                // Everything outside the visible part dims (`0 0 0 999px rgb(0 0 0 / .45)`).
                                let dim = Hsla { h: 0., s: 0., l: 0., a: 0.45 };
                                for b in [
                                    Bounds::from_corners(bounds.origin, point(bounds.right(), r.top())),
                                    Bounds::from_corners(point(bounds.left(), r.bottom()), bounds.bottom_right()),
                                    Bounds::from_corners(point(bounds.left(), r.top()), point(r.left(), r.bottom())),
                                    Bounds::from_corners(point(r.right(), r.top()), point(bounds.right(), r.bottom())),
                                ] {
                                    window.paint_quad(gpui::fill(b, dim));
                                }
                                window.paint_quad(gpui::quad(r, Corners::all(px(2.)), gpui::transparent_black(), gpui::Edges::all(px(2.)), accent, gpui::BorderStyle::default()));
                            },
                        )
                        .size_full(),
                    )
                    .on_mouse_down(MouseButton::Left, mover),
            )
            .into_any_element()
    }

    /// A press in the minimap: that point of the picture comes to the stage's centre (`moveTo`).
    fn minimap_move(&mut self, position: Point<Pixels>, (w, h): (f32, f32), cx: &mut Context<Self>) {
        let Some(origin) = cx.try_global::<MinimapAt>().map(|m| m.0) else { return };
        let (u, v) = (f32::from(position.x - origin.x) / w.max(1.), f32::from(position.y - origin.y) / h.max(1.));
        let nat = self.natural(cx);
        let view = self.current_view(Instant::now());
        let d = shown(nat, view.rot);
        let (dw, dh) = (d.w * view.scale, d.h * view.scale);
        let (stage, area) = (self.stage_size(), self.area());
        let (l, t) = (stage.w / 2. - u * dw, stage.h / 2. - v * dh);
        let next = View { x: l + dw / 2. - (area.left + area.w / 2.), y: t + dh / 2. - (area.top + area.h / 2.), ..view };
        self.apply(next, Duration::ZERO, cx);
    }

    /// A quarter turn: the fit decode turned in the background (its border turns with it).
    fn request_turn(&mut self, level0: &Option<crate::pictures::Picture>, rot: i32, cx: &mut Context<Self>) {
        let rot = rot.rem_euclid(360);
        let Some(level0) = level0 else { return };
        let Some(session) = &mut self.session else { return };
        let id = level0.image.id.0 as u64;
        if rot == 0 || session.turned.as_ref().is_some_and(|(i, r, _)| *i == id && *r == rot) || session.turn_job.is_some() {
            return;
        }
        let image = level0.image.clone();
        let turn = cx.background_spawn(async move {
            let sz = image.size(0);
            let raw = image.as_bytes(0).map(|b| b.to_vec()).unwrap_or_default();
            let buffer: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> = image::ImageBuffer::from_raw(sz.width.0 as u32, sz.height.0 as u32, raw)?;
            let turned = match rot {
                90 => image::imageops::rotate90(&buffer),
                180 => image::imageops::rotate180(&buffer),
                _ => image::imageops::rotate270(&buffer),
            };
            Some(RenderImage::new(vec![image::Frame::new(turned)]))
        });
        session.turn_job = Some(cx.spawn(async move |this, cx| {
            let turned = turn.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(s) = &mut this.session {
                    s.turn_job = None;
                    if let Some(turned) = turned {
                        if let Some((_, _, old)) = s.turned.replace((id, rot, Arc::new(turned))) {
                            cx.defer(move |cx| cx.drop_image(old, None));
                        }
                        cx.notify();
                    }
                }
            });
        }));
    }

    /// Starts (or restarts) the detail render for `view` when zoomed past the fit decode.
    fn request_detail(&mut self, view: View, nat: Size, window: &mut Window, cx: &mut Context<Self>) {
        let Some(level0) = self.level_zero(cx) else { return };
        let scale = window.scale_factor();
        let item_url = self.item().map(|i| i.url.clone()).unwrap_or_default();
        let picture = self.picture_bounds(view, nat);
        let Some(session) = &mut self.session else { return };
        // The fit decode has `out / natural` device px per natural px; past that, detail.
        let level0_density = level0.placement.out.0 as f32 / nat.w.max(1.);
        if view.scale * scale <= level0_density * 1.05 || session.tween.is_some() {
            if view.scale * scale <= level0_density * 1.05 {
                session.full = None;
                session.detail = None;
            }
            return;
        }
        if session.detail_job.is_some() {
            session.detail_wanted = Some((view, picture));
            return;
        }
        if session.detail.as_ref().is_some_and(|d| d.rot == view.rot) && session.detail_wanted.is_none() && session.full.is_some() {
            // Up to date unless the view moved since: compare the region.
        }
        let stage = session.stage;
        let rot = view.rot;
        let full = session.full.clone().filter(|(k, _)| *k == item_url);
        let Some(pictures) = Pictures::global(cx) else { return };
        let bytes = if full.is_none() { Some(pictures.update(cx, |p, cx| p.fetch_now(&source_of(&item_url), cx))) } else { None };
        // The visible part of the picture in natural px (unrotated).
        let vis = picture.intersect(&stage);
        if vis.size.width <= px(0.) || vis.size.height <= px(0.) {
            return;
        }
        let k = nat_per_px(picture, nat, rot);
        let region = unrotate([
            f32::from(vis.left() - picture.left()) * k,
            f32::from(vis.top() - picture.top()) * k,
            f32::from(vis.size.width) * k,
            f32::from(vis.size.height) * k,
        ], nat, rot);
        let out = ((f32::from(vis.size.width) * scale).round().max(1.) as u32, (f32::from(vis.size.height) * scale).round().max(1.) as u32);
        if session.detail.as_ref().is_some_and(|d| {
            d.rot == rot && d.out == out && d.region.iter().zip(region.iter()).all(|(a, b)| (a - b).abs() < 0.5)
        }) {
            return;
        }
        let nearest = view.scale >= 3.;
        let url = item_url.clone();
        session.detail_job = Some(cx.spawn(async move |this, cx| {
            let image = match full {
                Some((_, img)) => Ok(img),
                None => match bytes.expect("fetched when not kept").await {
                    Ok(bytes) => cx
                        .background_spawn(async move { decibell_attachments::thumbs::decode_image_bytes(&bytes).map(Arc::new).map_err(|e| e.to_string()) })
                        .await
                        .map_err(SharedString::from),
                    Err(e) => Err(e),
                },
            };
            let Ok(img) = image else {
                let _ = this.update(cx, |this, _| {
                    if let Some(s) = &mut this.session {
                        s.detail_job = None;
                    }
                });
                return;
            };
            let job_img = img.clone();
            let rendered = cx.background_spawn(async move { render_detail(&job_img, region, out, rot, nearest) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(s) = &mut this.session {
                    s.full = Some((url.clone(), img));
                    s.detail_job = None;
                    if let Some(old) = s.detail.take() {
                        let old = old.image;
                        cx.defer(move |cx| cx.drop_image(old, None));
                    }
                    s.detail = Some(Detail { image: Arc::new(rendered), out, region, rot });
                    s.detail_wanted = None;
                    cx.notify();
                }
            });
        }));
    }
}

/// Natural px per window px of the displayed picture.
fn nat_per_px(picture: Bounds<Pixels>, nat: Size, rot: i32) -> f32 {
    shown(nat, rot).w / f32::from(picture.size.width).max(0.001)
}

/// A region in displayed (rotated) natural coordinates → the file's own (unrotated) ones.
fn unrotate(r: [f32; 4], nat: Size, rot: i32) -> [f32; 4] {
    let [x, y, w, h] = r;
    match rot.rem_euclid(360) {
        90 => [y, nat.h - (x + w), h, w],
        180 => [nat.w - (x + w), nat.h - (y + h), w, h],
        270 => [nat.w - (y + h), x, h, w],
        _ => [x, y, w, h],
    }
}

/// The part `region` of `img` at `out` device px (turned by `rot`), nearest-neighbour when asked.
fn render_detail(img: &image::DynamicImage, region: [f32; 4], out: (u32, u32), rot: i32, nearest: bool) -> RenderImage {
    let (iw, ih) = (img.width(), img.height());
    let x = (region[0].max(0.) as u32).min(iw.saturating_sub(1));
    let y = (region[1].max(0.) as u32).min(ih.saturating_sub(1));
    let w = (region[2].ceil() as u32).clamp(1, iw - x);
    let h = (region[3].ceil() as u32).clamp(1, ih - y);
    let crop = img.crop_imm(x, y, w, h);
    let (ow, oh) = if rot.rem_euclid(180) == 90 { (out.1, out.0) } else { out };
    let filter = if nearest { image::imageops::FilterType::Nearest } else { image::imageops::FilterType::Triangle };
    let mut rgba = crop.resize_exact(ow.max(1), oh.max(1), filter).into_rgba8();
    rgba = match rot.rem_euclid(360) {
        90 => image::imageops::rotate90(&rgba),
        180 => image::imageops::rotate180(&rgba),
        270 => image::imageops::rotate270(&rgba),
        _ => rgba,
    };
    crate::pictures::decode::render_image(&rgba)
}

/// The viewer's screens: `image-viewer` (at fit, over the attachments channel) and
/// `image-viewer-zoomed` (at actual size, with the minimap).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return vec![
        decibell_harness::Screen { name: "image-viewer", about: "The image viewer at fit over the attachments channel", build: |window, cx| fixture_viewer(false, window, cx) },
        decibell_harness::Screen { name: "image-viewer-zoomed", about: "The image viewer zoomed to actual size, with the minimap", build: |window, cx| fixture_viewer(true, window, cx) },
    ];
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

#[cfg(feature = "fixtures")]
fn fixture_viewer(zoomed: bool, window: &mut Window, cx: &mut App) -> AnyView {
    let pane: gpui::Entity<crate::screens::ContentPane> = cx.new(|cx| crate::screens::ContentPane::new(crate::fixture::ATTACHMENTS, window, cx));
    let model = Model::global(cx).clone();
    let server = ServerId::from_central(decibell_shell::fixture::DAILY_GAMING);
    // The "Two side by side" message: a gallery of two.
    let slice = model.chat.channels.read(cx).get(server, crate::fixture::ATTACHMENTS);
    if let Some(slice) = slice
        && let Some(message) = slice.read(cx).rows().iter().find(|m| m.attachments.len() == 2).cloned()
    {
        let attachment_id = message.attachments[0].id;
        let list = pane.read(cx).list.entity_id();
        open_attachment(OpenRequest { server, message, attachment_id, bounds: Bounds::default(), cover: true, list }, cx);
        if zoomed {
            shared(cx).initial_scale = Some(1.);
        }
    }
    pane.into()
}
