//! The results panel (`features/search/SearchResultsPanel.tsx`, spec 2026-10-14): in the member
//! list's place while a search of the active community is open. The header (count, sort, ✕),
//! the encrypted-channels and "stopped early" notices, the loading / error / empty states, then
//! the results grouped by channel in result order, the matched words marked, paged in as the list
//! nears its end, and Jump (or a click anywhere on a result) opens the message in its channel.
//!
//! A cached view, alive only while the panel shows (no state, no rows when it is closed). The
//! list is windowed: every result's height is computed when its page arrives (one paragraph
//! layout, then dropped), and only the rows on screen hold their layouts and elements.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use decibell_common::{UserAvatar, Watch, person_name_color};
use decibell_model::chat::Message;
use decibell_model::chat::search::{SearchSort, SearchStatus, SearchStore, search_words};
use decibell_model::stores::community::Channel;
use decibell_model::stores::settings::TimeFormat;
use decibell_model::{ChannelId, Model, ServerId, actions};
use decibell_text::{Engine, FontSpec, TextStyle, Wrapping};
use decibell_ui::popover::visible_window_bounds;
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, RowHeights, ScrollArea, ScrollHandle, Segment, SegmentedControl, title_tooltip};
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClickEvent, Context, FocusHandle, Hsla, IntoElement, KeyDownEvent, Pixels, Render,
    SharedString, Subscription, WeakEntity, Window, div, prelude::*, px,
};

use crate::format::{result_stamp, results_title, today};
use crate::highlight::Highlighter;
use crate::result_row::{self, Body, BodyText, Item, ResultStyle};
use crate::runs::Runs;

/// The panel's width beside the chat, and floating below 1024 px (`w-[400px]`,
/// `max-lg:w-[360px]`).
pub const PANEL_WIDTH: f32 = 400.;
pub const FLOATING_WIDTH: f32 = 360.;
/// The scrollbar's width while the list overflows (Chromium's thin bar takes layout space).
const BAR: f32 = 10.;
/// The sentinel's `rootMargin: "400px 0px"`: the next page is asked for this near the end.
pub(crate) const LOAD_AHEAD: f32 = 400.;

/// The panel drawn last, while it lives (the self-tests check it is dropped when closed).
pub struct ShownResults(pub WeakEntity<ResultsView>);

impl gpui::Global for ShownResults {}

/// The results view that is drawn, if any.
pub fn shown_results(cx: &App) -> Option<gpui::Entity<ResultsView>> {
    cx.try_global::<ShownResults>().and_then(|shown| shown.0.upgrade())
}

/// Whether the window is narrow enough for the panel to float (`max-lg:`).
pub fn is_narrow(window: &Window, cx: &App) -> bool {
    f32::from(visible_window_bounds(window, cx).size.width) < decibell_theme::tailwind::BREAKPOINT_LG
}

/// What of the store the panel shows (compared on every store notify: typing in the box notifies
/// the store with its draft, which must not redraw the panel).
#[derive(Clone, PartialEq)]
struct Snapshot {
    seq: u32,
    status: SearchStatus,
    results: usize,
    total: i64,
    capped: bool,
    has_more: bool,
    loading_more: bool,
    skipped: usize,
    partial: bool,
    error: Option<SharedString>,
    jumped: i64,
    sort: SearchSort,
}

impl Snapshot {
    fn of(store: &SearchStore) -> Snapshot {
        Snapshot {
            seq: store.seq,
            status: store.status,
            results: store.results.len(),
            total: store.total,
            capped: store.total_capped,
            has_more: store.has_more,
            loading_more: store.loading_more,
            skipped: store.text_skipped.len(),
            partial: store.partial,
            error: store.error.clone(),
            jumped: store.jumped_id,
            sort: store.sort,
        }
    }
}

/// The list as laid out for one width and style: its rows and their offsets.
struct ListLayout {
    items: Rc<[Item]>,
    offsets: Rc<[Pixels]>,
    width: f32,
    style_key: u64,
    seq: u32,
    results: usize,
}

/// What a frame's rows need, shared with the windowed list's builder.
struct Rows {
    server: ServerId,
    style: Rc<ResultStyle>,
    highlighter: Option<Rc<Highlighter>>,
    /// The text column's width.
    column: f32,
    jumped: i64,
    today: decibell_model::chat::search::Day,
    time_format: TimeFormat,
    loading_more: bool,
    has_more: bool,
    channels: Rc<[Channel]>,
    /// Bodies of the rows drawn last (and this) frame, by message id.
    built: RefCell<HashMap<i64, Rc<Body>>>,
    used: RefCell<Vec<i64>>,
    /// The channel heading under the pointer (its text turns `text-text-primary`).
    hovered: RefCell<Option<ChannelId>>,
    view: WeakEntity<ResultsView>,
}

pub struct ResultsView {
    model: Model,
    server: ServerId,
    seen: Snapshot,
    scroll: ScrollHandle,
    style: Option<Rc<ResultStyle>>,
    list: Option<ListLayout>,
    /// Card heights by message id, for `heights_for` (width, style key).
    heights: HashMap<i64, f32>,
    heights_for: (u32, u64),
    rows: Option<Rc<Rows>>,
    highlighter: Option<Rc<Highlighter>>,
    query_seq: u32,
    focus: FocusHandle,
    /// A page asked for since the last one arrived (the sentinel fires once per page).
    asked: bool,
    /// Renders so far (the self-tests: the panel at rest draws nothing).
    renders: u64,
    _watch: Watch,
    _subscriptions: Vec<Subscription>,
}

