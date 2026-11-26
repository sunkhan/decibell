//! [`MessageList`]: the view that owns one list's state. Its `render` returns one custom
//! element ([`super::element::ListElement`]) plus the few overlays it shows itself (the
//! jump-to-present pill, the message menu); everything per frame happens in that element's
//! `prepaint` / `paint`, which call back into the methods here.
//!
//! What a frame does while only the offset changes: read the animator's position, binary-search
//! the first visible row, paint each visible row from its cached [`RowLayout`]. No layout, no
//! shaping, no allocation of ours (the design is in `gpui-client/CONVENTIONS.md`, "Chat").

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use decibell_common::{display_name, person_name_color, user_name, want_member};
use decibell_model::chat::{Changes, MessageSlice, RowKey, ScrollPosition};
use decibell_model::stores::settings::TimeFormat;
use decibell_model::stores::users::NamesChanged;
use decibell_model::{Model, Uid, actions};
use decibell_text::Engine;
use decibell_ui::identity::{avatar_color, person_slot};
use decibell_ui::scroll::{LINE_STEP_PX, ScrollAnimator, ScrollKind};
use decibell_ui::{ActiveTheme as _, ContextMenu, FontRole, Icon, IconName, Label, MenuEntry, MenuItem};
use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Hitbox, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollDelta, ScrollEvent, Subscription,
    Task, Window, div, point, prelude::*, px,
};

use super::core::{ListCore, OVERDRAW_PX, Placement, RETAIN_PX, Side};
use super::element::ListElement;
use crate::media::{SlotSpace, slot_epoch, slots};
use crate::row::{ReplySpec, RowAction, RowLayout, RowPos, RowSpec, Target};
use crate::source::{Jump, ListSource};
use crate::style::{BAR, ChatStyle};

/// Rows further than this above the live bottom surface the pill (`JUMP_PILL_ROWS`).
const JUMP_PILL_ROWS: usize = 20;
/// The jump landing's slide (`SLIDE_PX`, `SLIDE_MS`, ease-out).
const SLIDE_PX: f32 = 28.;
const SLIDE: Duration = Duration::from_millis(260);
/// How long a jump target stays highlighted, and a code block's "copied" tick shows.
const FLASH: Duration = Duration::from_millis(1600);
const COPIED: Duration = Duration::from_millis(1500);
/// The longest one idle slice of background measuring may take.
const SLICE_BUDGET: Duration = Duration::from_micros(1500);
/// `historyRequests.RETRY_DELAY_MS`.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// What the list asks of its owner.
#[derive(Clone, Debug, PartialEq)]
pub enum ListEvent {
    /// The reply arrow on a message: the composer's reply target.
    Reply(i64),
    /// The trash on a message (`skip_confirm`: Shift was held).
    Delete { id: i64, skip_confirm: bool },
    /// A link was clicked (not an invite: those open the join confirmation themselves).
    OpenLink(String),
    /// A media placeholder was clicked (the viewer's, once attachments are drawn).
    OpenMedia(i64),
}

/// One end of the selection: a row and a caret in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caret {
    pub key: RowKey,
    /// The row's index when last looked up (verified against `key` before use).
    pub index: usize,
    pub pos: RowPos,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    None,
    /// Selecting text.
    Select,
    /// Dragging the scrollbar thumb: the grab offset inside the thumb.
    Bar(f32),
}

/// What the last frame drew: for hit-testing between frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct Drawn {
    /// The scroll position, on whole device pixels.
    pub position: f64,
    pub first: usize,
    pub last: usize,
    /// The jump slide's offset, px.
    pub slide: f32,
    /// The animator's position for this frame, before snapping.
    pub exact: f64,
}

/// Per-frame counters for the benchmark and the self-tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct ListStats {
    /// Rows laid out since the list was made (cold layouts and re-measures).
    pub layouts: u64,
    /// µs of the last frame's prepaint (measuring included) and paint.
    pub prepaint_us: u32,
    pub paint_us: u32,
    pub frames: u64,
    /// µs of deferred work (pages, trims) and measuring slices since the last frame.
    pub between_us: u32,
}

