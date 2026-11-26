//! The attachments slot (`AttachmentList.tsx`): pictures and grids with the download button
//! beside them, video and audio cards at rest, documents, purged tombstones.
//!
//! [`layout`] is the one place the geometry is decided (the CSS of `AttachmentList.tsx`, box by
//! box); [`AttachmentSlot::height`] is its height and [`AttachmentSlot::element`] draws exactly
//! its boxes, absolutely positioned, so measuring and drawing can't disagree.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use decibell_attachments::AttachmentKind;
use decibell_chat::{RowSlot, SlotSpace};
use decibell_model::chat::{Attachment, Message};
use decibell_model::{Model, ServerId};
use decibell_ui::{ActiveTheme as _, Icon, IconName, title_tooltip};
use gpui::{AnyElement, App, Bounds, Corners, EntityId, MouseButton, Pixels, SharedString, Task, Window, div, prelude::*, px};

use crate::pictures::{Fit, PicKey, Pictures, Source, Spec, Want};
use crate::sizing::{self, GRID_GAP, GRID_ROW};
use crate::style::{ContentStyle, label};
use crate::tile::Tile;

/// `h-8 w-8` and `gap-1.5`.
const BUTTON: f32 = 32.;
const BUTTON_GAP: f32 = 6.;
/// `mt-1 flex flex-col gap-1` around the list; `mt-2` on a grid / video / audio card.
const LIST_MARGIN: f32 = 4.;
const LIST_GAP: f32 = 4.;
const MEDIA_MARGIN: f32 = 8.;
/// `max-w-[400px]` of the cards.
const CARD_MAX: f32 = 400.;
/// How long the download button shows its tick after a click.
const SAVED: Duration = Duration::from_millis(1600);

/// A grid cell: its box inside the grid's border, and which of the grid's rounded corners it
/// touches (top-left, top-right, bottom-right, bottom-left).
#[derive(Clone, Debug, PartialEq)]
pub struct GridCell {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub index: usize,
    pub corners: [bool; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub enum Media {
    /// A standalone picture: its bordered box.
    Image { x: f32, y: f32, w: f32, h: f32, index: usize },
    /// A standalone video card: the card (`mt-2` excluded) and the poster's height in it.
    Video { y: f32, w: f32, h: f32, poster: f32, index: usize },
    Grid { y: f32, w: f32, h: f32, cells: Vec<GridCell> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    /// The pictures / videos of the message and the download button beside them, in a row of
    /// `row` px at `y`.
    Media { y: f32, row: f32, media: Media, button: (f32, f32), all: Vec<usize> },
    Audio { y: f32, w: f32, h: f32, index: usize },
    Document { y: f32, w: f32, h: f32, index: usize },
    Tombstone { y: f32, w: f32, h: f32, index: usize },
}

/// The list's geometry for a message.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub height: f32,
    pub pieces: Vec<Piece>,
    /// The attachments in `position` order (what the indices point at).
    pub sorted: Vec<Attachment>,
}

/// Lays out `message`'s attachments for `space` (see the module doc).
pub fn layout(message: &Message, space: SlotSpace, style: &ContentStyle) -> Layout {
    if message.attachments.is_empty() {
        return Layout::default();
    }
    let mut sorted: Vec<Attachment> = message.attachments.to_vec();
    sorted.sort_by_key(|a| a.position);
    let gridable: Vec<usize> =
        (0..sorted.len()).filter(|&i| sorted[i].purged_at == 0 && matches!(sorted[i].kind, AttachmentKind::Image | AttachmentKind::Video)).collect();
    let rest: Vec<usize> = (0..sorted.len()).filter(|i| !gridable.contains(i)).collect();
    let mut pieces = Vec::new();
    let mut y = LIST_MARGIN;
    let mut first = true;
    let mut gap = |y: &mut f32| {
        if !std::mem::take(&mut first) {
            *y += LIST_GAP;
        }
    };
    if !gridable.is_empty() {
        gap(&mut y);
        if gridable.len() >= 2 {
            let w = sizing::grid_width(space.view_width);
            let rows = sizing::grid_rows(gridable.len());
            let h = rows.len() as f32 * GRID_ROW + (rows.len() - 1) as f32 * GRID_GAP + 2.;
            let mut cells = Vec::new();
            let (mut cy, mut next) = (0., 0);
            for (r, cols) in rows.iter().enumerate() {
                let cell_w = (w - 2. - (*cols - 1) as f32 * GRID_GAP) / *cols as f32;
                for c in 0..*cols {
                    let last_row = r + 1 == rows.len();
                    cells.push(GridCell {
                        x: c as f32 * (cell_w + GRID_GAP),
                        y: cy,
                        w: cell_w,
                        h: GRID_ROW,
                        index: gridable[next],
                        corners: [r == 0 && c == 0, r == 0 && c + 1 == *cols, last_row && c + 1 == *cols, last_row && c == 0],
                    });
                    next += 1;
                }
                cy += GRID_ROW + GRID_GAP;
            }
            // `flex items-center`: the grid and the button both carry `mt-2`.
            let row = MEDIA_MARGIN + h.max(BUTTON);
            pieces.push(Piece::Media {
                y,
                row,
                media: Media::Grid { y: MEDIA_MARGIN, w, h, cells },
                button: (w + BUTTON_GAP, MEDIA_MARGIN + (h - BUTTON).max(0.) / 2.),
                all: gridable.clone(),
            });
            y += row;
        } else {
            let index = gridable[0];
            let a = &sorted[index];
            let (w, h) = sizing::reserve_box(a.width, a.height, &a.mime, space);
            if a.kind == AttachmentKind::Video {
                // `mt-2`: the poster box, then the bar (`py-2`, 10.5 px text).
                let card = 2. + h + 16. + style.line(&style.meta_105);
                let row = MEDIA_MARGIN + card.max(BUTTON);
                pieces.push(Piece::Media {
                    y,
                    row,
                    media: Media::Video { y: MEDIA_MARGIN + (card.max(BUTTON) - card) / 2., w, h: card, poster: h, index },
                    button: (w + BUTTON_GAP, MEDIA_MARGIN + (card.max(BUTTON) - BUTTON) / 2.),
                    all: vec![index],
                });
                y += row;
            } else {
                let row = h.max(BUTTON);
                pieces.push(Piece::Media {
                    y,
                    row,
                    media: Media::Image { x: 0., y: (row - h) / 2., w, h, index },
                    button: (w + BUTTON_GAP, (row - BUTTON) / 2.),
                    all: vec![index],
                });
                y += row;
            }
        }
    }
    let card_w = space.width.min(CARD_MAX);
    for index in rest {
        gap(&mut y);
        let a = &sorted[index];
        if a.purged_at > 0 {
            // `p-3 border text-[12px]`.
            let h = 2. + 24. + style.line(&style.tombstone);
            pieces.push(Piece::Tombstone { y, w: card_w, h, index });
            y += h;
        } else if a.kind == AttachmentKind::Audio {
            // `mt-2 p-3 gap-2.5`: a 40 px head (or its text, if taller), a 32 px transport row.
            let head = (style.line(&style.audio_name) + style.line(&style.meta_105)).max(40.);
            let h = 2. + 24. + head + 10. + 32.;
            pieces.push(Piece::Audio { y: y + MEDIA_MARGIN, w: card_w, h, index });
            y += MEDIA_MARGIN + h;
        } else {
            // `p-3`, a 40 px icon beside two lines.
            let head = (style.line(&style.file_name) + style.line(&style.file_meta)).max(40.);
            let h = 2. + 24. + head;
            pieces.push(Piece::Document { y, w: card_w, h, index });
            y += h;
        }
    }
    Layout { height: y, pieces, sorted }
}

/// `formatFileSize`.
pub fn format_file_size(bytes: i64) -> String {
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", b / 1024.)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", b / 1024. / 1024.)
    } else {
        format!("{:.1} GB", b / 1024. / 1024. / 1024.)
    }
}

