//! The embeds slot (`LinkEmbeds.tsx`, `InviteEmbed.tsx`): under a message, for its first three
//! links (outside code and math), an invite card for each `decibell://invite/…` link and, with the
//! Privacy toggle "Show link previews" on and the sender allowed to embed (EMBED_LINKS:
//! `Message::suppress_embeds`), the unfurled link: a picture (a GIF link, a pasted image URL) or a
//! site card. A card appears only once its data is in, as in the renderer: no skeleton (one
//! growth when the card lands; the list keeps the row being read in place).
//!
//! **The lone link** (`MessageBubble.tsx`' `textHidden`): a message that is only a media link shows
//! just the picture once the link resolved to an image; the text and the card change height in
//! the same re-measure (`invalidate_slots`), and the list requests a row's unfurl when the row
//! comes within `NEAR_PX` of the viewport, so the change usually lands before the row is seen.
//! Late (a row being read whose link resolves now) it is one change, anchored like any other.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use decibell_chat::{RowSlot, SlotSpace};
use decibell_model::chat::Message;
use decibell_model::chat::link_previews::PreviewEntry;
use decibell_model::stores::invites::InviteEntry;
use decibell_model::{Model, ServerId, actions};
use decibell_richtext::EmbedKind;
use decibell_text::Wrapping;
use decibell_ui::{ActiveTheme as _, identity};
use decibell_unfurl::{LinkPreview, LinkPreviewImage, SitePreview};
use gpui::{AnyElement, App, Bounds, Corners, EntityId, Hsla, MouseButton, Pixels, SharedString, Subscription, Task, Window, div, prelude::*, px};

use crate::pictures::{Fit, Look, PicKey, Pictures, Source, Spec, Want};
use crate::sizing::{self, MediaKind};
use crate::style::{ContentStyle, clamp_anywhere, label, label_in};
use crate::tile::Tile;

/// Card width caps (`CARD_MAX_WIDTH_PX`): a site card, an invite card.
const SITE_MAX: f32 = 520.;
const INVITE_MAX: f32 = 432.;
/// A site card's horizontal chrome: the 4 px edge and `px-3`.
const SITE_CHROME: f32 = 28.;
const THUMB: f32 = 80.;
const BANNER: f32 = 112.;
/// "Joining…" gives up after this without an answer.
const JOIN_TIMEOUT: Duration = Duration::from_secs(15);

/// A message's embed links, parsed once per content.
#[derive(Default)]
struct Links {
    map: HashMap<SharedString, Rc<Vec<(SharedString, EmbedKind)>>>,
    order: VecDeque<SharedString>,
    lone: HashMap<SharedString, Option<SharedString>>,
}

impl Links {
    fn of(&mut self, content: &SharedString) -> (Rc<Vec<(SharedString, EmbedKind)>>, Option<SharedString>) {
        if let Some(found) = self.map.get(content) {
            return (found.clone(), self.lone.get(content).cloned().flatten());
        }
        let document = decibell_richtext::parse(content, &decibell_richtext::ParseOptions::default());
        let links: Vec<(SharedString, EmbedKind)> = document.embeds(content).into_iter().map(|(url, kind)| (SharedString::from(url.to_string()), kind)).collect();
        let lone = document.lone_link(content).map(|url| SharedString::from(url.to_string()));
        let links = Rc::new(links);
        self.map.insert(content.clone(), links.clone());
        self.lone.insert(content.clone(), lone.clone());
        self.order.push_back(content.clone());
        if self.order.len() > 1024
            && let Some(old) = self.order.pop_front()
        {
            self.map.remove(&old);
            self.lone.remove(&old);
        }
        (links, lone)
    }
}

/// An invite card's own state (`joining`, `error`).
#[derive(Default)]
struct Join {
    joining: bool,
    error: Option<SharedString>,
    _timeout: Option<Task<()>>,
}

/// The slot. See the module doc.
pub struct EmbedsSlot {
    links: RefCell<Links>,
    joins: Rc<RefCell<HashMap<SharedString, Join>>>,
    /// Remote pictures whose box waits for their size (no declared dimensions): their arrival
    /// re-measures.
    sized_by_pixels: Rc<RefCell<HashSet<Source>>>,
    /// Community pictures asked of central (once per version).
    asked_pictures: Rc<RefCell<HashSet<(ServerId, SharedString)>>>,
    _subscriptions: Vec<Subscription>,
}

fn previews_on(cx: &App) -> bool {
    cx.try_global::<Model>().is_some_and(|m| m.settings.read(cx).get().link_previews_enabled)
}

fn preview_of(url: &str, cx: &App) -> Option<Arc<LinkPreview>> {
    let model = cx.try_global::<Model>()?;
    match model.chat.link_previews.read(cx).get(url)? {
        PreviewEntry::Done(Some(preview)) => Some(preview.clone()),
        _ => None,
    }
}

