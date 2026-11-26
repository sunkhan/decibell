//! The user context menu (`features/voice/UserContextMenu.tsx`'s frame), opened by a right click
//! on a person (`decibell_common::open_user_menu`, `model.overlays.context_menu`): a 220 px panel
//! at the pointer, flipped and clamped like `useMenuPosition`, closed by a press outside, Escape
//! or a row's action. It holds the person's header (avatar and the name shown in the community),
//! then the **sections** crates register, in order:
//!
//! | Order | Section | Whose |
//! |---|---|---|
//! | [`order::PROFILE`] 100 | Profile, Message | this crate |
//! | [`order::FRIENDS`] 200 | Add friend / Accept / Decline / Cancel request / Remove friend, Block / Unblock | this crate |
//! | [`order::VOICE`] 300 | local Mute, the user volume (the renderer's sections, in its order) | the voice crate |
//! | [`order::VOICE_MODERATION`] 400 | "Moderation": server mute / deafen, Move to…, Disconnect | the voice crate |
//! | [`order::MODERATION`] 500 | Change nickname, Edit roles, Time out… / End timeout, Kick…, Ban… | this crate |
//! | [`order::COPY`] 600 | Copy username | this crate |
//!
//! ```ignore
//! // the voice crate's init(cx):
//! decibell_home::menu::register_user_menu(cx, UserMenuExtension {
//!     order: decibell_home::menu::order::VOICE,
//!     // the stores the sections read, observed while the menu is open:
//!     watch: Some(Rc::new(|cx| vec![cx.observe(&Model::global(cx).live.voice.session, |_, _, cx| cx.notify())])),
//!     build: Rc::new(|target, window, cx| {
//!         if target.own { return Vec::new(); }
//!         vec![MenuSection::new(Above::Rule, menu_section().child(menu_row("mute", "Mute").on_click(…)).into_any_element())]
//!     }),
//! });
//! ```
//!
//! A section says what separates it from the one above (`Above::Rule`: the full-width
//! `border-t border-border-divider` of the renderer's Moderation block; `Above::Inset`: its `mx-2.5
//! h-px` divider between Mute and the volume); the frame draws it only when a section precedes it
//! (the header has its own rule). Rows close the menu with [`close_user_menu`] (a toggle that keeps
//! it open doesn't). The menu re-renders when it opens and when a watched store notifies.
//!
//! None of this crate's sections exists in the renderer's `UserContextMenu` (it has only the
//! voice entries): their wording and gating come from where the renderer has the actions —
//! `UserProfileModal.tsx`' friend buttons and `MembersTab.tsx`' member menu and dialogs.

mod moderation;

use std::rc::Rc;
use std::time::Duration;

use decibell_common::{UserAvatar, Watch, want_member};
use decibell_model::live::friends::{FriendAction, FriendStatus};
use decibell_model::stores::community::{Member, Role};
use decibell_model::stores::overlays::UserPopup;
use decibell_model::stores::permissions::{Hierarchy, member_level, perm};
use decibell_model::{Model, ServerId, Uid, actions};
use decibell_ui::{ActiveTheme as _, DismissEvent, FontRole, Icon, IconName, Label, Popover};
use gpui::{
    AnyElement, AnyView, App, AppContext as _, ClickEvent, ClipboardItem, Context, Global, IntoElement, Render,
    SharedString, Stateful, Subscription, Task, Window, div, point, prelude::*, px,
};

pub use moderation::{ModKind, ModerationDialog, NicknameDialog};

use crate::profile::{ProfileTab, open_profile};

/// Where a section goes (lower first). Leave gaps: a crate may slot between two.
pub mod order {
    pub const PROFILE: u32 = 100;
    pub const FRIENDS: u32 = 200;
    pub const VOICE: u32 = 300;
    pub const VOICE_MODERATION: u32 = 400;
    pub const MODERATION: u32 = 500;
    pub const COPY: u32 = 600;
}