/// `formatDuration`.
pub fn format_duration(ms: u32) -> String {
    if ms == 0 {
        return String::new();
    }
    let total = ((ms as f64) / 1000.).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// The source a picture box shows for an attachment: the thumbnail `previewUrlFor` picks for the
/// box at the device pixels, else the original.
pub fn picture_source(server: ServerId, a: &Attachment, box_w: f32, box_h: f32, scale: f32) -> Source {
    let thumb = sizing::preview_thumb(a.thumbnail_sizes_mask, a.thumbnail_size_bytes, &a.mime, box_w, box_h, scale).unwrap_or(0);
    Source::Attachment { server: server.wire().into(), id: a.id, thumb }
}

/// A video's poster: the server thumbnail picked for the box, none when it has none.
pub fn poster_source(server: ServerId, a: &Attachment, box_w: f32, box_h: f32, scale: f32) -> Option<Source> {
    if a.thumbnail_size_bytes == 0 {
        return None;
    }
    let target = sizing::js_round(box_w.max(box_h) * scale) as u32;
    let size = decibell_attachments::thumbs::pick_thumbnail_size(a.thumbnail_sizes_mask, target)?;
    Some(Source::Attachment { server: server.wire().into(), id: a.id, thumb: size })
}

/// Whether the community's attachment endpoint is known (`buildAttachmentUrl` gives null until
/// `COMMUNITY_AUTH_RES` named it): screens and tests say yes.
fn endpoint_known(server: ServerId, cx: &App) -> bool {
    if crate::fixture_mode(cx) {
        return true;
    }
    decibell_model::chat::composer::CoreAttachmentService::attachments().is_some_and(|svc| svc.target(&server.wire()).is_some())
}

/// The picture keys of a layout, with the box each is decoded for: (key, visible-able).
fn keys_of(layout: &Layout, server: ServerId, space: SlotSpace, out: &mut Vec<PicKey>) {
    let scale = space.scale;
    let key_for = |a: &Attachment, w: f32, h: f32, fit: Fit| -> Option<PicKey> {
        let source = match a.kind {
            AttachmentKind::Video => poster_source(server, a, w, h, scale)?,
            _ => picture_source(server, a, w, h, scale),
        };
        Some(PicKey::new(source, Spec { width: w, height: h, scale, fit, upscale: true }))
    };
    for piece in &layout.pieces {
        let Piece::Media { media, .. } = piece else { continue };
        match media {
            // The picture sits inside the button's 1 px border.
            Media::Image { w, h, index, .. } => out.extend(key_for(&layout.sorted[*index], w - 2., h - 2., Fit::Contain)),
            // The poster fills the card's inner width (`bg-cover`).
            Media::Video { w, poster, index, .. } => out.extend(key_for(&layout.sorted[*index], w - 2., *poster, Fit::Cover)),
            Media::Grid { cells, .. } => {
                for cell in cells {
                    out.extend(key_for(&layout.sorted[cell.index], cell.w, cell.h, Fit::Cover));
                }
            }
        }
    }
}

/// The attachments slot. See the module doc.
pub struct AttachmentSlot {
    /// The download buttons showing their tick: (first attachment id) → its timer.
    saved: Rc<RefCell<HashMap<i64, Task<()>>>>,
    /// The lists whose near rows hold an audio or video card (redrawn when playback or the
    /// media volume changes).
    media_lists: Rc<RefCell<std::collections::HashSet<EntityId>>>,
    /// Near rows told before their community's attachment endpoint was known, per list.
    later: Rc<RefCell<HashMap<EntityId, Later>>>,
    /// The watch for those endpoints (only while some list waits).
    watching: RefCell<Option<Task<()>>>,
    this: RefCell<std::rc::Weak<AttachmentSlot>>,
    _subscriptions: Vec<gpui::Subscription>,
}

type Later = (ServerId, SlotSpace, Vec<Arc<Message>>, std::ops::Range<usize>);

impl AttachmentSlot {
    pub fn new(cx: &mut App) -> AttachmentSlot {
        let media_lists: Rc<RefCell<std::collections::HashSet<EntityId>>> = Rc::default();
        let mut subscriptions = Vec::new();
        if let Some(model) = cx.try_global::<Model>().cloned() {
            let redraw = |lists: Rc<RefCell<std::collections::HashSet<EntityId>>>| {
                move |cx: &mut App| {
                    let lists: Vec<EntityId> = lists.borrow().iter().copied().collect();
                    for list in lists {
                        cx.notify(list);
                    }
                }
            };
            let (a, v) = (redraw(media_lists.clone()), redraw(media_lists.clone()));
            subscriptions.push(cx.observe(&model.chat.audio, move |_, cx| a(cx)));
            subscriptions.push(cx.observe(&model.chat.video, move |_, cx| v(cx)));
            if let Some(playback) = crate::playback::Playback::global(cx) {
                // The playing video's controls and time, its status, the "Paused at" labels.
                let p = redraw(media_lists.clone());
                subscriptions.push(cx.observe(&playback, move |_, cx| p(cx)));
            }
            let s = redraw(media_lists.clone());
            let mut was = { let v = model.settings.read(cx).get(); (v.media_audio_volume, v.media_audio_muted) };
            subscriptions.push(cx.observe(&model.settings, move |settings, cx| {
                let now = { let v = settings.read(cx).get(); (v.media_audio_volume, v.media_audio_muted) };
                if now != was {
                    was = now;
                    s(cx);
                }
            }));
        }
        AttachmentSlot { saved: Rc::default(), media_lists, later: Rc::default(), watching: RefCell::default(), this: RefCell::default(), _subscriptions: subscriptions }
    }

    /// The slot in its `Rc` (so the endpoint watch can call back into it).
    pub fn shared(self) -> Rc<AttachmentSlot> {
        let slot = Rc::new(self);
        *slot.this.borrow_mut() = Rc::downgrade(&slot);
        slot
    }

    /// Checks every 250 ms, while some list waits, whether its community's endpoint arrived
    /// (the core says so through no event); tells the list's rows again when it has.
    fn watch_endpoints(&self, cx: &mut App) {
        if self.watching.borrow().is_some() {
            return;
        }
        let this = self.this.borrow().clone();
        let task = cx.spawn(async move |cx| {
            for _ in 0..240 {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let Some(slot) = this.upgrade() else { return };
                let done = cx.update(|cx| {
                    let ready: Vec<(EntityId, Later)> = {
                        let mut later = slot.later.borrow_mut();
                        let lists: Vec<EntityId> = later.iter().filter(|(_, (server, ..))| endpoint_known(*server, cx)).map(|(list, _)| *list).collect();
                        lists.into_iter().filter_map(|list| later.remove(&list).map(|l| (list, l))).collect()
                    };
                    for (list, (server, space, near, visible)) in ready {
                        slot.near(list, Some(server), space, &near, visible, cx);
                    }
                    slot.later.borrow().is_empty()
                });
                if done {
                    break;
                }
            }
            if let Some(slot) = this.upgrade() {
                slot.later.borrow_mut().clear();
                slot.watching.borrow_mut().take();
            }
        });
        *self.watching.borrow_mut() = Some(task);
    }
}

impl RowSlot for AttachmentSlot {
    fn height(&self, message: &Message, _: Option<ServerId>, space: SlotSpace, cx: &App) -> f32 {
        if message.attachments.is_empty() {
            return 0.;
        }
        layout(message, space, &ContentStyle::get(cx, space.scale)).height
    }

    fn element(&self, message: &Arc<Message>, server: Option<ServerId>, space: SlotSpace, window: &mut Window, cx: &mut App) -> AnyElement {
        let style = ContentStyle::get(cx, space.scale);
        let layout = layout(message, space, &style);
        let list = window.current_view();
        let mut root = div().relative().w(px(space.width)).h(px(layout.height));
        for piece in &layout.pieces {
            root = root.child(self.piece(message, server, &layout, piece, space, list, &style, cx));
        }
        root.into_any_element()
    }

    fn near(&self, list: EntityId, server: Option<ServerId>, space: SlotSpace, near: &[Arc<Message>], visible: std::ops::Range<usize>, cx: &mut App) {
        if near.is_empty() {
            self.media_lists.borrow_mut().remove(&list);
            self.later.borrow_mut().remove(&list);
        }
        let (Some(server), Some(pictures)) = (server, Pictures::global(cx)) else { return };
        if !endpoint_known(server, cx) {
            // The channel opened before the community said where its files are (a restored
            // channel at start-up): told again once it has.
            pictures.update(cx, |p, cx| p.set_wants((list, 0), Vec::new(), cx));
            if !near.is_empty() {
                self.later.borrow_mut().insert(list, (server, space, near.to_vec(), visible));
                self.watch_endpoints(cx);
            }
            return;
        }
        self.later.borrow_mut().remove(&list);
        let style = ContentStyle::get(cx, space.scale);
        let media = near.iter().any(|m| m.attachments.iter().any(|a| matches!(a.kind, AttachmentKind::Audio | AttachmentKind::Video)));
        if media {
            self.media_lists.borrow_mut().insert(list);
        } else {
            self.media_lists.borrow_mut().remove(&list);
        }
        if let Some(playback) = crate::playback::Playback::global(cx) {
            // A playing attachment whose row this list does not draw now shows nothing: its
            // player decodes on without pictures, and its time is not mirrored.
            let (video, audio) = playback.read(cx).active_ids();
            let drawn = |id: Option<i64>| {
                id.map(|id| near.iter().enumerate().any(|(i, m)| visible.contains(&i) && m.attachments.iter().any(|a| a.id == id)))
            };
            let (video, audio) = (drawn(video), drawn(audio));
            if video.is_some() || audio.is_some() {
                playback.update(cx, |playback, cx| playback.set_visible(video, audio, cx));
            }
        }
        let mut wants = Vec::new();
        let mut keys = Vec::new();
        for (i, message) in near.iter().enumerate() {
            if message.attachments.is_empty() {
                continue;
            }
            keys.clear();
            keys_of(&layout(message, space, &style), server, space, &mut keys);
            wants.extend(keys.drain(..).map(|key| Want { key, visible: visible.contains(&i) }));
        }
        pictures.update(cx, |p, cx| p.set_wants((list, 0), wants, cx));
    }
}

/// What a click on a picture opens: the message's images (`galleryFor`: the clicked message only,
/// owner decision 2026-10-28), from the tile's box on screen.
pub struct OpenRequest {
    pub server: ServerId,
    pub message: Arc<Message>,
    pub attachment_id: i64,
    pub bounds: Bounds<Pixels>,
    pub cover: bool,
    /// The list the tile is in (redrawn when the viewer hides / shows the thumbnail).
    pub list: EntityId,
}

impl AttachmentSlot {
    #[allow(clippy::too_many_arguments)]
    fn piece(
        &self,
        message: &Arc<Message>,
        server: Option<ServerId>,
        layout: &Layout,
        piece: &Piece,
        space: SlotSpace,
        list: EntityId,
        style: &Rc<ContentStyle>,
        cx: &mut App,
    ) -> AnyElement {
        let theme = cx.theme();
        let at = |x: f32, y: f32, w: f32, h: f32| div().absolute().left(px(x)).top(px(y)).w(px(w)).h(px(h));
        match piece {
            Piece::Media { y, row, media, button, all } => {
                let group: SharedString = format!("media-{}", message.id.max(0)).into();
                let mut row_div = at(0., *y, space.width, *row).id(SharedString::from(format!("media-row-{}", message.id))).group(group.clone());
                match media {
                    Media::Image { x, y, w, h, index } => {
                        let a = &layout.sorted[*index];
                        row_div = row_div.child(self.image(message, server, a, *x, *y, *w, *h, space, list, false, Corners::all(cx.theme().radius_lg - px(1.)), cx));
                    }
                    Media::Video { y, w, h, poster, index } => {
                        let a = &layout.sorted[*index];
                        row_div = row_div.child(video_card(message, server, a, *y, *w, *h, *poster, space, style, cx));
                    }
                    Media::Grid { y, w, h, cells } => {
                        let radius = theme.radius_md - px(1.);
                        let mut frame = at(0., *y, *w, *h).rounded(theme.radius_md).border_1().border_color(theme.color_border_divider);
                        for cell in cells {
                            let a = &layout.sorted[cell.index];
                            let corners = Corners {
                                top_left: if cell.corners[0] { radius } else { px(0.) },
                                top_right: if cell.corners[1] { radius } else { px(0.) },
                                bottom_right: if cell.corners[2] { radius } else { px(0.) },
                                bottom_left: if cell.corners[3] { radius } else { px(0.) },
                            };
                            let child = if a.kind == AttachmentKind::Video {
                                grid_video(message, server, a, cell, corners, space, style, cx)
                            } else {
                                self.image(message, server, a, cell.x, cell.y, cell.w, cell.h, space, list, true, corners, cx)
                            };
                            frame = frame.child(child);
                        }
                        row_div = row_div.child(frame);
                    }
                }
                if let Some(server) = server {
                    let items: Vec<Attachment> = all.iter().map(|i| layout.sorted[*i].clone()).collect();
                    row_div = row_div.child(self.download_button(message, server, items, *button, &group, list, cx));
                }
                row_div.into_any_element()
            }
            Piece::Audio { y, w, h, index } => audio_card(message, server, &layout.sorted[*index], *y, *w, *h, style, cx),
            Piece::Document { y, w, h, index } => document_card(server, &layout.sorted[*index], *y, *w, *h, style, cx),
            Piece::Tombstone { y, w, h, index } => {
                let a = &layout.sorted[*index];
                let name = if a.filename.is_empty() { "Attachment".to_string() } else { a.filename.to_string() };
                let theme = cx.theme();
                // `border-dashed bg-bg-secondary/50`: GPUI draws borders solid.
                at(0., *y, *w, *h)
                    .flex()
                    .items_center()
                    .p_3()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.color_border)
                    .bg(gpui::Hsla { a: theme.color_bg_secondary.a * 0.5, ..theme.color_bg_secondary })
                    .child(label(format!("{name} (deleted by retention policy)"), &style.tombstone, decibell_text::Wrapping::TRUNCATE))
                    .into_any_element()
            }
        }
    }

    /// One picture: the bordered box (standalone) or a grid cell.
    #[allow(clippy::too_many_arguments)]
    fn image(
        &self,
        message: &Arc<Message>,
        server: Option<ServerId>,
        a: &Attachment,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        space: SlotSpace,
        list: EntityId,
        cell: bool,
        corners: Corners<Pixels>,
        cx: &mut App,
    ) -> AnyElement {
        let theme = cx.theme();
        let (inner_w, inner_h) = if cell { (w, h) } else { (w - 2., h - 2.) };
        let key = server.filter(|s| endpoint_known(*s, cx)).map(|server| {
            let fit = if cell { Fit::Cover } else { Fit::Contain };
            PicKey::new(picture_source(server, a, inner_w, inner_h, space.scale), Spec { width: inner_w, height: inner_h, scale: space.scale, fit, upscale: true })
        });
        let hidden = crate::viewer::hides(server, a.id, cx);
        let tile = Tile {
            key,
            placeholder: a.placeholder.clone(),
            corners,
            background: Some(theme.color_bg_secondary),
            alt: a.filename.clone(),
            veil: None,
        };
        let bounds: Rc<Cell<Bounds<Pixels>>> = Rc::default();
        let painted = bounds.clone();
        let viewer_key = server.map(|s| crate::viewer::attachment_key(s, a.id));
        let probe_id = a.id;
        let canvas = gpui::canvas(
            |_, _, _| {},
            move |b, _, window, cx| {
                painted.set(b);
                if let Some(key) = &viewer_key {
                    crate::viewer::target_drawn(key.clone(), b, cx);
                }
                crate::probes::record(probe_id, b);
                if !hidden {
                    tile.paint(b, window, cx);
                }
            },
        )
        .size_full();
        let mut tile_div = div()
            .id(SharedString::from(format!("attachment-{}", a.id)))
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w))
            .h(px(h))
            .cursor_pointer();
        if !cell {
            tile_div = tile_div.rounded(theme.radius_lg).border_1().border_color(theme.color_border);
        }
        if hidden {
            // The viewer flies the picture out of / back into this box: the box hides meanwhile.
            tile_div = tile_div.opacity(0.);
        }
        let attachment = a.clone();
        let (open_message, open_bounds) = (message.clone(), bounds.clone());
        tile_div
            .child(canvas)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                if let Some(server) = server {
                    crate::viewer::open_attachment(
                        OpenRequest { server, message: open_message.clone(), attachment_id: attachment.id, bounds: open_bounds.get(), cover: cell, list },
                        cx,
                    );
                }
            })
            .on_mouse_down(MouseButton::Right, {
                let a = a.clone();
                move |event, _, cx| {
                    cx.stop_propagation();
                    if let Some(server) = server {
                        crate::menu::show_for_attachment(server, &a, event.position, decibell_model::chat::viewers::MediaMenuKind::Image, cx);
                    }
                }
            })
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn download_button(
        &self,
        message: &Arc<Message>,
        server: ServerId,
        items: Vec<Attachment>,
        (x, y): (f32, f32),
        group: &SharedString,
        list: EntityId,
        cx: &mut App,
    ) -> AnyElement {
        let theme = cx.theme();
        let first = items.first().map_or(0, |a| a.id);
        let saved = self.saved.borrow().contains_key(&first);
        let title: SharedString = if items.len() > 1 { format!("Download all {}", items.len()).into() } else { SharedString::new_static("Download") };
        let (color, hover_color) = if saved { (theme.color_accent, theme.color_accent) } else { (theme.color_text_muted, theme.color_text_primary) };
        let saved_map = self.saved.clone();
        let _ = message;
        div()
            .id(SharedString::from(format!("download-{first}")))
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(BUTTON))
            .h(px(BUTTON))
            .flex()
            .items_center()
            .justify_center()
            .rounded(theme.radius_sm)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_secondary)
            .cursor_pointer()
            .when(!saved, |d| d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.)))
            .hover(|s| s.bg(theme.color_surface_hover))
            .tooltip(title_tooltip(title))
            .child(
                Icon::new(if saved { IconName::ContentCheck } else { IconName::ViewerDownload })
                    .size(px(16.))
                    .color(color)
                    .group_hover(group.clone(), hover_color),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                // One at a time: with "ask where to save" on, each opens its own dialog.
                let items = items.clone();
                let saved_map = saved_map.clone();
                let task = cx.spawn(async move |cx| {
                    let mut any = false;
                    for a in items {
                        let started = cx.update(|cx| crate::downloads::start(server, &a, false, cx));
                        if started.await {
                            any = true;
                        }
                    }
                    if !any {
                        return;
                    }
                    let clear = saved_map.clone();
                    let timer = cx.spawn(async move |cx| {
                        cx.background_executor().timer(SAVED).await;
                        clear.borrow_mut().remove(&first);
                        cx.update(|cx| cx.notify(list));
                    });
                    saved_map.borrow_mut().insert(first, timer);
                    cx.update(|cx| cx.notify(list));
                });
                task.detach();
            })
            .into_any_element()
    }
}