impl ResultsView {
    pub fn new(server: ServerId, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .store_if(&model.chat.search, cx, |this: &mut ResultsView, store, _| {
                let next = Snapshot::of(store);
                if next.seq != this.seen.seq {
                    // A new search starts at the top.
                    this.scroll.scroll_to_top();
                }
                if next.results != this.seen.results || next.seq != this.seen.seq {
                    this.asked = false;
                }
                std::mem::replace(&mut this.seen, next.clone()) != next
            })
            .store(&model.settings, cx)
            .store(&model.communities, cx)
            .people(cx, |this: &ResultsView, uid| this.shows(uid));
        let mut subscriptions = Vec::new();
        if let Some(community) = model.community(server, cx) {
            // Channel names in the headings and the notice.
            subscriptions.push(cx.observe(&community.channels, |this, _, cx| {
                this.rows = None;
                cx.notify();
            }));
            // Nicknames.
            subscriptions.push(cx.observe(&community.roster, |_, _, cx| cx.notify()));
        }
        // Esc closes the panel unless something above it (a menu, a modal) took the key, or a
        // text field has the focus (`SearchResultsPanel`'s window listener).
        subscriptions.push(cx.observe_keystrokes(|this: &mut ResultsView, event, window, cx| {
            if event.keystroke.key == "escape" && this.escape_closes(window, cx) {
                this.model.chat.search.update(cx, |s, cx| s.close(cx));
            }
        }));
        let seen = Snapshot::of(model.chat.search.read(cx));
        cx.set_global(ShownResults(cx.weak_entity()));
        Self {
            focus: cx.focus_handle(),
            model,
            server,
            seen,
            scroll: ScrollHandle::new(),
            style: None,
            list: None,
            heights: HashMap::new(),
            heights_for: (0, 0),
            rows: None,
            highlighter: None,
            query_seq: 0,
            asked: false,
            renders: 0,
            _watch: watch,
            _subscriptions: subscriptions,
        }
    }

    fn shows(&self, uid: decibell_model::Uid) -> bool {
        self.list.as_ref().is_some_and(|list| {
            list.items.iter().any(|item| match item {
                Item::Result(m) => m.sender_uid == uid || m.reply.as_ref().is_some_and(|r| r.sender_uid == uid),
                _ => false,
            })
        })
    }

    /// The renderer ignores Esc when the key's target is an input. GPUI says nothing about the
    /// focused element's kind: the panel closes when the focus is nowhere, in the panel, or on
    /// the message list, and otherwise stays (a text field, but also a focused button elsewhere).
    fn escape_closes(&self, window: &Window, cx: &App) -> bool {
        let Some(focused) = window.focused(cx) else { return true };
        if self.focus.contains_focused(window, cx) {
            return true;
        }
        decibell_chat::shown_list(cx).is_some_and(|list| list.read(cx).focus_handle() == &focused)
    }

    pub fn server(&self) -> ServerId {
        self.server
    }

    /// How many times the panel rendered.
    pub fn renders(&self) -> u64 {
        self.renders
    }

    pub fn scroll_handle(&self) -> &ScrollHandle {
        &self.scroll
    }

    /// Rows with their bodies laid out now (tests: what the window holds).
    pub fn built_rows(&self) -> usize {
        self.rows.as_ref().map_or(0, |rows| rows.built.borrow().len())
    }

    /// Where paging stands, for a self-test that saw it stop: the scroll offset, its maximum, the
    /// viewport's height, the list's height to the sentinel, and whether the next page was asked for.
    pub fn paging(&self) -> String {
        let total = self.list.as_ref().and_then(|l| l.offsets.last().copied()).unwrap_or_default();
        format!(
            "offset {:?} of max {:?}, viewport {:?}, list {:?}, asked {}",
            self.scroll.offset(),
            self.scroll.max_offset(),
            self.scroll.viewport_height(),
            total,
            self.asked
        )
    }

    /// Results whose heights are known (one number each).
    pub fn measured(&self) -> usize {
        self.heights.len()
    }