impl EmbedsSlot {
    pub fn new(cx: &mut App) -> EmbedsSlot {
        let mut subscriptions = Vec::new();
        let asked_pictures: Rc<RefCell<HashSet<(ServerId, SharedString)>>> = Rc::default();
        let sized_by_pixels: Rc<RefCell<HashSet<Source>>> = Rc::default();
        if let Some(model) = cx.try_global::<Model>().cloned() {
            // A preview landing grows its row; an invite resolving fills its card in; the
            // toggle shows or hides every web card.
            subscriptions.push(cx.subscribe(&model.chat.link_previews, |_, event: &decibell_model::chat::link_previews::PreviewChanged, cx| {
                let _ = event;
                invalidate_soon(cx);
            }));
            subscriptions.push(cx.subscribe(&model.invites, |_, _: &decibell_model::stores::invites::InviteChanged, cx| invalidate_soon(cx)));
            let mut was = model.settings.read(cx).get().link_previews_enabled;
            subscriptions.push(cx.observe(&model.settings, move |settings, cx| {
                let now = settings.read(cx).get().link_previews_enabled;
                if now != was {
                    was = now;
                    invalidate_soon(cx);
                }
            }));
            subscriptions.push(cx.observe(&model.connections, |_, cx| invalidate_soon(cx)));
            // A community picture landing for an invite card's banner: the lists redraw (only
            // then: the servers store changes for much else).
            let asked = asked_pictures.clone();
            let mut had: usize = 0;
            subscriptions.push(cx.observe(&model.servers, move |servers, cx| {
                let have = asked.borrow().iter().filter(|(server, _)| servers.read(cx).picture(*server).is_some()).count();
                if have != had {
                    had = have;
                    invalidate_soon(cx);
                }
            }));
        }
        if let Some(pictures) = Pictures::global(cx) {
            let sized = sized_by_pixels.clone();
            subscriptions.push(cx.subscribe(&pictures, move |_, event: &crate::pictures::Settled, cx| {
                if let Source::Remote(_) = &event.source
                    && (!event.ok || sized.borrow().contains(&event.source))
                {
                    invalidate_soon(cx);
                }
            }));
        }
        EmbedsSlot { links: RefCell::default(), joins: Rc::default(), sized_by_pixels, asked_pictures, _subscriptions: subscriptions }
    }

    /// The lone-link rule (installed as `ChatSlots::text_hidden`).
    pub fn text_hidden(&self, message: &Message, cx: &App) -> bool {
        if message.suppress_embeds || !previews_on(cx) {
            return false;
        }
        let (_, lone) = self.links.borrow_mut().of(&message.content);
        lone.is_some_and(|url| preview_of(&url, cx).is_some_and(|p| matches!(*p, LinkPreview::Image(_))))
    }

    fn cards(&self, message: &Message, cx: &App) -> Vec<Card> {
        if message.content.is_empty() || message.decrypt_error.is_some() {
            return Vec::new();
        }
        let (links, _) = self.links.borrow_mut().of(&message.content);
        let web = previews_on(cx) && !message.suppress_embeds;
        let mut out = Vec::new();
        for (url, kind) in links.iter() {
            match kind {
                EmbedKind::Invite => out.push(Card::Invite(url.clone())),
                EmbedKind::Web if web => {
                    if let Some(preview) = preview_of(url, cx) {
                        out.push(Card::Web(url.clone(), preview));
                    }
                }
                EmbedKind::Web => {}
            }
        }
        out
    }
}

/// Re-measures the lists once per turn however many answers arrived in it.
fn invalidate_soon(cx: &mut App) {
    struct Pending;
    impl gpui::Global for Pending {}
    if cx.has_global::<Pending>() {
        return;
    }
    cx.set_global(Pending);
    cx.defer(|cx| {
        cx.remove_global::<Pending>();
        decibell_chat::invalidate_slots(cx);
    });
}

enum Card {
    Invite(SharedString),
    Web(SharedString, Arc<LinkPreview>),
}

/// How a link picture is boxed: with its declared or probed size, the attachment rules without the
/// button's room; without one, at its own size under the same caps once its pixels say it.
#[derive(Clone, Copy, Debug)]
struct ImageBox {
    /// The frame (border-box) when the size is known.
    known: Option<(f32, f32)>,
    /// The caps for an unknown size.
    caps: (f32, f32),
}

fn image_box(image_w: u32, image_h: u32, gif: bool, space: SlotSpace, column: Option<f32>) -> ImageBox {
    let kind = if gif { MediaKind::Gif } else { MediaKind::Image };
    let (w, h, known) = sizing::reserve_box_for(image_w, image_h, space, kind, 0.);
    let caps = if space.view_width > 0. {
        (sizing::max_image_width(space.view_width, kind, 0.), sizing::max_image_height(space.view_height, kind))
    } else if gif {
        (sizing::GIF_MAX_W, sizing::GIF_MAX_H)
    } else {
        (sizing::IMAGE_MAX_W, sizing::IMAGE_MAX_H)
    };
    if !known {
        return ImageBox { known: None, caps };
    }
    let (mut w, mut h) = (w, h);
    // In a card: no wider than the card's column (`innerMax`).
    if column.is_some() {
        let inner = SITE_MAX - SITE_CHROME;
        if w > inner {
            h = sizing::js_round(h * inner / w);
            w = inner;
        }
    }
    ImageBox { known: Some((w, h)), caps }
}

