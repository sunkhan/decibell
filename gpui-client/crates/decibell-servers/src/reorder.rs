//! Dragging the channel list to reorder it (`ServerChannelsSidebar.tsx`'s HTML5 drag-and-drop of
//! channels and categories, one flat list) and the category-sync prompt (`CategorySyncPrompt.tsx`).
//!
//! The sidebar (`decibell-shell`) draws the rows; it calls [`Reorder`] (registered as its
//! `ChannelReorder`) on a left press of a row, and draws what this returns: each row's probe and
//! the drop line at its top edge, the line at the list's end, and while a press is held the
//! dragged row under the pointer with the window's pointer listeners.
//!
//! **The rules** (pure functions below, unit-tested): the drop point is the hovered row (before it
//! in its top half, before the next row in the list order in its bottom half), the end of the list
//! over anything else in it, unchanged outside it; a category never lands inside another block (it
//! snaps forward to the next category, else the end); a text channel never goes below a voice
//! channel of its group nor a voice channel above a text one (no line, not droppable). A category
//! moves with its block. The new order is applied at once and sent whole (`reorder_channels`); the
//! community's `channel_list_updated` broadcast then settles it (and resyncs a refused one). A
//! channel dropped into another category whose channels can follow its permissions asks first:
//! Sync (the reorder, then `sync_channel_permissions` naming that category), Keep (the reorder),
//! Cancel (nothing: the list is as it was).
//!
//! **Frames**: a press redraws the sidebar once (the listeners go in); while dragging, each pointer
//! move redraws the sidebar alone (the line, the dragged row); the drop or Escape once more; then
//! nothing. No timer: auto-scroll near the list's edges happens on pointer moves.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use decibell_common::Watch;
use decibell_core::api;
use decibell_model::error::UiError;
use decibell_model::stores::community::{Channel, ChannelKind};
use decibell_model::stores::permissions::perm;
use decibell_model::{ChannelId, Model, ServerId};
use decibell_shell::ChannelReorder;
use decibell_ui::{
    ActiveTheme as _, ConfirmModal, ConfirmTone, FontRole, Icon, IconName, Label, Placed, Placement, ScrollHandle,
    visible_window_bounds,
};
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, DispatchPhase, Entity, Global, IntoElement, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, SharedString, Subscription, Window, canvas, deferred, div, point, prelude::*, px,
};

// ── The rules ──────────────────────────────────────────────────────────────────────────────

/// Where a drop lands: before this row, or at the end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Before {
    Id(ChannelId),
    End,
}

/// `onListDragOver`: the drop point for `dragged` with the pointer over `hovered` (a row and
/// whether the pointer is in its top half; `None`: inside the list but over no row). `None` back:
/// no line, not droppable.
pub fn drop_point(channels: &[Channel], dragged: &str, hovered: Option<(&str, bool)>) -> Option<Before> {
    let ids: Vec<&str> = channels.iter().map(|c| &*c.id).collect();
    let mut before = match hovered {
        None => Before::End,
        Some((id, true)) => Before::Id(id.into()),
        Some((id, false)) => match ids.iter().position(|x| *x == id) {
            Some(i) if i + 1 < ids.len() => Before::Id(ids[i + 1].into()),
            _ => Before::End,
        },
    };
    let dragged_kind = channels.iter().find(|c| &*c.id == dragged).map(|c| c.kind);
    match dragged_kind {
        // Categories can't nest: snap forward to the next block boundary.
        Some(ChannelKind::Category) => {
            if let Before::Id(id) = &before {
                let start = ids.iter().position(|x| *x == &**id).unwrap_or(ids.len());
                before = channels[start..]
                    .iter()
                    .find(|c| c.kind == ChannelKind::Category)
                    .map(|c| Before::Id(c.id.clone()))
                    .unwrap_or(Before::End);
            }
        }
        // Text above voice within every group: other spots get no line.
        Some(kind @ (ChannelKind::Text | ChannelKind::Voice)) => {
            let rest: Vec<&Channel> = channels.iter().filter(|c| &*c.id != dragged).collect();
            let i = match &before {
                Before::End => rest.len(),
                Before::Id(id) => rest.iter().position(|c| c.id == *id)?,
            };
            let group_start = rest[..i].iter().rposition(|c| c.kind == ChannelKind::Category).map_or(0, |j| j + 1);
            let group_end = rest[i..].iter().position(|c| c.kind == ChannelKind::Category).map_or(rest.len(), |j| i + j);
            let valid = if kind == ChannelKind::Text {
                !rest[group_start..i].iter().any(|c| c.kind == ChannelKind::Voice)
            } else {
                !rest[i..group_end].iter().any(|c| c.kind == ChannelKind::Text)
            };
            if !valid {
                return None;
            }
        }
        _ => {}
    }
    Some(before)
}

