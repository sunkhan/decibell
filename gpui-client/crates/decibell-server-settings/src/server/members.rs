//! Members (`tabs/MembersTab.tsx`): a searchable, role-filterable list of the community's
//! members, searched on the server (`MEMBER_SEARCH_REQ`; results never touch the roster store).
//! Each row shows who the member is, their roles (assignable in place) and when they joined;
//! every moderation action lives behind the row's ⋯ menu (or a right click), so rows stay quiet.
//! Hierarchy gates mirror the server. Windowed against the pane; the toolbar and column heads
//! stay put while the list scrolls.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use decibell_common::{AvatarDot, BoundsCell, UserAvatar, Watch, presence_of, user_name};
use decibell_core::events::CoreEvent;
use decibell_model::regional::{self, Regional};
use decibell_model::save_echo::{ECHO_WAIT, EchoSave};
use decibell_model::stores::community::{Member, Role};
use decibell_model::stores::permissions::{highest_role, member_level, perm};
use decibell_model::{Model, ServerId, Uid};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{
    ActiveTheme as _, Button, ButtonVariant, CloseRequest, DismissEvent, EmptyState, FontRole, Icon, IconName, Label,
    Modal, Popover, SectionLabel, Select, SelectIcon, SelectOption,
};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, MouseButton, MouseDownEvent, Pixels, Point,
    Render, SharedString, Styled, Subscription, Task, Window, div, point, prelude::*, px,
};

use super::{SettingsTab, TabContext};
use crate::member_search::MemberSearch;
use crate::requests::{self, Request};
use crate::rows::{PaneRows, StickyTop};
use crate::sentence::{self, Base, Run, Sentence, Shaped};
use crate::ui::{InputBox, RoleChip, SearchBox, dot, error_banner, menu_divider, menu_item, outline_button, plural, role_color};

/// Visible role chips per row before the rest fold into "+N".
const MAX_CHIPS: usize = 3;
pub const NICKNAME_MAX_BYTES: usize = 32;
/// `h-14`.
const ROW: f32 = 56.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModKind {
    Kick,
    Ban,
    Timeout,
}

/// What the local user may do to a member: one place, read by the row and its menu alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Acts {
    pub nick: bool,
    pub roles: bool,
    pub timeout: bool,
    pub end_timeout: bool,
    pub kick: bool,
    pub ban: bool,
}

impl Acts {
    fn any(self) -> bool {
        self.nick || self.roles || self.timeout || self.end_timeout || self.kick || self.ban
    }
}

/// A role toggle waiting for its echo: the set sent, the record it was built on, and when to
/// stop waiting.
struct PendingRoles {
    role_ids: Vec<i64>,
    base: Rc<[i64]>,
    until: Instant,
}

use crate::now_secs;