    /// The list's rows: (message id or 0, top, height), for the self-tests.
    pub fn row_offsets(&self) -> Vec<(i64, f32, f32)> {
        let Some(list) = &self.list else { return Vec::new() };
        list.items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let id = if let Item::Result(m) = item { m.id } else { 0 };
                (id, f32::from(list.offsets[i]), f32::from(list.offsets[i + 1] - list.offsets[i]))
            })
            .collect()
    }

    /// The style for this window's scale and the theme (rebuilt when either moves).
    fn style(&mut self, window: &Window, cx: &mut App) -> Rc<ResultStyle> {
        let scale = window.scale_factor();
        let fresh = ResultStyle::new(scale, cx);
        match &self.style {
            Some(style) if style.key == fresh.key => style.clone(),
            _ => {
                let style = Rc::new(fresh);
                self.style = Some(style.clone());
                style
            }
        }
    }

    /// Lays the list out for `width` px of content: headings where the channel changes, each
    /// result's card height (computed once per width and style), the sentinel.
    fn layout(&mut self, width: f32, style: &Rc<ResultStyle>, cx: &mut App) {
        let (seq, count) = {
            let store = self.model.chat.search.read(cx);
            (store.seq, store.results.len())
        };
        if let Some(list) = &self.list
            && list.width == width
            && list.style_key == style.key
            && list.seq == seq
            && list.results == count
        {
            return;
        }
        let key = (width.to_bits(), style.key);
        if self.heights_for != key || self.query_seq != seq {
            self.heights.clear();
            self.heights_for = key;
        }
        let (results, done, query) = {
            let store = self.model.chat.search.read(cx);
            (store.results.clone(), store.status == SearchStatus::Done, store.query.clone())
        };
        if self.query_seq != seq {
            self.query_seq = seq;
            self.highlighter = Highlighter::for_query(&query).map(Rc::new);
        }
        let results: Vec<Arc<Message>> = results;
        let engine = Engine::global(cx);
        let column = result_row::column_width(width);
        let mut items: Vec<Item> = Vec::with_capacity(results.len() * 2 + 1);
        let mut offsets: Vec<Pixels> = Vec::with_capacity(results.len() * 2 + 2);
        let mut y = 0f32;
        let mut previous: Option<&str> = None;
        for message in &results {
            if previous != Some(&*message.channel) {
                let first = previous.is_none();
                previous = Some(&message.channel);
                offsets.push(px(y));
                items.push(Item::Heading { channel: message.channel.clone(), first });
                y += if first { result_row::HEADING } else { result_row::HEADING_AFTER_CARD };
            }
            let height = match self.heights.get(&message.id) {
                Some(height) => *height,
                None => {
                    // A sender (or quoted author) the roster hasn't loaded: look them up, so the
                    // nickname shows (`useDisplayName`; the roster's change redraws the cards).
                    decibell_common::want_member(self.server, message.sender_uid, cx);
                    if let Some(reply) = message.reply.as_ref().filter(|reply| !reply.sender.is_empty()) {
                        decibell_common::want_member(self.server, reply.sender_uid, cx);
                    }
                    let height = card_height(message, self.highlighter.as_deref(), column, style, &engine);
                    self.heights.insert(message.id, height);
                    height
                }
            };
            offsets.push(px(y));
            items.push(Item::Result(message.clone()));
            y += height;
        }
        if done && !results.is_empty() {
            offsets.push(px(y));
            items.push(Item::Sentinel);
            y += result_row::SENTINEL + result_row::LIST_PAD_BOTTOM;
        }
        offsets.push(px(y));
        self.list = Some(ListLayout { items: items.into(), offsets: offsets.into(), width, style_key: style.key, seq, results: count });
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.model.chat.search.update(cx, |s, cx| s.close(cx));
    }

    /// The sentinel's observer (`IntersectionObserver`, `rootMargin: 400px`, re-made when the
    /// results change): in every frame the list draws, with that frame's position
    /// (`ScrollArea::on_frame`), the next page is asked for once the end of what is loaded is
    /// within 400 px of the viewport's bottom. From the frame, not from `render`: a render sees the
    /// position the frame before drew, and the scroll that lands at the end (a touchpad's delta, a
    /// wheel animation's last step, a jump) renders nothing after it, so a check in `render` never
    /// saw the end when that last move was longer than the look-ahead and paging stopped
    /// (`search-paging`, 2026-11-27). `None` while there is nothing to ask for.
    fn load_trigger(&self, cx: &mut Context<Self>) -> Option<impl Fn(decibell_ui::ScrollFrame, &mut Window, &mut App) + 'static> {
        let list = self.list.as_ref()?;
        if self.asked || !self.seen.has_more || self.seen.loading_more {
            return None;
        }
        let total = f32::from(list.offsets.last().copied().unwrap_or_default());
        let sentinel = total - (result_row::SENTINEL + result_row::LIST_PAD_BOTTOM);
        let view = cx.weak_entity();
        let fired = std::cell::Cell::new(false);
        Some(move |frame: decibell_ui::ScrollFrame, _: &mut Window, cx: &mut App| {
            if fired.get() || frame.viewport <= 0. || frame.offset + frame.viewport + LOAD_AHEAD < sentinel {
                return;
            }
            fired.set(true);
            let view = view.clone();
            // After this frame.
            cx.defer(move |cx| {
                let _ = view.update(cx, |this, cx| this.load_more(cx));
            });
        })
    }

    /// The next page, once per page.
    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.asked || !self.seen.has_more || self.seen.loading_more {
            return;
        }
        self.asked = true;
        self.model.chat.search.update(cx, |s, cx| s.load_more(cx));
    }
}

/// One card's height (its `mb-1.5` included).
fn card_height(message: &Message, highlight: Option<&Highlighter>, column: f32, style: &ResultStyle, engine: &Rc<Engine>) -> f32 {
    let decrypt = message.decrypt_error.as_ref().map_or(0., |e| {
        engine.label_layout(result_row::decrypt_text(e), &style.notice, Wrapping::NORMAL, (column - 18.).max(1.), style.scale).height()
    });
    let body = (message.decrypt_error.is_none() && !message.content.is_empty())
        .then(|| result_row::body(message, highlight, column, style, engine));
    result_row::card_height(message, body.as_ref(), decrypt, style)
}