pub struct MessageList {
    pub(super) source: Rc<dyn ListSource>,
    pub(super) slice: Entity<MessageSlice>,
    pub(super) core: ListCore,
    pub(super) style: Option<Rc<ChatStyle>>,
    style_stamp: (usize, u32, u32, u32),
    /// The width rows are laid out for (the bounds less the scrollbar).
    pub(super) width: f32,
    pub(super) bounds: Bounds<Pixels>,
    /// The pane's size (the renderer's `chatViewSize`); zero: the list's own bounds.
    view_size: Rc<Cell<(f32, f32)>>,
    placement: Placement,
    pub(super) focus: FocusHandle,
    pub(super) hover: Option<(usize, Target)>,
    pub(super) selection: Option<(Caret, Caret)>,
    drag: Drag,
    pressed: Option<(RowKey, Target)>,
    pub(super) highlighted: Option<i64>,
    highlight_task: Option<Task<()>>,
    pub(super) reply_target: Option<i64>,
    pub(super) editing: Option<i64>,
    pub(super) copied: Option<(RowKey, u32)>,
    copied_task: Option<Task<()>>,
    /// Scroll offsets of code blocks that were scrolled: (row, span, x, y).
    pub(super) code_scroll: Vec<(RowKey, u32, f32, f32)>,
    menu: Option<(Point<Pixels>, RowKey)>,
    pending_jump: Option<i64>,
    jump_task: Option<Task<()>>,
    slide: Option<(Instant, f32)>,
    pill: bool,
    asked_older: i64,
    asked_newer: i64,
    retry: Option<Task<()>>,
    measuring: Option<Task<()>>,
    after_frame: bool,
    pub(super) drawn: Drawn,
    /// Senders whose presence and picture the visible rows keep watched.
    retained: Vec<Uid>,
    senders_scratch: Vec<Uid>,
    last_frame: Option<Instant>,
    pointer: Point<Pixels>,
    today: i32,
    time_format: TimeFormat,
    pub stats: ListStats,
    copy_sink: Option<Rc<std::cell::RefCell<Option<String>>>>,
    last_input: Option<Instant>,
    pub(super) bench: Option<crate::bench::Recording>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ListEvent> for MessageList {}

/// A future that lets the main thread's other work (input, frames) run once.
struct YieldNow(bool);

impl Future for YieldNow {
    type Output = ();
    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        if self.0 {
            return std::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

fn ease_out(t: f32) -> f32 {
    // CSS `ease-out`: cubic-bezier(0, 0, 0.58, 1).
    decibell_ui::motion::CubicBezier::new(0., 0., 0.58, 1.).y(t as f64) as f32
}

impl MessageList {
    pub fn new(source: Rc<dyn ListSource>, cx: &mut Context<Self>) -> Self {
        let slice = source.slice(cx);
        let saved = slice.read(cx).scroll();
        let placement = match saved {
            Some(ScrollPosition { at_bottom: false, anchor_id, offset }) if anchor_id > 0 => Placement::Restore { id: anchor_id, offset },
            _ => Placement::Bottom,
        };
        let mut subscriptions = vec![cx.observe(&slice, |_, _, cx| cx.notify())];
        if let Some(model) = cx.try_global::<Model>().cloned() {
            // Names and the clock format are baked into row layouts: build those rows again.
            subscriptions.push(cx.subscribe(&model.users, |this, _, changed: &NamesChanged, cx| {
                if this.invalidate_where(|uid| changed.0.contains(&uid)) {
                    cx.notify();
                }
            }));
            subscriptions.push(cx.observe(&model.settings, |this, settings, cx| {
                let format = settings.read(cx).time_format();
                if format != this.time_format {
                    this.time_format = format;
                    this.invalidate_where(|_| true);
                    cx.notify();
                }
            }));
        }
        // Nicknames live in the community's roster, which notifies for every delta: only rows
        // whose shown name really changed are built again.
        if let Some(server) = source.server()
            && let Some(community) = cx.try_global::<Model>().cloned().and_then(|model| model.community(server, cx))
        {
            subscriptions.push(cx.observe(&community.roster, move |this, _, cx| {
                let mut any = false;
                for entry in &mut this.core.entries {
                    let stale = entry.layout.as_ref().is_some_and(|layout| {
                        !layout.grouped && display_name(Some(server), entry.message.sender_uid, &entry.message.sender, cx) != layout.shown_name
                    });
                    if stale {
                        entry.invalidate();
                        any = true;
                    }
                }
                if any {
                    this.core.inexact = this.core.entries.iter().filter(|entry| !entry.exact).count();
                    cx.notify();
                }
            }));
        }
        let epoch = slot_epoch(cx);
        subscriptions.push(cx.observe(&epoch, |this, _, cx| {
            this.invalidate_where(|_| true);
            cx.notify();
        }));
        let time_format = cx.try_global::<Model>().map_or(TimeFormat::Auto, |model| model.settings.read(cx).time_format());
        cx.on_release(|this, cx| this.leave(cx)).detach();
        Self {
            source,
            slice,
            core: ListCore::starting(placement == Placement::Bottom),
            style: None,
            style_stamp: (usize::MAX, 0, 0, 0),
            width: 0.,
            bounds: Bounds::default(),
            view_size: Rc::new(Cell::new((0., 0.))),
            placement,
            focus: cx.focus_handle(),
            hover: None,
            selection: None,
            drag: Drag::None,
            pressed: None,
            highlighted: None,
            highlight_task: None,
            reply_target: None,
            editing: None,
            copied: None,
            copied_task: None,
            code_scroll: Vec::new(),
            menu: None,
            pending_jump: None,
            jump_task: None,
            slide: None,
            pill: false,
            asked_older: 0,
            asked_newer: 0,
            retry: None,
            measuring: None,
            after_frame: false,
            drawn: Drawn::default(),
            retained: Vec::new(),
            senders_scratch: Vec::new(),
            last_frame: None,
            pointer: Point::default(),
            today: decibell_model::chat::time::local_day(chrono_now()),
            time_format,
            stats: ListStats::default(),
            copy_sink: None,
            last_input: None,
            bench: None,
            _subscriptions: subscriptions,
        }
    }

    /// The cell the pane writes its size into (the renderer's `chatViewSize`: media is scaled
    /// against the pane, not the list, so a growing composer resizes nothing).
    pub fn view_size_cell(&self) -> Rc<Cell<(f32, f32)>> {
        self.view_size.clone()
    }

    /// Pins "today" for the header times (screens and tests: the clock must not show).
    pub fn set_today(&mut self, day: i32, cx: &mut Context<Self>) {
        if self.today != day {
            self.today = day;
            self.invalidate_where(|_| true);
            cx.notify();
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The composer is replying to this message (its row shows the rail).
    pub fn set_reply_target(&mut self, id: Option<i64>, cx: &mut Context<Self>) {
        if self.reply_target != id {
            self.reply_target = id;
            cx.notify();
        }
    }

    fn windowed(&self, cx: &App) -> bool {
        self.slice.read(cx).list_windowed()
    }

    fn now(cx: &App) -> Instant {
        cx.background_executor().now()
    }

    /// Leaving the channel: the slice remembers where we were; the watched senders are let go.
    fn leave(&mut self, cx: &mut App) {
        let scale = self.style.as_ref().map_or(1., |style| style.scale as f64);
        let (at_bottom, anchor_id, offset) = self.core.position(Self::now(cx), scale);
        self.slice.update(cx, |slice, _| slice.set_scroll(ScrollPosition { at_bottom, anchor_id, offset }));
        if !self.retained.is_empty() {
            actions::presence::release(std::mem::take(&mut self.retained), cx);
        }
    }

    fn invalidate_where(&mut self, mut sender: impl FnMut(Uid) -> bool) -> bool {
        let mut any = false;
        for entry in &mut self.core.entries {
            let reply = entry.message.reply.as_ref().map_or(0, |reply| reply.sender_uid);
            if sender(entry.message.sender_uid) || (reply != 0 && sender(reply)) {
                entry.invalidate();
                any = true;
            }
        }
        if any {
            self.core.inexact = self.core.entries.iter().filter(|entry| !entry.exact).count();
        }
        any
    }

    // ── Bringing the entries in step with the slice ───────────────────────────────────────────

    /// Applies what the slice logged since the last frame. Returns whether rows changed.
    fn sync(&mut self, now: Instant, cx: &mut Context<Self>) -> bool {
        let slice = self.slice.read(cx);
        let revision = slice.revision();
        let first = self.core.seen == 0 && self.core.is_empty();
        if revision == self.core.seen && !first {
            return false;
        }
        let pinned = !slice.list_windowed();
        // A jump lands on the first commit whose rows hold its target.
        if let Some(id) = self.pending_jump
            && slice.find(id).is_some()
        {
            self.placement = Placement::Center { id };
        }
        let placement = self.placement.clone();
        match slice.changes_since(self.core.seen) {
            Changes::Ops(ops) if !first => self.core.apply(slice.rows(), slice.facts(), Some(ops), &placement, pinned, now),
            _ => self.core.apply(slice.rows(), slice.facts(), None, &placement, pinned, now),
        }
        self.core.seen = revision;
        if matches!(placement, Placement::Bottom) {
            self.core.at_bottom = true;
        }
        // The paging guards are per boundary: new rows, new boundary.
        self.asked_older = 0;
        self.asked_newer = 0;
        self.relocate_selection();
        self.hover = None;
        self.code_scroll.retain(|(key, ..)| self.core.entries.iter().any(|entry| &entry.key == key));
        true
    }

    fn relocate_selection(&mut self) {
        let Some((mut anchor, mut focus)) = self.selection.take() else { return };
        for caret in [&mut anchor, &mut focus] {
            if self.core.entries.get(caret.index).is_none_or(|entry| entry.key != caret.key) {
                match self.core.index_of(&caret.key) {
                    Some(index) => caret.index = index,
                    // The row left the slice (a trim far away, a delete): the selection is gone.
                    None => return,
                }
            }
        }
        self.selection = Some((anchor, focus));
    }

    // ── Measuring ──────────────────────────────────────────────────────────────────────────────

    fn slot_space(&self) -> SlotSpace {
        let (view_width, view_height) = match self.view_size.get() {
            (w, h) if w > 0. => (w, h),
            // No pane told us: the list's own box is the view (`COMPOSER_REST_PX` is the pane's).
            _ => (f32::from(self.bounds.size.width).round(), f32::from(self.bounds.size.height).round()),
        };
        SlotSpace { width: self.width, view_width, view_height, scale: self.style.as_ref().map_or(1., |style| style.scale) }
    }

    /// Lays row `index` out (its height becomes exact).
    fn build_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(style) = self.style.clone() else { return };
        let engine = Engine::global(cx);
        let server = self.source.server();
        let entry = &self.core.entries[index];
        let message = entry.message.clone();
        let (facts, expanded) = (entry.facts, entry.expanded);
        let shown = display_name(server, message.sender_uid, &message.sender, cx);
        let account = user_name(message.sender_uid, &message.sender, cx);
        if let (Some(server), false) = (server, facts.grouped) {
            want_member(server, message.sender_uid, cx);
        }
        let reply = message.reply.as_ref().map(|reply| {
            // The parent as loaded wins over the preview the message carries (an edit shows).
            let slice = self.slice.read(cx);
            let (uid, sender, content, kinds) = match slice.find(reply.id) {
                Some(parent) => (
                    parent.sender_uid,
                    parent.sender.clone(),
                    parent.content.clone(),
                    parent.attachments.iter().map(|attachment| attachment.kind).collect::<Arc<[_]>>(),
                ),
                None => (reply.sender_uid, reply.sender.clone(), reply.content.clone(), reply.attachment_kinds.clone()),
            };
            let author = (!sender.is_empty()).then(|| (uid, display_name(server, uid, &sender, cx), user_name(uid, &sender, cx)));
            ReplySpec { parent: reply.id, author, content, kinds }
        });
        let colors = |uid: Uid, name: &str, cx: &App| (person_name_color(uid, name, cx), avatar_color(cx.theme(), person_slot(uid, name)));
        let (name_color, av_color) = colors(message.sender_uid, &account, cx);
        let reply_colors = match reply.as_ref().and_then(|reply| reply.author.as_ref()) {
            Some((uid, _, account)) => colors(*uid, account, cx),
            None => (name_color, av_color),
        };
        let editing = self.editing == Some(message.id) && message.id > 0;
        let space = self.slot_space();
        let registry = slots(cx);
        let (attachments, inflight, embeds, editor) =
            (registry.attachments.clone(), registry.inflight.clone(), registry.embeds.clone(), registry.editor.clone());
        let slot_height = |slot: &Option<Rc<dyn crate::media::RowSlot>>, cx: &App| slot.as_ref().map(|slot| slot.height(&message, server, space, cx));
        let editor_height = match (&editor, editing) {
            (Some(editor), true) => editor.height(&message, space.width, cx),
            (None, true) => crate::list::element::placeholder_editor_height(&style),
            _ => 0.,
        };
        let spec = RowSpec {
            message: &message,
            facts,
            server,
            display_name: shown,
            user_name: account,
            name_color,
            avatar_color: av_color,
            reply,
            reply_colors,
            expanded,
            text_hidden: false,
            editing,
            editor_height,
            today: self.today,
            time_format: self.time_format,
            space,
            slot_heights: [slot_height(&attachments, cx), slot_height(&inflight, cx), slot_height(&embeds, cx)],
        };
        let view = cx.weak_entity();
        let ready = move || -> Box<dyn FnOnce(&mut App)> {
            let view = view.clone();
            Box::new(move |cx| {
                let _ = view.update(cx, |this, cx| this.code_highlighted(cx));
            })
        };
        let layout = RowLayout::build(&spec, self.width, &style, &engine, cx, &ready);
        let entry = &mut self.core.entries[index];
        entry.height = layout.height;
        entry.exact = true;
        entry.layout = Some(Rc::new(layout));
        self.stats.layouts += 1;
    }

    /// A code block's colours arrived: its row is built again (same height, new paint).
    fn code_highlighted(&mut self, cx: &mut Context<Self>) {
        for entry in &mut self.core.entries {
            if entry.layout.as_ref().is_some_and(|layout| layout.codes.iter().any(|code| !code.highlighted && code.language.is_some())) {
                entry.layout = None;
            }
        }
        cx.notify();
    }

    /// Measures every row in and around the viewport, until the viewport's rows are stable.
    fn measure_visible(&mut self, now: Instant, cx: &mut Context<Self>) {
        let pinned = !self.windowed(cx);
        for _ in 0..8 {
            let position = self.core.scroller.position(now);
            let range = self.core.rows_between(position - OVERDRAW_PX, position + self.core.viewport_height + OVERDRAW_PX);
            let mut heights = false;
            for index in range {
                let entry = &self.core.entries[index];
                if entry.exact && entry.layout.is_some() {
                    continue;
                }
                let (was, known) = (entry.height, entry.exact);
                self.build_row(index, cx);
                heights |= !known || self.core.entries[index].height != was;
            }
            if !heights {
                break;
            }
            self.core.heights_changed(pinned, now);
            self.place(now);
        }
    }

    /// Re-applies a placement that depends on heights (a centred jump target, the bottom).
    fn place(&mut self, now: Instant) {
        match self.placement {
            Placement::Center { id } => self.core.center(id),
            Placement::Restore { id, offset } => self.core.restore(id, offset, now),
            Placement::Bottom => {
                let (_, max) = self.core.scroller.range();
                self.core.scroller.jump_to(max);
            }
            _ => {}
        }
    }

    /// One idle slice: the unmeasured rows nearest the viewport first. Returns whether more remain.
    fn measure_slice(&mut self, cx: &mut Context<Self>) -> bool {
        let started = Instant::now();
        let now = Self::now(cx);
        let pinned = !self.windowed(cx);
        let Some((anchor, _)) = self.core.anchor(now) else {
            self.measuring = None;
            return false;
        };
        let len = self.core.len();
        let mut measured = false;
        // Outward from the anchor: below it first (the way a reader of history scrolls back down
        // matters less than the page that just arrived above, but both are a few slices).
        let mut step = 0usize;
        while started.elapsed() < SLICE_BUDGET {
            let (below, above) = (anchor + step, anchor.checked_sub(step + 1));
            if below >= len && above.is_none() {
                break;
            }
            for index in [Some(below).filter(|index| *index < len), above].into_iter().flatten() {
                if !self.core.entries[index].exact {
                    self.build_row(index, cx);
                    // A far row keeps only its height: its layout is rebuilt if it scrolls in.
                    self.core.entries[index].layout = None;
                    measured = true;
                }
            }
            step += 1;
        }
        if measured {
            self.core.heights_changed(pinned, now);
        }
        self.stats.between_us += started.elapsed().as_micros() as u32;
        if self.core.inexact > 0 {
            return true;
        }
        self.measuring = None;
        // The range settled: one frame for the scrollbar, and the edges / trims look again.
        cx.notify();
        false
    }

    fn schedule_measuring(&mut self, cx: &mut Context<Self>) {
        if self.measuring.is_some() || self.core.inexact == 0 {
            return;
        }
        self.measuring = Some(cx.spawn(async move |this, cx| {
            loop {
                YieldNow(false).await;
                if !this.update(cx, |this, cx| this.measure_slice(cx)).unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    // ── The frame (called by the element) ─────────────────────────────────────────────────────

    /// Everything that must be true before painting: the style, the rows in step with the
    /// slice, the visible rows measured, the position.
    pub(super) fn frame_prepaint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let started = Instant::now();
        let now = Self::now(cx);
        let scale = window.scale_factor();
        let stamp = {
            let theme = cx.theme();
            (theme.id.index(), theme.scaled.text_body.to_bits(), theme.scaled.list_row_avatar_scale.to_bits(), scale.to_bits())
        };
        if stamp != self.style_stamp {
            self.style_stamp = stamp;
            self.style = Some(ChatStyle::new(scale, cx));
            self.invalidate_where(|_| true);
        }
        self.bounds = bounds;
        let changed = self.sync(now, cx);
        let pinned = !self.windowed(cx);

        // The bar takes its width from the rows while the content overflows (a classic bar).
        let height = f32::from(bounds.size.height) as f64;
        let full = f32::from(bounds.size.width);
        let overflowing = self.core.content_height() > height + 0.5;
        let width = (full - if overflowing { BAR } else { 0. }).max(1.);
        if width != self.width {
            self.width = width;
            self.invalidate_where(|_| true);
        }
        self.core.set_viewport(height, pinned, now);
        self.autoscroll(now);
        if let Some(bench) = &mut self.bench {
            // The benchmark's drive: a constant speed, applied per frame like a touchpad's deltas.
            let delta = bench.frame_started(self.stats.layouts);
            self.core.scroller.precise(now, -delta);
            window.request_animation_frame();
        }
        self.measure_visible(now, cx);
        if self.core.content_height() > height + 0.5 && !overflowing && self.width == full {
            // Measuring made it overflow: the rows get the narrower width in this same frame.
            self.width = (full - BAR).max(1.);
            self.invalidate_where(|_| true);
            self.measure_visible(now, cx);
        }

        // A placement holds until its rows are measured; then the anchor rule takes over.
        if !matches!(self.placement, Placement::Hold) && !self.core.is_empty() {
            if let Placement::Center { id } = self.placement
                && self.pending_jump == Some(id)
            {
                self.landed(id, now, cx);
            }
            self.placement = Placement::Hold;
        }

        let edges = self.core.edges(now);
        let position = self.core.scroller.position(now);
        let snapped = (position * scale as f64).round() / scale as f64;
        let range = self.core.rows_between(snapped, snapped + height);
        let slide = match self.slide {
            Some((start, from)) => {
                let t = now.saturating_duration_since(start).as_secs_f32() / SLIDE.as_secs_f32();
                if t >= 1. {
                    self.slide = None;
                    0.
                } else {
                    from * (1. - ease_out(t))
                }
            }
            None => 0.,
        };
        let moved = range.start != self.drawn.first || range.end != self.drawn.last || changed;
        self.drawn = Drawn { position: snapped, first: range.start, last: range.end, slide, exact: position };
        if moved {
            self.watch_senders(cx);
        }
        if self.core.scroller.is_animating() || self.slide.is_some() || self.drag == Drag::Select && self.autoscrolling() {
            window.request_animation_frame();
        }
        self.last_frame = Some(now);

        // What needs the model (paging, trims, the pill) runs after the frame, and only when
        // there is something to do: a plain scroll frame defers nothing.
        let windowed = !pinned;
        let pill = windowed || (!self.core.at_bottom && self.core.len().saturating_sub(1).saturating_sub(range.end.saturating_sub(1)) > JUMP_PILL_ROWS);
        let slice = self.slice.read(cx);
        let want_older = edges.near_top && slice.fetched && slice.has_more_before && slice.oldest_stored_id() != self.asked_older && slice.oldest_stored_id() != 0;
        let want_newer = edges.near_bottom && slice.has_more_after && !slice.resyncing && slice.newest_stored_id() != self.asked_newer && slice.newest_stored_id() != 0;
        let overflow = self.core.len() > super::core::MAX_ROWS && self.core.inexact == 0;
        if (want_older || want_newer || overflow || pill != self.pill || self.core.inexact > 0) && !self.after_frame {
            self.after_frame = true;
            cx.defer_in(window, |this, _, cx| this.frame_done(cx));
        }
        self.stats.prepaint_us = started.elapsed().as_micros() as u32;
        self.stats.frames += 1;
    }

    /// After a frame that found work: pages, trims, the pill, background measuring.
    fn frame_done(&mut self, cx: &mut Context<Self>) {
        self.after_frame = false;
        let started = Instant::now();
        let now = Self::now(cx);
        self.schedule_measuring(cx);
        let edges = self.core.edges(now);
        let (fetched, has_before, has_after, resyncing, oldest, newest, windowed) = {
            let slice = self.slice.read(cx);
            (slice.fetched, slice.has_more_before, slice.has_more_after, slice.resyncing, slice.oldest_stored_id(), slice.newest_stored_id(), slice.list_windowed())
        };
        let source = self.source.clone();
        if edges.near_top && fetched && has_before && oldest != 0 && oldest != self.asked_older {
            self.asked_older = oldest;
            let task = source.load_older(cx);
            self.watch_page(task, true, cx);
        }
        if edges.near_bottom && has_after && !resyncing && newest != 0 && newest != self.asked_newer {
            self.asked_newer = newest;
            let task = source.load_newer(cx);
            self.watch_page(task, false, cx);
        }
        if let Some((side, keep)) = self.core.overflow(now) {
            self.slice.update(cx, |slice, cx| match side {
                Side::Tail => slice.trim_tail(keep, cx),
                Side::Head => slice.trim_head(keep, cx),
            });
        }
        let last = self.drawn.last.saturating_sub(1);
        let pill = windowed || (!self.core.at_bottom && self.core.len().saturating_sub(1).saturating_sub(last) > JUMP_PILL_ROWS);
        if pill != self.pill {
            self.pill = pill;
            cx.notify();
        }
        self.stats.between_us += started.elapsed().as_micros() as u32;
    }

    /// A page request that gets no answer: the same boundary may be asked again, and the
    /// edges are looked at once more shortly (someone resting at the edge isn't left waiting).
    fn watch_page(&mut self, task: Option<Task<Option<bool>>>, older: bool, cx: &mut Context<Self>) {
        let Some(task) = task else { return };
        self.retry = Some(cx.spawn(async move |this, cx| {
            if task.await != Some(false) {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if older { this.asked_older = 0 } else { this.asked_newer = 0 }
                cx.notify();
            });
        }));
    }

    /// Keeps the senders on screen in the presence watch set (their pictures, renames and
    /// statuses arrive while they show): what a drawn `UserAvatar` does for itself.
    fn watch_senders(&mut self, cx: &mut Context<Self>) {
        // Into a buffer kept across frames: the usual answer is "the same people", with no
        // allocation on the way to it.
        let mut now = std::mem::take(&mut self.senders_scratch);
        now.clear();
        for entry in &self.core.entries[self.drawn.first.min(self.core.len())..self.drawn.last.min(self.core.len())] {
            let uid = entry.message.sender_uid;
            if uid > 0 && !entry.facts.grouped && !now.contains(&uid) {
                now.push(uid);
            }
        }
        if now.iter().all(|uid| self.retained.contains(uid)) && now.len() == self.retained.len() {
            self.senders_scratch = now;
            return;
        }
        let added: Vec<Uid> = now.iter().copied().filter(|uid| !self.retained.contains(uid)).collect();
        let gone: Vec<Uid> = self.retained.iter().copied().filter(|uid| !now.contains(uid)).collect();
        self.senders_scratch = std::mem::replace(&mut self.retained, now);
        cx.defer(move |cx| {
            if !added.is_empty() {
                actions::presence::retain(added.iter().copied(), cx);
                if let Some(model) = cx.try_global::<Model>().cloned() {
                    model.live.presence.avatars.update(cx, |avatars, cx| added.iter().for_each(|uid| avatars.fetch_if_needed(*uid, cx)));
                }
            }
            if !gone.is_empty() {
                actions::presence::release(gone, cx);
            }
        });
    }

    /// Drops the layouts of rows far from the viewport (their heights stay exact). Called when
    /// a scroll comes to rest.
    pub(super) fn release_far_layouts(&mut self) {
        let (top, bottom) = (self.drawn.position - RETAIN_PX, self.drawn.position + self.core.viewport_height + RETAIN_PX);
        let keep = self.core.rows_between(top, bottom);
        for (index, entry) in self.core.entries.iter_mut().enumerate() {
            if !keep.contains(&index) && entry.exact {
                entry.layout = None;
            }
        }
    }

    // ── Jumps ──────────────────────────────────────────────────────────────────────────────────

    /// Jump to a message and flash it (`jumpToMessage`): now if it is loaded, else on the
    /// commit that brings the window around it.
    pub fn jump_to_message(&mut self, id: i64, cx: &mut Context<Self>) {
        let (_, anchor, _) = self.core.position(Self::now(cx), 1.);
        // Travel direction: towards an older row slides down into place, a newer one up.
        let from = if anchor > 0 && id > anchor { SLIDE_PX } else { -SLIDE_PX };
        self.pending_jump = Some(id);
        self.slide = None;
        self.jump_task = None;
        let source = self.source.clone();
        match source.jump_to_message(id, cx) {
            Jump::Loaded => {
                self.placement = Placement::Center { id };
                self.jump_dir(from);
                // Its row and the ones around it are measured in the next frame, then centred.
                self.core.center(id);
            }
            Jump::Fetching(task) => {
                self.jump_dir(from);
                self.jump_task = Some(cx.spawn(async move |this, cx| {
                    if task.await != Some(true) {
                        let _ = this.update(cx, |this, _| {
                            if this.pending_jump == Some(id) {
                                this.pending_jump = None;
                            }
                        });
                    }
                }));
            }
            Jump::Waiting => self.jump_dir(from),
            Jump::Refused => self.pending_jump = None,
        }
        cx.notify();
    }

    fn jump_dir(&mut self, from: f32) {
        // Kept until the landing starts the slide.
        self.slide = Some((Instant::now() + Duration::from_secs(3600), from));
    }

    fn landed(&mut self, id: i64, now: Instant, cx: &mut Context<Self>) {
        self.pending_jump = None;
        if let Some((_, from)) = self.slide {
            self.slide = (!cx.reduce_motion()).then_some((now, from));
        }
        self.flash(id, cx);
        let source = self.source.clone();
        cx.defer(move |cx| source.jump_landed(cx));
    }

    fn flash(&mut self, id: i64, cx: &mut Context<Self>) {
        self.highlighted = Some(id);
        self.highlight_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH).await;
            let _ = this.update(cx, |this, cx| {
                this.highlighted = None;
                cx.notify();
            });
        }));
    }

    /// "Jump to present": the bottom when the history below is contiguous, else the newest page.
    pub fn jump_to_present(&mut self, cx: &mut Context<Self>) {
        if self.windowed(cx) {
            self.pending_jump = None;
            self.placement = Placement::Bottom;
            let source = self.source.clone();
            source.jump_to_present(cx);
        } else {
            // `scrollTo({ behavior: "smooth" })`: Chromium's programmatic smooth scroll, here the
            // key curve to the end.
            let now = Self::now(cx);
            let (_, max) = self.core.scroller.range();
            let delta = max - self.core.scroller.target();
            self.core.scroller.input(now, delta, ScrollKind::Key);
        }
        cx.notify();
    }

    /// The saved position as it would be written on leaving: (at bottom, anchor id, offset).
    pub fn position(&mut self, cx: &App) -> (bool, i64, f32) {
        let scale = self.style.as_ref().map_or(1., |style| style.scale as f64);
        self.core.position(Self::now(cx), scale)
    }

    /// Screen y (px from the list's top, on device pixels) of row `id`, for the self-tests.
    pub fn row_screen_y(&self, id: i64) -> Option<f32> {
        let index = self.core.index_of_id(id)?;
        let scale = self.style.as_ref()?.scale as f64;
        Some((((self.core.top(index) * scale).round() - (self.drawn.position * scale).round()) / scale) as f32)
    }

    /// (row id, top in content px relative to the first row, height) of every row: the dump
    /// compared with the Electron client's `offsetTop`s. Rows not yet measured are measured.
    pub fn row_tops(&mut self, cx: &mut Context<Self>) -> Vec<(i64, f64, f32)> {
        for index in 0..self.core.len() {
            if !self.core.entries[index].exact {
                self.build_row(index, cx);
                self.core.entries[index].layout = None;
            }
        }
        self.core.heights_changed(false, Self::now(cx));
        let first = if self.core.is_empty() { 0. } else { self.core.top(0) };
        (0..self.core.len()).map(|index| (self.core.entries[index].message.id, self.core.top(index) - first, self.core.entries[index].height)).collect()
    }

    /// Where the list was last drawn, in window coordinates.
    pub fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// The position the last frame drew (on device pixels) and that frame's time.
    pub fn drawn(&self) -> (f64, Option<Instant>) {
        (self.drawn.position, self.last_frame)
    }

    /// The animator's position at the last frame, before it was put on a device pixel.
    pub fn drawn_exact(&self) -> f64 {
        self.drawn.exact
    }

    /// The time the last wheel or key step was given to the animator.
    pub fn last_input(&self) -> Option<Instant> {
        self.last_input
    }

    /// Copies go to the returned cell instead of the system clipboard (tests must leave the
    /// user's clipboard alone).
    pub fn capture_copies(&mut self) -> Rc<std::cell::RefCell<Option<String>>> {
        let cell = Rc::new(std::cell::RefCell::new(None));
        self.copy_sink = Some(cell.clone());
        cell
    }

    fn copy(&self, text: String, cx: &mut App) {
        match &self.copy_sink {
            Some(sink) => *sink.borrow_mut() = Some(text),
            None => cx.write_to_clipboard(ClipboardItem::new_string(text)),
        }
    }

    /// Ids of the rows the last frame drew, top to bottom.
    pub fn visible_ids(&self) -> Vec<i64> {
        self.core.entries[self.drawn.first.min(self.core.len())..self.drawn.last.min(self.core.len())].iter().map(|entry| entry.message.id).collect()
    }

    /// A row's message and facts.
    pub fn row(&self, id: i64) -> Option<(Arc<decibell_model::chat::Message>, decibell_model::chat::RowFacts)> {
        let entry = &self.core.entries[self.core.index_of_id(id)?];
        Some((entry.message.clone(), entry.facts))
    }

    /// Drives the list at a constant speed (px/s, + = into history) from its own frames and
    /// records every frame, until [`Self::end_bench`]: the scroll benchmark.
    pub fn begin_bench(&mut self, speed: f64, cx: &mut Context<Self>) {
        self.bench = Some(crate::bench::Recording::new(speed));
        cx.notify();
    }

    pub fn end_bench(&mut self) -> Option<crate::bench::Recording> {
        self.bench.take()
    }

    /// A row's exact height, once measured.
    pub fn row_height(&self, id: i64) -> Option<f32> {
        let entry = &self.core.entries[self.core.index_of_id(id)?];
        entry.exact.then_some(entry.height)
    }

    /// The window position of a caret in row `id`'s body text (`fraction` of the way through
    /// its first line), for pointer-driven tests.
    pub fn body_point(&self, id: i64, fraction: f32) -> Option<Point<Pixels>> {
        let index = self.core.index_of_id(id)?;
        let layout = self.core.entries[index].layout.as_ref()?;
        let rich = layout.rich()?;
        let [x, y, ..] = layout.body_bounds();
        let (_, first) = rich.texts().next()?;
        let line = first.line(0)?;
        let row_y = self.row_screen_y(id)?;
        Some(point(self.bounds.left() + px(x + line.width * fraction), self.bounds.top() + px(row_y + y + line.height / 2.)))
    }

    pub fn scroll_range(&self) -> (f64, f64) {
        self.core.scroller.range()
    }

    pub fn scroll_position(&mut self, cx: &App) -> f64 {
        self.core.scroller.position(Self::now(cx))
    }

    pub fn is_animating(&self) -> bool {
        self.core.scroller.is_animating()
    }

    pub fn rows_unmeasured(&self) -> usize {
        self.core.inexact
    }

    pub fn row_count(&self) -> usize {
        self.core.len()
    }

    pub fn shows_pill(&self) -> bool {
        self.pill
    }

    pub fn at_bottom(&self) -> bool {
        self.core.at_bottom
    }

    /// Bytes the list keeps for its rows beyond the text engine's caches, and how many rows
    /// hold a layout.
    pub fn retained(&self) -> (usize, usize) {
        let layouts = self.core.entries.iter().filter_map(|entry| entry.layout.as_ref());
        let bytes: usize = layouts.clone().map(|layout| layout.retained_bytes()).sum();
        (bytes + self.core.entries.capacity() * size_of::<super::core::Entry>(), layouts.count())
    }

    /// Scrolls by `delta` px at once (a precise delta: the benchmark's constant-speed drive).
    pub fn scroll_by_precise(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.core.scroller.precise(Self::now(cx), delta);
        cx.notify();
    }

    // ── Pointer ────────────────────────────────────────────────────────────────────────────────

    /// The row under a window position and the point in that row's coordinates. Clamps to the
    /// nearest row for a point above or below the rows (a drag that left the list).
    fn locate(&self, position: Point<Pixels>) -> Option<(usize, f32, f32)> {
        if self.core.is_empty() {
            return None;
        }
        let scale = self.style.as_ref()?.scale as f64;
        let y = f32::from(position.y - self.bounds.top()) - self.drawn.slide;
        let content_y = self.drawn.position + y as f64;
        let index = self.core.row_at(content_y);
        let row_top = ((self.core.top(index) * scale).round() - (self.drawn.position * scale).round()) / scale;
        Some((index, f32::from(position.x - self.bounds.left()), y - row_top as f32))
    }

    fn can_delete(&self, index: usize, cx: &App) -> bool {
        self.core.entries[index].facts.can_delete(self.source.can_delete_others(cx))
    }

    fn hit(&self, position: Point<Pixels>, cx: &App) -> Option<(usize, crate::row::RowHit)> {
        let (index, x, y) = self.locate(position)?;
        let layout = self.core.entries[index].layout.as_ref()?;
        let hovered = self.hover.is_some_and(|(row, _)| row == index);
        Some((index, layout.hit(x, y, self.width, hovered, self.can_delete(index, cx))))
    }

    fn caret(&self, position: Point<Pixels>, cx: &App) -> Option<Caret> {
        let (index, hit) = self.hit(position, cx)?;
        Some(Caret { key: self.core.entries[index].key.clone(), index, pos: hit.pos })
    }

    fn in_bar(&self, position: Point<Pixels>) -> bool {
        self.core.content_height() > self.core.viewport_height + 0.5 && position.x >= self.bounds.right() - px(BAR) && self.bounds.contains(&position)
    }

    /// The hover target under the pointer; a frame only when it changed.
    pub(super) fn update_hover(&mut self, position: Point<Pixels>, inside: bool, cx: &mut Context<Self>) -> bool {
        self.pointer = position;
        let hover = if inside && !self.in_bar(position) && self.menu.is_none() {
            // First which row (the actions exist only on the hovered one), then what in it.
            self.locate(position).and_then(|(index, x, y)| {
                let layout = self.core.entries[index].layout.as_ref()?;
                (y >= layout.bubble_top()).then(|| (index, layout.hit(x, y, self.width, true, self.can_delete(index, cx)).target))
            })
        } else {
            None
        };
        if hover == self.hover {
            return false;
        }
        self.hover = hover;
        true
    }

    pub(super) fn mouse_move(&mut self, event: &MouseMoveEvent, hitbox: &Hitbox, window: &mut Window, cx: &mut Context<Self>) {
        match self.drag {
            Drag::Bar(grab) => {
                let geometry = super::element::BarGeometry::of(self);
                let thumb_top = f32::from(event.position.y - self.bounds.top()) - grab;
                let (min, max) = self.core.scroller.range();
                self.core.scroller.jump_to(min + geometry.position_for(thumb_top) as f64 * (max - min));
                cx.notify();
            }
            Drag::Select if event.pressed_button == Some(MouseButton::Left) => {
                self.pointer = event.position;
                if self.extend_selection(event.position, cx) || self.autoscrolling() {
                    cx.notify();
                }
            }
            Drag::Select => self.drag = Drag::None,
            Drag::None => {
                if self.update_hover(event.position, hitbox.is_hovered(window), cx) {
                    cx.notify();
                }
            }
        }
    }

    fn extend_selection(&mut self, position: Point<Pixels>, cx: &App) -> bool {
        // Past the top or bottom edge the caret is the first / last visible row's nearest.
        let clamped = point(position.x, position.y.clamp(self.bounds.top(), self.bounds.bottom() - px(1.)));
        let (Some((anchor, focus)), Some(caret)) = (self.selection.clone(), self.caret(clamped, cx)) else { return false };
        if caret == focus {
            return false;
        }
        self.selection = Some((anchor, caret));
        true
    }

    fn autoscrolling(&self) -> bool {
        self.drag == Drag::Select && (self.pointer.y < self.bounds.top() || self.pointer.y > self.bounds.bottom())
    }

    /// A drag past an edge scrolls, faster the further out (Chromium's autoscroll).
    fn autoscroll(&mut self, now: Instant) {
        if !self.autoscrolling() {
            return;
        }
        let Some(last) = self.last_frame else { return };
        let out = if self.pointer.y < self.bounds.top() { f32::from(self.pointer.y - self.bounds.top()) } else { f32::from(self.pointer.y - self.bounds.bottom()) };
        // 15 px/s for every px outside, at least 120 px/s, at most two viewports a second.
        let speed = (out.abs() * 15.).clamp(120., 2. * self.core.viewport_height as f32) * out.signum();
        let dt = now.saturating_duration_since(last).as_secs_f32().min(0.05);
        self.core.scroller.precise(now, (speed * dt) as f64);
    }

    pub(super) fn after_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self.autoscrolling() {
            let pointer = self.pointer;
            self.extend_selection(pointer, cx);
        }
    }

    pub(super) fn mouse_down(&mut self, event: &MouseDownEvent, hitbox: &Hitbox, window: &mut Window, cx: &mut Context<Self>) {
        if !hitbox.is_hovered(window) {
            return;
        }
        if event.button == MouseButton::Right {
            return self.context_menu(event.position, cx);
        }
        if event.button != MouseButton::Left {
            return;
        }
        if !self.focus.is_focused(window) {
            self.focus.focus(window, cx);
        }
        if self.in_bar(event.position) {
            return self.bar_press(event.position, cx);
        }
        let Some((index, hit)) = self.hit(event.position, cx) else { return };
        let key = self.core.entries[index].key.clone();
        let caret = Caret { key: key.clone(), index, pos: hit.pos };
        self.pressed = Some((key, hit.target));
        let unit = |this: &Self, count: usize| {
            let layout = this.core.entries[index].layout.as_ref()?;
            let (from, to) = layout.unit_at(layout.resolve(hit.pos, false), count);
            Some((Caret { pos: from, ..caret.clone() }, Caret { pos: to, ..caret.clone() }))
        };
        self.selection = match event.click_count {
            2 => unit(self, 2),
            3 => unit(self, 3),
            _ if event.modifiers.shift => match self.selection.take() {
                Some((anchor, _)) => Some((anchor, caret)),
                None => Some((caret.clone(), caret)),
            },
            _ => Some((caret.clone(), caret)),
        };
        if event.click_count == 1 {
            self.drag = Drag::Select;
            window.capture_pointer(hitbox.id);
        }
        cx.notify();
    }

    pub(super) fn mouse_up(&mut self, event: &MouseUpEvent, hitbox: &Hitbox, window: &mut Window, cx: &mut Context<Self>) {
        if event.button != MouseButton::Left {
            return;
        }
        if self.drag != Drag::None {
            window.release_pointer();
        }
        self.drag = Drag::None;
        let Some((key, target)) = self.pressed.take() else { return };
        // A click: released on what was pressed, with nothing selected by the way. The
        // collapsed caret stays: a later Shift+click extends from it, as in Chromium.
        let collapsed = self.selection.as_ref().is_none_or(|(anchor, focus)| anchor == focus);
        if !collapsed || !hitbox.is_hovered(window) {
            return;
        }
        let Some((index, hit)) = self.hit(event.position, cx) else { return };
        if self.core.entries[index].key != key || hit.target != target {
            return;
        }
        self.activate(index, target, event, cx);
    }

    fn activate(&mut self, index: usize, target: Target, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let entry = &self.core.entries[index];
        let (message, key) = (entry.message.clone(), entry.key.clone());
        let Some(layout) = entry.layout.clone() else { return };
        let server = self.source.server();
        match target {
            Target::Link(span) => {
                let Some(document) = layout.document() else { return };
                let Some(span) = document.spans().get(span as usize) else { return };
                let url = document.text(&message.content, span).to_string();
                if matches!(span.kind, decibell_richtext::SpanKind::Link { invite: true, .. }) {
                    // An invite opens the join confirmation, never a browser.
                    actions::servers::open_invite_link(&url, cx);
                } else {
                    cx.emit(ListEvent::OpenLink(url));
                }
            }
            Target::Sender if message.sender_uid != 0 => {
                // `{ x: rect.right + 8, y: rect.top }` of what was clicked.
                decibell_common::open_profile_at(message.sender_uid, point(event.position.x + px(8.), event.position.y), server, cx);
            }
            Target::ReplyName => {
                if let Some(reply) = &message.reply {
                    decibell_common::open_profile_at(reply.sender_uid, point(event.position.x + px(8.), event.position.y), server, cx);
                }
            }
            Target::ReplyLine => {
                if let Some(reply) = &message.reply {
                    self.jump_to_message(reply.id, cx);
                }
            }
            Target::Action(RowAction::Reply) => cx.emit(ListEvent::Reply(message.id)),
            Target::Action(RowAction::Edit) => self.start_edit(message.id, cx),
            Target::Action(RowAction::Delete) => cx.emit(ListEvent::Delete { id: message.id, skip_confirm: event.modifiers.shift }),
            Target::CodeCopy(span) => {
                if let Some(document) = layout.document()
                    && let Some(code) = document.spans().get(span as usize)
                {
                    self.copy(document.text(&message.content, code).to_string(), cx);
                    self.copied = Some((key, span));
                    self.copied_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(COPIED).await;
                        let _ = this.update(cx, |this, cx| {
                            this.copied = None;
                            cx.notify();
                        });
                    }));
                    cx.notify();
                }
            }
            Target::CodeToggle(span) => {
                // The `n`-th code block of the row.
                if let Some(nth) = layout.codes.iter().position(|code| code.span == span) {
                    let entry = &mut self.core.entries[index];
                    entry.expanded ^= 1 << nth.min(63);
                    entry.invalidate();
                    self.code_scroll.retain(|(row, at, ..)| !(row == &key && *at == span));
                    self.core.inexact += 1;
                    cx.notify();
                }
            }
            Target::Media => cx.emit(ListEvent::OpenMedia(message.id)),
            _ => {}
        }
    }

    fn context_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((index, hit)) = self.hit(position, cx) else { return };
        let entry = &self.core.entries[index];
        if hit.target == Target::Sender && entry.message.sender_uid != 0 {
            decibell_common::open_user_menu(entry.message.sender_uid, position, self.source.server(), cx);
            return;
        }
        self.menu = Some((position, entry.key.clone()));
        self.hover = Some((index, Target::None));
        cx.notify();
    }

    pub(super) fn scroll_wheel(&mut self, event: &ScrollEvent, hitbox: &Hitbox, window: &mut Window, cx: &mut Context<Self>) {
        if !hitbox.should_handle_scroll(window) {
            return;
        }
        let now = Self::now(cx);
        let (delta_x, delta, precise) = match event.delta {
            ScrollDelta::Lines(lines) => (ScrollAnimator::lines_to_px(lines.x), ScrollAnimator::lines_to_px(lines.y), false),
            ScrollDelta::Pixels(pixels) => (-(f32::from(pixels.x) as f64), -(f32::from(pixels.y) as f64), true),
        };
        // A code block under the pointer scrolls first; at its end the wheel chains to the list
        // (the renderer leaves `overscroll-behavior` at auto for exactly that).
        if self.scroll_code(event.position, delta_x as f32, delta as f32, event.modifiers.shift, cx) {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if delta == 0. || !self.core.scroller.can_scroll(delta) {
            return;
        }
        self.last_input = Some(now);
        if precise {
            self.core.scroller.precise(now, delta);
        } else {
            self.core.scroller.input(now, delta, ScrollKind::Wheel);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn scroll_code(&mut self, position: Point<Pixels>, dx: f32, dy: f32, shift: bool, cx: &App) -> bool {
        let Some((index, hit)) = self.hit(position, cx) else { return false };
        let Target::CodeBody(span) = hit.target else { return false };
        let entry = &self.core.entries[index];
        let Some(code) = entry.layout.as_ref().and_then(|layout| layout.codes.iter().find(|code| code.span == span)) else { return false };
        let (range_x, range_y) = code.scroll_range();
        // Shift turns the wheel sideways, as in Chromium.
        let (dx, dy) = if shift { (dy, 0.) } else { (dx, dy) };
        let key = entry.key.clone();
        let at = self.code_scroll.iter().position(|(row, s, ..)| row == &key && *s == span);
        let (x, y) = at.map_or((0., 0.), |at| (self.code_scroll[at].2, self.code_scroll[at].3));
        let (new_x, new_y) = ((x + dx).clamp(0., range_x), (y + dy).clamp(0., range_y));
        if new_x == x && new_y == y {
            return false;
        }
        match at {
            Some(at) => self.code_scroll[at] = (key, span, new_x, new_y),
            None => self.code_scroll.push((key, span, new_x, new_y)),
        }
        true
    }

    fn bar_press(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let geometry = super::element::BarGeometry::of(self);
        let y = f32::from(position.y - self.bounds.top());
        let now = Self::now(cx);
        let page = ScrollAnimator::page_step(self.core.viewport_height);
        if y >= geometry.thumb_top && y < geometry.thumb_top + geometry.thumb_len {
            self.drag = Drag::Bar(y - geometry.thumb_top);
        } else {
            // The arrows step a line, the track a page, animated like a key.
            let delta = if y < geometry.track_top {
                -LINE_STEP_PX
            } else if y >= geometry.track_top + geometry.track_len {
                LINE_STEP_PX
            } else if y < geometry.thumb_top {
                -page
            } else {
                page
            };
            self.core.scroller.input(now, delta, ScrollKind::Key);
        }
        cx.notify();
    }

    // ── Keys, copy ─────────────────────────────────────────────────────────────────────────────

    /// The list's keys (docs/wheel-scroll.md §4): ArrowDown, PageUp / PageDown, Home / End
    /// scroll; ArrowUp and Space are the composer's. Ctrl+C copies the selection.
    pub(super) fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if (modifiers.control || modifiers.platform) && keystroke.key == "c" {
            if let Some(text) = self.selected_text(cx).filter(|text| !text.is_empty()) {
                self.copy(text, cx);
                cx.stop_propagation();
            }
            return;
        }
        if keystroke.key == "escape" {
            if self.editing.is_some() {
                self.cancel_edit(cx);
                cx.stop_propagation();
            } else if self.selection.take().is_some() {
                cx.notify();
                cx.stop_propagation();
            }
            return;
        }
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        let page = ScrollAnimator::page_step(self.core.viewport_height);
        let target = self.core.scroller.target();
        let (min, max) = self.core.scroller.range();
        let delta = match (keystroke.key.as_str(), modifiers.shift) {
            ("down", false) => LINE_STEP_PX,
            ("pagedown", _) => page,
            ("pageup", _) => -page,
            ("home", false) => min - target,
            ("end", false) => max - target,
            _ => return,
        };
        let now = Self::now(cx);
        self.last_input = Some(now);
        self.core.scroller.input(now, delta, ScrollKind::Key);
        cx.stop_propagation();
        cx.notify();
    }

    /// The selection's share of row `index`, as the row paints it.
    pub(super) fn selection_in(&self, index: usize) -> Option<(RowPos, RowPos)> {
        let (anchor, focus) = self.selection.as_ref()?;
        let (from, to) = if (anchor.index, anchor.pos) <= (focus.index, focus.pos) { (anchor, focus) } else { (focus, anchor) };
        if index < from.index || index > to.index || (from.index == to.index && from.pos == to.pos) {
            return None;
        }
        let layout = self.core.entries[index].layout.as_ref()?;
        let start = layout.resolve(if index == from.index { from.pos } else { RowPos::START }, false);
        let end = layout.resolve(if index == to.index { to.pos } else { RowPos::END }, true);
        (start < end).then_some((start, end))
    }

    /// What Ctrl+C puts on the clipboard: the selection as displayed, a row after the other.
    pub fn selected_text(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let (anchor, focus) = self.selection.clone()?;
        let (from, to) = if (anchor.index, anchor.pos) <= (focus.index, focus.pos) { (anchor, focus) } else { (focus, anchor) };
        let mut out = String::new();
        for index in from.index..=to.index.min(self.core.len().saturating_sub(1)) {
            // A row scrolled far away has no layout any more: its text comes from the model.
            let dropped = self.core.entries[index].layout.is_none();
            if dropped {
                self.build_row(index, cx);
            }
            if let Some(layout) = self.core.entries[index].layout.clone() {
                let start = layout.resolve(if index == from.index { from.pos } else { RowPos::START }, false);
                let end = layout.resolve(if index == to.index { to.pos } else { RowPos::END }, true);
                if start < end {
                    layout.copy_text(start, end, &mut out);
                }
            }
            if dropped {
                self.core.entries[index].layout = None;
            }
        }
        Some(out)
    }

    /// Selects from `from` to `to` (row ids with a caret each): the self-tests' way in.
    pub fn select(&mut self, from: (i64, RowPos), to: (i64, RowPos), cx: &mut Context<Self>) {
        let caret = |this: &Self, (id, pos): (i64, RowPos)| {
            let index = this.core.index_of_id(id)?;
            Some(Caret { key: this.core.entries[index].key.clone(), index, pos })
        };
        self.selection = caret(self, from).zip(caret(self, to));
        cx.notify();
    }

    pub fn has_selection(&self) -> bool {
        self.selection.as_ref().is_some_and(|(anchor, focus)| anchor != focus)
    }

    // ── Editing ────────────────────────────────────────────────────────────────────────────────

    pub fn start_edit(&mut self, id: i64, cx: &mut Context<Self>) {
        self.set_editing(Some(id), cx);
    }

    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.set_editing(None, cx);
    }

    /// Saves an edit (the editor slot's `submit`).
    pub fn submit_edit(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(id) = self.editing else { return };
        let text = text.trim();
        if !text.is_empty() {
            let source = self.source.clone();
            source.edit(id, text, cx);
        }
        self.set_editing(None, cx);
    }

    fn set_editing(&mut self, id: Option<i64>, cx: &mut Context<Self>) {
        if self.editing == id {
            return;
        }
        for changed in [self.editing, id].into_iter().flatten() {
            if let Some(index) = self.core.index_of_id(changed) {
                self.core.entries[index].invalidate();
                self.core.inexact += 1;
            }
        }
        self.editing = id;
        cx.notify();
    }

    pub fn editing(&self) -> Option<i64> {
        self.editing
    }

    fn pill(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        // ChatPanel's pill: the accent button idiom with the float shadow, `bottom-3 right-4`.
        div()
            .id("jump-to-present")
            .absolute()
            .bottom_3()
            .right_4()
            .flex()
            .items_center()
            .gap_1p5()
            .px_4()
            .py_2()
            .rounded(theme.radius_sm)
            .bg(theme.color_accent)
            .hover(|style| style.bg(theme.color_accent_hover))
            .shadow(theme.shadow_float.clone())
            .cursor_pointer()
            .occlude()
            .child(Label::new("Jump to present").font(FontRole::Sans).size(13.).weight(600.).color(theme.color_on_accent).nowrap())
            .child(Icon::new(IconName::ChatJumpPresent).size(px(14.)).color(theme.color_on_accent))
            .on_click(cx.listener(|this, _, _, cx| this.jump_to_present(cx)))
    }

    fn message_menu(&self, at: Point<Pixels>, key: &RowKey, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let index = self.core.index_of(key)?;
        let entry = &self.core.entries[index];
        let (facts, id) = (entry.facts, entry.message.id);
        let can_delete = self.can_delete(index, cx);
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.menu = None;
            this.hover = None;
            cx.notify();
        };
        let mut menu = ContextMenu::new("message-menu", at);
        if self.has_selection() {
            menu = menu.entry(MenuItem::new("Copy").on_click(cx.listener(move |this, _, _, cx| {
                if let Some(text) = this.selected_text(cx) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                close(this, cx);
            })));
        } else if !entry.message.content.is_empty() && entry.message.decrypt_error.is_none() {
            menu = menu.entry(MenuItem::new("Copy text").on_click(cx.listener(move |this, _, _, cx| {
                if let Some(index) = this.core.index_of_id(id) {
                    let message = this.core.entries[index].message.clone();
                    let document = decibell_richtext::parse(&message.content, &decibell_richtext::ParseOptions::default());
                    cx.write_to_clipboard(ClipboardItem::new_string(decibell_richtext::plain::as_displayed(&document, &message.content)));
                }
                close(this, cx);
            })));
        }
        if facts.can_reply {
            menu = menu.entry(MenuItem::new("Reply").icon(IconName::ChatReply).on_click(cx.listener(move |this, _, _, cx| {
                cx.emit(ListEvent::Reply(id));
                close(this, cx);
            })));
        }
        if facts.can_edit {
            menu = menu.entry(MenuItem::new("Edit message").icon(IconName::ChatEdit).on_click(cx.listener(move |this, _, _, cx| {
                this.start_edit(id, cx);
                close(this, cx);
            })));
        }
        if can_delete {
            menu = menu.entry(MenuEntry::Separator).entry(MenuItem::new("Delete message").icon(IconName::ChatTrash).danger().on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    cx.emit(ListEvent::Delete { id, skip_confirm: event.modifiers().shift });
                    close(this, cx);
                }),
            ));
        }
        Some(menu.on_dismiss(cx.listener(move |this, _, _, cx| close(this, cx))))
    }
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs() as i64)
}

impl Render for MessageList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pill = self.pill.then(|| self.pill(cx));
        let menu = self.menu.clone().and_then(|(at, key)| self.message_menu(at, &key, cx));
        div()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .child(ListElement::new(cx.entity()))
            .children(pill)
            .children(menu)
    }
}
