//! Server Settings (`ServerSettingsModal.tsx`): the shared settings frame with the community's
//! tabs, gated by the viewer's permissions as the server gates them: Overview and Members for
//! everyone (editing inside them is gated per control), Roles with MANAGE_ROLES, Audit Log with
//! VIEW_AUDIT_LOG, Bans with BAN_MEMBERS, Storage with MANAGE_SERVER. Non-owners get Leave Server
//! pinned to the sidebar's foot.
//!
//! A tab with unsaved changes holds the window (`leaveGuard.ts`): closing it (Esc, ✕, the
//! backdrop) or changing tab shakes that tab's save bar instead. Only the open tab exists: a
//! tab switch drops the old tab's view and everything it held.

pub mod audit;
pub mod bans;
pub mod color_picker;
pub mod members;
pub mod overview;
pub mod roles;
pub mod storage;

use std::cell::Cell;
use std::rc::Rc;

use decibell_common::Watch;
use decibell_model::stores::navigation::ActiveView;
use decibell_model::stores::permissions::perm;
use decibell_model::{Model, ServerId};
use decibell_ui::{
    ActiveTheme as _, Button, ButtonVariant, CloseRequest, FontRole, IconName, Label, Modal, NavItem, ScrollHandle,
    SettingsWindow,
};
use gpui::{AnyView, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, Render, SharedString, Window, div, prelude::*, px};

use crate::requests::{self, Request};

/// The leave guard a window shares with its open tab: how many leaves were refused since the
/// tab's changes began (each shakes the tab's save bar).
pub type Nudge = Rc<Cell<u32>>;

/// What a tab tells the window.
pub trait SettingsTab {
    /// Unsaved changes that hold the window.
    fn dirty(&self, cx: &App) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabId {
    Overview,
    Members,
    Roles,
    Audit,
    Bans,
    Storage,
}

impl TabId {
    pub fn label(self) -> &'static str {
        match self {
            TabId::Overview => "Overview",
            TabId::Members => "Members",
            TabId::Roles => "Roles",
            TabId::Audit => "Audit Log",
            TabId::Bans => "Bans",
            TabId::Storage => "Storage",
        }
    }

    fn icon(self) -> IconName {
        match self {
            TabId::Overview => IconName::ServerTabOverview,
            TabId::Members => IconName::ServerTabMembers,
            TabId::Roles => IconName::ServerTabRoles,
            TabId::Audit => IconName::ServerTabAudit,
            TabId::Bans => IconName::ServerTabBans,
            TabId::Storage => IconName::ServerTabStorage,
        }
    }

    fn key(self) -> &'static str {
        match self {
            TabId::Overview => "overview",
            TabId::Members => "members",
            TabId::Roles => "roles",
            TabId::Audit => "audit",
            TabId::Bans => "bans",
            TabId::Storage => "storage",
        }
    }
}

/// The open tab's view.
#[derive(Clone)]
pub enum Tab {
    Overview(Entity<overview::OverviewTab>),
    Members(Entity<members::MembersTab>),
    Roles(Entity<roles::RolesTab>),
    Audit(Entity<audit::AuditLogTab>),
    Bans(Entity<bans::BansTab>),
    Storage(Entity<storage::StorageTab>),
}

impl Tab {
    pub fn id(&self) -> TabId {
        match self {
            Tab::Overview(_) => TabId::Overview,
            Tab::Members(_) => TabId::Members,
            Tab::Roles(_) => TabId::Roles,
            Tab::Audit(_) => TabId::Audit,
            Tab::Bans(_) => TabId::Bans,
            Tab::Storage(_) => TabId::Storage,
        }
    }

    pub fn dirty(&self, cx: &App) -> bool {
        match self {
            Tab::Overview(t) => t.read(cx).dirty(cx),
            Tab::Members(t) => t.read(cx).dirty(cx),
            Tab::Roles(t) => t.read(cx).dirty(cx),
            Tab::Audit(t) => t.read(cx).dirty(cx),
            Tab::Bans(t) => t.read(cx).dirty(cx),
            Tab::Storage(t) => t.read(cx).dirty(cx),
        }
    }

    fn view(&self) -> AnyView {
        match self {
            Tab::Overview(t) => t.clone().into(),
            Tab::Members(t) => t.clone().into(),
            Tab::Roles(t) => t.clone().into(),
            Tab::Audit(t) => t.clone().into(),
            Tab::Bans(t) => t.clone().into(),
            Tab::Storage(t) => t.clone().into(),
        }
    }
}

/// What a tab is built with.
#[derive(Clone)]
pub struct TabContext {
    pub server: ServerId,
    /// The pane's scroll: lists window against it, the save bar sticks to its bottom.
    pub scroll: ScrollHandle,
    pub nudge: Nudge,
}