/// `applyReorder`'s new order: `dragged` (with its block, for a category) moved before `before`;
/// `None` when nothing moves.
pub fn reorder(channels: &[Channel], dragged: &str, before: &Before) -> Option<Vec<ChannelId>> {
    let ordered: Vec<ChannelId> = channels.iter().map(|c| c.id.clone()).collect();
    let mut ids = ordered.clone();
    let start = ids.iter().position(|id| &**id == dragged)?;
    // A category moves with its whole block of children.
    let mut len = 1;
    if channels[start].kind == ChannelKind::Category {
        len += channels[start + 1..].iter().take_while(|c| c.kind != ChannelKind::Category).count();
    }
    let block: Vec<ChannelId> = ids.drain(start..start + len).collect();
    let target = match before {
        Before::End => ids.len(),
        // `before` inside the dragged block (a category dropped onto itself): nothing.
        Before::Id(id) => ids.iter().position(|x| x == id)?,
    };
    ids.splice(target..target, block);
    (ids != ordered).then_some(ids)
}

/// `categoryOf`: the category `id` sits under in `order`.
pub fn category_of(order: &[ChannelId], channels: &[Channel], id: &str) -> Option<ChannelId> {
    let mut category = None;
    for x in order {
        if &**x == id {
            return category;
        }
        if channels.iter().find(|c| c.id == *x).is_some_and(|c| c.kind == ChannelKind::Category) {
            category = Some(x.clone());
        }
    }
    None
}

/// What a drop does.
#[derive(Clone, Debug, PartialEq)]
pub enum DropAction {
    Commit(Vec<ChannelId>),
    /// Into another category whose channels can follow its permissions: ask first.
    Ask { ids: Vec<ChannelId>, channel: ChannelId, category: ChannelId },
}

pub fn resolve_drop(channels: &[Channel], dragged: &str, before: &Before) -> Option<DropAction> {
    let ids = reorder(channels, dragged, before)?;
    let kind = channels.iter().find(|c| &*c.id == dragged).map(|c| c.kind);
    if kind != Some(ChannelKind::Category) {
        let ordered: Vec<ChannelId> = channels.iter().map(|c| c.id.clone()).collect();
        let to = category_of(&ids, channels, dragged);
        if let Some(to) = to
            && Some(&to) != category_of(&ordered, channels, dragged).as_ref()
            && channels.iter().find(|c| c.id == to).is_some_and(|c| c.category_sync)
        {
            return Some(DropAction::Ask { ids, channel: dragged.into(), category: to });
        }
    }
    Some(DropAction::Commit(ids))
}

// ── The state ──────────────────────────────────────────────────────────────────────────────

/// Blink's `kDragThresholdX / Y`: a press becomes a drag once the pointer moved 4 px either way.
const DRAG_THRESHOLD: f32 = 4.;
/// Blink paints the drag image at 75 % (`kDragImageAlpha`).
const DRAG_IMAGE_ALPHA: f32 = 0.75;
/// How close to the list's top or bottom edge the pointer scrolls it while dragging.
const AUTOSCROLL_ZONE: f32 = 24.;

struct Press {
    server: ServerId,
    channel: ChannelId,
    origin: Point<Pixels>,
    list: ScrollHandle,
    /// The pressed row's bounds (where the pointer took it).
    row: Option<Bounds<Pixels>>,
}

/// A channel dropped into a syncing category, waiting for Sync / Keep / Cancel. Kept (with `open`
/// false) through the dialog's fade-out so its text doesn't blank.
#[derive(Clone, Debug, PartialEq)]
pub struct SyncPrompt {
    pub server: ServerId,
    pub ids: Vec<ChannelId>,
    pub channel: ChannelId,
    pub category: ChannelId,
    pub open: bool,
}

#[derive(Default)]
pub struct DragState {
    press: Option<Press>,
    dragging: bool,
    before: Option<Before>,
    pointer: Point<Pixels>,
    /// The press that ended was a drag: the row's click does nothing.
    swallow: bool,
    pub prompt: Option<SyncPrompt>,
    _escape: Option<Subscription>,
}