impl Render for ResultsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        let narrow = is_narrow(window, cx);
        let style = self.style(window, cx);
        let ink = Ink::of(cx.theme());
        let channels: Vec<Channel> = self.model.community(self.server, cx).map(|c| c.channels.read(cx).list().to_vec()).unwrap_or_default();
        let (status, title, sort, error, partial, skipped, has_results, relevance_off) = {
            let store = self.model.chat.search.read(cx);
            let status = store.status;
            let title = results_title(status == SearchStatus::Loading, status == SearchStatus::Error, store.total, store.total_capped, store.results.len());
            let skipped: Vec<SharedString> =
                store.text_skipped.iter().filter_map(|id| channels.iter().find(|c| c.id == *id).map(|c| c.name.clone())).collect();
            // `SORTS`: "Relevant" needs words to rank by, and the index (not the inside-words scan);
            // the disabled option says which (its `title`).
            let relevance_off = if store.query.matches_inside() {
                Some("Not available when matching inside words")
            } else if search_words(&store.query.text).is_empty() {
                Some("Add words to sort by relevance")
            } else {
                None
            };
            (
                status,
                SharedString::from(title),
                store.sort,
                store.error.clone(),
                store.partial && status == SearchStatus::Done,
                skipped,
                !store.results.is_empty(),
                relevance_off,
            )
        };
        let sorts = vec![
            Segment::new(SearchSort::Newest, "Newest"),
            Segment::new(SearchSort::Oldest, "Oldest"),
            match relevance_off {
                Some(why) => Segment::new(SearchSort::Relevance, "Relevant").disabled(true).title(why),
                None => Segment::new(SearchSort::Relevance, "Relevant"),
            },
        ];
        decibell_shell::probes::text("search-title", &title);
        let header = div()
            .flex()
            .flex_none()
            .h_12()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(ink.divider)
            .pl_4()
            .pr_2()
            .child(div().min_w_0().flex_1().child(Label::new(title).size(14.).weight(600.).color(ink.bright).truncate()))
            .child(
                div().w(px(196.)).flex_none().child(
                    SegmentedControl::new("search-sort", sorts, sort).on_change(|sort: &SearchSort, _, cx| {
                        let model = Model::global(cx).clone();
                        model.chat.search.update(cx, |s, cx| s.set_sort(*sort, cx));
                    }),
                ),
            )
            .child(
                div()
                    .id("search-close")
                    .relative()
                    .group("search-close")
                    .flex()
                    .flex_none()
                    .size_8()
                    .items_center()
                    .justify_center()
                    .rounded(ink.radius_sm)
                    .cursor_pointer()
                    .hover(|s| s.bg(ink.hover))
                    .tooltip(title_tooltip("Close (Esc)"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)))
                    .child(Icon::new(IconName::SettingsX).size(px(16.)).color(ink.muted).group_hover("search-close", ink.secondary))
                    .children(crate::probe("search-close")),
            );

        let panel = if narrow { FLOATING_WIDTH } else { PANEL_WIDTH };
        let mut notices = Vec::new();
        // The server names them only when text was searched (words, inside words, or has: link).
        decibell_shell::probes::text("search-skipped", &skipped.iter().map(|n| n.as_ref()).collect::<Vec<_>>().join(",").into());
        if !skipped.is_empty() {
            notices.push(notice(IconName::ChatLock, skipped_notice(&skipped, &ink, cx), &ink));
        }
        if partial {
            let text = Label::new("This search took too long, so it stopped early. Add a filter to narrow it down.")
                .size(12.)
                .line_height(12. * 1.375)
                .color(ink.muted)
                .into_any_element();
            notices.push(notice(IconName::SettingsHourglass, text, &ink));
        }

        let body: AnyElement = match status {
            SearchStatus::Loading | SearchStatus::Idle => skeleton(&ink, cx.reduce_motion()),
            SearchStatus::Error => {
                let retry = div()
                    .id("search-retry")
                    .relative()
                    .mt_3()
                    .rounded(ink.radius_sm)
                    .bg(ink.accent)
                    .px_4()
                    .py_2()
                    .cursor_pointer()
                    .hover(|s| s.bg(ink.accent_hover))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.model.chat.search.update(cx, |s, cx| s.retry_search(cx));
                    }))
                    .child(Label::new("Try again").size(13.).weight(600.).color(ink.on_accent).nowrap())
                    .children(crate::probe("search-retry"))
                    .into_any_element();
                empty(IconName::SettingsAlert, "Search didn't work", error.as_deref().unwrap_or("Something went wrong."), Some(retry), &ink, cx)
            }
            SearchStatus::Done if !has_results => {
                empty(IconName::SettingsSearch, "No results", "Nothing matched. Try fewer words, other spellings, or remove a filter.", None, &ink, cx)
            }
            SearchStatus::Done => self.list_element(panel, &style, &ink, cx),
        };

        div()
            .id("search-results")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(ink.border)
            .bg(ink.dark)
            .child(header)
            .children(notices)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .children(crate::probe("search-results"))
    }
}