/// A standalone video card at rest: the poster (the server thumbnail, else the blur) on black
/// with the play button, the bar with the name and size (`VideoItem`). Playing is the persistent
/// player's (another crate): a click sets `chat.video`'s active attachment.
#[allow(clippy::too_many_arguments)]
fn video_card(message: &Arc<Message>, server: Option<ServerId>, a: &Attachment, y: f32, w: f32, h: f32, poster: f32, space: SlotSpace, style: &Rc<ContentStyle>, cx: &mut App) -> AnyElement {
    let inner = cx.theme().radius_lg - px(1.);
    let surface = video_surface(message, server, a, w - 2., poster, false, Corners { top_left: inner, top_right: inner, ..Corners::default() }, space, style, cx);
    let theme = cx.theme();
    // `metaRight`: where it was left, else its size.
    let bar_meta = crate::playback::paused_label(&message.channel, a.id, cx).unwrap_or_else(|| format_file_size(a.size_bytes));
    div()
        .absolute()
        .left(px(0.))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .flex()
        .flex_col()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.color_border)
        .bg(theme.color_bg_light)
        .shadow(theme.shadow_raised.clone())
        .child(div().w_full().h(px(poster)).child(surface))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px(px(11.))
                .py_2()
                .child(div().min_w_0().child(label(a.filename.clone(), &style.meta_105, decibell_text::Wrapping::TRUNCATE)))
                .child(div().flex_none().child(label(bar_meta, &style.meta_105, decibell_text::Wrapping::NOWRAP))),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn grid_video(message: &Arc<Message>, server: Option<ServerId>, a: &Attachment, cell: &GridCell, corners: Corners<Pixels>, space: SlotSpace, style: &Rc<ContentStyle>, cx: &mut App) -> AnyElement {
    div()
        .absolute()
        .left(px(cell.x))
        .top(px(cell.y))
        .w(px(cell.w))
        .h(px(cell.h))
        .child(video_surface(message, server, a, cell.w, cell.h, true, corners, space, style, cx))
        .into_any_element()
}