impl DragState {
    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    pub fn before(&self) -> Option<&Before> {
        self.before.as_ref()
    }
}

/// The sidebar's rows as last laid out (window coordinates): what the pointer is over.
type Rows = Rc<RefCell<HashMap<ChannelId, Bounds<Pixels>>>>;

struct Handle {
    state: Entity<DragState>,
    rows: Rows,
}

impl Global for Handle {}

fn handle(cx: &mut App) -> (Entity<DragState>, Rows) {
    if let Some(h) = cx.try_global::<Handle>() {
        return (h.state.clone(), h.rows.clone());
    }
    let state = cx.new(|_| DragState::default());
    let rows: Rows = Rc::default();
    cx.set_global(Handle { state: state.clone(), rows: rows.clone() });
    (state, rows)
}

/// The drag state (made on first use).
pub fn state(cx: &mut App) -> Entity<DragState> {
    handle(cx).0
}

/// The sync prompt as it stands (none before the first drag).
pub fn state_read(cx: &App) -> Option<SyncPrompt> {
    cx.try_global::<Handle>().and_then(|h| h.state.read(cx).prompt.clone())
}

/// The registered `ChannelReorder`.
pub struct Reorder {
    state: Entity<DragState>,
    rows: Rows,
}

impl Reorder {
    pub fn new(cx: &mut App) -> Self {
        let (state, rows) = handle(cx);
        Self { state, rows }
    }
}

fn channels(server: ServerId, cx: &App) -> Vec<Channel> {
    Model::global(cx).community(server, cx).map(|c| c.channels.read(cx).list().to_vec()).unwrap_or_default()
}

impl DragState {
    fn reset(&mut self) {
        self.press = None;
        self.dragging = false;
        self.before = None;
        self._escape = None;
    }

    fn moved(&mut self, position: Point<Pixels>, held: bool, rows: &Rows, cx: &mut Context<Self>) {
        let Some(press) = &self.press else { return };
        if !held {
            // The release went somewhere we didn't see: as if dropped outside the list.
            let was = self.dragging;
            self.reset();
            self.swallow = was;
            cx.notify();
            return;
        }
        if !self.dragging {
            let d = position - press.origin;
            if f32::from(d.x).abs() < DRAG_THRESHOLD && f32::from(d.y).abs() < DRAG_THRESHOLD {
                return;
            }
            self.dragging = true;
            // Escape cancels the drag (Chromium's drag session does).
            let state = cx.entity().downgrade();
            self._escape = Some(cx.intercept_keystrokes(move |event, _, cx| {
                if event.keystroke.key == "escape"
                    && let Some(state) = state.upgrade()
                {
                    state.update(cx, |s, cx| {
                        if s.dragging {
                            s.reset();
                            s.swallow = true;
                            cx.notify();
                        }
                    });
                    cx.stop_propagation();
                }
            }));
        }
        self.pointer = position;
        let press = self.press.as_ref().expect("held");
        let viewport = press.list.viewport_bounds();
        if viewport.contains(&position) {
            // Auto-scroll near the edges, a step per pointer move.
            let (y, top, bottom) = (f32::from(position.y), f32::from(viewport.top()), f32::from(viewport.bottom()));
            let depth = if y < top + AUTOSCROLL_ZONE {
                -(AUTOSCROLL_ZONE - (y - top))
            } else if y > bottom - AUTOSCROLL_ZONE {
                AUTOSCROLL_ZONE - (bottom - y)
            } else {
                0.
            };
            if depth != 0. {
                let step = depth.signum() * (2. + 14. * depth.abs() / AUTOSCROLL_ZONE);
                let next = (f32::from(press.list.offset()) + step).clamp(0., f32::from(press.list.max_offset()));
                press.list.scroll_to(px(next));
            }
            // `onListDragOver`: the row under the pointer (`closest("[data-reorder-id]")`).
            let hovered = rows
                .borrow()
                .iter()
                .find(|(_, b)| b.contains(&position))
                .map(|(id, b)| (id.clone(), f32::from(position.y) < f32::from(b.top()) + f32::from(b.size.height) / 2.));
            let channels = channels(press.server, cx);
            self.before = drop_point(&channels, &press.channel, hovered.as_ref().map(|(id, top)| (&**id, *top)));
        }
        // Outside the list no `dragover` reaches it: the line stays where it was.
        cx.notify();
    }