impl ResultsView {
    fn list_element(&mut self, panel: f32, style: &Rc<ResultStyle>, ink: &Ink, cx: &mut Context<Self>) -> AnyElement {
        let overflowing = self.scroll.max_offset() > px(0.5);
        let width = panel - 1. - if overflowing { BAR } else { 0. } - 2. * result_row::LIST_PAD_X;
        self.layout(width, style, cx);
        let load_trigger = self.load_trigger(cx);
        let column = result_row::column_width(width);
        let (jumped, loading_more, has_more) = {
            let store = self.model.chat.search.read(cx);
            (store.jumped_id, store.loading_more, store.has_more)
        };
        let reuse = self.rows.as_ref().is_some_and(|rows| {
            Rc::ptr_eq(&rows.style, style)
                && rows.column == column
                && rows.jumped == jumped
                && rows.loading_more == loading_more
                && rows.has_more == has_more
                && rows.highlighter.as_ref().map(Rc::as_ptr) == self.highlighter.as_ref().map(Rc::as_ptr)
        });
        if !reuse {
            // Bodies laid out for the same width and style stay.
            let built = match self.rows.take() {
                Some(previous) if Rc::ptr_eq(&previous.style, style) && previous.column == column => previous.built.take(),
                _ => HashMap::new(),
            };
            let channels: Rc<[Channel]> =
                self.model.community(self.server, cx).map(|c| c.channels.read(cx).list().into()).unwrap_or_else(|| Rc::from(Vec::new()));
            self.rows = Some(Rc::new(Rows {
                server: self.server,
                style: style.clone(),
                highlighter: self.highlighter.clone(),
                column,
                jumped,
                today: today(cx),
                time_format: self.model.settings.read(cx).time_format(),
                loading_more,
                has_more,
                channels,
                built: RefCell::new(built),
                used: RefCell::new(Vec::new()),
                hovered: RefCell::new(None),
                view: cx.weak_entity(),
            }));
        }
        let (Some(list), Some(rows)) = (&self.list, self.rows.clone()) else { return div().into_any_element() };
        let items = list.items.clone();
        let ink = *ink;
        ScrollArea::new("search-results-list")
            .track_scroll(&self.scroll)
            .when_some(load_trigger, |area, trigger| area.on_frame(trigger))
            .flex_1()
            .windowed(items.len(), RowHeights::Offsets(list.offsets.clone()), move |range, window, cx| {
                rows.used.borrow_mut().clear();
                let out: Vec<AnyElement> = range.map(|index| row_element(&items[index], &rows, &ink, window, cx)).collect();
                // Only the rows on screen keep their layouts.
                let used = rows.used.borrow();
                rows.built.borrow_mut().retain(|id, _| used.contains(id));
                out
            })
            .into_any_element()
    }
}

/// The panel's colours (copied out of the theme: listeners are made while they are in use).
#[derive(Clone, Copy)]
pub(crate) struct Ink {
    primary: Hsla,
    secondary: Hsla,
    muted: Hsla,
    bright: Hsla,
    hover: Hsla,
    divider: Hsla,
    border: Hsla,
    accent: Hsla,
    accent_hover: Hsla,
    accent_half: Hsla,
    on_accent: Hsla,
    dark: Hsla,
    darkest: Hsla,
    mid: Hsla,
    light: Hsla,
    lighter: Hsla,
    radius_sm: Pixels,
    radius_md: Pixels,
    radius_lg: Pixels,
}

impl Ink {
    fn of(theme: &decibell_ui::Theme) -> Ink {
        Ink {
            primary: theme.color_text_primary,
            secondary: theme.color_text_secondary,
            muted: theme.color_text_muted,
            bright: theme.color_text_bright,
            hover: theme.color_surface_hover,
            divider: theme.color_border_divider,
            border: theme.color_border,
            accent: theme.color_accent,
            accent_hover: theme.color_accent_hover,
            accent_half: theme.color_accent.opacity(0.5),
            on_accent: theme.color_on_accent,
            dark: theme.color_bg_dark,
            darkest: theme.color_bg_darkest,
            mid: theme.color_bg_mid,
            light: theme.color_bg_light,
            lighter: theme.color_bg_lighter,
            radius_sm: theme.radius_sm,
            radius_md: theme.radius_md,
            radius_lg: theme.radius_lg,
        }
    }
}

/// `Notice`: `flex gap-2 border-b border-border-divider px-4 py-2.5 text-[12px] leading-snug
/// text-text-muted`, the icon `mt-px`.
fn notice(icon: IconName, text: AnyElement, ink: &Ink) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .gap_2()
        .border_b_1()
        .border_color(ink.divider)
        .px_4()
        .py_2p5()
        .child(div().flex_none().mt(px(1.)).child(Icon::new(icon).size(px(13.)).color(ink.muted)))
        .child(div().min_w_0().flex_1().child(text))
        .into_any_element()
}

/// "Text isn't searched in encrypted channel #a, #b. Other filters still find its messages.",
/// the names `font-semibold text-text-secondary`.
fn skipped_notice(names: &[SharedString], ink: &Ink, cx: &mut App) -> AnyElement {
    let engine = Engine::global(cx);
    let theme = cx.theme();
    let tracking = theme.palette.t_ls_body * 16.;
    let face = |weight| engine.face(&FontSpec { family: theme.font(FontRole::Sans), weight, italic: false });
    let plain = TextStyle::new(face(400), 12., ink.muted).leading(1.375).letter_spacing(tracking);
    let name = TextStyle::new(face(600), 12., ink.secondary).leading(1.375).letter_spacing(tracking);
    let one = names.len() == 1;
    let mut runs = vec![(format!("Text isn't searched in encrypted {} ", if one { "channel" } else { "channels" }), plain)];
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            runs.push((", ".into(), plain));
        }
        runs.push((format!("#{n}"), name));
    }
    runs.push((format!(". Other filters still find {} messages.", if one { "its" } else { "their" }), plain));
    Runs::new(plain, Wrapping::NORMAL).runs(runs).into_any_element()
}