/// The picture key of a link image drawn in `inner` (the frame's content box).
fn link_key(url: &str, inner: (f32, f32), scale: f32) -> PicKey {
    PicKey::new(Source::Remote(url.to_string().into()), Spec { width: inner.0, height: inner.1, scale, fit: Fit::Contain, upscale: false })
}

/// The frame of an image embed: (frame width, frame height, key, failed). `standalone` frames have
/// a 1 px border; the ones in a card have none and are capped to `column`.
fn image_frame(image: &LinkPreviewImage, gif: bool, standalone: bool, column: Option<f32>, space: SlotSpace, cx: &App) -> Option<(f32, f32, PicKey)> {
    let border = if standalone { 2. } else { 0. };
    let pictures = Pictures::global(cx);
    let b = image_box(image.width, image.height, gif, space, column);
    match b.known {
        Some((w, h)) => {
            // `max-w-full`: a card's frame is no wider than its column (the height stays).
            let w = column.map_or(w, |c| w.min(c));
            let key = link_key(&image.url, (w - border, h - border), space.scale);
            if pictures.is_some_and(|p| matches!(p.read(cx).look(&key), Look::Failed(_))) {
                return None;
            }
            Some((w, h, key))
        }
        None => {
            // Unknown size: the picture at its own size under the caps; the frame is that plus
            // its border, and empty until the pixels say how big it is.
            let caps = (column.map_or(b.caps.0, |c| c.min(b.caps.0)), b.caps.1);
            let key = link_key(&image.url, caps, space.scale);
            let look = pictures.map(|p| p.read(cx).look(&key));
            match look {
                Some(Look::Failed(_)) => None,
                Some(Look::Ready(picture)) => Some((picture.placement.width + border, picture.placement.height + border, key)),
                _ => Some((border, border, key)),
            }
        }
    }
}

struct SiteGeometry {
    width: f32,
    column: f32,
    height: f32,
    thumb: Option<PicKey>,
    large: Option<(f32, f32, PicKey)>,
}

fn site_geometry(site: &SitePreview, space: SlotSpace, style: &ContentStyle, cx: &App) -> SiteGeometry {
    let width = space.width.min(SITE_MAX);
    let pictures = Pictures::global(cx);
    let image = site.image.as_ref().filter(|image| {
        // A picture that failed is dropped (`imageFailed`): the card lays out without it.
        let probe = if site.large_image {
            image_frame(image, false, false, Some(width - SITE_CHROME), space, cx).is_none()
        } else {
            let key = PicKey::new(Source::Remote(image.url.clone().into()), Spec { width: THUMB, height: THUMB, scale: space.scale, fit: Fit::Cover, upscale: true });
            pictures.as_ref().is_some_and(|p| matches!(p.read(cx).look(&key), Look::Failed(_)))
        };
        !probe
    });
    let thumb = image.filter(|_| !site.large_image).map(|image| {
        PicKey::new(Source::Remote(image.url.clone().into()), Spec { width: THUMB, height: THUMB, scale: space.scale, fit: Fit::Cover, upscale: true })
    });
    let column = width - 4. - 24. - if thumb.is_some() { THUMB + 12. } else { 0. };
    let large = image.filter(|_| site.large_image).and_then(|image| image_frame(image, false, false, Some(column), space, cx));
    let mut h = 10.;
    if let Some(name) = site.site_name.as_deref().filter(|s| !s.is_empty()) {
        h += style.height(name, &style.site_name, Wrapping::TRUNCATE, column);
    }
    if let Some(title) = site.title.as_deref().filter(|s| !s.is_empty()) {
        h += 2. + style.height(title, &style.site_title, clamp_anywhere(2), column);
    }
    if let Some(description) = site.description.as_deref().filter(|s| !s.is_empty()) {
        h += 4. + style.height(description, &style.site_description, clamp_anywhere(3), column);
    }
    if let Some((_, frame_h, _)) = &large {
        h += 8. + frame_h;
    }
    h += 10.;
    if thumb.is_some() {
        h = h.max(THUMB + 20.);
    }
    SiteGeometry { width, column, height: h, thumb, large }
}

/// What an invite card shows (`InviteEmbed`'s derived values).
struct InviteView {
    code: SharedString,
    host: SharedString,
    port: u16,
    server_id: Option<ServerId>,
    entry: Option<InviteEntry>,
    invalid: bool,
    title: SharedString,
    description: SharedString,
    members: Option<i32>,
    joined: bool,
    label: SharedString,
    error: Option<SharedString>,
    joining: bool,
    picture: Option<SharedString>,
}