/// Whom the menu is open for.
#[derive(Debug, Clone, PartialEq)]
pub struct UserMenuTarget {
    pub uid: Uid,
    /// The community it was opened in (a member row, a voice row), none from a DM or Friends.
    pub server: Option<ServerId>,
    /// Their current name (the users store; a placeholder's member row).
    pub name: SharedString,
    /// The name shown in that community (nickname, else the name).
    pub display_name: SharedString,
    /// The menu is about ourselves.
    pub own: bool,
}

/// What goes above a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Above {
    /// `border-t border-border-divider` across the menu.
    Rule,
    /// `mx-2.5 h-px bg-border-divider`.
    Inset,
    /// Nothing.
    None,
}

pub struct MenuSection {
    pub above: Above,
    pub element: AnyElement,
}

impl MenuSection {
    pub fn new(above: Above, element: impl IntoElement) -> Self {
        Self { above, element: element.into_any_element() }
    }
}

type Watcher = Rc<dyn Fn(&mut Context<UserMenu>) -> Vec<Subscription>>;
type Builder = Rc<dyn Fn(&UserMenuTarget, &mut Window, &mut App) -> Vec<MenuSection>>;

/// One crate's sections.
#[derive(Clone)]
pub struct UserMenuExtension {
    pub order: u32,
    /// Subscriptions to the stores the sections read, held while the menu is open (each should
    /// `cx.notify()` the menu).
    pub watch: Option<Watcher>,
    pub build: Builder,
}

#[derive(Default)]
struct Registry(Vec<UserMenuExtension>);

impl Global for Registry {}

/// Add sections to the user menu (once at start-up, from a crate's `init`).
pub fn register_user_menu(cx: &mut App, extension: UserMenuExtension) {
    let registry = cx.default_global::<Registry>();
    registry.0.push(extension);
    registry.0.sort_by_key(|e| e.order);
}

fn extensions(cx: &App) -> Vec<UserMenuExtension> {
    cx.try_global::<Registry>().map(|r| r.0.clone()).unwrap_or_default()
}

/// Close the menu (a row's action).
pub fn close_user_menu(cx: &mut App) {
    let overlays = Model::global(cx).overlays.clone();
    overlays.update(cx, |o, cx| o.close_context_menu(cx));
}

/// A block of rows: `px-[5px] py-1`.
pub fn menu_section() -> gpui::Div {
    div().px(px(5.)).py_1()
}

/// A row: `flex w-full items-center rounded-md px-2.5 py-[7px] text-[13px] text-text-primary
/// hover:bg-surface-hover` (the renderer's moderation rows).
pub fn menu_row(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, cx: &App) -> Stateful<gpui::Div> {
    let t = cx.theme();
    row(id.into(), label.into(), t.color_text_primary, t.color_surface_hover, None, cx)
}

/// A danger row (`text-error hover:bg-error/10`, "Disconnect from voice").
pub fn menu_row_danger(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, cx: &App) -> Stateful<gpui::Div> {
    let t = cx.theme();
    row(id.into(), label.into(), t.color_error, t.color_error.opacity(0.1), None, cx)
}

/// A row with a 15 px icon before its label (settingsUi's `MenuItem`: `text-text-muted`, the
/// danger rows' in `text-error`).
pub fn menu_row_icon(id: impl Into<gpui::ElementId>, icon: IconName, label: impl Into<SharedString>, danger: bool, cx: &App) -> Stateful<gpui::Div> {
    let t = cx.theme();
    let (fg, hover, icon_color) = if danger {
        (t.color_error, t.color_error.opacity(0.1), t.color_error)
    } else {
        (t.color_text_primary, t.color_surface_hover, t.color_text_muted)
    };
    row(id.into(), label.into(), fg, hover, Some((icon, icon_color)), cx)
}

