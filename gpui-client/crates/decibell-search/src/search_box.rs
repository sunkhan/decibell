//! The search box in a text channel's header (`features/search/SearchBox.tsx`, spec 2026-10-14):
//! filters are chips before the text; typing `from:`, `in:`, `has:`, `before:`, `during:` or
//! `after:` opens a picker for it; Enter runs the search into the results panel; Ctrl+F (⌘F)
//! focuses it from anywhere in the server view.
//!
//! The view fills `Slot::HeaderSearch` (244 px at the header's end) and follows the navigation's
//! community. Its text and chips are the community's draft in the search store (memory only:
//! nothing about a search is stored on the device). It is drawn cached: a key re-renders this
//! view and the field, never the chat or the results.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use decibell_common::Watch;
use decibell_model::chat::search::{Day, SearchFilter, SearchQuery};
use decibell_model::stores::community::{Channel, Member};
use decibell_model::{ChannelId, Model, ServerId, Uid};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, FocusNext, FocusPrev, Icon, IconName, Label, TextField, title_tooltip};
use gpui::{
    App, Bounds, ClickEvent, Context, Entity, Focusable as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Render,
    SharedString, Subscription, Task, Window, canvas, div, linear_color_stop, linear_gradient, prelude::*, px,
};

use crate::calendar;
use crate::format::{format_day, today};
use crate::menu::{Row, build_rows, default_active, step_active};
use crate::tokens::{
    self, ChannelRef, MemberRef, Op, TokenContext, TypedToken, add_chip, chip_tokens, is_searchable_channel, on_text, resolve_token,
    start_op, toggle_inside, trailing_token,
};

/// The box's top in the header (see `render`).
const BOX_TOP: f32 = 8.;
/// Typing settles this long before the member lookup asks again (`useMemberSearch`'s debounce).
const LOOKUP_DEBOUNCE: Duration = Duration::from_millis(250);
/// The member lookup's seqs: apart from other member searches' (server settings count from 1).
const LOOKUP_SEQ_BASE: u32 = 0x5EA0_0000;

/// The server-side member search while a `from:` name is typed (`MemberLookup`): offline members
/// past the loaded roster can be picked. Exists while that token has a value.
struct Lookup {
    applied: String,
    seq: u32,
    debounce: Option<Task<()>>,
    _answers: Subscription,
}

pub struct SearchBox {
    pub(crate) model: Model,
    pub(crate) server: Option<ServerId>,
    pub(crate) channel: Option<ChannelId>,
    pub(crate) input: Entity<InputState>,
    /// The box's text and chips (the community's draft in the store).
    pub(crate) draft: SearchQuery,
    pub(crate) focused: bool,
    pub(crate) menu: bool,
    /// Bumped each time the menu opens: its entrance's id.
    pub(crate) menu_epoch: u32,
    pub(crate) active: i32,
    active_key: (Option<Op>, usize, i32),
    /// The calendar's highlighted day.
    pub(crate) cal_day: Day,
    pub(crate) token: Option<TypedToken>,
    pub(crate) rows: Vec<Row>,
    ctx: Option<Rc<TokenContext>>,
    /// Members the lookup found (kept after it ends, as the renderer's `found` state is).
    found: Vec<MemberRef>,
    lookup: Option<Lookup>,
    next_seq: u32,
    community: Option<(ServerId, [Subscription; 2])>,
    focus_seen: u64,
    pub(crate) chips_scroll: gpui::ScrollHandle,
    /// Where the menu was last drawn (a press there must not count as outside the box).
    pub(crate) menu_bounds: Rc<Cell<Bounds<Pixels>>>,
    _watch: Watch,
    _subscriptions: Vec<Subscription>,
}

// Search messages (Ctrl+F / ⌘F): focuses the box from anywhere in the server view.
gpui::actions!(decibell_search, [FocusSearch]);