fn invite_view(url: &str, sender_uid: i64, joins: &HashMap<SharedString, Join>, cx: &App) -> Option<InviteView> {
    let parsed = decibell_richtext::links::parse_invite_link(url)?;
    let code: SharedString = parsed.code.to_uppercase().into();
    let model = cx.try_global::<Model>()?;
    let entry = model.invites.read(cx).get(&code).cloned();
    let resolved = entry.as_ref().and_then(|e| e.resolved.clone());
    let invalid = entry.as_ref().is_some_and(|e| !e.loading && e.invalid);
    let (host, port): (SharedString, u16) = match (parsed.endpoint, &resolved) {
        (Some((host, port)), _) => (host.to_string().into(), port),
        (None, Some(r)) => (r.host.clone(), r.port),
        _ => (SharedString::default(), 0),
    };
    let host_key = (!host.is_empty() && port != 0).then(|| format!("{host}:{port}"));
    let server_id = resolved.as_ref().filter(|r| r.server_id > 0).map(|r| ServerId::from_central(r.server_id as i32));
    let connections = model.connections.read(cx);
    let joined = server_id.is_some_and(|s| connections.is_connected(s)) || host_key.as_ref().is_some_and(|k| connections.is_connected(ServerId::from_wire(k)));
    let name = resolved.as_ref().map(|r| r.server_name.clone()).unwrap_or_default();
    let resolving = entry.as_ref().is_none_or(|e| e.loading);
    let title: SharedString = if invalid {
        "Invalid invite".into()
    } else if !name.is_empty() {
        name.clone()
    } else if let Some(k) = &host_key {
        k.clone().into()
    } else if resolving {
        "Resolving invite…".into()
    } else {
        "Couldn't resolve invite".into()
    };
    let resolve_error = (host_key.is_none() && entry.as_ref().is_some_and(|e| !e.loading && e.resolved.is_none() && !e.invalid))
        .then(|| entry.as_ref().and_then(|e| e.error.clone()))
        .flatten();
    let description = if invalid {
        SharedString::new_static("This invite is unknown, expired, or used up.")
    } else {
        resolved.as_ref().map(|r| r.server_description.clone()).unwrap_or_default()
    };
    let members = resolved.as_ref().map(|r| r.member_count).filter(|n| *n > 0);
    let me = model.auth.read(cx).uid;
    let label = if me != 0 && sender_uid == me { "You sent an invite to join a server" } else { "You've been invited to join a server" };
    let join = joins.get(&code);
    let picture = server_id.filter(|_| !invalid).and_then(|s| model.servers.read(cx).picture(s).cloned());
    Some(InviteView {
        code,
        host,
        port,
        server_id,
        entry,
        invalid,
        title,
        description,
        members,
        joined,
        label: SharedString::new_static(label),
        error: join.and_then(|j| j.error.clone()).or(resolve_error),
        joining: join.is_some_and(|j| j.joining),
        picture,
    })
}

fn invite_height(view: &InviteView, width: f32, style: &ContentStyle) -> f32 {
    let inner = width - 2. - 32.;
    let label_h = style.height(&view.label.to_uppercase(), &style.invite_label, Wrapping::NORMAL, inner);
    // The column beside the button: what's left of the row after `gap-3` and the button.
    let button_w = if view.joined { 2. + 32. + 36. } else { 32. + 30. };
    let column = (inner - 12. - button_w).max(1.);
    let mut col = style.line(&style.invite_title);
    if !view.description.is_empty() {
        col += style.height(&view.description, &style.invite_description, clamp_anywhere(2), column);
    }
    if !view.invalid && (view.members.is_some() || view.joined) {
        col += 2. + style.line(&style.invite_meta);
    }
    let button = 16. + style.line(&style.button);
    let mut h = 2. + BANNER + 16. + label_h + 4. + col.max(button) + 16.;
    if let Some(error) = &view.error {
        h += 4. + style.height(error, &style.invite_meta, Wrapping::NORMAL, inner);
    }
    h
}

impl RowSlot for EmbedsSlot {
    fn height(&self, message: &Message, _: Option<ServerId>, space: SlotSpace, cx: &App) -> f32 {
        let cards = self.cards(message, cx);
        if cards.is_empty() {
            return 0.;
        }
        let style = ContentStyle::get(cx, space.scale);
        let joins = self.joins.borrow();
        let mut h = 0.;
        for card in &cards {
            match card {
                Card::Invite(url) => {
                    if let Some(view) = invite_view(url, message.sender_uid, &joins, cx) {
                        h += 4. + invite_height(&view, space.width.min(INVITE_MAX), &style);
                    }
                }
                Card::Web(_, preview) => match &**preview {
                    LinkPreview::Image(image) => {
                        let gif = image.gif || decibell_richtext::links::is_gif_url(&image.url);
                        let as_image = LinkPreviewImage { url: image.url.clone(), width: image.width, height: image.height };
                        if let Some((_, frame_h, _)) = image_frame(&as_image, gif, true, None, space, cx) {
                            h += 4. + frame_h;
                        }
                    }
                    LinkPreview::Site(site) => h += 4. + site_geometry(site, space, &style, cx).height,
                },
            }
        }
        h
    }