fn row(id: gpui::ElementId, label: SharedString, fg: gpui::Hsla, hover: gpui::Hsla, icon: Option<(IconName, gpui::Hsla)>, cx: &App) -> Stateful<gpui::Div> {
    let probe = decibell_shell::probes::probe(format!("user-menu:{id}"));
    div()
        .id(id)
        .relative()
        .children(probe)
        .flex()
        .w_full()
        .items_center()
        .gap_2()
        .rounded(cx.theme().radius_md)
        .px_2p5()
        .py(px(7.))
        .cursor_pointer()
        .text_color(fg)
        .hover(move |s| s.bg(hover))
        .children(icon.map(|(icon, color)| Icon::new(icon).size(px(15.)).color(color)))
        .child(div().flex_1().min_w_0().child(Label::new(label).size(13.).truncate()))
}

/// A section heading: `px-2.5 pb-1 pt-1.5 text-[10px] font-semibold uppercase tracking-[0.07em]
/// text-text-muted` ("Moderation").
pub fn menu_label(text: &str, cx: &App) -> impl IntoElement + use<> {
    div().px_2p5().pb_1().pt_1p5().child(
        Label::new(SharedString::from(text.to_uppercase())).size(10.).weight(600.).tracking(0.07).color(cx.theme().color_text_muted),
    )
}

/// What the local user may do to a member (`MembersTab`'s `actionsFor`, read by the menu and
/// its dialogs alike; the server checks the same).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Acts {
    pub nick: bool,
    pub roles: bool,
    pub timeout: bool,
    pub end_timeout: bool,
    pub kick: bool,
    pub ban: bool,
}