/// "2h 59m" / "4d 3h" / "45s" until `epoch` (`formatRemaining`).
pub fn format_remaining(epoch: i64, now: i64) -> String {
    let mut s = (epoch - now).max(0);
    let d = s / 86400;
    s -= d * 86400;
    let h = s / 3600;
    s -= h * 3600;
    let m = s / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

fn same_set(a: &[i64], b: &[i64]) -> bool {
    a.len() == b.len() && a.iter().all(|id| b.contains(id))
}

/// Who may do what: the permissions and the hierarchy, from the stores (`useHierarchy`).
pub struct Gates {
    me: Uid,
    owner: Uid,
    roles: Vec<Role>,
    is_owner: bool,
    level: i32,
    /// Our highest role lets its holders manage each other.
    peer: bool,
    can_kick: bool,
    can_ban: bool,
    can_timeout: bool,
    can_manage_roles: bool,
    can_manage_nicknames: bool,
    can_change_own_nickname: bool,
    /// Roles we may assign or remove: strictly below our highest (the owner bypasses), never the
    /// default role. Mirrors the server's MEMBER_ROLES_UPDATE gate.
    assignable: Vec<Role>,
}

impl Gates {
    pub fn read(server: ServerId, cx: &App) -> Gates {
        let model = Model::global(cx);
        let me = model.auth.read(cx).uid;
        let community = model.community(server, cx);
        let roles: Vec<Role> = community.as_ref().map(|c| c.roles.read(cx).list().to_vec()).unwrap_or_default();
        let owner = community.as_ref().map_or(0, |c| c.meta.read(cx).owner);
        let my_roles: Vec<i64> = community
            .as_ref()
            .and_then(|c| c.roster.read(cx).member(me).map(|m| m.role_ids.to_vec()))
            .unwrap_or_default();
        let is_owner = me != 0 && owner != 0 && owner == me;
        let level = member_level(&roles, &my_roles);
        let peer = highest_role(&roles, &my_roles).is_some_and(|r| r.manage_each_other);
        let has = |bit| model.has_permission(server, bit, cx);
        let assignable: Vec<Role> = roles.iter().filter(|r| !r.is_default && (is_owner || r.position < level)).cloned().collect();
        Gates {
            me,
            owner,
            roles,
            is_owner,
            level,
            peer,
            can_kick: has(perm::KICK_MEMBERS),
            can_ban: has(perm::BAN_MEMBERS),
            can_timeout: has(perm::MODERATE_MEMBERS),
            can_manage_roles: has(perm::MANAGE_ROLES),
            can_manage_nicknames: has(perm::MANAGE_NICKNAMES),
            can_change_own_nickname: has(perm::CHANGE_NICKNAME),
            assignable,
        }
    }

    /// `canManageRecord` (`Hierarchy::can_manage_record`): from the record itself, since an
    /// offline member found by search may not be in the roster.
    fn manages(&self, m: &Member) -> bool {
        if m.uid == self.owner {
            return false;
        }
        if self.is_owner {
            return true;
        }
        let theirs = member_level(&self.roles, &m.role_ids);
        theirs < self.level || (self.peer && theirs == self.level && m.uid != self.me)
    }

    pub fn acts(&self, m: &Member, now: i64) -> Acts {
        let is_self = m.uid == self.me;
        let outranked = self.is_owner || member_level(&self.roles, &m.role_ids) < self.level;
        let timed_out = m.timed_out_until > now;
        let moderate = !m.is_owner && !is_self && outranked;
        Acts {
            nick: (is_self && self.can_change_own_nickname) || (!is_self && self.can_manage_nicknames && self.manages(m)),
            roles: self.can_manage_roles && !m.is_owner && !self.assignable.is_empty(),
            timeout: moderate && self.can_timeout && !timed_out,
            end_timeout: moderate && self.can_timeout && timed_out,
            kick: moderate && self.can_kick,
            ban: moderate && self.can_ban,
        }
    }
}

/// The member a popup or dialog is about (kept as its record: search results come and go).
#[derive(Clone)]
struct Target {
    uid: Uid,
    at: Point<Pixels>,
}

pub struct MembersTab {
    ctx: TabContext,
    query: SearchBox,
    /// 0 = all roles, -1 = no roles, else a role id.
    role_filter: i64,
    search: Entity<MemberSearch>,
    error: Option<SharedString>,
    pending_action: Option<String>,
    confirm: Option<(ModKind, Member)>,
    menu: Option<Target>,
    role_picker: Option<Target>,
    nick_open: Option<Uid>,
    nick: Entity<InputState>,
    nick_save: EchoSave<(Uid, String)>,
    reason: Entity<InputState>,
    ban_duration: i64,
    purge: i32,
    timeout: i64,
    assigning: HashMap<Uid, PendingRoles>,
    _assign_timer: Option<Task<()>>,
    /// Where each row's ⋯ and + buttons were laid out (popups open under them).
    buttons: Rc<RefCell<HashMap<(Uid, u8), BoundsCell>>>,
    count_line: RefCell<Option<(String, Rc<Shaped>)>>,
    _watch: Watch,
    _subs: Vec<Subscription>,
}

impl SettingsTab for MembersTab {
    fn dirty(&self, _: &App) -> bool {
        false
    }
}

const TIMEOUTS: [(i64, &str); 6] = [(60, "60 s"), (300, "5 min"), (600, "10 min"), (3600, "1 hour"), (86400, "1 day"), (7 * 86400, "1 week")];

impl MembersTab {
    pub fn new(ctx: TabContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let server = ctx.server;
        let mut watch = Watch::new();
        watch.store(&model.settings, cx).store(&model.auth, cx).people(cx, |_, _| true);
        if let Some(community) = model.community(server, cx) {
            watch.store(&community.roles, cx).store(&community.meta, cx);
            // The counts and our own row (the hierarchy).
            watch.store(&community.roster, cx);
        }
        let search = cx.new(|cx| MemberSearch::new(server, "", 0, cx));
        let mut subs = vec![cx.observe(&search, |this: &mut Self, _, cx| {
            this.settle_echoes(cx);
            cx.notify();
        })];
        let query = SearchBox::new("Search members", window, cx, |this: &mut Self, text, cx| {
            this.search.update(cx, |s, cx| s.set_query(text, cx));
        });
        let weak = cx.weak_entity();
        let nick = cx.new(|cx| {
            let mut state = InputState::single_line(window, cx).max_bytes(NICKNAME_MAX_BYTES);
            state.on_escape(move |_, cx| {
                let weak = weak.clone();
                cx.defer(move |cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.nick_open = None;
                        cx.notify();
                    });
                });
                true
            });
            state
        });
        subs.push(cx.subscribe(&nick, |this: &mut Self, _, e: &InputEvent, cx| {
            if *e == InputEvent::Submit {
                this.save_nickname(cx);
            }
        }));
        let reason = cx.new(|cx| {
            InputState::multi_line(window, cx).max_length(200).placeholder("Optional. They see it, and so does the audit log.")
        });
        // Refusals (the router toasts them). Always listening, so a quick reply can't beat it.
        subs.push(cx.subscribe(&model.answers, move |this: &mut Self, _, event: &CoreEvent, cx| match event {
            CoreEvent::RoleActionResponded(p) if ServerId::from_wire(&p.server_id) == server && p.action == "assign" && !p.success => {
                this.assigning.clear();
                this._assign_timer = None;
                cx.notify();
            }
            CoreEvent::ModActionResponded(p) if ServerId::from_wire(&p.server_id) == server && p.action == "nickname" && !p.success => {
                this.nick_save.stop();
                cx.notify();
            }
            _ => {}
        }));
        MembersTab {
            ctx,
            query,
            role_filter: 0,
            search,
            error: None,
            pending_action: None,
            confirm: None,
            menu: None,
            role_picker: None,
            nick_open: None,
            nick,
            nick_save: EchoSave::default(),
            reason,
            ban_duration: 0,
            purge: 0,
            timeout: 600,
            assigning: HashMap::new(),
            _assign_timer: None,
            buttons: Rc::default(),
            count_line: RefCell::new(None),
            _watch: watch,
            _subs: subs,
        }
    }

    #[cfg(feature = "fixtures")]
    /// Focus the search box (the self-tests type into it).
    pub(crate) fn focus_search(&self, window: &mut Window, cx: &mut App) {
        self.query.focus(window, cx);
    }

    pub fn search(&self) -> &Entity<MemberSearch> {
        &self.search
    }

    pub(crate) fn set_role_filter(&mut self, filter: i64, cx: &mut Context<Self>) {
        self.role_filter = filter;
        self.search.update(cx, |s, cx| s.set_role(filter, cx));
        cx.notify();
    }

    fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.query.clear(cx);
        self.set_role_filter(0, cx);
    }

    fn member(&self, uid: Uid, cx: &App) -> Option<Member> {
        self.search.read(cx).members.iter().find(|m| m.uid == uid).cloned()
    }

    /// The roles a row shows: the pending toggle's set, else the record's.
    fn role_ids_of(&self, m: &Member) -> Vec<i64> {
        self.assigning.get(&m.uid).map_or_else(|| m.role_ids.to_vec(), |p| p.role_ids.clone())
    }

    /// A save waiting for its echo ends when the search's record shows it.
    fn settle_echoes(&mut self, cx: &mut Context<Self>) {
        let members = self.search.read(cx).members.clone();
        if let Some(sent) = self.nick_save.settle(|(uid, nickname)| members.iter().find(|m| m.uid == *uid).is_some_and(|m| &*m.nickname == nickname))
            && self.nick_open == Some(sent.0)
        {
            self.nick_open = None;
        }
        let now = Instant::now();
        self.assigning.retain(|uid, p| {
            let echoed = members.iter().find(|m| m.uid == *uid).is_some_and(|m| !Rc::ptr_eq(&m.role_ids, &p.base) && same_set(&m.role_ids, &p.role_ids));
            !echoed && p.until > now
        });
        self.schedule_assign_expiry(cx);
    }

    /// One timer, for the earliest wait to run out (none while nothing waits).
    fn schedule_assign_expiry(&mut self, cx: &mut Context<Self>) {
        let Some(next) = self.assigning.values().map(|p| p.until).min() else {
            self._assign_timer = None;
            return;
        };
        let wait = next.saturating_duration_since(Instant::now());
        self._assign_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                let now = Instant::now();
                this.assigning.retain(|_, p| p.until > now);
                this.schedule_assign_expiry(cx);
                cx.notify();
            });
        }));
    }

    fn run(&mut self, key: String, request: Request, close_confirm: bool, cx: &mut Context<Self>) {
        self.pending_action = Some(key);
        self.error = None;
        cx.notify();
        let weak = cx.weak_entity();
        requests::send(request, cx, move |result, cx| {
            let _ = weak.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.error = Some(e.js_string());
                }
                this.pending_action = None;
                if close_confirm {
                    this.confirm = None;
                }
                cx.notify();
            });
        });
    }

    pub(crate) fn run_mod(&mut self, cx: &mut Context<Self>) {
        let Some((kind, m)) = self.confirm.clone() else { return };
        let name = user_name(m.uid, &m.username, cx).to_string();
        let reason = self.reason.read(cx).value().trim().to_string();
        let server = self.ctx.server;
        let now = now_secs();
        match kind {
            ModKind::Kick => self.run(format!("kick:{}", m.uid), Request::KickMember { server, username: name, uid: m.uid, reason }, true, cx),
            ModKind::Ban => self.run(
                format!("ban:{}", m.uid),
                Request::BanMember {
                    server,
                    username: name,
                    uid: m.uid,
                    reason,
                    expires_at: if self.ban_duration > 0 { now + self.ban_duration } else { 0 },
                    delete_message_seconds: self.purge,
                },
                true,
                cx,
            ),
            ModKind::Timeout => self.run(
                format!("timeout:{}", m.uid),
                Request::TimeoutMember { server, username: name, uid: m.uid, until: now + self.timeout, reason },
                true,
                cx,
            ),
        }
    }

    fn end_timeout(&mut self, m: &Member, cx: &mut Context<Self>) {
        let name = user_name(m.uid, &m.username, cx).to_string();
        self.run(
            format!("timeout:{}", m.uid),
            Request::TimeoutMember { server: self.ctx.server, username: name, uid: m.uid, until: 0, reason: String::new() },
            true,
            cx,
        );
    }

    pub(crate) fn open_confirm(&mut self, kind: ModKind, m: Member, cx: &mut Context<Self>) {
        self.reason.update(cx, |s, cx| s.clear(cx));
        self.ban_duration = 0;
        self.purge = 0;
        self.timeout = 600;
        self.confirm = Some((kind, m));
        self.menu = None;
        cx.notify();
    }

    pub(crate) fn open_nick(&mut self, m: &Member, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = user_name(m.uid, &m.username, cx);
        let nickname = m.nickname.to_string();
        self.nick.update(cx, |s, cx| {
            s.set_placeholder(placeholder, cx);
            s.set_value(&nickname, cx);
            s.focus(window, cx);
        });
        self.nick_open = Some(m.uid);
        self.menu = None;
        cx.notify();
    }

    /// The member whose nickname is being edited, and whether its save waits on the echo (the
    /// self-tests).
    #[cfg(feature = "fixtures")]
    pub(crate) fn nickname_editor(&self) -> (Option<Uid>, bool) {
        (self.nick_open, self.nick_save.saving())
    }

    fn save_nickname(&mut self, cx: &mut Context<Self>) {
        let Some(uid) = self.nick_open else { return };
        if self.nick_save.saving() {
            return;
        }
        let Some(m) = self.member(uid, cx) else { return };
        let nickname = self.nick.read(cx).value().trim().to_string();
        self.error = None;
        self.nick_save.start((uid, nickname.clone()), cx, |this| &mut this.nick_save);
        let name = user_name(uid, &m.username, cx).to_string();
        let weak = cx.weak_entity();
        requests::send(Request::SetNickname { server: self.ctx.server, username: name, uid, nickname }, cx, move |result, cx| {
            if let Err(e) = result {
                let _ = weak.update(cx, |this, cx| {
                    this.error = Some(e.js_string());
                    this.nick_save.stop();
                    cx.notify();
                });
            }
        });
    }

    /// Toggle one role: shows at once and holds until the member's record (patched by the
    /// roster delta) matches; the next toggle builds on it rather than on the stale record. A
    /// refusal or the 5 s wait shows the record again.
    pub fn toggle_role(&mut self, uid: Uid, role: i64, cx: &mut Context<Self>) {
        let Some(m) = self.member(uid, cx) else { return };
        let mut ids = self.role_ids_of(&m);
        if let Some(i) = ids.iter().position(|id| *id == role) {
            ids.remove(i);
        } else {
            ids.push(role);
        }
        self.assigning.insert(uid, PendingRoles { role_ids: ids.clone(), base: m.role_ids.clone(), until: Instant::now() + ECHO_WAIT });
        self.schedule_assign_expiry(cx);
        let name = user_name(uid, &m.username, cx).to_string();
        self.pending_action = Some(format!("assign:{uid}"));
        self.error = None;
        cx.notify();
        let weak = cx.weak_entity();
        requests::send(Request::SetMemberRoles { server: self.ctx.server, username: name, uid, role_ids: ids }, cx, move |result, cx| {
            let _ = weak.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.assigning.remove(&uid);
                    this.error = Some(e.js_string());
                }
                this.pending_action = None;
                cx.notify();
            });
        });
    }

    /// The toolbar's count: matches while filtering, else members and online.
    fn count_runs(&self, cx: &App) -> Vec<Run> {
        let theme = cx.theme();
        let regional = Regional::get();
        let s = self.search.read(cx);
        let filtering = !self.query.text(cx).trim().is_empty() || self.role_filter != 0;
        if filtering {
            if s.loaded && !s.settling() {
                return vec![
                    Run::Colored(regional.group_signed(s.total), theme.color_text_secondary),
                    Run::Text(if s.total == 1 { " match".into() } else { " matches".into() }),
                ];
            }
            return vec![Run::Text("Searching…".into())];
        }
        let model = Model::global(cx);
        let meta = model.community(self.ctx.server, cx).and_then(|c| c.roster.read(cx).meta().cloned());
        let total = meta.as_ref().map_or(s.total, |m| m.total_members);
        let online = meta.as_ref().map_or(0, |m| m.online_count);
        vec![
            Run::Colored(regional.group_signed(total), theme.color_text_secondary),
            Run::Text(" members".into()),
            Run::Colored(" \u{00a0}·\u{00a0} ".into(), theme.color_text_faint),
            Run::Colored("●".into(), theme.color_success),
            Run::Text(format!(" {} online", regional.group_signed(online))),
        ]
    }

    fn count_line(&self, window: &Window, cx: &mut App) -> Rc<Shaped> {
        let runs = self.count_runs(cx);
        let (theme_id, size, muted, secondary) = {
            let theme = cx.theme();
            (theme.id, theme.scaled.text_meta, theme.color_text_muted, theme.color_text_secondary)
        };
        let key = format!("{runs:?}{theme_id:?}{}", window.scale_factor());
        if let Some((k, shaped)) = &*self.count_line.borrow()
            && *k == key
        {
            return shaped.clone();
        }
        let base = Base { font: FontRole::Meta, size, line_height: size * 1.5, color: muted, strong: secondary };
        let shaped = Shaped::new(sentence::build(&runs, base, window.scale_factor(), cx));
        *self.count_line.borrow_mut() = Some((key, shaped.clone()));
        shaped
    }

    fn heads_height(cx: &App) -> f32 {
        cx.theme().scaled.text_section + 8. + 1.
    }
}