/// The poster / play surface the persistent video player overlays when it plays this attachment:
/// it covers exactly this box (see `crate::player` for what the player needs from it).
#[allow(clippy::too_many_arguments)]
fn video_surface(
    message: &Arc<Message>,
    server: Option<ServerId>,
    a: &Attachment,
    w: f32,
    h: f32,
    cell: bool,
    corners: Corners<Pixels>,
    space: SlotSpace,
    style: &Rc<ContentStyle>,
    cx: &mut App,
) -> AnyElement {
    // Playing: the picture and its controls take the box (`PersistentVideoLayer`).
    if let Some(stage) = crate::playback::video_stage(a.id, server.map(|server| (server, a.clone())), corners, false, style, cx) {
        return stage;
    }
    let theme = cx.theme();
    let black = gpui::black();
    let key = server
        .filter(|s| endpoint_known(*s, cx))
        .and_then(|server| poster_source(server, a, w, h, space.scale))
        .map(|source| PicKey::new(source, Spec { width: w, height: h, scale: space.scale, fit: Fit::Cover, upscale: true }));
    let has_poster = key.is_some();
    let active = cx.try_global::<Model>().is_some_and(|m| m.chat.video.read(cx).active.as_ref().is_some_and(|v| v.attachment_id == a.id));
    let tile = Tile {
        key,
        placeholder: a.placeholder.clone(),
        corners,
        background: Some(black),
        alt: SharedString::default(),
        veil: has_poster.then_some(gpui::Hsla { a: 0.3, ..black }),
    };
    let bounds: Rc<Cell<Bounds<Pixels>>> = Rc::default();
    let painted = bounds.clone();
    let host = a.id;
    let surface = gpui::canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            painted.set(b);
            crate::player::host_drawn(host, b, cx);
            tile.paint(b, window, cx);
        },
    )
    .absolute()
    .size_full();
    let meta = format_file_size(a.size_bytes);
    let (attachment, message, a_id) = (a.clone(), message.clone(), a.id);
    let mut out = div()
        .id(SharedString::from(format!("video-{a_id}")))
        .relative()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .when(!active, |d| d.cursor_pointer())
        .tooltip(title_tooltip(a.filename.clone()))
        .child(surface);
    if !active {
        out = out.child(
            div()
                .relative()
                .size_12()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_lg)
                .bg(theme.color_accent)
                .hover(|s| s.bg(theme.color_accent_hover))
                .child(Icon::new(IconName::ContentPlay).size(px(28.)).color(theme.color_on_accent)),
        );
    }
    if cell {
        // Grid cells keep the scrim with the name and size.
        out = out.child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .px(px(14.))
                .pb(px(10.))
                .pt_10()
                .bg(gpui::linear_gradient(
                    0.,
                    gpui::linear_color_stop(gpui::Hsla { a: 0.85, ..black }, 0.),
                    gpui::linear_color_stop(gpui::Hsla { a: 0., ..black }, 1.),
                ))
                .child(div().min_w_0().child(label(a.filename.clone(), &style.scrim, decibell_text::Wrapping::TRUNCATE)))
                .child(div().flex_none().child(crate::style::label_in(meta, &style.scrim, decibell_text::Wrapping::NOWRAP, theme.color_text_muted))),
        );
    }
    out.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            if let Some(server) = server {
                crate::player::play_video(server, &message, &attachment, bounds.get(), cx);
            }
        })
        .on_mouse_down(MouseButton::Right, {
            let a = a.clone();
            move |event, _, cx| {
                cx.stop_propagation();
                if let Some(server) = server {
                    crate::menu::show_for_attachment(server, &a, event.position, decibell_model::chat::viewers::MediaMenuKind::Video, cx);
                }
            }
        })
        .into_any_element()
}