    fn released(&mut self, cx: &mut Context<Self>) {
        let Some(press) = self.press.take() else { return };
        let dragging = self.dragging;
        let before = self.before.take();
        let inside = press.list.viewport_bounds().contains(&self.pointer);
        self.reset();
        self.swallow = dragging;
        cx.notify();
        if !dragging || !inside {
            // A drop outside the list is no drop (`onListDrop` never runs; `dragend` resets).
            return;
        }
        let Some(before) = before else { return };
        let channels = channels(press.server, cx);
        match resolve_drop(&channels, &press.channel, &before) {
            Some(DropAction::Commit(ids)) => commit(press.server, ids, None, cx),
            Some(DropAction::Ask { ids, channel, category }) => {
                self.prompt = Some(SyncPrompt { server: press.server, ids, channel, category, open: true });
                cx.notify();
            }
            None => {}
        }
    }

    /// For the screens: a drag in progress.
    #[allow(clippy::too_many_arguments)]
    /// `row`: where the dragged row is, if known (else the dragged row is drawn as if taken 40 px
    /// from its left edge, halfway down).
    pub fn show_drag(&mut self, server: ServerId, channel: ChannelId, row: Option<Bounds<Pixels>>, pointer: Point<Pixels>, before: Option<Before>, list: &ScrollHandle, cx: &mut Context<Self>) {
        let origin = row.map_or(pointer, |row| point(row.left() + px(40.), row.top() + row.size.height / 2.));
        self.press = Some(Press { server, channel, origin, list: list.clone(), row });
        self.dragging = true;
        self.pointer = pointer;
        self.before = before;
        cx.notify();
    }

    /// For the screens: the prompt as a drop would open it.
    pub fn show_prompt(&mut self, prompt: SyncPrompt, cx: &mut Context<Self>) {
        self.prompt = Some(prompt);
        cx.notify();
    }
}

/// `commitReorder`: the order applied at once, sent whole, then (Sync) the sync with the category
/// it was dropped into, which the server refuses if the reorder didn't land.
pub fn commit(server: ServerId, ids: Vec<ChannelId>, sync: Option<(ChannelId, ChannelId)>, cx: &mut App) {
    let model = Model::global(cx).clone();
    if let Some(community) = model.community(server, cx) {
        let current = community.channels.read(cx).list().to_vec();
        let next: Vec<Channel> = ids.iter().filter_map(|id| current.iter().find(|c| c.id == *id).cloned()).collect();
        community.channels.update(cx, |c, cx| c.set_all(next, cx));
    }
    if !decibell_model::core::is_running() {
        return;
    }
    let args = api::channels::ReorderChannelsArgs { server_id: server.wire(), channel_ids: ids.iter().map(|id| id.to_string()).collect() };
    cx.spawn(async move |_| {
        if let Err(e) = api::channels::reorder_channels(args).await.map_err(UiError::from) {
            log::error!("reorder_channels: {e}");
        }
        if let Some((channel, category)) = sync {
            let args = api::channels::SyncChannelPermissionsArgs {
                server_id: server.wire(),
                channel_id: channel.to_string(),
                category_id: Some(category.to_string()),
            };
            if let Err(e) = api::channels::sync_channel_permissions(args).await.map_err(UiError::from) {
                log::error!("sync_channel_permissions: {e}");
            }
        }
    })
    .detach();
}

// ── What the sidebar draws ───────────────────────────────────────────────────────────────

/// `DropLine`: 2 px of accent, `inset-x-1.5 -top-[1px] rounded-full`.
fn drop_line(cx: &App) -> AnyElement {
    div().absolute().left(px(6.)).right(px(6.)).top(px(-1.)).h(px(2.)).rounded_full().bg(cx.theme().chrome.color_accent).into_any_element()
}

impl ChannelReorder for Reorder {
    fn subscribe(&self, redraw: Box<dyn Fn(&mut App)>, cx: &mut App) -> Subscription {
        cx.observe(&self.state, move |_, cx| redraw(cx))
    }

    fn begin(&self, _: ServerId, _: &mut App) {
        self.rows.borrow_mut().clear();
    }