    fn element(&self, message: &Arc<Message>, server: Option<ServerId>, space: SlotSpace, window: &mut Window, cx: &mut App) -> AnyElement {
        let cards = self.cards(message, cx);
        let style = ContentStyle::get(cx, space.scale);
        let list = window.current_view();
        let mut root = div().w(px(space.width)).flex().flex_col();
        for card in cards {
            let child = match card {
                Card::Invite(url) => self.invite_card(&url, message, space, &style, cx),
                Card::Web(url, preview) => match &*preview {
                    LinkPreview::Image(image) => {
                        let gif = image.gif || decibell_richtext::links::is_gif_url(&image.url);
                        let as_image = LinkPreviewImage { url: image.url.clone(), width: image.width, height: image.height };
                        self.image_embed(&as_image, gif, true, None, &url, message, server, space, list, cx)
                    }
                    LinkPreview::Site(site) => self.site_card(site, message, server, space, list, &style, cx),
                },
            };
            if let Some(child) = child {
                root = root.child(child);
            }
        }
        root.into_any_element()
    }

    fn near(&self, list: EntityId, _: Option<ServerId>, space: SlotSpace, near: &[Arc<Message>], visible: std::ops::Range<usize>, cx: &mut App) {
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        let web = previews_on(cx);
        let mut wants = Vec::new();
        let mut sized = Vec::new();
        for (i, message) in near.iter().enumerate() {
            if message.content.is_empty() || message.decrypt_error.is_some() {
                continue;
            }
            let (links, _) = self.links.borrow_mut().of(&message.content);
            for (url, kind) in links.iter() {
                match kind {
                    EmbedKind::Invite => {
                        if let Some(parsed) = decibell_richtext::links::parse_invite_link(url) {
                            let code = parsed.code.to_string();
                            model.invites.update(cx, |invites, cx| invites.request(&code, cx));
                            // The community's picture for the banner (`useFetchServerPictureIfMissing`):
                            // its version registered, the bytes asked of central once.
                            let resolved = model.invites.read(cx).get(&code).and_then(|e| e.resolved.clone());
                            if let Some(r) = resolved.filter(|r| r.server_id > 0 && !r.picture_version.is_empty()) {
                                let server = ServerId::from_central(r.server_id as i32);
                                let missing = model.servers.read(cx).picture(server).is_none();
                                let version = r.picture_version.clone();
                                model.servers.update(cx, |s, cx| s.set_picture_version(server, &version, cx));
                                if missing && self.asked_pictures.borrow_mut().insert((server, version.clone())) {
                                    actions::servers::fetch_server_picture(server, cx);
                                }
                            }
                        }
                    }
                    EmbedKind::Web if web && !message.suppress_embeds => {
                        model.chat.link_previews.update(cx, |memo, cx| memo.request(url.clone(), cx));
                    }
                    EmbedKind::Web => {}
                }
            }
            // The pictures of the cards that are in.
            for card in self.cards(message, cx) {
                let Card::Web(_, preview) = card else { continue };
                let keys: Vec<(PicKey, bool)> = match &*preview {
                    LinkPreview::Image(image) => {
                        let gif = image.gif || decibell_richtext::links::is_gif_url(&image.url);
                        let as_image = LinkPreviewImage { url: image.url.clone(), width: image.width, height: image.height };
                        image_frame(&as_image, gif, true, None, space, cx).map(|(_, _, key)| (key, as_image.width == 0 || as_image.height == 0)).into_iter().collect()
                    }
                    LinkPreview::Site(site) => {
                        let style = ContentStyle::get(cx, space.scale);
                        let g = site_geometry(site, space, &style, cx);
                        let unknown = site.image.as_ref().is_some_and(|i| i.width == 0 || i.height == 0);
                        g.thumb.map(|k| (k, false)).into_iter().chain(g.large.map(|(_, _, k)| (k, unknown))).collect()
                    }
                };
                for (key, unknown) in keys {
                    if unknown {
                        sized.push(key.source.clone());
                    }
                    wants.push(Want { key, visible: visible.contains(&i) });
                }
            }
        }
        self.sized_by_pixels.borrow_mut().extend(sized);
        if let Some(pictures) = Pictures::global(cx) {
            pictures.update(cx, |p, cx| p.set_wants((list, 2), wants, cx));
        }
    }
}