/// The audio card at rest (`AudioItem`): kind icon, name, duration · size, the download button;
/// play, the scrub track, the time, the volume. Playback is the persistent player's: play sets
/// `chat.audio`'s active attachment, and the card follows its mirrored state.
#[allow(clippy::too_many_arguments)]
fn audio_card(message: &Arc<Message>, server: Option<ServerId>, a: &Attachment, y: f32, w: f32, h: f32, style: &Rc<ContentStyle>, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let model = cx.try_global::<Model>().cloned();
    let (_active, playing, time, duration) = model.as_ref().map_or((false, false, 0., 0.), |m| {
        let audio = m.chat.audio.read(cx);
        let active = audio.active.as_ref().is_some_and(|x| x.attachment_id == a.id);
        (active, active && audio.playing, if active { audio.time_secs } else { 0. }, if active { audio.duration_secs } else { 0. })
    });
    let seeded = if a.duration_ms > 0 { a.duration_ms as f64 / 1000. } else { 0. };
    let shown = if duration > 0. { duration } else { seeded };
    let progress = if shown > 0. { (time / shown).clamp(0., 1.) as f32 } else { 0. };
    let fmt = |s: f64| if !s.is_finite() || s < 0. { "0:00".to_string() } else { format!("{}:{:02}", (s / 60.).floor() as u64, (s % 60.).floor() as u64) };
    let meta = if a.duration_ms > 0 { format!("{} · {}", format_duration(a.duration_ms), format_file_size(a.size_bytes)) } else { format_file_size(a.size_bytes) };
    let (volume, muted) = model.as_ref().map_or((1., false), |m| {
        let s = m.settings.read(cx).get();
        (s.media_audio_volume.unwrap_or(1.) as f32, s.media_audio_muted)
    });
    let shown_volume = if muted { 0. } else { volume };
    let (attachment, message) = (a.clone(), message.clone());
    let download = {
        let a = a.clone();
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            if let Some(server) = server {
                crate::downloads::start(server, &a, false, cx).detach();
            }
        }
    };
    div()
        .absolute()
        .left(px(0.))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .flex()
        .flex_col()
        .gap(px(10.))
        .p_3()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.color_border)
        .bg(theme.color_bg_light)
        .shadow(theme.shadow_float.clone())
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div().size_10().flex_none().flex().items_center().justify_center().rounded(theme.radius_md).bg(theme.color_accent_soft).child(
                        Icon::new(IconName::ChatAudio).size(px(18.)).color(theme.color_accent_bright),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(label(a.filename.clone(), &style.audio_name, decibell_text::Wrapping::TRUNCATE))
                        .child(label(meta, &style.meta_105, decibell_text::Wrapping::NOWRAP)),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("audio-download-{}", a.id)))
                        .size_7()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius_sm)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.color_surface_hover))
                        .tooltip(title_tooltip("Download"))
                        .child(Icon::new(IconName::ViewerDownload).size(px(14.)).color(theme.color_text_muted))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(download),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .id(SharedString::from(format!("audio-play-{}", a.id)))
                        .h_8()
                        .w_10()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius_md)
                        .bg(theme.color_accent)
                        .hover(|s| s.bg(theme.color_accent_hover))
                        .cursor_pointer()
                        .tooltip(title_tooltip(if playing { "Pause" } else { "Play" }))
                        .child(Icon::new(if playing { IconName::ContentPause } else { IconName::ContentPlay }).size(px(14.)).color(theme.color_on_accent))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| {
                            if let Some(server) = server {
                                crate::player::play_audio(server, &message, &attachment, cx);
                            }
                        }),
                )
                .child(
                    div()
                        .relative()
                        .h_3()
                        .flex_1()
                        .flex()
                        .items_center()
                        .child(div().absolute().left_0().right_0().h(px(4.)).rounded_full().bg(theme.color_bg_lighter))
                        .child(div().absolute().left_0().h(px(4.)).w(gpui::relative(progress)).rounded_full().bg(theme.color_accent))
                        // `onScrubDown`: the active attachment's, once its duration is known.
                        .children(crate::playback::audio_scrub(a.id, cx)),
                )
                .child(div().flex_none().child(label(format!("{} / {}", fmt(time), fmt(shown)), &style.meta_10, decibell_text::Wrapping::NOWRAP)))
                .child(
                    div()
                        .id(SharedString::from(format!("audio-mute-{}", a.id)))
                        .size_7()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius_sm)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.color_surface_hover))
                        .tooltip(title_tooltip(if muted { "Unmute" } else { "Mute" }))
                        .child(
                            Icon::new(if muted || volume == 0. { IconName::ContentVolumeMuted } else { IconName::ContentVolume })
                                .size(px(14.))
                                .color(theme.color_text_muted),
                        )
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        // `audioToggleMute`: a setting, so it works before anything plays.
                        .on_click(|_, _, cx| {
                            if let Some(model) = cx.try_global::<Model>().cloned() {
                                model.settings.update(cx, |s, cx| s.change(|v| v.media_audio_muted = !v.media_audio_muted, cx));
                            }
                        }),
                )
                .child(
                    div()
                        .relative()
                        .h_3()
                        .w_16()
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(div().absolute().left_0().right_0().h(px(3.)).rounded_full().bg(theme.color_bg_lighter))
                        .child(div().absolute().left_0().h(px(3.)).w(gpui::relative(shown_volume)).rounded_full().bg(theme.color_accent))
                        // `onVolumeMouseDown` and the wheel: a setting, so before anything plays too.
                        .children(crate::playback::audio_volume_track(cx)),
                ),
        )
        .into_any_element()
}