/// `Skeleton`: five placeholder cards pulsing (`animate-pulse`) while the first page is awaited.
fn skeleton(ink: &Ink, reduce_motion: bool) -> AnyElement {
    let widths = [0.8, 0.55, 0.7, 0.45, 0.6];
    let bar = |w: gpui::Length, ink: &Ink| div().h_3().w(w).rounded(ink.radius_sm).bg(ink.lighter);
    let cards = widths.iter().map(|w| {
        div()
            .mb_2()
            .flex()
            .gap_2p5()
            .rounded(ink.radius_lg)
            .bg(ink.mid)
            .px_3()
            .py_3()
            .child(div().size_8().flex_none().rounded_full().bg(ink.lighter))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .pt_0p5()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(bar(px(96.).into(), ink))
                    .child(bar(gpui::relative(*w).into(), ink)),
            )
    });
    let content = div().px_2().pt_3().children(cards);
    if reduce_motion {
        return content.into_any_element();
    }
    // `animate-pulse`: opacity 1 → .5 → 1 over 2 s, `cubic-bezier(0.4, 0, 0.6, 1)` per half. It
    // runs only while the first page is awaited (the element goes with the loading state).
    let curve = decibell_ui::motion::CubicBezier::new(0.4, 0., 0.6, 1.);
    content
        .with_animation("search-skeleton", Animation::new(Duration::from_secs(2)).repeat(), move |el, t| {
            let half = if t < 0.5 { t * 2. } else { (t - 0.5) * 2. };
            let eased = curve.y(half as f64) as f32;
            el.opacity(if t < 0.5 { 1. - 0.5 * eased } else { 0.5 + 0.5 * eased })
        })
        .into_any_element()
}

/// `Empty`: the icon tile, a title and a centred line, and what goes under them.
fn empty(icon: IconName, title: &'static str, body: &str, action: Option<AnyElement>, ink: &Ink, cx: &mut App) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .px_6()
        .pt_16()
        .child(div().flex().size_12().items_center().justify_center().rounded(ink.radius_lg).bg(ink.mid).child(Icon::new(icon).size(px(22.)).color(ink.muted)))
        .child(div().mt_3().child(Label::new(title).size(14.).weight(600.).color(ink.primary).nowrap()))
        .child(div().mt_1().max_w(px(260.)).child(centred_lines(body, 13., 13. * 1.375, ink.muted, 260., cx)))
        .children(action)
        .into_any_element()
}

/// `text-center` text of one style: laid out at `max` px, each line drawn centred.
fn centred_lines(text: &str, size: f32, line_height: f32, color: Hsla, max: f32, cx: &mut App) -> AnyElement {
    let engine = Engine::global(cx);
    let theme = cx.theme();
    let face = engine.face(&FontSpec { family: theme.font(FontRole::Sans), weight: 400, italic: false });
    let style = TextStyle::new(face, size, color).line_height_px(line_height).letter_spacing(theme.palette.t_ls_body * 16.);
    let layout = engine.label_layout(text, &style, Wrapping::NORMAL, max, 1.);
    let lines: Vec<SharedString> = (0..layout.line_count())
        .filter_map(|i| layout.line(i))
        .map(|line| text[line.text.0 as usize..line.text.1 as usize].trim_end().to_string().into())
        .collect();
    div()
        .flex()
        .flex_col()
        .items_center()
        .children(lines.into_iter().map(|line| Label::new(line).size(size).line_height(line_height).color(color).nowrap()))
        .into_any_element()
}

/// One row of the windowed list.
fn row_element(item: &Item, rows: &Rc<Rows>, ink: &Ink, window: &mut Window, cx: &mut App) -> AnyElement {
    match item {
        Item::Heading { channel, first } => heading(channel, *first, rows, ink),
        Item::Sentinel => {
            let text = if rows.loading_more {
                "Loading more…"
            } else if rows.has_more {
                ""
            } else {
                "That's everything."
            };
            div()
                .px_2()
                .h(px(result_row::SENTINEL))
                .flex()
                .items_center()
                .justify_center()
                .child(Label::new(text).size(12.).color(ink.muted).nowrap())
                .into_any_element()
        }
        Item::Result(message) => result_card(message, rows, ink, window, cx),
    }
}