impl EmbedsSlot {
    /// `ImageEmbed`: a link picture in its frame; a click opens the viewer, a right click the menu.
    #[allow(clippy::too_many_arguments)]
    fn image_embed(
        &self,
        image: &LinkPreviewImage,
        gif: bool,
        standalone: bool,
        column: Option<f32>,
        page_url: &SharedString,
        message: &Arc<Message>,
        server: Option<ServerId>,
        space: SlotSpace,
        list: EntityId,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let (w, h, key) = image_frame(image, gif, standalone, column, space, cx)?;
        let theme = cx.theme();
        let (radius, border) = if standalone { (theme.radius_lg, 1.) } else { (theme.radius_md, 0.) };
        let inner_radius = radius - px(border);
        let tile = Tile { key: Some(key), placeholder: SharedString::default(), corners: Corners::all(inner_radius), background: None, alt: SharedString::default(), veil: None };
        let filename = filename_of(&image.url);
        let viewer_key: SharedString = format!("u:{}:{}", message.id.max(0), image.url).into();
        let hidden = crate::viewer::hides_key(&viewer_key, cx);
        let bounds: Rc<std::cell::Cell<Bounds<Pixels>>> = Rc::default();
        let painted = bounds.clone();
        let target_key = viewer_key.clone();
        let canvas = gpui::canvas(
            |_, _, _| {},
            move |b, _, window, cx| {
                painted.set(b);
                crate::viewer::target_drawn(target_key.clone(), b, cx);
                if !hidden {
                    tile.paint(b, window, cx);
                }
            },
        )
        .size_full();
        let (url, page, msg) = (SharedString::from(image.url.clone()), page_url.clone(), message.clone());
        let (open_url, open_page, menu_name) = (url.clone(), page.clone(), filename.clone());
        let size = (image.width, image.height);
        Some(
            div()
                .id(viewer_key.clone())
                .mt(px(if standalone { 4. } else { 8. }))
                .w(px(w))
                .h(px(h))
                .flex_none()
                .rounded(radius)
                .when(standalone, |d| d.border_1().border_color(theme.color_border))
                .bg(theme.color_bg_secondary)
                .cursor_pointer()
                .when(hidden, |d| d.opacity(0.))
                .child(canvas)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    crate::viewer::open_link(
                        crate::viewer::LinkOpen {
                            key: viewer_key.clone(),
                            url: open_url.clone(),
                            page_url: open_page.clone(),
                            filename: filename.clone(),
                            size,
                            server,
                            message: msg.clone(),
                            bounds: bounds.get(),
                            list,
                        },
                        cx,
                    );
                })
                .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                    cx.stop_propagation();
                    crate::menu::show_for_link(url.clone(), Some(page.clone()), menu_name.clone(), event.position, cx);
                })
                .into_any_element(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn site_card(
        &self,
        site: &SitePreview,
        message: &Arc<Message>,
        server: Option<ServerId>,
        space: SlotSpace,
        list: EntityId,
        style: &Rc<ContentStyle>,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let g = site_geometry(site, space, style, cx);
        let theme = cx.theme();
        let edge = site.color.as_deref().and_then(parse_hex).unwrap_or(theme.color_accent);
        let (bg_dark, bg_secondary, radius_md, radius_sm, accent) = (theme.color_bg_dark, theme.color_bg_secondary, theme.radius_md, theme.radius_sm, theme.color_accent);
        let mut column = div().min_w_0().flex_1().px_3().py(px(10.));
        // Each text in a box of exactly the height the geometry counted (a label rounds its own
        // up to whole pixels, which would push what follows down by the fractions).
        let exact = |text: &str, text_style: &decibell_text::TextStyle, wrapping: Wrapping| {
            div().h(px(style.height(text, text_style, wrapping, g.column))).child(label(text.to_string(), text_style, wrapping))
        };
        if let Some(name) = site.site_name.as_deref().filter(|s| !s.is_empty()) {
            column = column.child(exact(name, &style.site_name, Wrapping::TRUNCATE));
        }
        if let Some(title) = site.title.as_deref().filter(|s| !s.is_empty()) {
            let url = site.url.clone();
            column = column.child(
                div()
                    .id(SharedString::from(format!("site-title-{}-{}", message.id, site.url)))
                    .mt(px(2.))
                    .cursor_pointer()
                    .child(exact(title, &style.site_title, clamp_anywhere(2)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        if let Some(target) = decibell_unfurl::links::external_link_target(&url) {
                            cx.open_url(&target);
                        }
                    }),
            );
        }
        if let Some(description) = site.description.as_deref().filter(|s| !s.is_empty()) {
            column = column.child(div().mt_1().child(exact(description, &style.site_description, clamp_anywhere(3))));
        }
        if g.large.is_some()
            && let Some(image) = &site.image
            && let Some(embed) = self.image_embed(image, false, false, Some(g.column), &site.url.clone().into(), message, server, space, list, cx)
        {
            column = column.child(embed);
        }
        let mut card = div()
            .mt_1()
            .w(px(g.width))
            .h(px(g.height))
            .flex()
            .rounded(radius_md)
            .border_l_4()
            .border_color(edge)
            .bg(bg_dark)
            .child(column);
        if let Some(key) = g.thumb {
            let tile = Tile { key: Some(key), placeholder: SharedString::default(), corners: Corners::all(radius_sm), background: Some(bg_secondary), alt: SharedString::default(), veil: None };
            card = card.child(div().my(px(10.)).mr_3().flex_none().w(px(THUMB)).h(px(THUMB)).child(tile.element()));
        }
        let _ = accent;
        Some(card.into_any_element())
    }

    fn invite_card(&self, url: &str, message: &Arc<Message>, space: SlotSpace, style: &Rc<ContentStyle>, cx: &mut App) -> Option<AnyElement> {
        let view = invite_view(url, message.sender_uid, &self.joins.borrow(), cx)?;
        let width = space.width.min(INVITE_MAX);
        let height = invite_height(&view, width, style);
        let banner_key = match (&view.picture, view.invalid) {
            (Some(data), false) => crate::pictures::data_url_key(data, width - 2., BANNER, space.scale, cx),
            _ => None,
        };
        let theme = cx.theme();
        let name_for_banner: String = {
            let resolved_name = view.entry.as_ref().and_then(|e| e.resolved.as_ref()).map(|r| r.server_name.to_string()).unwrap_or_default();
            if !resolved_name.is_empty() { resolved_name } else if !view.host.is_empty() { view.host.to_string() } else { view.code.to_string() }
        };
        let banner: AnyElement = match (&view.picture, view.invalid) {
            (Some(_), false) => {
                let tile = Tile { key: banner_key, placeholder: SharedString::default(), corners: Corners { top_left: theme.radius_xl - px(1.), top_right: theme.radius_xl - px(1.), ..Corners::default() }, background: None, alt: SharedString::default(), veil: None };
                div().w_full().h(px(BANNER)).child(tile.element()).into_any_element()
            }
            _ => {
                let gradient_key = {
                    let resolved_name = view.entry.as_ref().and_then(|e| e.resolved.as_ref()).map(|r| r.server_name.to_string()).unwrap_or_default();
                    if resolved_name.is_empty() { view.code.to_string() } else { resolved_name }
                };
                let slot = identity::ramp_index(&gradient_key);
                let color = identity::avatar_color(theme, slot);
                let letter: String = if view.invalid { "?".into() } else { name_for_banner.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default() };
                let letter_style = decibell_text::TextStyle { size: 36., ..style.invite_title }.line_height_px(40.);
                let mut band = div().w_full().h(px(BANNER)).flex().items_center().justify_center().rounded_t(theme.radius_xl - px(1.));
                band = if view.invalid {
                    band.bg(theme.color_bg_secondary).child(label_in(letter, &letter_style, Wrapping::NOWRAP, theme.color_text_muted))
                } else {
                    band.bg(gpui::linear_gradient(135., gpui::linear_color_stop(color, 0.), gpui::linear_color_stop(darker(color), 1.)))
                        .child(label_in(letter, &letter_style, Wrapping::NOWRAP, theme.color_av_fg))
                };
                band.into_any_element()
            }
        };
        let title_color = if view.invalid { theme.color_text_muted } else { theme.color_text_bright };
        // In boxes of the heights `invite_height` counted (labels round theirs up).
        let inner = width - 2. - 32.;
        let column_w = (inner - 12. - if view.joined { 2. + 32. + 36. } else { 32. + 30. }).max(1.);
        let mut column = div()
            .min_w_0()
            .flex_1()
            .child(div().h(px(style.line(&style.invite_title))).child(label_in(view.title.clone(), &style.invite_title, Wrapping::TRUNCATE, title_color)));
        if !view.description.is_empty() {
            let h = style.height(&view.description, &style.invite_description, clamp_anywhere(2), column_w);
            column = column.child(div().h(px(h)).child(label(view.description.clone(), &style.invite_description, clamp_anywhere(2))));
        }
        if !view.invalid && (view.members.is_some() || view.joined) {
            let mut meta = div().mt(px(2.)).flex();
            if let Some(n) = view.members {
                meta = meta.child(label(format!("{} {}", crate::fmt::count(n as i64), if n == 1 { "member" } else { "members" }), &style.invite_meta, Wrapping::NOWRAP));
            }
            if view.joined {
                let text = if view.members.is_some() { "· Joined" } else { "Joined" };
                meta = meta.child(div().when(view.members.is_some(), |d| d.ml_1()).child(label_in(text, &style.invite_meta, Wrapping::NOWRAP, theme.color_success)));
            }
            column = column.child(meta);
        }
        let host_key = (!view.host.is_empty() && view.port != 0).then(|| format!("{}:{}", view.host, view.port));
        let button: AnyElement = if view.joined {
            let open_id = view.server_id.filter(|s| cx.try_global::<Model>().is_some_and(|m| m.connections.read(cx).is_connected(*s)));
            let target = open_id.or_else(|| host_key.as_ref().map(|k| ServerId::from_wire(k)));
            div()
                .id(SharedString::from(format!("invite-open-{}-{}", message.id, view.code)))
                .flex_none()
                .px_4()
                .py_2()
                .rounded(theme.radius_sm)
                .border_1()
                .border_color(theme.color_border)
                .bg(theme.color_bg_primary)
                .hover(|s| s.bg(theme.color_surface_hover))
                .cursor_pointer()
                .child(label_in("Open", &style.button, Wrapping::NOWRAP, theme.color_text_primary))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    if let Some(target) = target {
                        actions::servers::open_server(target, cx);
                    }
                })
                .into_any_element()
        } else {
            let busy = view.joining;
            let disabled = busy || view.invalid || host_key.is_none();
            let (host, port, code) = (view.host.to_string(), view.port, view.code.clone());
            let joins = self.joins.clone();
            // `disabled:opacity-50` fades the button as one layer; GPUI fades each primitive on
            // its own (the label would blend with the card, not the button), so the faded
            // colours are mixed here: each half over the card.
            let card = theme.color_bg_dark;
            let fade = |c: Hsla| if disabled { card.blend(c.opacity(0.5)) } else { c };
            div()
                .id(SharedString::from(format!("invite-join-{}-{}", message.id, view.code)))
                .flex_none()
                .px_4()
                .py_2()
                .rounded(theme.radius_sm)
                .bg(fade(theme.color_accent))
                .when(!disabled, |d| d.hover(|s| s.bg(theme.color_accent_hover)).cursor_pointer())
                .when(disabled, |d| d.cursor_not_allowed())
                .child(label_in(if busy { "Joining…" } else { "Join" }, &style.button, Wrapping::NOWRAP, fade(style.button.color)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    if disabled {
                        return;
                    }
                    join(joins.clone(), code.clone(), host.clone(), port, cx);
                })
                .into_any_element()
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_4()
            .child(label(view.label.to_uppercase(), &style.invite_label, Wrapping::NORMAL))
            .child(div().flex().items_center().justify_between().gap_3().child(column).child(button));
        if let Some(error) = &view.error {
            body = body.child(label_in(error.clone(), &style.invite_meta, Wrapping::NORMAL, theme.color_error));
        }
        Some(
            div()
                .mt_1()
                .w(px(width))
                .h(px(height))
                .rounded(theme.radius_xl)
                .border_1()
                .border_color(theme.color_border)
                .bg(theme.color_bg_dark)
                .child(banner)
                .child(body)
                .into_any_element(),
        )
    }
}