/// A document (`DocumentItem`): one button that downloads it through the Transfers manager.
fn document_card(server: Option<ServerId>, a: &Attachment, y: f32, w: f32, h: f32, style: &Rc<ContentStyle>, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let available = server.is_some_and(|s| endpoint_known(s, cx)) && a.purged_at == 0;
    let attachment = a.clone();
    div()
        .id(SharedString::from(format!("document-{}", a.id)))
        .absolute()
        .left(px(0.))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .flex()
        .items_center()
        .gap_3()
        .p_3()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.color_border)
        .bg(theme.color_bg_secondary)
        .when(available, |d| d.cursor_pointer().hover(|s| s.bg(theme.color_bg_light)))
        .when(!available, |d| d.opacity(0.5))
        .tooltip(title_tooltip(format!("Download {}", a.filename)))
        .child(
            div()
                .size_10()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .bg(theme.color_bg_light)
                .child(Icon::new(IconName::TransfersKindDocument).size(px(22.)).color(theme.color_text_muted)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(label(a.filename.clone(), &style.file_name, decibell_text::Wrapping::TRUNCATE))
                .child(label(format_file_size(a.size_bytes), &style.file_meta, decibell_text::Wrapping::NOWRAP)),
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            if available && let Some(server) = server {
                crate::downloads::start(server, &attachment, false, cx).detach();
            }
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_format_like_the_renderer() {
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(240_000), "234.4 KB");
        assert_eq!(format_file_size(5 * 1024 * 1024 + 1), "5.0 MB");
        assert_eq!(format_duration(61_000), "1:01");
        assert_eq!(format_duration(59_600), "1:00");
        assert_eq!(format_duration(0), "");
    }
}