impl Acts {
    pub fn any(&self) -> bool {
        self.nick || self.roles || self.timeout || self.end_timeout || self.kick || self.ban
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Roles the local user may give or take (strictly below their highest role, the owner all;
/// never the default role): the server's MEMBER_ROLES_UPDATE gate.
pub fn assignable_roles(server: ServerId, cx: &App) -> Vec<Role> {
    let model = Model::global(cx);
    let Some(community) = model.community(server, cx) else { return Vec::new() };
    let own = model.auth.read(cx).uid;
    let owner = community.meta.read(cx).owner;
    let roles = community.roles.read(cx).list().to_vec();
    let roster = community.roster.read(cx);
    let h = Hierarchy::new(own, owner, &roles, Some(roster.members()));
    roles.iter().filter(|r| !r.is_default && (h.is_owner || r.position < h.level)).cloned().collect()
}

/// `actionsFor(member)`.
pub fn acts_for(server: ServerId, member: &Member, cx: &App) -> Acts {
    let model = Model::global(cx);
    let Some(community) = model.community(server, cx) else { return Acts::default() };
    let own = model.auth.read(cx).uid;
    let owner = community.meta.read(cx).owner;
    let roles = community.roles.read(cx).list();
    let roster = community.roster.read(cx);
    let h = Hierarchy::new(own, owner, roles, Some(roster.members()));
    let is_self = member.uid == own;
    // From the record itself (an offline member a search found may not be in the roster).
    let outranked = h.is_owner || member_level(roles, &member.role_ids) < h.level;
    let timed_out = member.timed_out_until > now_secs();
    let moderate = !member.is_owner && !is_self && outranked;
    let can = |bit: u64| model.has_permission(server, bit, cx);
    let assignable = roles.iter().any(|r| !r.is_default && (h.is_owner || r.position < h.level));
    Acts {
        nick: (is_self && can(perm::CHANGE_NICKNAME)) || (!is_self && can(perm::MANAGE_NICKNAMES) && h.can_manage_record(member)),
        roles: can(perm::MANAGE_ROLES) && !member.is_owner && assignable,
        timeout: moderate && can(perm::MODERATE_MEMBERS) && !timed_out,
        end_timeout: moderate && can(perm::MODERATE_MEMBERS) && timed_out,
        kick: moderate && can(perm::KICK_MEMBERS),
        ban: moderate && can(perm::BAN_MEMBERS),
    }
}

/// A dialog asked for from outside the menu (the screens, the self-tests).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DialogRequest {
    Nickname { server: ServerId, uid: Uid },
    Moderation { kind: ModKind, server: ServerId, uid: Uid },
}

#[derive(Default)]
pub struct DialogRequests(Option<DialogRequest>);

struct Requests(gpui::Entity<DialogRequests>);

impl Global for Requests {}

fn requests(cx: &mut App) -> gpui::Entity<DialogRequests> {
    if let Some(r) = cx.try_global::<Requests>() {
        return r.0.clone();
    }
    let entity = cx.new(|_| DialogRequests::default());
    cx.set_global(Requests(entity.clone()));
    entity
}

/// Open a member dialog without the menu (the menu's overlay takes it).
pub fn request_dialog(request: DialogRequest, cx: &mut App) {
    requests(cx).update(cx, |r, cx| {
        r.0 = Some(request);
        cx.notify();
    });
}

/// A role toggle waiting for its echo: the set sent and the record it was built on.
struct PendingRoles {
    uid: Uid,
    role_ids: Vec<i64>,
    base: Rc<[i64]>,
    _limit: Task<()>,
}

pub struct UserMenu {
    open: Option<UserPopup>,
    opened: u64,
    /// "Edit roles" unfolded in place (a dropdown over the menu would be a second layer).
    roles_open: bool,
    pending_roles: Option<PendingRoles>,
    /// A moderation or nickname dialog (it outlives the menu).
    dialog: Option<AnyView>,
    extension_watches: Vec<Subscription>,
    community: Vec<Subscription>,
    _watch: Watch,
    _subscriptions: Vec<Subscription>,
}

impl UserMenu {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .people(cx, |this: &UserMenu, uid| this.open.as_ref().is_some_and(|o| o.uid == uid))
            .store_if(&model.live.friends, cx, |this: &mut UserMenu, _, _| this.open.is_some());
        let requests = requests(cx);
        let subscriptions = vec![
            cx.observe_in(&model.overlays, window, |this, overlays, _, cx| {
                let next = overlays.read(cx).context_menu.clone();
                if next != this.open {
                    this.opened_changed(next, cx);
                }
            }),
            cx.observe_in(&requests, window, |this, requests, window, cx| this.take_request(&requests, window, cx)),
        ];
        let mut this = Self {
            open: None,
            opened: 0,
            roles_open: false,
            pending_roles: None,
            dialog: None,
            extension_watches: Vec::new(),
            community: Vec::new(),
            _watch: watch,
            _subscriptions: subscriptions,
        };
        // Open already (a screen built over a model with the menu open).
        if let Some(open) = model.overlays.read(cx).context_menu.clone() {
            this.opened_changed(Some(open), cx);
        }
        this.take_request(&requests, window, cx);
        let entity = cx.entity();
        crate::hooks::remember(&entity, cx);
        this
    }

    fn take_request(&mut self, requests: &gpui::Entity<DialogRequests>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = requests.update(cx, |r, _| r.0.take()) else { return };
        match request {
            DialogRequest::Nickname { server, uid } => self.show_nickname(server, uid, window, cx),
            DialogRequest::Moderation { kind, server, uid } => self.show_moderation(kind, server, uid, window, cx),
        }
    }

    fn opened_changed(&mut self, next: Option<UserPopup>, cx: &mut Context<Self>) {
        self.open = next;
        self.opened += 1;
        self.roles_open = false;
        self.extension_watches.clear();
        self.community.clear();
        if let Some(open) = self.open.clone() {
            for extension in extensions(cx) {
                if let Some(watch) = extension.watch {
                    self.extension_watches.extend(watch(cx));
                }
            }
            if let Some(server) = open.server
                && let Some(community) = Model::global(cx).community(server, cx)
            {
                // A placeholder has its name only on the member; the hierarchy needs its roles.
                if community.roster.read(cx).member(open.uid).is_none() {
                    want_member(server, open.uid, cx);
                }
                self.community.push(cx.observe(&community.roster, |this, _, cx| {
                    this.settle_roles(cx);
                    cx.notify();
                }));
                self.community.push(cx.observe(&community.roles, |_, _, cx| cx.notify()));
                self.community.push(cx.observe(&community.meta, |_, _, cx| cx.notify()));
            }
        }
        cx.notify();
    }

    /// The menu's target as the sections see it.
    pub fn target(&self, cx: &App) -> Option<UserMenuTarget> {
        let open = self.open.as_ref()?;
        Some(target_of(open, cx))
    }

    fn settle_roles(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = &self.pending_roles else { return };
        let Some(server) = self.open.as_ref().and_then(|o| o.server) else { return };
        let Some(member) = Model::global(cx).community(server, cx).and_then(|c| c.roster.read(cx).member(pending.uid).cloned()) else {
            return;
        };
        let same = member.role_ids.len() == pending.role_ids.len() && pending.role_ids.iter().all(|id| member.role_ids.contains(id));
        // A fresh record (not the one the toggle was built on) that matches.
        if same && !Rc::ptr_eq(&member.role_ids, &pending.base) {
            self.pending_roles = None;
        }
    }

    /// The role set shown for a member: a toggle waiting for its echo, else the record's.
    fn role_ids_of(&self, member: &Member) -> Vec<i64> {
        match &self.pending_roles {
            Some(p) if p.uid == member.uid => p.role_ids.clone(),
            _ => member.role_ids.to_vec(),
        }
    }

    /// `runToggleMemberRole`: the whole set goes to the server; the box shows the toggle at once
    /// and holds it until the member's record matches (or 5 s, or a refusal).
    fn toggle_role(&mut self, server: ServerId, member: &Member, role: i64, cx: &mut Context<Self>) {
        let mut ids = self.role_ids_of(member);
        if let Some(at) = ids.iter().position(|id| *id == role) {
            ids.remove(at);
        } else {
            ids.push(role);
        }
        let uid = member.uid;
        let limit = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(5000)).await;
            let _ = this.update(cx, |this, cx| {
                if this.pending_roles.as_ref().is_some_and(|p| p.uid == uid) {
                    this.pending_roles = None;
                    cx.notify();
                }
            });
        });
        self.pending_roles = Some(PendingRoles { uid, role_ids: ids.clone(), base: member.role_ids.clone(), _limit: limit });
        cx.notify();
        let name = decibell_common::user_name(uid, &member.username, cx).to_string();
        let this = cx.weak_entity();
        let request = decibell_core::api::community::set_member_roles(decibell_core::api::community::SetMemberRolesArgs {
            server_id: server.wire(),
            username: Some(name),
            uid,
            role_ids: ids,
        });
        decibell_model::call::call(cx, request, move |result, cx| {
            if let Err(e) = result {
                decibell_model::call::show_error(&e, "Couldn't update roles", cx);
                let _ = this.update(cx, |this, cx| {
                    this.pending_roles = None;
                    cx.notify();
                });
            }
        });
    }

    fn open_dialog(&mut self, dialog: AnyView, cx: &mut Context<Self>) {
        self.dialog = Some(dialog);
        close_user_menu(cx);
        cx.notify();
    }

    /// A dialog closed itself.
    pub fn close_dialog(&mut self, cx: &mut Context<Self>) {
        if self.dialog.take().is_some() {
            cx.notify();
        }
    }

    /// The open dialog (tests, screens).
    pub fn dialog(&self) -> Option<&AnyView> {
        self.dialog.as_ref()
    }

    /// Open a moderation dialog directly (the screens).
    pub fn show_moderation(&mut self, kind: ModKind, server: ServerId, uid: Uid, window: &mut Window, cx: &mut Context<Self>) {
        let menu = cx.weak_entity();
        let dialog = cx.new(|cx| ModerationDialog::new(kind, server, uid, menu, window, cx));
        self.open_dialog(dialog.into(), cx);
    }

    /// Open the nickname dialog directly (the screens and the self-test).
    pub fn show_nickname(&mut self, server: ServerId, uid: Uid, window: &mut Window, cx: &mut Context<Self>) {
        let menu = cx.weak_entity();
        let dialog = cx.new(|cx| NicknameDialog::new(server, uid, menu, window, cx));
        self.open_dialog(dialog.into(), cx);
    }

    /// This crate's moderation section (it needs the menu's own state: the unfolded roles, a
    /// pending toggle).
    fn moderation_section(&self, target: &UserMenuTarget, cx: &mut Context<Self>) -> Option<MenuSection> {
        let server = target.server?;
        let community = Model::global(cx).community(server, cx)?;
        let member = community.roster.read(cx).member(target.uid).cloned()?;
        let acts = acts_for(server, &member, cx);
        if !acts.any() {
            return None;
        }
        let uid = target.uid;
        let name: SharedString = if member.nickname.is_empty() { target.name.clone() } else { member.nickname.clone() };
        let mut rows: Vec<AnyElement> = Vec::new();
        let icon_row = |id: &'static str, icon: IconName, label: SharedString, danger: bool, cx: &App| menu_row_icon(id, icon, label, danger, cx);
        if acts.nick {
            let label: SharedString = if member.nickname.is_empty() { "Set nickname".into() } else { "Change nickname".into() };
            rows.push(
                icon_row("menu-nickname", IconName::SettingsPencil, label, false, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.show_nickname(server, uid, window, cx)))
                    .into_any_element(),
            );
        }
        if acts.roles {
            let open = self.roles_open;
            let t = cx.theme();
            let (muted, accent) = (t.color_text_muted, t.color_accent);
            rows.push(
                menu_row("menu-roles", "Edit roles", cx)
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size(px(12.))
                            .stroke_width(2.5)
                            .color(if open { accent } else { muted }),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.roles_open = !this.roles_open;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            if open {
                let assignable = assignable_roles(server, cx);
                let all = community.roles.read(cx).list().to_vec();
                let held = self.role_ids_of(&member);
                let t = cx.theme();
                let (border, divider, hover, accent, on_accent, lighter, primary, muted) = (
                    t.color_border,
                    t.color_border_divider,
                    t.color_surface_hover,
                    t.color_accent,
                    t.color_on_accent,
                    t.color_bg_lighter,
                    t.color_text_primary,
                    t.color_text_muted,
                );
                let radius_md = t.radius_md;
                let radius_sm = t.radius_sm;
                let mut list = div().mb_1().ml_2p5().max_h(px(192.)).border_l_1().border_color(divider).pl_1();
                for role in &assignable {
                    let has = held.contains(&role.id);
                    let role_id = role.id;
                    let member = member.clone();
                    let color = decibell_common::role_color(role.color).unwrap_or(muted);
                    list = list.child(
                        div()
                            .id(("menu-role", role.id as u64))
                            .flex()
                            .w_full()
                            .items_center()
                            .gap_2p5()
                            .rounded(radius_md)
                            .px_2()
                            .py_1p5()
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_role(server, &member, role_id, cx)))
                            .child(div().flex_none().size(px(10.)).rounded_full().bg(color))
                            .child(div().flex_1().min_w_0().child(Label::new(role.name.clone()).size(13.).weight(500.).color(primary).truncate()))
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .size_4()
                                    .items_center()
                                    .justify_center()
                                    .rounded(radius_sm)
                                    .border_1()
                                    .border_color(if has { accent } else { border })
                                    .bg(if has { accent } else { lighter })
                                    .when(has, |this| this.child(Icon::new(IconName::SettingsCheck).size(px(11.)).stroke_width(3.).color(on_accent))),
                            ),
                    );
                }
                if all.iter().any(|r| !r.is_default && !assignable.iter().any(|a| a.id == r.id)) {
                    list = list.child(div().px_2().pb_1().pt_2().child(
                        Label::new("Roles at or above your highest role aren’t listed.").size(11.).line_height(11. * 1.45).color(muted),
                    ));
                }
                rows.push(list.into_any_element());
            }
        }
        if acts.timeout {
            rows.push(
                icon_row("menu-timeout", IconName::SettingsClock, "Time out…".into(), false, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.show_moderation(ModKind::Timeout, server, uid, window, cx)))
                    .into_any_element(),
            );
        }
        if acts.end_timeout {
            rows.push(
                icon_row("menu-end-timeout", IconName::SettingsClock, "End timeout".into(), false, cx)
                    .on_click(move |_: &ClickEvent, _, cx| {
                        moderation::end_timeout(server, uid, cx);
                        close_user_menu(cx);
                    })
                    .into_any_element(),
            );
        }
        if acts.kick || acts.ban {
            rows.push(div().mx_2p5().my_1().h(px(1.)).bg(cx.theme().color_border_divider).into_any_element());
        }
        if acts.kick {
            rows.push(
                icon_row("menu-kick", IconName::SettingsUserX, format!("Kick {name}…").into(), true, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.show_moderation(ModKind::Kick, server, uid, window, cx)))
                    .into_any_element(),
            );
        }
        if acts.ban {
            rows.push(
                icon_row("menu-ban", IconName::SettingsBan, format!("Ban {name}…").into(), true, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.show_moderation(ModKind::Ban, server, uid, window, cx)))
                    .into_any_element(),
            );
        }
        Some(MenuSection::new(Above::Rule, menu_section().children(rows)))
    }
}