    fn press(&self, server: ServerId, channel: ChannelId, position: Point<Pixels>, list: &ScrollHandle, cx: &mut App) {
        let row = self.rows.borrow().get(&channel).copied();
        let list = list.clone();
        self.state.update(cx, |s, cx| {
            s.reset();
            s.swallow = false;
            s.pointer = position;
            s.press = Some(Press { server, channel, origin: position, list, row });
            cx.notify();
        });
    }

    fn swallow_click(&self, cx: &App) -> bool {
        self.state.read(cx).swallow
    }

    fn row_extras(&self, channel: &ChannelId, cx: &App) -> Vec<AnyElement> {
        let rows = self.rows.clone();
        let id = channel.clone();
        let probe = canvas(
            move |bounds, _, _| {
                rows.borrow_mut().insert(id.clone(), bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
        .into_any_element();
        let state = self.state.read(cx);
        let line = (state.dragging && state.before.as_ref() == Some(&Before::Id(channel.clone()))).then(|| drop_line(cx));
        std::iter::once(probe).chain(line).collect()
    }

    fn end_line(&self, cx: &App) -> Option<AnyElement> {
        let state = self.state.read(cx);
        (state.dragging && state.before == Some(Before::End)).then(|| drop_line(cx))
    }

    fn overlay(&self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let s = self.state.read(cx);
        let press = s.press.as_ref()?;
        let bounds = visible_window_bounds(window, cx);
        // The pointer's listeners, for every move and the release, wherever they happen.
        let (state, rows) = (self.state.clone(), self.rows.clone());
        let listeners = canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let (state_move, rows) = (state.clone(), rows.clone());
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        let held = e.pressed_button == Some(gpui::MouseButton::Left);
                        state_move.update(cx, |s, cx| s.moved(e.position, held, &rows, cx));
                    }
                });
                let state_up = state.clone();
                window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture && e.button == gpui::MouseButton::Left {
                        state_up.update(cx, |s, cx| {
                            s.pointer = e.position;
                            s.released(cx)
                        });
                    }
                });
            },
        )
        .absolute()
        .size_0();
        let ghost = s.dragging.then(|| ghost(press, s.pointer, bounds.origin, cx)).flatten();
        let layer = div()
            .w(bounds.size.width)
            .h(bounds.size.height)
            .relative()
            .child(listeners)
            // While dragging nothing under the pointer takes hover (Chromium sends the page no
            // mouse events during a drag).
            .when(s.dragging, |this| this.child(div().id("reorder-shield").absolute().inset_0().occlude()))
            .children(ghost);
        Some(deferred(Placed::new(Placement::At(bounds.origin)).child(layer)).with_priority(20).into_any_element())
    }
}

/// The dragged row under the pointer, where the pointer took it (Blink's drag image: the row as it
/// looked, hovered, at 75 %).
fn ghost(press: &Press, pointer: Point<Pixels>, origin: Point<Pixels>, cx: &App) -> Option<AnyElement> {
    let channel = channels(press.server, cx).into_iter().find(|c| c.id == press.channel)?;
    let t = &cx.theme().chrome;
    let scaled = &cx.theme().scaled;
    let scale = scaled.list_row_avatar_scale;
    let text = scaled.text_channel;
    let (top_left, width) = match press.row {
        Some(row) => (pointer - (press.origin - row.origin), row.size.width),
        None => {
            // A row's height: `py` 7 × density around the glyph slot or the line, whichever is taller.
            let height = 14. * scale + (text * 1.45).round().max(text * 1.5);
            let sidebar = Model::global(cx).layout.read(cx).prefs().sidebar_width as f32;
            (pointer - point(px(40.), px(height / 2.)), px(sidebar - 16.))
        }
    };
    let at = top_left - origin;
    let content: AnyElement = match channel.kind {
        ChannelKind::Category => div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .child(Icon::new(IconName::SettingsChevronDown).size(px(10.)).color(t.color_text_muted))
            .child(
                Label::new(SharedString::from(channel.name.to_uppercase()))
                    .font(FontRole::Section)
                    .size(scaled.text_section)
                    .line_height(scaled.text_section)
                    .weight(500.)
                    .tracking(cx.theme().palette.tracking_section)
                    .color(t.color_text_secondary)
                    .truncate(),
            )
            .into_any_element(),
        kind => {
            let slot = (text * 1.45).round();
            let icon: AnyElement = if kind == ChannelKind::Voice {
                Icon::new(IconName::SettingsSpeaker).size(px(slot * 0.82)).color(t.color_text_muted).into_any_element()
            } else {
                Label::new("#").font(FontRole::Channel).size(slot).line_height(slot).weight(500.).color(t.color_text_muted).into_any_element()
            };
            div()
                .flex()
                .items_center()
                .py(px(7. * scale))
                .px(px(10. * scale))
                .gap(px(8. * scale))
                .rounded(cx.theme().radius_sm)
                .bg(t.color_surface_hover)
                .child(div().flex().flex_none().size(px(slot)).items_center().justify_center().child(icon))
                .child(div().min_w_0().child(Label::new(channel.name.clone()).font(FontRole::Channel).size(text).color(t.color_text_primary).truncate()))
                .into_any_element()
        }
    };
    Some(
        div()
            .absolute()
            .left(at.x)
            .top(at.y)
            .w(width)
            .opacity(DRAG_IMAGE_ALPHA)
            .child(content)
            .into_any_element(),
    )
}