impl SearchBox {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let input = cx.new(|cx| InputState::single_line(window, cx).placeholder("Search"));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| this.input_event(event, window, cx)),
            cx.observe(&model.nav, |this, _, cx| this.follow(cx)),
            // A community connecting: its channels and roster to watch.
            cx.observe(&model.communities, |this, _, cx| {
                this.community = None;
                this.ctx = None;
                this.watch_community(cx);
            }),
            // Ctrl+F asks the store; the box that is drawn takes the focus.
            cx.observe_in(&model.chat.search, window, |this, store, window, cx| {
                let nonce = store.read(cx).focus_nonce;
                if nonce != this.focus_seen {
                    this.focus_seen = nonce;
                    this.input.update(cx, |input, cx| {
                        input.focus(window, cx);
                        input.select_all(cx);
                    });
                }
            }),
        ];
        let mut watch = Watch::new();
        // Chips name people by their current name (renames, nicknames) and the menu lists them.
        watch.people(cx, |this: &SearchBox, uid| this.shows_person(uid));
        let focus_seen = model.chat.search.read(cx).focus_nonce;
        let mut this = Self {
            model,
            server: None,
            channel: None,
            input,
            draft: SearchQuery::default(),
            focused: false,
            menu: false,
            menu_epoch: 0,
            active: -1,
            active_key: (None, 0, -1),
            cal_day: today(cx),
            token: None,
            rows: Vec::new(),
            ctx: None,
            found: Vec::new(),
            lookup: None,
            next_seq: LOOKUP_SEQ_BASE,
            community: None,
            focus_seen,
            chips_scroll: gpui::ScrollHandle::new(),
            menu_bounds: Rc::new(Cell::new(Bounds::default())),
            _watch: watch,
            _subscriptions: subscriptions,
        };
        this.follow(cx);
        // The screens' box takes the focus once it is drawn (a capture can't click).
        #[cfg(feature = "fixtures")]
        if cx.has_global::<crate::fixture::FocusOnMount>() {
            // Drawn focused with the menu open from the first frame (the focus itself lands
            // after it).
            this.focused = true;
            this.open_menu(cx);
            let input = this.input.clone();
            window.defer(cx, move |window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
        }
        this
    }

    fn shows_person(&self, uid: Uid) -> bool {
        self.draft.filters.iter().any(|f| matches!(f, SearchFilter::From { uid: u, .. } if *u == uid))
            || (self.menu && self.rows.iter().any(|r| matches!(r, Row::Member(m) if m.uid == uid)))
    }

    /// The navigation moved: another community's draft, or another channel (`this channel`).
    fn follow(&mut self, cx: &mut Context<Self>) {
        let (server, channel) = {
            let nav = self.model.nav.read(cx);
            (nav.active_server, nav.active_channel.clone())
        };
        if server != self.server {
            // The old community keeps its draft; the new one's comes back.
            if let Some(old) = self.server {
                let draft = self.draft.clone();
                self.model.chat.search.update(cx, |s, cx| s.set_draft(old, draft, cx));
            }
            self.server = server;
            self.ctx = None;
            self.found.clear();
            self.lookup = None;
            self.community = None;
            let draft = server.map(|s| self.model.chat.search.read(cx).draft(s)).unwrap_or_default();
            self.set_draft(draft, true, cx);
        }
        if channel != self.channel {
            self.channel = channel;
            self.refresh_rows(cx);
        }
        self.watch_community(cx);
        cx.notify();
    }

    /// The community's channels and roster: what the menu lists and the chips resolve against.
    fn watch_community(&mut self, cx: &mut Context<Self>) {
        let Some(server) = self.server else { return };
        if self.community.as_ref().is_some_and(|(id, _)| *id == server) {
            return;
        }
        let Some(community) = self.model.community(server, cx) else { return };
        let stale = |this: &mut SearchBox, cx: &mut Context<SearchBox>| {
            this.ctx = None;
            if this.menu {
                this.refresh_rows(cx);
                cx.notify();
            } else if this.draft.filters.iter().any(|f| matches!(f, SearchFilter::From { .. } | SearchFilter::In { .. })) {
                // A chip names a member (their nickname) or a channel (its name).
                cx.notify();
            }
        };
        self.community = Some((
            server,
            [cx.observe(&community.channels, move |this, _, cx| stale(this, cx)), cx.observe(&community.roster, move |this, _, cx| stale(this, cx))],
        ));
    }

    /// What filters can name: the readable text channels, the loaded roster (online first, in
    /// roster order, as the renderer's `membersByServer` holds a first page) then the lookup's.
    pub(crate) fn ctx(&mut self, cx: &mut App) -> Rc<TokenContext> {
        if let Some(ctx) = &self.ctx {
            return ctx.clone();
        }
        let ctx = Rc::new(self.build_ctx(cx));
        self.ctx = Some(ctx.clone());
        ctx
    }

    fn build_ctx(&self, cx: &App) -> TokenContext {
        let Some(server) = self.server else { return TokenContext::default() };
        let Some(community) = self.model.community(server, cx) else { return TokenContext::default() };
        let is_owner = self.model.is_owner(server, cx);
        let channels = community
            .channels
            .read(cx)
            .list()
            .iter()
            .filter(|c| is_searchable_channel(c, is_owner))
            .map(|c: &Channel| ChannelRef { id: c.id.clone(), name: c.name.clone(), encrypted: c.encrypted })
            .collect();
        let roster = community.roster.read(cx);
        let mut held: Vec<&Member> = roster.members().values().collect();
        held.sort_by(|a, b| b.is_online.cmp(&a.is_online).then_with(|| a.key.cmp(&b.key)));
        let mut members: Vec<MemberRef> = held
            .iter()
            .map(|m| MemberRef { uid: m.uid, name: self.model.name_of(m.uid, &m.username, cx), nickname: m.nickname.clone() })
            .collect();
        for m in &self.found {
            if !members.iter().any(|known| known.uid == m.uid) {
                members.push(MemberRef { name: self.model.name_of(m.uid, &m.name, cx), ..m.clone() });
            }
        }
        TokenContext { members, channels }
    }

    /// Replace the box's contents. `to_input`: also put the text in the field.
    fn set_draft(&mut self, draft: SearchQuery, to_input: bool, cx: &mut Context<Self>) {
        let filters_changed = draft.filters.len() != self.draft.filters.len();
        self.draft = draft;
        if to_input {
            // Its `Change` comes back after this update: the text then equals the draft's.
            let text = self.draft.text.clone();
            self.input.update(cx, |input, cx| input.set_value(&text, cx));
        }
        let placeholder = if self.draft.filters.is_empty() { "Search" } else { "" };
        self.input.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
        if let Some(server) = self.server {
            let draft = self.draft.clone();
            self.model.chat.search.update(cx, |s, cx| s.set_draft(server, draft, cx));
        }
        if filters_changed && self.focused {
            // The focused field stays in view after the chips (the row scrolls sideways when they
            // outgrow it, as Chromium brings a focused input into view).
            self.chips_scroll.scroll_to_item(self.draft.filters.len());
        }
        self.refresh_rows(cx);
        cx.notify();
    }

    /// The token at the end of the text, the menu's rows, the active row as the renderer's
    /// effects reset it, the member lookup.
    pub(crate) fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        self.token = trailing_token(&self.draft.text);
        let today = today(cx);
        // A typed date jumps the calendar to it.
        if let Some(token) = self.token.as_ref().filter(|t| t.op.is_date())
            && let Some(day) = decibell_model::chat::search::parse_day(&token.value, today)
        {
            self.cal_day = day;
        }
        let ctx = self.ctx(cx);
        self.rows = build_rows(self.token.as_ref(), &self.draft, &ctx.members, &ctx.channels, self.channel.as_ref(), today);
        let default = default_active(self.token.as_ref(), &self.rows);
        let key = (self.token.as_ref().map(|t| t.op), self.rows.len(), default);
        if key != self.active_key {
            self.active_key = key;
            self.active = default;
        }
        self.update_lookup(cx);
    }

    fn update_lookup(&mut self, cx: &mut Context<Self>) {
        let query = self
            .token
            .as_ref()
            .filter(|t| t.op == Op::From)
            .map(|t| tokens::js_trim(t.value.strip_prefix('@').unwrap_or(&t.value)).to_string())
            .filter(|q| !q.is_empty());
        let (Some(query), Some(server)) = (query, self.server) else {
            self.lookup = None;
            return;
        };
        match &mut self.lookup {
            None => {
                // Mounting starts with nothing found, and asks at once.
                self.found.clear();
                self.ctx = None;
                let answers = cx.subscribe(&self.model.answers, move |this, _, event: &decibell_core::events::CoreEvent, cx| {
                    let decibell_core::events::CoreEvent::MemberSearchReceived(p) = event else { return };
                    let Some(lookup) = &this.lookup else { return };
                    if ServerId::from_wire(&p.server_id) != server || p.seq != lookup.seq || !p.success {
                        return;
                    }
                    let found: Vec<MemberRef> = p
                        .members
                        .iter()
                        .map(|m| MemberRef { uid: m.uid, name: m.username.clone().into(), nickname: m.nickname.clone().into() })
                        .collect();
                    this.found = if p.first_page { found } else { [std::mem::take(&mut this.found), found].concat() };
                    this.ctx = None;
                    this.refresh_rows(cx);
                    cx.notify();
                });
                let seq = self.take_seq();
                self.lookup = Some(Lookup { applied: query.clone(), seq, debounce: None, _answers: answers });
                send_lookup(server, query, seq, cx);
            }
            Some(lookup) if lookup.applied != query => {
                lookup.debounce = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(LOOKUP_DEBOUNCE).await;
                    let _ = this.update(cx, |this, cx| {
                        let seq = this.take_seq();
                        if let Some(lookup) = &mut this.lookup {
                            lookup.applied = query.clone();
                            lookup.seq = seq;
                            lookup.debounce = None;
                            send_lookup(server, query, seq, cx);
                        }
                    });
                }));
            }
            Some(lookup) => lookup.debounce = None,
        }
    }

    fn take_seq(&mut self) -> u32 {
        self.next_seq = self.next_seq.wrapping_add(1).max(LOOKUP_SEQ_BASE);
        self.next_seq
    }

    fn input_event(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                let text = self.input.read(cx).value().to_string();
                if text == *self.draft.text {
                    // The view's own `set_value`, or nothing new.
                    return;
                }
                let ctx = self.ctx(cx);
                let (draft, typed) = on_text(&text, &self.draft, &ctx, today(cx));
                if typed {
                    self.set_draft(draft, false, cx);
                    self.open_menu(cx);
                } else {
                    self.set_draft(draft, true, cx);
                }
            }
            InputEvent::Focus => {
                self.focused = true;
                self.chips_scroll.scroll_to_item(self.draft.filters.len());
                self.open_menu(cx);
                cx.notify();
            }
            InputEvent::Blur => {
                self.focused = false;
                self.menu = false;
                cx.notify();
            }
            _ => {}
        }
        let _ = window;
    }

    pub(crate) fn open_menu(&mut self, cx: &mut Context<Self>) {
        if !self.menu {
            self.menu = true;
            self.menu_epoch = self.menu_epoch.wrapping_add(1);
            self.refresh_rows(cx);
            cx.notify();
        }
    }

    fn date_op(&self) -> Option<Op> {
        self.token.as_ref().map(|t| t.op).filter(|op| op.is_date())
    }

    pub(crate) fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.clone();
        // `requestAnimationFrame(() => input.focus())`: after this change is drawn.
        window.defer(cx, move |window, cx| {
            let focused = input.read(cx).focus_handle(cx).is_focused(window);
            if !focused {
                input.update(cx, |input, cx| input.focus(window, cx));
            }
        });
    }

    /// `addChip`.
    pub(crate) fn add_chip(&mut self, f: SearchFilter, window: &mut Window, cx: &mut Context<Self>) {
        let draft = add_chip(&self.draft, self.token.as_ref(), f);
        self.set_draft(draft, true, cx);
        self.focus_input(window, cx);
    }

    /// `doRun`: every complete filter in the text becomes a chip, the search runs into the
    /// panel, the menu closes and the box lets go of the focus.
    pub(crate) fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.server else { return };
        let ctx = self.ctx(cx);
        let query = chip_tokens(&self.draft, &ctx, today(cx));
        if !query.is_searchable() {
            return;
        }
        self.set_draft(query.clone(), true, cx);
        self.model.chat.search.update(cx, |s, cx| s.run(server, &query, cx));
        self.menu = false;
        window.blur(cx);
        cx.notify();
    }

    /// `pick`.
    pub(crate) fn pick(&mut self, row: Row, window: &mut Window, cx: &mut Context<Self>) {
        match row {
            Row::Run(_) => self.run(window, cx),
            Row::Op(op) => {
                let draft = start_op(&self.draft, op);
                self.set_draft(draft, true, cx);
                self.open_menu(cx);
                self.focus_input(window, cx);
            }
            Row::Inside => {
                let draft = toggle_inside(&self.draft);
                self.set_draft(draft, false, cx);
                self.focus_input(window, cx);
            }
            Row::Member(m) => self.add_chip(SearchFilter::From { uid: m.uid, username: m.name }, window, cx),
            Row::Channel(c) => self.add_chip(SearchFilter::In { channel: c.id }, window, cx),
            Row::Has(kind) => self.add_chip(SearchFilter::Has(kind), window, cx),
            Row::Day { day, .. } => {
                if let Some(f) = self.date_op().and_then(|op| op.date_filter(day)) {
                    self.add_chip(f, window, cx);
                }
            }
        }
    }

    /// A day of the calendar picked (a click, or Enter / Tab on the highlighted one).
    pub(crate) fn pick_day(&mut self, day: Day, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = self.date_op().and_then(|op| op.date_filter(day)) {
            self.add_chip(f, window, cx);
        }
    }

    pub(crate) fn set_cal_day(&mut self, day: Day, cx: &mut Context<Self>) {
        if self.cal_day != day {
            self.cal_day = day;
            cx.notify();
        }
    }

    pub(crate) fn set_active(&mut self, active: i32, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            cx.notify();
        }
    }

    pub(crate) fn remove_chip(&mut self, index: usize, cx: &mut Context<Self>) {
        let mut draft = self.draft.clone();
        if index < draft.filters.len() {
            draft.filters.remove(index);
            self.set_draft(draft, false, cx);
        }
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_draft(SearchQuery::default(), true, cx);
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// `onKeyDown`, in the capture phase: the keys the field must not see first.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.read(cx).is_composing() {
            return;
        }
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            cx.stop_propagation();
            if self.menu {
                self.menu = false;
                cx.notify();
            } else {
                window.blur(cx);
            }
            return;
        }
        if key == "backspace" {
            let selection = self.input.read(cx).selection();
            if selection.anchor == 0 && selection.head == 0 && !self.draft.filters.is_empty() {
                cx.stop_propagation();
                let mut draft = self.draft.clone();
                draft.filters.pop();
                self.set_draft(draft, false, cx);
                return;
            }
        }
        if !self.menu {
            match key {
                "enter" => {
                    cx.stop_propagation();
                    self.run(window, cx);
                }
                // The caret still goes to the end, as in the renderer's `<input>`.
                "down" => self.open_menu(cx),
                _ => {}
            }
            return;
        }
        if self.date_op().is_some() {
            let step = match key {
                "left" => -1,
                "right" => 1,
                "up" => -7,
                "down" => 7,
                _ => 0,
            };
            if step != 0 {
                cx.stop_propagation();
                let day = calendar::move_day(self.cal_day, step, today(cx));
                self.set_cal_day(day, cx);
            } else if key == "enter" {
                cx.stop_propagation();
                self.pick_typed_day(window, cx);
            }
            return;
        }
        match key {
            "down" | "up" => {
                cx.stop_propagation();
                let active = step_active(self.active, self.rows.len(), key == "down");
                self.set_active(active, cx);
            }
            "enter" => {
                cx.stop_propagation();
                match usize::try_from(self.active).ok().and_then(|i| self.rows.get(i)).cloned() {
                    Some(row) => self.pick(row, window, cx),
                    None => self.run(window, cx),
                }
            }
            _ => {}
        }
    }

    /// Enter or Tab on a date picker: the typed date, else the calendar's.
    fn pick_typed_day(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.token.as_ref().and_then(|t| decibell_model::chat::search::parse_day(&t.value, today(cx)));
        let day = typed.unwrap_or(self.cal_day);
        self.pick_day(day, window, cx);
    }

    /// Tab (and Shift+Tab: the renderer reads `e.key`) while the menu is open: picks the
    /// active row of a picker, or the calendar's day. Otherwise focus moves on as usual.
    fn tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.menu || self.input.read(cx).is_composing() {
            return;
        }
        if self.date_op().is_some() {
            cx.stop_propagation();
            self.pick_typed_day(window, cx);
            return;
        }
        if self.token.is_some()
            && let Some(row) = usize::try_from(self.active).ok().and_then(|i| self.rows.get(i)).cloned()
        {
            cx.stop_propagation();
            self.pick(row, window, cx);
        }
    }

    /// A chip's text: what it filters on, then the value (`chipText`).
    pub(crate) fn chip_text(&self, f: &SearchFilter, cx: &App) -> (&'static str, SharedString) {
        match f {
            SearchFilter::From { uid, username } => ("from", decibell_common::display_name(self.server, *uid, username, cx)),
            SearchFilter::In { channel } => {
                let name = self
                    .server
                    .and_then(|s| self.model.community(s, cx))
                    .and_then(|c| c.channels.read(cx).get(channel).map(|c| c.name.clone()));
                ("in", format!("#{}", name.as_deref().unwrap_or("deleted-channel")).into())
            }
            SearchFilter::Has(kind) => ("has", kind.label().to_lowercase().into()),
            SearchFilter::Inside => ("match", "inside words".into()),
            SearchFilter::Before(day) => ("before", format_day(*day).into()),
            SearchFilter::During(day) => ("on", format_day(*day).into()),
            SearchFilter::After(day) => ("after", format_day(*day).into()),
        }
    }

    /// Resolve a typed filter now (tests).
    pub fn resolve(&mut self, op: Op, value: &str, cx: &mut App) -> Option<SearchFilter> {
        let ctx = self.ctx(cx);
        resolve_token(op, value, &ctx, today(cx))
    }

    /// The input (tests, the self-tests' typing).
    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    pub fn draft(&self) -> &SearchQuery {
        &self.draft
    }

    pub fn menu_open(&self) -> bool {
        self.menu
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }
}