/// Join with the card (`InviteEmbed`'s `join`): the connection opens under `host:port`; joining
/// ends when it connects, on an error, or after 15 s.
fn join(joins: Rc<RefCell<HashMap<SharedString, Join>>>, code: SharedString, host: String, port: u16, cx: &mut App) {
    let timeout = {
        let (joins, code) = (joins.clone(), code.clone());
        cx.spawn(async move |cx| {
            cx.background_executor().timer(JOIN_TIMEOUT).await;
            if let Some(j) = joins.borrow_mut().get_mut(&code) {
                j.joining = false;
            }
            cx.update(decibell_chat::invalidate_slots);
        })
    };
    joins.borrow_mut().insert(code.clone(), Join { joining: true, error: None, _timeout: Some(timeout) });
    decibell_chat::invalidate_slots(cx);
    let done_joins = joins.clone();
    actions::servers::redeem_invite(host, port, code.to_string(), cx, move |result, cx| {
        if let Some(j) = done_joins.borrow_mut().get_mut(&code) {
            match result {
                // Connected: the card shows "Joined" from the connections store.
                Ok(()) => j.joining = false,
                Err(e) => {
                    j.joining = false;
                    j.error = Some(e.plain());
                }
            }
            j._timeout = None;
        }
        decibell_chat::invalidate_slots(cx);
    });
}