/// `grid-cols-[minmax(0,1.15fr)_minmax(0,1.35fr)_7rem_2.25rem] gap-4`.
#[cfg(feature = "fixtures")]
impl MembersTab {
    /// The `server-settings-members-search` screen: `query` typed and searched.
    pub fn fixture_search(&mut self, query: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.query.state.update(cx, |s, cx| s.set_value(query, cx));
        self.search.update(cx, |s, cx| {
            s.set_query(query, cx);
            s.apply_now(cx);
        });
        cx.notify();
    }
}

fn grid_row(cols: [AnyElement; 4]) -> gpui::Div {
    let [who, roles, joined, more] = cols;
    let grow = |mut d: gpui::Div, f: f32| {
        d.style().flex_grow = Some(f);
        d.style().flex_shrink = Some(1.);
        d.flex_basis(px(0.)).min_w_0()
    };
    div()
        .flex()
        .items_center()
        .gap_4()
        .child(grow(div(), 1.15).child(who))
        .child(grow(div(), 1.35).child(roles))
        .child(div().w(px(112.)).flex_none().child(joined))
        .child(div().w(px(36.)).flex_none().child(more))
}

struct RowData {
    member: Member,
    name: SharedString,
    acts: Acts,
    roles: Vec<Role>,
    top_color: Option<u32>,
    timed_out: bool,
    is_self: bool,
    menu_open: bool,
    picker_open: bool,
    assignable: Vec<i64>,
    presence: decibell_model::live::presence::DisplayStatus,
}