fn target_of(open: &UserPopup, cx: &App) -> UserMenuTarget {
    let model = Model::global(cx);
    let member_name = open
        .server
        .and_then(|s| model.community(s, cx))
        .and_then(|c| c.roster.read(cx).member(open.uid).map(|m| m.username.clone()));
    let fallback = member_name.unwrap_or_else(|| SharedString::new_static(decibell_common::UNKNOWN_USER));
    let name = decibell_common::user_name(open.uid, &fallback, cx);
    let display_name = decibell_common::display_name(open.server, open.uid, &name, cx);
    UserMenuTarget { uid: open.uid, server: open.server, name, display_name, own: open.uid == model.auth.read(cx).uid }
}

/// This crate's sections that need no menu state: registered in `init` like any other crate's.
pub(crate) fn register_own_sections(cx: &mut App) {
    register_user_menu(
        cx,
        UserMenuExtension {
            order: order::PROFILE,
            watch: None,
            build: Rc::new(|target, _, cx| {
                let (uid, server) = (target.uid, target.server);
                let mut rows: Vec<AnyElement> = vec![
                    menu_row("menu-profile", "Profile", cx)
                        .on_click(move |_: &ClickEvent, _, cx| {
                            close_user_menu(cx);
                            open_profile(uid, server, ProfileTab::Profile, cx);
                        })
                        .into_any_element(),
                ];
                // A pending request either way, or a block, is not a conversation yet.
                let friend = Model::global(cx).live.friends.read(cx).get(uid).map(|f| f.status);
                if uid > 0 && !target.own && friend.is_none_or(|s| s.is_accepted()) {
                    let name = target.name.clone();
                    rows.push(
                        menu_row("menu-message", "Message", cx)
                            .on_click(move |_: &ClickEvent, _, cx| {
                                close_user_menu(cx);
                                actions::dm::open_dm(uid, name.clone(), cx);
                            })
                            .into_any_element(),
                    );
                }
                vec![MenuSection::new(Above::Rule, menu_section().children(rows))]
            }),
        },
    );
    register_user_menu(
        cx,
        UserMenuExtension {
            order: order::FRIENDS,
            watch: None,
            build: Rc::new(|target, _, cx| {
                // A community placeholder has no account to befriend.
                if target.own || target.uid <= 0 {
                    return Vec::new();
                }
                let (uid, name) = (target.uid, target.name.clone());
                let status = Model::global(cx).live.friends.read(cx).get(uid).map(|f| f.status);
                let row = |id: &'static str, label: &'static str, action: FriendAction, danger: bool, cx: &App| {
                    let name = name.clone();
                    let row = if danger { menu_row_danger(id, label, cx) } else { menu_row(id, label, cx) };
                    row.on_click(move |_: &ClickEvent, _, cx| {
                        close_user_menu(cx);
                        actions::friends::act(action, uid, name.clone(), cx);
                    })
                    .into_any_element()
                };
                let mut rows = match status {
                    None => vec![row("menu-add-friend", "Add friend", FriendAction::Add, false, cx)],
                    Some(FriendStatus::PendingIncoming) => vec![
                        row("menu-accept", "Accept", FriendAction::Accept, false, cx),
                        row("menu-decline", "Decline", FriendAction::Reject, false, cx),
                    ],
                    Some(FriendStatus::PendingOutgoing) => vec![row("menu-cancel-request", "Cancel request", FriendAction::Remove, false, cx)],
                    Some(FriendStatus::Online | FriendStatus::Offline) => vec![row("menu-remove-friend", "Remove friend", FriendAction::Remove, false, cx)],
                    Some(FriendStatus::Blocked) => vec![row("menu-unblock", "Unblock", FriendAction::Remove, false, cx)],
                };
                if status != Some(FriendStatus::Blocked) {
                    rows.push(row("menu-block", "Block", FriendAction::Block, true, cx));
                }
                vec![MenuSection::new(Above::Rule, menu_section().children(rows))]
            }),
        },
    );
    register_user_menu(
        cx,
        UserMenuExtension {
            order: order::COPY,
            watch: None,
            build: Rc::new(|target, _, cx| {
                let name = target.name.clone();
                vec![MenuSection::new(
                    Above::Rule,
                    menu_section().child(menu_row("menu-copy-username", "Copy username", cx).on_click(move |_: &ClickEvent, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(name.to_string()));
                        close_user_menu(cx);
                    })),
                )]
            }),
        },
    );
}