fn send_lookup(server: ServerId, query: String, seq: u32, cx: &mut App) {
    use decibell_core::api;
    let args = api::community::SearchMembersArgs {
        server_id: server.wire(),
        query: Some(query),
        role_id: Some(0),
        after: Some(String::new()),
        limit: Some(100),
        seq: Some(seq),
    };
    decibell_model::call::call(cx, api::community::search_members(args), |result, _| {
        if let Err(e) = result {
            log::debug!("member lookup: {e}");
        }
    });
}

impl Render for SearchBox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `text-[13px]`, `tracking-normal`, line-height `normal`: the face's rounded ascent and
        // descent (Blink's line spacing), so the text sits where Chromium centres it in the input.
        let mut style = TextField::input_style(decibell_ui::FontRole::Sans, 13., 1.2109, cx);
        style.text.letter_spacing = 0.;
        let scale = window.scale_factor();
        let (ascent, descent) = style.text.face.line_metrics(13., scale);
        style.text.line_height = decibell_text::LineHeight::Px((ascent + descent) / scale);
        let theme = cx.theme();
        let focused = self.focused;
        let has_content = !self.draft.text.is_empty() || !self.draft.filters.is_empty();
        let scrolled = self.chips_scroll.offset().x < px(0.);

        let texts: Vec<(&'static str, SharedString)> = self.draft.filters.iter().map(|f| self.chip_text(f, cx)).collect();
        let chips: Vec<gpui::AnyElement> = texts.into_iter().enumerate().map(|(index, (op, value))| chip(index, op, value, cx)).collect();

        let menu_bounds = self.menu_bounds.clone();
        let box_ = div()
            .id("search-box")
            .relative()
            .flex()
            .h_8()
            .w_full()
            .flex_none()
            .items_center()
            .gap_1()
            .rounded(theme.radius_md)
            .border_1()
            .bg(theme.color_bg_lighter)
            .pl_1p5()
            .pr_1()
            .cursor_text()
            .map(|this| {
                if focused {
                    this.border_color(theme.color_accent).shadow(theme.shadow_ring.clone())
                } else {
                    let hover = theme.color_accent.opacity(0.4);
                    this.border_color(theme.color_border).hover(move |s| s.border_color(hover))
                }
            })
            .tooltip(title_tooltip("Search messages (Ctrl+F)"))
            // Clicks on the box's padding and chips focus the text.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    if window.default_prevented() {
                        return;
                    }
                    window.prevent_default();
                    this.input.update(cx, |input, cx| input.focus(window, cx));
                }),
            )
            .when(focused, |this| {
                this.on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // A press on the menu is the box's own (`onMouseDown` preventDefault there).
                    if this.menu && menu_bounds.get().contains(&event.position) {
                        return;
                    }
                    this.blur_on_outside(window, cx);
                }))
            })
            .capture_key_down(cx.listener(Self::key_down))
            .capture_action(cx.listener(|this, _: &FocusNext, window, cx| this.tab(window, cx)))
            .capture_action(cx.listener(|this, _: &FocusPrev, window, cx| this.tab(window, cx)))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        div()
                            .id("search-chips")
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .items_center()
                            .gap_1()
                            .overflow_x_scroll()
                            .track_scroll(&self.chips_scroll)
                            .children(chips)
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_w(px(56.))
                                    .h_full()
                                    .items_center()
                                    .pl_1()
                                    .child(decibell_text::input::TextInput::new(&self.input, style)),
                            ),
                    )
                    // `mask-image: linear-gradient(to right, transparent, black 2.5rem)` once
                    // scrolled: the box's own ground fading in over the cut-off chip.
                    .when(scrolled, |this| {
                        this.child(div().absolute().top_0().bottom_0().left_0().w(px(40.)).bg(linear_gradient(
                            90.,
                            linear_color_stop(theme.color_bg_lighter, 0.),
                            linear_color_stop(theme.color_bg_lighter.opacity(0.), 1.),
                        )))
                    }),
            )
            .child(if has_content {
                div()
                    .id("search-clear")
                    .flex()
                    .flex_none()
                    .size_6()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_sm)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.color_surface_hover))
                    .group("search-clear")
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.clear(window, cx)))
                    .child(
                        Icon::new(IconName::SettingsX)
                            .size(px(12.))
                            .stroke_width(2.5)
                            .color(theme.color_text_muted)
                            .group_hover("search-clear", theme.color_text_primary),
                    )
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_none()
                    .size_6()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::SettingsSearch).size(px(14.)).color(theme.color_text_muted))
                    .into_any_element()
            })
            .children(decibell_shell::probes::probe("search-box"));

        let engine = decibell_text::Engine::global(cx);
        let menu = self.menu.then(|| self.render_menu(&engine, window, cx));
        // Chromium centres the 32 px box in the header's 47 px at 7.5 and draws it from 8.
        div().relative().size_full().flex().items_start().pt(px(BOX_TOP)).child(box_).children(menu)
    }
}