impl Render for MembersTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let server = self.ctx.server;
        let gates = Gates::read(server, cx);
        let now = now_secs();
        let pref = model.settings.read(cx).time_format();
        let regional = Regional::get();
        let search = self.search.read(cx);
        let (loaded, has_more, loading, total, search_error) = (search.loaded, search.has_more, search.loading, search.total, search.error.clone());
        let members = search.members.clone();
        let filtering = !self.query.text(cx).trim().is_empty() || self.role_filter != 0;
        let count_line = self.count_line(window, cx);
        let theme = cx.theme();

        // ── toolbar ──
        let mut options = vec![SelectOption::new(0i64, "All roles")];
        for r in gates.roles.iter().filter(|r| !r.is_default) {
            options.push(
                SelectOption::new(r.id, r.name.clone())
                    .hint(plural(r.member_count, "member"))
                    .icon(SelectIcon::Dot(role_color(r.color, cx))),
            );
        }
        options.push(SelectOption::new(-1i64, "No roles"));
        let select = Select::options("members-role-filter", Some(self.role_filter), options)
            .on_change(cx.listener(|this, v: &i64, _, cx| this.set_role_filter(*v, cx)));
        let heads = grid_row([
            SectionLabel::new("Member").into_any_element(),
            SectionLabel::new("Roles").into_any_element(),
            SectionLabel::new("Joined").into_any_element(),
            div().into_any_element(),
        ])
        .border_b_1()
        .border_color(theme.color_border_divider)
        .px_3()
        .pb_2();
        let toolbar = div()
            .mx(px(-8.))
            .bg(theme.color_bg_dark)
            .px_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_4()
                    .child(div().min_w_0().max_w(px(320.)).flex_1().child(self.query.field("members-search")))
                    .child(div().w(px(192.)).flex_none().child(select))
                    .child(div().ml_auto().flex_none().pl_2().child(Sentence::new(&count_line))),
            )
            .child(heads);
        let sticky_h = 56. + Self::heads_height(cx);

        // ── list ──
        let mut body = div().flex().flex_col().pt(px(sticky_h));
        if let Some(error) = self.error.clone().or(search_error.clone()) {
            let dismiss = self.error.is_some().then(|| {
                let weak = cx.weak_entity();
                std::rc::Rc::new(move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ = weak.update(cx, |this, cx| {
                        this.error = None;
                        cx.notify();
                    });
                }) as std::rc::Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>
            });
            body = body.child(div().mt_3().child(error_banner("members-error-dismiss", error, dismiss, cx)));
        }
        if !loaded {
            body = body.child(div().px_3().py_6().child(Label::new("Loading members…").size(13.).color(theme.color_text_muted)));
        } else if members.is_empty() && search_error.is_some() {
        } else if members.is_empty() && !filtering {
            body = body.child(EmptyState::new(IconName::SettingsUsers, "No members yet", "Invite people and they'll show up here."));
        } else if members.is_empty() {
            body = body.child(
                EmptyState::new(IconName::SettingsSearch, "No matching members", "Try another name, or a different role in the filter.").action(
                    Button::new("members-clear", "Clear filters")
                        .variant(ButtonVariant::Soft)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear_filters(cx))),
                ),
            );
        } else {
            let rows: Vec<RowData> = members
                .iter()
                .map(|m| {
                    let ids = self.role_ids_of(m);
                    let mut roles: Vec<Role> = ids.iter().filter_map(|id| gates.roles.iter().find(|r| r.id == *id).cloned()).collect();
                    roles.sort_by_key(|r| std::cmp::Reverse(r.position));
                    let top_color = roles.iter().find(|r| r.color != 0).map(|r| r.color);
                    RowData {
                        name: user_name(m.uid, &m.username, cx),
                        acts: gates.acts(m, now),
                        top_color,
                        roles,
                        timed_out: m.timed_out_until > now,
                        is_self: m.uid == gates.me,
                        menu_open: self.menu.as_ref().is_some_and(|t| t.uid == m.uid) || self.role_picker.as_ref().is_some_and(|t| t.uid == m.uid),
                        picker_open: self.role_picker.as_ref().is_some_and(|t| t.uid == m.uid),
                        assignable: gates.assignable.iter().map(|r| r.id).collect(),
                        presence: presence_of(m.uid, m.is_online, cx),
                        member: m.clone(),
                    }
                })
                .collect();
            let rows = Rc::new(rows);
            let weak = cx.weak_entity();
            let nick_open = self.nick_open;
            let nick = self.nick.clone();
            let nick_saving = self.nick_save.saving();
            let buttons = self.buttons.clone();
            let count = rows.len();
            let list = PaneRows::uniform(&self.ctx.scroll, count, px(ROW), move |range, _, cx| {
                range
                    .map(|i| {
                        member_row(&rows[i], nick_open == Some(rows[i].member.uid), &nick, nick_saving, &buttons, &weak, now, pref, cx)
                    })
                    .collect()
            });
            body = body.child(div().flex().flex_col().pt_1p5().child(list).when(has_more, |this| {
                let more = (total - members.len() as i64).max(0);
                this.child(
                    div().flex().justify_center().pt_3().child(
                        outline_button(
                            "members-more",
                            if loading { "Loading…".to_string() } else { format!("Show {} more", regional.group_signed(more)) },
                            loading,
                            false,
                            cx,
                        )
                        .when(!loading, |b| b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.search.update(cx, |s, cx| s.load_more(cx))))),
                    ),
                )
            }));
        }

        let popups = self.popups(&gates, now, pref, window, cx);
        let _ = regional::local(0);
        div()
            .relative()
            .flex()
            .flex_col()
            .child(body)
            .child(StickyTop::new(&self.ctx.scroll, toolbar))
            .children(popups)
    }
}