/// `color-mix(in srgb, av-N 72%, #000)`.
fn darker(color: Hsla) -> Hsla {
    let mut rgba: gpui::Rgba = color.into();
    rgba.r *= 0.72;
    rgba.g *= 0.72;
    rgba.b *= 0.72;
    rgba.into()
}

/// `#rgb` / `#rrggbb` (the unfurler validates the theme colour).
fn parse_hex(text: &str) -> Option<Hsla> {
    let hex = text.strip_prefix('#')?;
    let value = match hex.len() {
        3 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            (r * 17) << 16 | (g * 17) << 8 | (b * 17)
        }
        6 => u32::from_str_radix(hex, 16).ok()?,
        _ => return None,
    };
    Some(gpui::rgb(value).into())
}

/// `filenameOf`: the URL's last path segment, else its host.
pub fn filename_of(url: &str) -> SharedString {
    let Ok(parsed) = url::Url::parse(url) else { return url.to_string().into() };
    let last = parsed.path_segments().and_then(|mut segments| segments.rfind(|s| !s.is_empty()).map(str::to_string));
    match last {
        Some(segment) => percent_decode(&segment).into(),
        None => parsed.host_str().unwrap_or(url).to_string().into(),
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_come_from_the_path() {
        assert_eq!(&*filename_of("https://media.klipy.com/gifs/abc/hd.gif"), "hd.gif");
        assert_eq!(&*filename_of("https://example.com/a%20b.png?x=1"), "a b.png");
        assert_eq!(&*filename_of("https://example.com/"), "example.com");
        assert_eq!(parse_hex("#fff"), Some(gpui::rgb(0xffffff).into()));
        assert_eq!(parse_hex("#5865f2"), Some(gpui::rgb(0x5865f2).into()));
        assert_eq!(parse_hex("red"), None);
    }
}