/// A channel's heading over its results: a button that opens the channel (`text-text-muted
/// hover:text-text-primary`; a label's colour is fixed when it is built, so the hover is followed
/// with `on_hover` and the row drawn again).
fn heading(channel: &ChannelId, first: bool, rows: &Rc<Rows>, ink: &Ink) -> AnyElement {
    let found = rows.channels.iter().find(|c| c.id == *channel);
    let name: SharedString = found.map_or_else(|| "deleted-channel".into(), |c| c.name.clone());
    let encrypted = found.is_some_and(|c| c.encrypted);
    let server = rows.server;
    let target = channel.clone();
    let hovered = rows.hovered.borrow().as_ref() == Some(channel);
    let color = if hovered { ink.primary } else { ink.muted };
    let hover_rows = rows.clone();
    let hover_channel = channel.clone();
    div()
        .px_2()
        .pt(px(if first { 12. } else { 12. - result_row::CARD_GAP }))
        .pb_1p5()
        .flex()
        .child(
            div()
                .id(gpui::ElementId::Name(format!("search-heading-{channel}").into()))
                .flex()
                .max_w_full()
                .min_w_0()
                .h(px(18.))
                .items_center()
                .gap_1()
                .rounded(ink.radius_sm)
                .px_1p5()
                .cursor_pointer()
                .on_hover(move |on, _, cx| {
                    let next = on.then(|| hover_channel.clone());
                    if *hover_rows.hovered.borrow() != next {
                        *hover_rows.hovered.borrow_mut() = next;
                        let _ = hover_rows.view.update(cx, |_, cx| cx.notify());
                    }
                })
                .on_click(move |_, _, cx| actions::chat::go_to_message(server, target.clone(), 0, cx))
                .child(div().flex_none().child(Label::new("#").size(12.).weight(600.).color(color).nowrap()))
                .child(div().min_w_0().child(Label::new(name).size(12.).weight(600.).color(color).truncate()))
                .when(encrypted, |this| this.child(Icon::new(IconName::ChatLock).size(px(11.)).color(color))),
        )
        .into_any_element()
}