#[allow(clippy::too_many_arguments)]
fn member_row(
    r: &RowData,
    editing: bool,
    nick: &Entity<InputState>,
    nick_saving: bool,
    buttons: &Rc<RefCell<HashMap<(Uid, u8), BoundsCell>>>,
    weak: &gpui::WeakEntity<MembersTab>,
    now: i64,
    pref: decibell_model::stores::settings::TimeFormat,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let m = &r.member;
    let uid = m.uid;
    let regional = Regional::get();
    let group: SharedString = format!("member-row-{uid}").into();
    let cell = |which: u8| buttons.borrow_mut().entry((uid, which)).or_default().clone();
    let display: SharedString = if m.nickname.is_empty() { r.name.clone() } else { m.nickname.clone() };

    // Who.
    let avatar = UserAvatar::new(uid, r.name.clone(), 32.).dot(AvatarDot::new(r.presence, theme.color_bg_dark, 13.));
    let who: AnyElement = if editing {
        let w = weak.clone();
        div()
            .flex()
            .min_w_0()
            .flex_1()
            .items_center()
            .gap_1p5()
            .child(div().min_w_0().flex_1().child(InputBox::new(("nick-input", uid as u64), nick).height(32.).pad(10., 0.).small_radius().focused_look()))
            .child(
                div()
                    .id(("nick-save", uid as u64))
                    .flex()
                    .flex_none()
                    .size_8()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_sm)
                    .bg(theme.color_accent)
                    .when(nick_saving, |this| this.opacity(0.5))
                    .when(!nick_saving, |this| {
                        this.cursor_pointer().on_click(move |_, _, cx| {
                            let _ = w.update(cx, |this, cx| this.save_nickname(cx));
                        })
                    })
                    .child(Icon::new(IconName::SettingsCheck).size(px(14.)).stroke_width(2.5).color(theme.color_on_accent)),
            )
            .into_any_element()
    } else {
        let name_color = r.top_color.map_or(theme.color_text_primary, |c| role_color(c, cx));
        div()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_1p5()
                    .child(div().min_w_0().child(Label::new(display.clone()).size(14.).weight(500.).color(name_color).truncate()))
                    .when(m.is_owner, |this| {
                        this.child(
                            div()
                                .id(("owner-crown", uid as u64))
                                .flex()
                                .flex_none()
                                .tooltip(decibell_ui::title_tooltip("Server owner"))
                                .child(Icon::new(IconName::SettingsCrown).size(px(13.)).stroke_width(2.25).color(theme.color_warning)),
                        )
                    })
                    .when(r.is_self, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .rounded(theme.radius_sm)
                                .bg(theme.color_accent_soft)
                                .px_1p5()
                                .py(px(1.))
                                .child(Label::new("You").size(11.).weight(600.).color(theme.color_accent_bright)),
                        )
                    })
                    .when(r.timed_out, |this| {
                        let title = format!("Timed out until {}", regional.date_time(m.timed_out_until, pref));
                        this.child(
                            div()
                                .id(("timed-out", uid as u64))
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap_1()
                                .rounded(theme.radius_sm)
                                .bg(theme.color_warning.opacity(0.12))
                                .px_1p5()
                                .py(px(1.))
                                .tooltip(decibell_ui::title_tooltip(title))
                                .child(Icon::new(IconName::SettingsClock).size(px(11.)).stroke_width(2.5).color(theme.color_warning))
                                .child(Label::new(format_remaining(m.timed_out_until, now)).size(11.).weight(600.).color(theme.color_warning)),
                        )
                    }),
            )
            .when(!m.nickname.is_empty(), |this| {
                this.child(Label::new(r.name.clone()).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted).truncate())
            })
            .into_any_element()
    };
    let who = div().flex().min_w_0().items_center().gap_3().child(avatar).child(who).into_any_element();

    // Roles.
    let mut roles = div().flex().min_w_0().items_center().gap_1p5();
    for role in r.roles.iter().take(MAX_CHIPS) {
        let mut chip = RoleChip::new(("member-role", (uid as u64) << 20 | (role.id as u64 & 0xfffff)), role.name.clone(), role.color);
        if r.acts.roles && r.assignable.contains(&role.id) {
            let (w, id) = (weak.clone(), role.id);
            chip = chip.on_remove(move |_, _, cx| {
                let _ = w.update(cx, |this, cx| this.toggle_role(uid, id, cx));
            });
        }
        roles = roles.child(chip);
    }
    if r.roles.len() > MAX_CHIPS {
        let rest: Vec<String> = r.roles[MAX_CHIPS..].iter().map(|r| r.name.to_string()).collect();
        roles = roles.child(
            div()
                .id(("member-more-roles", uid as u64))
                .flex()
                .h_6()
                .flex_none()
                .items_center()
                .rounded(theme.radius_sm)
                .border_1()
                .border_color(theme.color_border_divider)
                .bg(theme.color_bg_light)
                .px_1p5()
                .tooltip(decibell_ui::title_tooltip(rest.join(", ")))
                .child(Label::new(format!("+{}", r.roles.len() - MAX_CHIPS)).size(12.).weight(500.).color(theme.color_text_muted)),
        );
    }
    if r.acts.roles {
        let has_roles = !r.roles.is_empty();
        let open = r.picker_open;
        let (w, add_cell) = (weak.clone(), cell(1));
        let (muted, bright, accent, border) = (theme.color_text_muted, theme.color_accent_bright, theme.color_accent, theme.color_border);
        let button = div()
            .id(("member-add-role", uid as u64))
            .relative()
            .flex()
            .h_6()
            .flex_none()
            .items_center()
            .justify_center()
            .gap_1()
            .rounded(theme.radius_sm)
            .border_1()
            .cursor_pointer()
            .child(add_cell.probe())
            .map(|this| {
                if has_roles {
                    this.w_6()
                        .border_color(if open { accent } else { border })
                        .when(!open, |this| this.invisible().group_hover(group.clone(), |s| s.visible()))
                        .child(Icon::new(IconName::SettingsPlus).size(px(12.)).stroke_width(2.5).color(if open { bright } else { muted }))
                        .tooltip(decibell_ui::title_tooltip("Add role"))
                } else {
                    this.ml(px(-8.))
                        .px_2()
                        .border_color(if open { accent } else { gpui::transparent_black() })
                        .when(!open, |this| this.group_hover(group.clone(), |s| s.border_color(border)))
                        .child(
                            div()
                                .when(open, |this| this.hidden())
                                .when(!open, |this| this.group_hover(group.clone(), |s| s.hidden()))
                                .child(Label::new("No roles").size(12.).weight(500.).color(muted)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .when(!open, |this| this.hidden().group_hover(group.clone(), |s| s.flex()))
                                .child(Icon::new(IconName::SettingsPlus).size(px(12.)).stroke_width(2.5).color(if open { bright } else { muted }))
                                .child(Label::new("Add role").size(12.).weight(500.).color(if open { bright } else { muted })),
                        )
                }
            })
            .on_click(move |_, _, cx| {
                let at = add_cell.get();
                let _ = w.update(cx, |this, cx| {
                    this.menu = None;
                    this.role_picker = if this.role_picker.as_ref().is_some_and(|t| t.uid == uid) {
                        None
                    } else {
                        Some(Target { uid, at: point(at.left(), at.bottom() + px(4.)) })
                    };
                    cx.notify();
                });
            });
        roles = roles.child(button);
    } else if r.roles.is_empty() {
        roles = roles.child(Label::new("No roles").size(12.).color(theme.color_text_muted));
    }

    // Joined.
    let joined = div()
        .id(("member-joined", uid as u64))
        .tooltip(decibell_ui::title_tooltip(regional.date_time(m.joined_at, pref)))
        .child(
            Label::new(if m.joined_at == 0 {
                "—".to_string()
            } else {
                regional::local(m.joined_at).map_or_else(String::new, |t| regional.medium_date(t.date))
            })
            .font(FontRole::Meta)
            .size(theme.scaled.text_meta)
            .color(theme.color_text_secondary)
            .truncate(),
        )
        .into_any_element();

    // ⋯
    let more: AnyElement = if r.acts.any() {
        let open = r.menu_open && !r.picker_open;
        let (w, more_cell) = (weak.clone(), cell(0));
        let (active, primary, muted) = (theme.color_surface_active, theme.color_text_primary, theme.color_text_muted);
        div()
            .flex()
            .justify_end()
            .child(
                div()
                    .id(("member-more", uid as u64))
                    .group(SharedString::from(format!("member-more-{uid}")))
                    .relative()
                    .flex()
                    .size_8()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_sm)
                    .cursor_pointer()
                    .when(open, |this| this.bg(active))
                    .hover(move |s| s.bg(active))
                    .child(more_cell.probe())
                    .child(
                        Icon::new(IconName::SettingsMore)
                            .size(px(16.))
                            .color(if open { primary } else { muted })
                            .group_hover(SharedString::from(format!("member-more-{uid}")), primary),
                    )
                    .on_click(move |_, _, cx| {
                        let at = more_cell.get();
                        let _ = w.update(cx, |this, cx| {
                            this.role_picker = None;
                            this.menu = if this.menu.as_ref().is_some_and(|t| t.uid == uid) {
                                None
                            } else {
                                Some(Target { uid, at: point(at.left(), at.bottom() + px(4.)) })
                            };
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    } else {
        div().into_any_element()
    };

    let hover = theme.color_surface_hover;
    let any = r.acts.any();
    let w = weak.clone();
    grid_row([who, roles.into_any_element(), joined, more])
        .id(("member-row", uid as u64))
        .group(group)
        .h(px(ROW))
        .rounded(theme.radius_md)
        .px_3()
        .when(r.menu_open, |this| this.bg(hover))
        .hover(move |s| s.bg(hover))
        .when(any, |this| {
            this.on_mouse_down(MouseButton::Right, move |e: &MouseDownEvent, _, cx| {
                let at = e.position;
                let _ = w.update(cx, |this, cx| {
                    this.role_picker = None;
                    this.menu = Some(Target { uid, at });
                    cx.notify();
                });
            })
        })
        .into_any_element()
}

impl MembersTab {
    fn popups(&mut self, gates: &Gates, now: i64, _: decibell_model::stores::settings::TimeFormat, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let theme = cx.theme();
        // The row menu.
        if let Some(target) = self.menu.clone()
            && let Some(m) = self.member(target.uid, cx)
        {
            let acts = gates.acts(&m, now);
            let username = user_name(m.uid, &m.username, cx);
            let name: SharedString = if m.nickname.is_empty() { username.clone() } else { m.nickname.clone() };
            let mut menu = div()
                .w(px(224.))
                .p_1p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .px_2()
                        .pb_2()
                        .pt_1()
                        .child(UserAvatar::new(m.uid, username.clone(), 24.))
                        .child(
                            div()
                                .min_w_0()
                                .child(Label::new(name.clone()).size(13.).weight(600.).color(theme.color_text_primary).truncate())
                                .when(!m.nickname.is_empty(), |this| {
                                    this.child(Label::new(username.clone()).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted).truncate())
                                }),
                        ),
                )
                .child(menu_divider(cx));
            if acts.nick {
                let m2 = m.clone();
                menu = menu.child(
                    menu_item("menu-nick", IconName::SettingsPencil, if m.nickname.is_empty() { "Set nickname" } else { "Change nickname" }, false, cx)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_nick(&m2, window, cx))),
                );
            }
            if acts.roles {
                let at = target.at;
                menu = menu.child(menu_item("menu-roles", IconName::SettingsTag, "Edit roles", false, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.role_picker = Some(Target { uid: target.uid, at });
                        this.menu = None;
                        cx.notify();
                    },
                )));
            }
            if acts.timeout {
                let m2 = m.clone();
                menu = menu.child(
                    menu_item("menu-timeout", IconName::SettingsClock, "Time out…", false, cx)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_confirm(ModKind::Timeout, m2.clone(), cx))),
                );
            }
            if acts.end_timeout {
                let m2 = m.clone();
                menu = menu.child(menu_item("menu-end-timeout", IconName::SettingsClock, "End timeout", false, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.end_timeout(&m2, cx);
                        this.menu = None;
                    },
                )));
            }
            if acts.kick || acts.ban {
                menu = menu.child(menu_divider(cx));
            }
            if acts.kick {
                let m2 = m.clone();
                menu = menu.child(
                    menu_item("menu-kick", IconName::SettingsUserX, format!("Kick {name}…"), true, cx)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_confirm(ModKind::Kick, m2.clone(), cx))),
                );
            }
            if acts.ban {
                let m2 = m.clone();
                menu = menu.child(
                    menu_item("menu-ban", IconName::SettingsBan, format!("Ban {name}…"), true, cx)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_confirm(ModKind::Ban, m2.clone(), cx))),
                );
            }
            out.push(
                Popover::new("member-menu", target.at)
                    .on_dismiss(cx.listener(|this, _: &DismissEvent, _, cx| {
                        this.menu = None;
                        cx.notify();
                    }))
                    .child(menu)
                    .into_any_element(),
            );
        }
        // The role picker.
        if let Some(target) = self.role_picker.clone()
            && let Some(m) = self.member(target.uid, cx)
        {
            let theme = cx.theme();
            let ids = self.role_ids_of(&m);
            let busy = self.pending_action.as_deref() == Some(&format!("assign:{}", m.uid));
            let mut picker = div().w(px(240.)).p_1p5().child(div().px_2().pb_1p5().pt_1().child(SectionLabel::new("Roles")));
            for role in &gates.assignable {
                let has = ids.contains(&role.id);
                let (uid, id) = (m.uid, role.id);
                let hover = theme.color_surface_hover;
                picker = picker.child(
                    div()
                        .id(("picker-role", role.id as u64))
                        .flex()
                        .w_full()
                        .items_center()
                        .gap_2p5()
                        .rounded(theme.radius_md)
                        .px_2()
                        .py_1p5()
                        .hover(move |s| s.bg(hover))
                        .when(!busy, |this| this.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_role(uid, id, cx))))
                        .child(dot(role_color(role.color, cx), 10.))
                        .child(div().min_w_0().flex_1().child(Label::new(role.name.clone()).size(13.).weight(500.).color(theme.color_text_primary).truncate()))
                        .child(check_square(has, cx)),
                );
            }
            if gates.roles.iter().any(|r| !r.is_default && !gates.assignable.iter().any(|a| a.id == r.id)) {
                picker = picker.child(
                    div().px_2().pb_1().pt_2().child(
                        Label::new("Roles at or above your highest role aren\u{2019}t listed.").size(11.).line_height(11. * 1.45).color(theme.color_text_muted),
                    ),
                );
            }
            out.push(
                Popover::new("member-role-picker", target.at)
                    .on_dismiss(cx.listener(|this, _: &DismissEvent, _, cx| {
                        this.role_picker = None;
                        cx.notify();
                    }))
                    .child(picker)
                    .into_any_element(),
            );
        }
        // The kick / ban / timeout dialog.
        let open = self.confirm.is_some();
        let card = self.confirm.clone().map(|(kind, m)| self.moderation_card(kind, &m, cx));
        out.push(
            Modal::new("member-moderation", open)
                .on_close(cx.listener(|this, _: &CloseRequest, _, cx| {
                    this.confirm = None;
                    cx.notify();
                }))
                .children(card)
                .into_any_element(),
        );
        out
    }

    fn moderation_card(&self, kind: ModKind, m: &Member, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let username = user_name(m.uid, &m.username, cx);
        let name: SharedString = if m.nickname.is_empty() { username.clone() } else { m.nickname.clone() };
        let (title, body, cta, icon) = match kind {
            ModKind::Kick => (format!("Kick {name}"), "They're disconnected now and can come back with a valid invite.", "Kick", IconName::SettingsUserX),
            ModKind::Ban => (
                format!("Ban {name}"),
                "They're disconnected and can't rejoin until the ban ends or someone lifts it.",
                "Ban",
                IconName::SettingsBan,
            ),
            ModKind::Timeout => (
                format!("Time out {name}"),
                "They stay in the server but can't send messages, join voice or stream until it ends.",
                "Time out",
                IconName::SettingsClock,
            ),
        };
        let (badge_bg, badge_fg) = if kind == ModKind::Timeout { (theme.color_warning, theme.color_bg_dark) } else { (theme.color_error, theme.color_on_error) };
        let busy = self.pending_action.is_some();
        let mut card = div()
            .occlude()
            .w_full()
            .max_w(px(448.))
            .rounded(theme.radius_xl)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_dark)
            .p_6()
            .shadow(theme.shadow_modal.clone())
            .child(
                div()
                    .mb_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div().relative().flex_none().child(UserAvatar::new(m.uid, username.clone(), 40.)).child(
                            div()
                                .absolute()
                                .bottom(px(-4.))
                                .right(px(-4.))
                                .flex()
                                .size_5()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .border_2()
                                .border_color(theme.color_bg_dark)
                                .bg(badge_bg)
                                .child(Icon::new(icon).size(px(11.)).stroke_width(2.75).color(badge_fg)),
                        ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .child(Label::new(title).font(FontRole::Display).size(16.).weight(600.).color(theme.color_text_primary).truncate())
                            .when(!m.nickname.is_empty(), |this| {
                                this.child(Label::new(username.clone()).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted).truncate())
                            }),
                    ),
            )
            .child(div().mb_4().child(Label::new(body).size(13.).line_height(13. * 1.55).color(theme.color_text_secondary)));
        match kind {
            ModKind::Timeout => {
                let mut grid = div().flex().gap_1p5();
                for (value, label) in TIMEOUTS {
                    let on = self.timeout == value;
                    let (accent, soft, bright, border, secondary, hover) = (
                        theme.color_accent,
                        theme.color_accent_soft,
                        theme.color_accent_bright,
                        theme.color_border,
                        theme.color_text_secondary,
                        theme.color_surface_hover,
                    );
                    grid = grid.child(
                        div()
                            .id(("timeout-option", value as u64))
                            .flex()
                            .flex_1()
                            .justify_center()
                            .rounded(theme.radius_sm)
                            .border_1()
                            .py_2()
                            .cursor_pointer()
                            .map(|this| if on { this.border_color(accent).bg(soft) } else { this.border_color(border).hover(move |s| s.bg(hover)) })
                            .child(Label::new(label).size(12.).weight(600.).color(if on { bright } else { secondary }))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.timeout = value;
                                cx.notify();
                            })),
                    );
                }
                card = card.child(div().mb_4().child(div().mb_2().child(SectionLabel::new("Duration"))).child(grid));
            }
            ModKind::Ban => {
                let duration = Select::options(
                    "ban-duration",
                    Some(self.ban_duration),
                    vec![
                        SelectOption::new(0i64, "Permanent"),
                        SelectOption::new(3600i64, "1 hour"),
                        SelectOption::new(86400i64, "1 day"),
                        SelectOption::new(7 * 86400i64, "7 days"),
                        SelectOption::new(30 * 86400i64, "30 days"),
                    ],
                )
                .on_change(cx.listener(|this, v: &i64, _, cx| {
                    this.ban_duration = *v;
                    cx.notify();
                }));
                let purge = Select::options(
                    "ban-purge",
                    Some(self.purge),
                    vec![
                        SelectOption::new(0i32, "Keep all"),
                        SelectOption::new(3600i32, "Last hour"),
                        SelectOption::new(86400i32, "Last 24 hours"),
                        SelectOption::new(7 * 86400i32, "Last 7 days"),
                    ],
                )
                .on_change(cx.listener(|this, v: &i32, _, cx| {
                    this.purge = *v;
                    cx.notify();
                }));
                card = card.child(
                    div()
                        .mb_4()
                        .flex()
                        .gap_3()
                        .child(div().flex().flex_col().flex_1().gap_2().child(SectionLabel::new("Duration")).child(duration))
                        .child(div().flex().flex_col().flex_1().gap_2().child(SectionLabel::new("Delete their messages")).child(purge)),
                );
            }
            ModKind::Kick => {}
        }
        let theme = cx.theme();
        card.child(
            div()
                .mb_5()
                .flex()
                .flex_col()
                .gap_2()
                .child(SectionLabel::new("Reason"))
                .child(InputBox::new("mod-reason", &self.reason).text(13., 400).leading(1.5).pad(12., 10.).rows(2).color(theme.color_text_primary)),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(Button::new("mod-cancel", "Cancel").variant(ButtonVariant::Ghost).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.confirm = None;
                    cx.notify();
                })))
                .child(
                    Button::new("mod-confirm", if busy { "Working…" } else { cta })
                        .variant(ButtonVariant::Danger)
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.run_mod(cx))),
                ),
        )
        .into_any_element()
    }
}

/// A 16 px check square (the role picker's): accent with a tick when on.
pub fn check_square(on: bool, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    div()
        .flex()
        .flex_none()
        .size_4()
        .items_center()
        .justify_center()
        .rounded(theme.radius_sm)
        .border_1()
        .map(|this| {
            if on {
                this.border_color(theme.color_accent)
                    .bg(theme.color_accent)
                    .child(Icon::new(IconName::SettingsCheck).size(px(11.)).stroke_width(3.).color(theme.color_on_accent))
            } else {
                this.border_color(theme.color_border).bg(theme.color_bg_lighter)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_time_reads_as_the_renderer() {
        assert_eq!(format_remaining(100 + 45, 100), "45s");
        assert_eq!(format_remaining(100 + 5 * 60, 100), "5m");
        assert_eq!(format_remaining(100 + 2 * 3600 + 59 * 60, 100), "2h 59m");
        assert_eq!(format_remaining(100 + 4 * 86400 + 3 * 3600, 100), "4d 3h");
        assert_eq!(format_remaining(50, 100), "0s");
    }
}