/// The tabs the viewer may open, in the renderer's order.
pub fn visible_tabs(server: ServerId, cx: &App) -> Vec<TabId> {
    let model = Model::global(cx);
    let has = |bit| model.has_permission(server, bit, cx);
    let mut tabs = vec![TabId::Overview, TabId::Members];
    if has(perm::MANAGE_ROLES) {
        tabs.push(TabId::Roles);
    }
    if has(perm::VIEW_AUDIT_LOG) {
        tabs.push(TabId::Audit);
    }
    if has(perm::BAN_MEMBERS) {
        tabs.push(TabId::Bans);
    }
    if has(perm::MANAGE_SERVER) {
        tabs.push(TabId::Storage);
    }
    tabs
}

pub struct ServerSettingsWindow {
    server: ServerId,
    open: bool,
    tab: Tab,
    scroll: ScrollHandle,
    nudge: Nudge,
    leave_confirm: bool,
    _watch: Watch,
}

impl ServerSettingsWindow {
    pub fn new(server: ServerId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        // The tab list (permissions: roles, the roster's own row, the owner), the name.
        watch.store(&model.auth, cx).store(&model.servers, cx);
        if let Some(community) = model.community(server, cx) {
            // Only our own roles move the tabs: a roster change elsewhere draws nothing.
            let me = model.auth.read(cx).uid;
            let mut mine: Option<Vec<i64>> = community.roster.read(cx).member(me).map(|m| m.role_ids.to_vec());
            watch.store(&community.meta, cx).store(&community.roles, cx).store_if(&community.roster, cx, move |_, roster, cx| {
                let me = Model::global(cx).auth.read(cx).uid;
                let now = roster.member(me).map(|m| m.role_ids.to_vec());
                let changed = now != mine;
                mine = now;
                changed
            });
        }
        // Fresh bans and roles whenever the screen opens: tabs feed off these, and the pushed
        // broadcasts keep them live afterwards. Not the roster: its deltas have kept it (and its
        // counts) live since sign-in.
        requests::fire(Request::ListBans { server }, cx);
        requests::fire(Request::ListRoles { server }, cx);
        let scroll = ScrollHandle::new();
        let nudge: Nudge = Rc::default();
        let ctx = TabContext { server, scroll: scroll.clone(), nudge: nudge.clone() };
        let tab = Tab::Overview(cx.new(|cx| overview::OverviewTab::new(ctx, window, cx)));
        ServerSettingsWindow { server, open: true, tab, scroll, nudge, leave_confirm: false, _watch: watch }
    }

    pub fn server(&self) -> ServerId {
        self.server
    }

    pub fn tab(&self) -> &Tab {
        &self.tab
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.open != open {
            self.open = open;
            if !open {
                self.leave_confirm = false;
            }
            cx.notify();
        }
    }

    fn ctx(&self) -> TabContext {
        TabContext { server: self.server, scroll: self.scroll.clone(), nudge: self.nudge.clone() }
    }

    /// Open `id` (no guard: [`Self::select`] is the guarded one).
    pub fn show(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        // Only the tabs the permissions show (a hidden one would still ask the server).
        if self.tab.id() == id || !visible_tabs(self.server, cx).contains(&id) {
            return;
        }
        let ctx = self.ctx();
        self.tab = match id {
            TabId::Overview => Tab::Overview(cx.new(|cx| overview::OverviewTab::new(ctx, window, cx))),
            TabId::Members => Tab::Members(cx.new(|cx| members::MembersTab::new(ctx, window, cx))),
            TabId::Roles => Tab::Roles(cx.new(|cx| roles::RolesTab::new(ctx, window, cx))),
            TabId::Audit => Tab::Audit(cx.new(|cx| audit::AuditLogTab::new(ctx, window, cx))),
            TabId::Bans => Tab::Bans(cx.new(|cx| bans::BansTab::new(ctx, cx))),
            TabId::Storage => Tab::Storage(cx.new(|cx| storage::StorageTab::new(ctx, window, cx))),
        };
        self.nudge.set(0);
        self.scroll.scroll_to_top();
        cx.notify();
    }

    /// Leave the open tab (close, or switch tabs), unless it has unsaved changes: then shake its
    /// save bar. True when it may go.
    pub fn leave_tab(&mut self, cx: &mut Context<Self>) -> bool {
        if self.tab.dirty(cx) {
            self.nudge.set(self.nudge.get() + 1);
            cx.notify();
            false
        } else {
            true
        }
    }

    pub fn select(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if id != self.tab.id() && self.leave_tab(cx) {
            self.show(id, window, cx);
        }
    }