// ── The sync prompt ──────────────────────────────────────────────────────────────────────

/// `CategorySyncPrompt`: over everything, while a drop waits for a choice.
pub struct SyncPromptOverlay {
    state: Entity<DragState>,
    _watch: Watch,
}

impl SyncPromptOverlay {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state = state(cx);
        let mut watch = Watch::new();
        // Only the prompt redraws this (not every pointer move of a drag).
        let mut last: Option<SyncPrompt> = None;
        watch.store_if(&state, cx, move |_, s, _| {
            let next = s.prompt.clone();
            std::mem::replace(&mut last, next.clone()) != next
        });
        let model = Model::global(cx).clone();
        watch.store(&model.communities, cx);
        Self { state, _watch: watch }
    }

    fn close(&self, cx: &mut App) {
        self.state.update(cx, |s, cx| {
            if let Some(p) = s.prompt.as_mut() {
                p.open = false;
                cx.notify();
            }
        });
    }
}

impl Render for SyncPromptOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prompt = self.state.read(cx).prompt.clone();
        let open = prompt.as_ref().is_some_and(|p| p.open);
        let model = Model::global(cx).clone();
        let (label, category_name, can_sync) = match &prompt {
            Some(p) => {
                let channels = channels(p.server, cx);
                let channel = channels.iter().find(|c| c.id == p.channel);
                let category = channels.iter().find(|c| c.id == p.category);
                let label: SharedString = match channel {
                    Some(c) if c.kind == ChannelKind::Text => format!("#{}", c.name).into(),
                    Some(c) => c.name.clone(),
                    None => "this channel".into(),
                };
                // Syncing rewrites the channel's overwrites: MANAGE_ROLES in it, as on the server.
                let can_sync = model.channel_permission(p.server, &p.channel, perm::MANAGE_ROLES, cx);
                (label, category.map(|c| c.name.clone()).unwrap_or_else(|| "this category".into()), can_sync)
            }
            None => ("this channel".into(), "this category".into(), false),
        };
        let t = cx.theme();
        let (secondary, muted) = (t.color_text_secondary, t.color_text_muted);
        let ids = prompt.clone();
        let sync_ids = prompt.clone();
        ConfirmModal::new("category-sync", open, format!("Move {label} into {category_name}?"), "Sync permissions")
            .tone(ConfirmTone::Accent)
            .confirm_disabled(!can_sync)
            .secondary("Keep permissions", cx.listener(move |this, _, _, cx| {
                if let Some(p) = ids.clone().filter(|p| p.open) {
                    commit(p.server, p.ids.clone(), None, cx);
                    this.close(cx);
                }
            }))
            .on_confirm(cx.listener(move |this, _, _, cx| {
                if let Some(p) = sync_ids.clone().filter(|p| p.open) {
                    commit(p.server, p.ids.clone(), Some((p.channel.clone(), p.category.clone())), cx);
                    this.close(cx);
                }
            }))
            .on_cancel(cx.listener(|this, _, _, cx| this.close(cx)))
            .child(
                div().mt_2().child(
                    Label::new(SharedString::from(format!(
                        "{category_name} has permissions its channels can follow. Sync {label} to use them, or keep the permissions it has now."
                    )))
                    .size(13.)
                    .line_height(13. * 1.55)
                    .color(secondary),
                ),
            )
            .when(!can_sync, |this| {
                this.child(
                    div().mt_2().child(
                        Label::new(SharedString::from(format!("Syncing needs Manage Roles in {label}.")))
                            .size(12.)
                            .line_height(12. * 1.55)
                            .color(muted),
                    ),
                )
            })
            // Where the body ends (the self-tests find the buttons under it).
            .children(decibell_shell::probes::probe("sync-prompt-body").map(|p| div().relative().w_full().h_0().child(p)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(id: &str, kind: ChannelKind, sync: bool) -> Channel {
        Channel {
            id: id.into(),
            name: id.into(),
            kind,
            voice_bitrate_kbps: 0,
            retention_days_text: 0,
            retention_days_image: 0,
            retention_days_video: 0,
            retention_days_document: 0,
            retention_days_audio: 0,
            my_permissions: 0,
            slowmode_seconds: 0,
            encrypted: false,
            permissions_synced: false,
            category_sync: sync,
            topic: SharedString::default(),
        }
    }

    use ChannelKind::{Category as C, Text as T, Voice as V};

    /// general, chat | [cat-a: a1, a-voice] | [cat-b (syncing): b1]
    fn list() -> Vec<Channel> {
        vec![
            ch("general", T, false),
            ch("lounge", V, false),
            ch("cat-a", C, false),
            ch("a1", T, false),
            ch("a-voice", V, false),
            ch("cat-b", C, true),
            ch("b1", T, false),
        ]
    }

    fn ids(v: &[&str]) -> Vec<ChannelId> {
        v.iter().map(|s| ChannelId::from(*s)).collect()
    }

    #[test]
    fn the_drop_point_follows_the_hovered_half() {
        let l = list();
        assert_eq!(drop_point(&l, "a1", Some(("b1", true))), Some(Before::Id("b1".into())));
        assert_eq!(drop_point(&l, "a1", Some(("b1", false))), Some(Before::End));
        // Over no row inside the list: the end.
        assert_eq!(drop_point(&l, "a1", None), Some(Before::End));
        // Its own top half: before itself, which isn't a spot.
        assert_eq!(drop_point(&l, "a1", Some(("a1", true))), None);
    }

    #[test]
    fn text_stays_above_voice() {
        let l = list();
        // A text channel below a voice channel of its group: no line.
        assert_eq!(drop_point(&l, "b1", Some(("a-voice", false))), None);
        assert_eq!(drop_point(&l, "b1", Some(("a-voice", true))), Some(Before::Id("a-voice".into())));
        // A voice channel above a text one: no line.
        assert_eq!(drop_point(&l, "lounge", Some(("general", true))), None);
        assert_eq!(drop_point(&l, "lounge", Some(("b1", false))), Some(Before::End));
    }

    #[test]
    fn categories_snap_to_block_boundaries() {
        let l = list();
        assert_eq!(drop_point(&l, "cat-b", Some(("general", true))), Some(Before::Id("cat-a".into())));
        assert_eq!(drop_point(&l, "cat-b", Some(("a1", false))), Some(Before::Id("cat-b".into())));
        assert_eq!(drop_point(&l, "cat-a", Some(("b1", true))), Some(Before::End));
    }

    #[test]
    fn a_category_moves_with_its_block() {
        let l = list();
        assert_eq!(
            reorder(&l, "cat-b", &Before::Id("cat-a".into())),
            Some(ids(&["general", "lounge", "cat-b", "b1", "cat-a", "a1", "a-voice"]))
        );
        // Onto itself: nothing.
        assert_eq!(reorder(&l, "cat-b", &Before::Id("cat-b".into())), None);
        assert_eq!(reorder(&l, "b1", &Before::End), None);
    }

    #[test]
    fn a_drop_into_a_syncing_category_asks() {
        let l = list();
        assert_eq!(
            resolve_drop(&l, "a1", &Before::End),
            Some(DropAction::Ask { ids: ids(&["general", "lounge", "cat-a", "a-voice", "cat-b", "b1", "a1"]), channel: "a1".into(), category: "cat-b".into() })
        );
        // Into a plain category, or within its own: a move.
        assert_eq!(
            resolve_drop(&l, "b1", &Before::Id("a1".into())),
            Some(DropAction::Commit(ids(&["general", "lounge", "cat-a", "b1", "a1", "a-voice", "cat-b"])))
        );
        // A category never asks.
        assert!(matches!(resolve_drop(&l, "cat-a", &Before::End), Some(DropAction::Commit(_))));
    }
}