/// The card of one result.
fn result_card(message: &Arc<Message>, rows: &Rc<Rows>, ink: &Ink, window: &mut Window, cx: &mut App) -> AnyElement {
    let server = rows.server;
    let style = &rows.style;
    let model = Model::global(cx).clone();
    let sender = decibell_common::user_name(message.sender_uid, &message.sender, cx);
    let name = decibell_common::display_name(Some(server), message.sender_uid, &sender, cx);
    if decibell_shell::probes::enabled() {
        decibell_shell::probes::text(format!("search-result-author:{}", message.id), &name);
    }
    let name_color = person_name_color(message.sender_uid, &sender, cx);
    let stamp = message.timestamp.map(|secs| result_stamp(secs, rows.today, rows.time_format)).unwrap_or_default();
    let jumped = message.id == rows.jumped;
    rows.used.borrow_mut().push(message.id);
    let body = (message.decrypt_error.is_none() && !message.content.is_empty()).then(|| {
        let mut built = rows.built.borrow_mut();
        built
            .entry(message.id)
            .or_insert_with(|| {
                let engine = Engine::global(cx);
                Rc::new(result_row::body(message, rows.highlighter.as_deref(), rows.column, style, &engine))
            })
            .clone()
    });
    let id = message.id;
    let channel = message.channel.clone();
    let view = rows.view.clone();
    let jump = move |window: &mut Window, cx: &mut App| {
        let narrow = is_narrow(window, cx);
        let _ = view.update(cx, |_, cx| {
            let model = Model::global(cx).clone();
            model.chat.search.update(cx, |s, cx| s.set_jumped(id, cx));
        });
        actions::chat::go_to_message(server, channel.clone(), id, cx);
        // Narrow windows: the panel floats over the chat, so it gets out of the way of the
        // message (Enter in the box brings it back).
        if narrow {
            let model = Model::global(cx).clone();
            model.chat.search.update(cx, |s, cx| s.close(cx));
        }
    };
    let jump = Rc::new(jump);
    let focus = window
        .use_keyed_state(("search-result-focus", id as u64 as usize), cx, |_, cx| cx.focus_handle().tab_stop(true))
        .read(cx)
        .clone();
    let ring = cx.theme().shadow_ring.clone();
    let group: SharedString = format!("search-result-{id}").into();

    let reply = message.reply.as_ref().map(|reply| {
        let mut line = div().mb_1().flex().min_w_0().h(px(result_row::REPLY_LINE)).items_center().gap_1p5().pl(px(42.)).child(
            Icon::new(IconName::ChatReply).size(px(12.)).color(ink.muted),
        );
        if reply.sender.is_empty() {
            line = line.child(Label::new("Original message was deleted").size(12.).italic().color(ink.muted).nowrap());
        } else {
            let reply_name = decibell_common::display_name(Some(server), reply.sender_uid, &reply.sender, cx);
            let color = person_name_color(reply.sender_uid, &reply.sender, cx);
            let document = decibell_richtext::plain::preview(&reply.content);
            let preview = decibell_richtext::plain::as_displayed(&document, &reply.content).replace('\n', " ");
            line = line
                .child(div().flex_none().child(Label::new(reply_name).size(12.).weight(600.).color(color).nowrap()))
                .child(div().min_w_0().child(Label::new(preview).size(12.).color(ink.muted).truncate()));
        }
        line
    });

    let header = div()
        .flex()
        .h(px(style.header_height))
        .gap_2()
        .pr(px(result_row::HEADER_PAD_RIGHT))
        .child(
            div()
                .min_w_0()
                .mt(px(style.name_top))
                .child(Label::new(name).font(FontRole::Channel).size(14.).weight(600.).color(name_color).truncate()),
        )
        .child(div().flex_none().mt(px(style.time_top)).child(Label::new(stamp).font(FontRole::Meta).size(12.).color(ink.muted).nowrap()));

    let text: Option<AnyElement> = if let Some(error) = &message.decrypt_error {
        Some(
            div()
                .mt_0p5()
                .flex()
                .items_center()
                .gap_1p5()
                .child(Icon::new(IconName::ChatLock).size(px(12.)).color(ink.muted))
                .child(
                    div().min_w_0().flex_1().child(
                        Label::new(result_row::decrypt_text(error))
                            .size(style.notice.size)
                            .italic()
                            .line_height(style.notice.size * cx.theme().palette.t_lh_body)
                            .color(ink.muted),
                    ),
                )
                .into_any_element(),
        )
    } else {
        body.map(|body| div().mt_0p5().child(BodyText::new(body)).into_any_element())
    };

    let (media, files) = result_row::split_attachments(message);
    let media_row = (!media.is_empty()).then(|| {
        let more = media.len().saturating_sub(4);
        div()
            .mt_2()
            .flex()
            .gap_1p5()
            .children(media.iter().take(4).enumerate().map(|(i, a)| {
                // The picture itself is the attachments feature's: until it draws here, the
                // tile is its placeholder (a thumbnail with a source shows the dark tile while it
                // loads; one without shows the kind's icon, as the renderer does).
                let has_source = a.kind == decibell_attachments::AttachmentKind::Image || a.thumbnail_size_bytes > 0;
                let video = a.kind == decibell_attachments::AttachmentKind::Video;
                div()
                    .id(("search-media", a.id as u64))
                    // `title={a.filename}`.
                    .tooltip(decibell_ui::title_tooltip(a.filename.clone()))
                    .relative()
                    .size_16()
                    .flex_none()
                    .overflow_hidden()
                    .rounded(ink.radius_md)
                    .bg(ink.darkest)
                    .when(!has_source, |this| {
                        this.flex().items_center().justify_center().child(
                            Icon::new(if video { IconName::SettingsFilm } else { IconName::SettingsImage }).size(px(18.)).color(ink.muted),
                        )
                    })
                    .when(video && has_source, |this| {
                        // A literal of the renderer's: `bg-black/60 text-white`.
                        this.child(
                            div()
                                .absolute()
                                .bottom_1()
                                .left_1()
                                .flex()
                                .size_4()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(gpui::black().opacity(0.6))
                                .child(Icon::new(IconName::SearchPlay).size(px(8.)).color(gpui::white())),
                        )
                    })
                    .when(i == 3 && more > 0, |this| {
                        // `bg-black/55 text-white`.
                        this.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(gpui::black().opacity(0.55))
                                .child(Label::new(format!("+{more}")).size(13.).weight(600.).color(gpui::white()).nowrap()),
                        )
                    })
            }))
    });
    let file_rows = files.iter().map(|a| {
        let icon = match a.kind {
            decibell_attachments::AttachmentKind::Audio => IconName::SettingsMusic,
            decibell_attachments::AttachmentKind::Video => IconName::SettingsFilm,
            decibell_attachments::AttachmentKind::Image => IconName::SettingsImage,
            decibell_attachments::AttachmentKind::Document => IconName::SettingsFile,
        };
        let size: SharedString = if a.purged_at > 0 { "removed".into() } else { result_row::format_file_size(a.size_bytes).into() };
        div()
            .mt_2()
            .flex()
            .max_w_full()
            .h(px(result_row::file_row_height(style)))
            .items_center()
            .gap_2()
            .rounded(ink.radius_md)
            .border_1()
            .border_color(ink.divider)
            .bg(ink.dark)
            .px_2p5()
            .child(Icon::new(icon).size(px(15.)).color(ink.muted))
            .child(div().min_w_0().flex_1().child(
                Runs::new(style.file_name, Wrapping::TRUNCATE).runs(result_row::marked_runs(&a.filename, rows.highlighter.as_deref(), style.file_name, style)),
            ))
            .child(div().flex_none().child(Label::new(size).size(12.).color(ink.muted).nowrap()))
    });

    let column = div().min_w_0().flex_1().child(header).children(text).children(media_row).children(file_rows);
    let (keys_jump, click_jump) = (jump.clone(), jump.clone());
    let card = div()
        .id(("search-result", id as u64 as usize))
        .group(group.clone())
        .track_focus(&focus)
        .relative()
        .rounded(ink.radius_lg)
        .border_1()
        .px_3()
        .py_2p5()
        .cursor_pointer()
        .map(|this| {
            if jumped {
                this.border_color(ink.accent_half).bg(ink.light)
            } else {
                let light = ink.light;
                this.border_color(gpui::transparent_black()).bg(ink.mid).hover(move |s| s.bg(light))
            }
        })
        .focus_visible(move |s| s.shadow(ring))
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            if event.keystroke.key == "enter" {
                cx.stop_propagation();
                keys_jump(window, cx);
            }
        })
        .on_click(move |_, window, cx| click_jump(window, cx))
        .children(reply)
        .child(div().flex().gap_2p5().child(UserAvatar::new(message.sender_uid, sender, 32.)).child(column))
        .child(
            div()
                .absolute()
                .right_2()
                .top_2()
                .invisible()
                .group_hover(group.clone(), |s| s.visible())
                .rounded(ink.radius_sm)
                .border_1()
                .border_color(ink.border)
                .bg(ink.lighter)
                .px_2()
                .py_0p5()
                .child(Label::new("Jump").size(12.).weight(600.).color(ink.secondary).nowrap()),
        )
        .children(crate::probe(format!("search-result:{id}")));
    let _ = model;
    div().px_2().pb(px(result_row::CARD_GAP)).child(card).into_any_element()
}