    pub fn request_close(&mut self, cx: &mut Context<Self>) {
        if self.leave_tab(cx) {
            let overlays = Model::global(cx).overlays.clone();
            overlays.update(cx, |o, cx| o.close_modal(cx));
        }
    }

    fn run_leave(&mut self, cx: &mut Context<Self>) {
        let server = self.server;
        // Mirrors the sidebar's Disconnect flow.
        requests::fire(Request::LeaveServer { server }, cx);
        let model = Model::global(cx).clone();
        model.connections.update(cx, |c, cx| {
            c.remove_connected(server, cx);
            c.remove_pending(server, cx);
        });
        model.nav.update(cx, |nav, cx| {
            nav.set_active_server(None, cx);
            nav.set_active_channel(None, cx);
            nav.set_active_view(ActiveView::Home, cx);
        });
        model.overlays.update(cx, |o, cx| o.close_modal(cx));
    }

    #[cfg(feature = "fixtures")]
    /// The tab pane's bounds as last laid out (the self-tests tell the rail from the sidebar by it).
    pub(crate) fn pane_bounds(&self) -> gpui::Bounds<gpui::Pixels> {
        self.scroll.viewport_bounds()
    }

    pub fn nudge(&self) -> u32 {
        self.nudge.get()
    }

    /// The self-tests: the pane scrolled to its end (what lies below the fold, e.g. Transfer
    /// ownership), as a wheel would take it there.
    #[cfg(feature = "fixtures")]
    pub(crate) fn scroll_pane_to_bottom(&self, cx: &mut Context<Self>) {
        self.scroll.scroll_to_bottom();
        cx.notify();
    }
}

impl Render for ServerSettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let server = self.server;
        let tabs = visible_tabs(server, cx);
        // A live permission change can hide the tab we're on: fall back.
        if !tabs.contains(&self.tab.id()) {
            self.show(TabId::Overview, window, cx);
        }
        // A nudge belongs to the changes it guarded; the next ones start calm.
        if self.nudge.get() > 0 && !self.tab.dirty(cx) {
            self.nudge.set(0);
        }
        let name: SharedString = model
            .community(server, cx)
            .map(|c| c.meta.read(cx).name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| model.servers.read(cx).get(server).map(|s| s.name.clone()))
            .unwrap_or_else(|| "Server".into());
        let me = model.auth.read(cx).uid;
        let owner = model.community(server, cx).map_or(0, |c| c.meta.read(cx).owner);
        let is_owner = me != 0 && owner != 0 && owner == me;
        let active = self.tab.id();

        let mut frame = SettingsWindow::new("server-settings", self.open, &self.scroll)
            .nav_label(name.clone())
            .title(active.label())
            .context(name.clone())
            .on_close(cx.listener(|this, _: &CloseRequest, _, cx| this.request_close(cx)));
        for id in tabs {
            frame = frame.item(
                NavItem::new(gpui::ElementId::Name(format!("server-tab-{}", id.key()).into()), id.icon(), id.label())
                    .probe(format!("server-tab:{}", id.key()))
                    .active(id == active)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.select(id, window, cx))),
            );
        }
        if !is_owner {
            frame = frame.foot_item(NavItem::new("server-leave", IconName::SettingsLogout, "Leave Server").probe("server-leave").danger().on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.leave_confirm = true;
                    cx.notify();
                }),
            ));
        }
        let theme = cx.theme();
        let leave_card = div()
            .occlude()
            .w_full()
            .max_w(px(384.))
            .rounded(theme.radius_xl)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_dark)
            .p_6()
            .shadow(theme.shadow_modal.clone())
            .child(div().mb_2().child(Label::new("Leave this server?").font(FontRole::Display).size(16.).weight(600.).color(theme.color_text_primary)))
            .child(
                div()
                    .mb_5()
                    .child(Label::new("You will need a new invite to rejoin.").size(13.).line_height(13. * 1.55).color(theme.color_text_secondary)),
            )
            .child(
                div()
                    .flex()
                    .gap_2p5()
                    .child(
                        div().flex_1().child(
                            Button::new("leave-cancel", "Cancel").variant(ButtonVariant::Secondary).full_width().on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    this.leave_confirm = false;
                                    cx.notify();
                                },
                            )),
                        ),
                    )
                    .child(
                        div().flex_1().child(
                            Button::new("leave-confirm", "Leave Server")
                                .variant(ButtonVariant::Danger)
                                .full_width()
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.run_leave(cx))),
                        ),
                    ),
            );
        div()
            .child(frame.child(self.tab.view()))
            .child(
                Modal::new("server-leave-confirm", self.leave_confirm && self.open)
                    .on_close(cx.listener(|this, _: &CloseRequest, _, cx| {
                        this.leave_confirm = false;
                        cx.notify();
                    }))
                    .child(leave_card),
            )
    }
}