impl Render for UserMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog = self.dialog.clone().map(|d| d.into_any_element());
        let Some(open) = self.open.clone() else {
            return div().children(dialog).into_any_element();
        };
        let target = target_of(&open, cx);
        let mut sections: Vec<(u32, MenuSection)> = Vec::new();
        for extension in extensions(cx) {
            let built = (extension.build)(&target, window, cx);
            sections.extend(built.into_iter().map(|s| (extension.order, s)));
        }
        if let Some(moderation) = self.moderation_section(&target, cx) {
            sections.push((order::MODERATION, moderation));
        }
        sections.sort_by_key(|(order, _)| *order);
        crate::hooks::record("user-menu-sections", || sections.iter().map(|(order, _)| order.to_string()).collect());
        let t = cx.theme();
        let (divider, primary) = (t.color_border_divider, t.color_text_primary);
        let mut body: Vec<AnyElement> = Vec::new();
        for (index, (_, section)) in sections.into_iter().enumerate() {
            match section.above {
                _ if index == 0 => body.push(section.element),
                Above::Rule => body.push(div().border_t_1().border_color(divider).child(section.element).into_any_element()),
                Above::Inset => {
                    body.push(div().mx_2p5().h(px(1.)).bg(divider).into_any_element());
                    body.push(section.element);
                }
                Above::None => body.push(section.element),
            }
        }
        let header = div()
            .flex()
            .items_center()
            .gap_2p5()
            .border_b_1()
            .border_color(divider)
            .px_3p5()
            .py_3()
            .child(UserAvatar::new(target.uid, target.name.clone(), 32.))
            .child(div().flex_1().min_w_0().child(Label::new(target.display_name.clone()).font(FontRole::Display).size(14.).weight(500.).color(primary).truncate()));
        let anchor = point(px(open.anchor.x), px(open.anchor.y));
        div()
            .child(
                Popover::new(("user-menu", self.opened), anchor)
                    .on_dismiss(|_: &DismissEvent, _, cx| close_user_menu(cx))
                    .child(div().id("user-menu-body").w(px(220.)).child(header).children(body)),
            )
            .children(dialog)
            .into_any_element()
    }
}