impl SearchBox {
    /// A press outside the box and its menu: the field loses the focus, as on a page.
    fn blur_on_outside(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.read(cx).focus_handle(cx).is_focused(window) {
            // Whatever was pressed takes the focus itself if it can (GPUI focuses on mouse down
            // after this capture-phase listener); if it can't, nothing keeps it.
            window.blur(cx);
        }
    }
}

/// A chip (`Chip`): `op:` muted, the value, a remove button.
fn chip(index: usize, op: &'static str, value: SharedString, cx: &Context<SearchBox>) -> gpui::AnyElement {
    let theme = cx.theme();
    let group: SharedString = format!("search-chip-x-{index}").into();
    div()
        .id(("search-chip", index))
        .flex()
        .flex_none()
        .h_6()
        .max_w(px(180.))
        .items_center()
        .gap_1()
        .rounded(theme.radius_sm)
        .bg(theme.color_accent_soft)
        .pl_1p5()
        .pr_0p5()
        .child(div().flex_none().child(Label::new(format!("{op}:")).size(12.).weight(500.).tracking(0.).color(theme.color_text_muted).nowrap()))
        .child(div().min_w_0().child(Label::new(value).size(12.).weight(500.).tracking(0.).color(theme.color_text_primary).truncate()))
        .child(
            div()
                .id(("search-chip-remove", index))
                .group(group.clone())
                .flex()
                .flex_none()
                .size_5()
                .items_center()
                .justify_center()
                .rounded(theme.radius_sm)
                .cursor_pointer()
                .hover(|s| s.bg(theme.color_surface_hover))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.remove_chip(index, cx);
                }))
                .child(Icon::new(IconName::SettingsX).size(px(10.)).stroke_width(3.).color(theme.color_text_muted).group_hover(group, theme.color_text_primary)),
        )
        .into_any_element()
}

/// The canvas that records where the menu is (a press there must not blur the box).
pub(crate) fn bounds_probe(cell: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {}).absolute().size_full()
}
