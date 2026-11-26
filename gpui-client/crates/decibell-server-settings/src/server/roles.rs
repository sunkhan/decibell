//! Roles (`tabs/RolesTab.tsx`): the hierarchy on the left (drag to reorder, higher outranks
//! lower), the selected role's editor on the right (Display, Permissions, Members), with changes
//! held in a draft until the save bar commits them. Gates mirror the server: only roles strictly
//! below your highest are editable; `everyone`'s permissions by any manager. Switching role with
//! unsaved changes shakes the save bar (owner decision): they are never dropped silently.

use std::rc::Rc;

use decibell_common::{AvatarDot, BoundsCell, UserAvatar, Watch, presence_of, user_name};
use decibell_core::events::CoreEvent;
use decibell_model::regional::Regional;
use decibell_model::save_echo::EchoSave;
use decibell_model::stores::community::{Member, Role};
use decibell_model::stores::permissions::{EDITABLE_PERMISSIONS, PERMISSION_GROUPS, has_bits, member_level, perm, toggle_bit};
use decibell_model::{Model, ServerId, Uid};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{
    ActiveTheme as _, Button, ButtonVariant, ConfirmModal, ConfirmTone, DismissEvent, EmptyState, FontRole, Icon, IconName,
    Label, Popover, SaveBar, SectionLabel, TextField,
};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, DragMoveEvent, Entity, Hsla, IntoElement, MouseButton, Pixels, Point,
    Render, SharedString, Subscription, Window, div, point, prelude::*, px,
};

use super::color_picker::{ColorPicker, ColorPickerEvent, PICKER_W, custom_tile, on_swatch};
use super::members::check_square;
use super::{SettingsTab, TabContext};
use crate::member_search::MemberSearch;
use crate::requests::{self, Request};
use crate::rows::PaneRows;
use crate::ui::{RoleChip, SearchBox, dot, error_banner, notice, outline_button, perm_row, plural, role_color};

pub const ROLE_NAME_MAX_BYTES: usize = 64;
pub const ROLE_COLOR_PRESETS: [u32; 10] = [0, 0xe74c3c, 0xe67e22, 0xf1c40f, 0x2ecc71, 0x1abc9c, 0x3498db, 0x9b59b6, 0xe91e63, 0x95a5a6];
/// The server's cap on roles (review B: 250 a community).
pub const MAX_ROLES: usize = 250;
/// A hierarchy row (`h-9`) and the list's `gap-0.5`.
const ROLE_ROW: f32 = 36. + 2.;
/// A holder row (`h-12`).
const HOLDER_ROW: f32 = 48.;

/// The editable fields of a role.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoleFields {
    pub name: String,
    pub color: u32,
    pub permissions: u64,
    pub manage_each_other: bool,
}

impl RoleFields {
    fn of(r: &Role) -> RoleFields {
        RoleFields { name: r.name.to_string(), color: r.color, permissions: r.permissions, manage_each_other: r.manage_each_other }
    }

    fn trimmed(&self) -> RoleFields {
        RoleFields { name: self.name.trim().to_string(), ..self.clone() }
    }
}

/// Which role the editor shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selected {
    Role(i64),
    /// A role being created (a draft with no id).
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Display,
    Permissions,
    Members,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Display => "Display",
            Section::Permissions => "Permissions",
            Section::Members => "Members",
        }
    }
}

/// What a save sent: the role (none while creating one) and the fields as the server will store
/// them.
#[derive(Debug, Clone, PartialEq)]
pub struct SentRole {
    pub role_id: Option<i64>,
    pub fields: RoleFields,
}

fn perm_count(mask: u64) -> i64 {
    EDITABLE_PERMISSIONS.iter().filter(|p| has_bits(mask, p.bit)).count() as i64
}

/// The value dragged in the hierarchy.
#[derive(Clone)]
pub struct RoleDrag {
    id: i64,
    name: SharedString,
    color: u32,
}

/// What a dragged row looks like under the pointer.
struct DragGhost {
    name: SharedString,
    color: u32,
}

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .h_9()
            .w(px(208.))
            .items_center()
            .gap_2()
            .rounded(theme.radius_md)
            .bg(theme.color_surface_active)
            .opacity(0.8)
            .pl_5()
            .pr_2p5()
            .child(dot(role_color(self.color, cx), 10.))
            .child(Label::new(self.name.clone()).size(13.).weight(500.).color(theme.color_text_primary).truncate())
    }
}

struct AddPicker {
    search: Entity<MemberSearch>,
    query: SearchBox,
    /// Records, not names: a pick stays picked while the search moves on.
    picked: Vec<Member>,
    anchor: Point<Pixels>,
    _observe: Subscription,
}

pub struct RolesTab {
    ctx: TabContext,
    selected: Option<Selected>,
    edits: Option<RoleFields>,
    name: Entity<InputState>,
    syncing: bool,
    section: Section,
    /// Refused role switches (each shakes the bar).
    nudge: u32,
    error: Option<SharedString>,
    created_id: Option<i64>,
    confirm_delete: Option<Role>,
    drag_drop: Option<(i64, Option<usize>)>,
    save: EchoSave<SentRole>,
    perm_query: SearchBox,
    holders: Option<(i64, Entity<MemberSearch>, Subscription)>,
    holders_query: SearchBox,
    busy: Option<Uid>,
    add_picker: Option<AddPicker>,
    add_button: BoundsCell,
    color_picker: Option<(Entity<ColorPicker>, Point<Pixels>, Subscription)>,
    custom_tile: BoundsCell,
    _watch: Watch,
    _subs: Vec<Subscription>,
}

impl SettingsTab for RolesTab {
    fn dirty(&self, cx: &App) -> bool {
        self.is_dirty(cx)
    }
}

impl RolesTab {
    pub fn new(ctx: TabContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let server = ctx.server;
        let mut watch = Watch::new();
        watch.store(&model.auth, cx).store(&model.settings, cx).people(cx, |_, _| true);
        let mut subs = Vec::new();
        if let Some(community) = model.community(server, cx) {
            watch.store(&community.meta, cx).store(&community.roster, cx);
            subs.push(cx.observe(&community.roles, |this: &mut Self, _, cx| {
                this.on_roles(cx);
                cx.notify();
            }));
        }
        let name = cx.new(|cx| InputState::single_line(window, cx).placeholder("e.g. Moderator").max_bytes(ROLE_NAME_MAX_BYTES));
        subs.push(cx.subscribe(&name, |this: &mut Self, name, e: &InputEvent, cx| {
            if *e == InputEvent::Change && !this.syncing {
                let value = name.read(cx).value().to_string();
                this.change(cx, |f| f.name = value);
            }
        }));
        // The reply to a save: a refusal ends the wait (the router toasts it), a create names the
        // new role. Always listening, so a quick reply can't beat the listener.
        subs.push(cx.subscribe(&model.answers, move |this: &mut Self, _, event: &CoreEvent, cx| {
            if let CoreEvent::RoleActionResponded(p) = event
                && ServerId::from_wire(&p.server_id) == server
                && (p.action == "update" || p.action == "create")
            {
                if !p.success {
                    this.save.stop();
                } else if p.action == "create"
                    && let Some(role) = &p.role
                {
                    this.created_id = Some(role.id);
                    this.settle(cx);
                }
                cx.notify();
            }
        }));
        let perm_query = SearchBox::new("Search permissions", window, cx, |_: &mut Self, _, cx| cx.notify());
        let holders_query = SearchBox::new("Search members with this role", window, cx, |this: &mut Self, text, cx| {
            if let Some((_, search, _)) = &this.holders {
                search.update(cx, |s, cx| s.set_query(text, cx));
            }
        });
        let mut this = RolesTab {
            ctx,
            selected: None,
            edits: None,
            name,
            syncing: false,
            section: Section::Display,
            nudge: 0,
            error: None,
            created_id: None,
            confirm_delete: None,
            drag_drop: None,
            save: EchoSave::default(),
            perm_query,
            holders: None,
            holders_query,
            busy: None,
            add_picker: None,
            add_button: BoundsCell::new(),
            color_picker: None,
            custom_tile: BoundsCell::new(),
            _watch: watch,
            _subs: subs,
        };
        this.on_roles(cx);
        this
    }

    fn roles(&self, cx: &App) -> Vec<Role> {
        Model::global(cx).community(self.ctx.server, cx).map(|c| c.roles.read(cx).list().to_vec()).unwrap_or_default()
    }

    /// Most senior first, `everyone` apart.
    fn ranked(roles: &[Role]) -> Vec<Role> {
        let mut ranked: Vec<Role> = roles.iter().filter(|r| !r.is_default).cloned().collect();
        ranked.sort_by_key(|r| std::cmp::Reverse(r.position));
        ranked
    }

    fn can_manage_roles(&self, cx: &App) -> bool {
        Model::global(cx).has_permission(self.ctx.server, perm::MANAGE_ROLES, cx)
    }

    /// (is owner, our level).
    fn hierarchy(&self, roles: &[Role], cx: &App) -> (bool, i32) {
        let model = Model::global(cx);
        let me = model.auth.read(cx).uid;
        let is_owner = model.is_owner(self.ctx.server, cx);
        let mine: Vec<i64> = model
            .community(self.ctx.server, cx)
            .and_then(|c| c.roster.read(cx).member(me).map(|m| m.role_ids.to_vec()))
            .unwrap_or_default();
        (is_owner, member_level(roles, &mine))
    }

    fn can_edit_role(&self, r: &Role, roles: &[Role], cx: &App) -> bool {
        let (is_owner, level) = self.hierarchy(roles, cx);
        self.can_manage_roles(cx) && (r.is_default || is_owner || r.position < level)
    }

    pub(crate) fn role(&self, cx: &App) -> Option<Role> {
        match self.selected {
            Some(Selected::Role(id)) => self.roles(cx).into_iter().find(|r| r.id == id),
            _ => None,
        }
    }

    fn is_new(&self) -> bool {
        self.selected == Some(Selected::New)
    }

    fn stored(&self, cx: &App) -> RoleFields {
        self.role(cx).map_or_else(RoleFields::default, |r| RoleFields::of(&r))
    }

    pub fn fields(&self, cx: &App) -> RoleFields {
        self.edits.clone().unwrap_or_else(|| self.stored(cx))
    }

    pub fn is_dirty(&self, cx: &App) -> bool {
        self.is_new() || self.edits.as_ref().is_some_and(|e| *e != self.stored(cx))
    }

    pub fn selected(&self) -> Option<Selected> {
        self.selected
    }

    pub fn nudges(&self) -> u32 {
        self.nudge
    }

    /// The role list changed: pick a role on first visit (the most senior one you can edit,
    /// else the top one), settle a save, keep an unedited name field in step.
    fn on_roles(&mut self, cx: &mut Context<Self>) {
        let roles = self.roles(cx);
        let valid = match self.selected {
            Some(Selected::New) => true,
            Some(Selected::Role(id)) => roles.iter().any(|r| r.id == id),
            None => false,
        };
        if !valid {
            let ranked = Self::ranked(&roles);
            let first = ranked
                .iter()
                .find(|r| self.can_edit_role(r, &roles, cx))
                .or(ranked.first())
                .or(roles.iter().find(|r| r.is_default))
                .map(|r| r.id);
            if let Some(id) = first {
                self.selected = Some(Selected::Role(id));
                self.edits = None;
            }
        }
        self.settle(cx);
        if self.edits.is_none() {
            let name = self.stored(cx).name;
            self.set_name_field(&name, cx);
        }
    }

    /// A save settles on the role list the server pushes after it: the draft stays until the list
    /// shows what was sent. A new role's id comes back in the create reply, just before the list.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let roles = self.roles(cx);
        let created = self.created_id;
        let echoed = self.save.settle(|sent| match sent.role_id {
            None => created.is_some_and(|id| roles.iter().any(|r| r.id == id)),
            Some(id) => roles.iter().find(|r| r.id == id).is_some_and(|r| RoleFields::of(r) == sent.fields),
        });
        if let Some(sent) = echoed {
            // Edits made while the save was out stay as a fresh draft.
            if self.edits.as_ref().is_none_or(|e| e.trimmed() == sent.fields) {
                self.edits = None;
            }
            if sent.role_id.is_none()
                && let Some(id) = self.created_id.take()
            {
                self.selected = Some(Selected::Role(id));
                self.edits = None;
            }
            let name = self.fields(cx).name;
            self.set_name_field(&name, cx);
        }
    }

    pub(crate) fn set_name_field(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.name.read(cx).value() != name {
            self.syncing = true;
            self.name.update(cx, |s, cx| s.set_value(name, cx));
            self.syncing = false;
        }
    }

    pub(crate) fn change(&mut self, cx: &mut Context<Self>, patch: impl FnOnce(&mut RoleFields)) {
        let mut fields = self.fields(cx);
        patch(&mut fields);
        self.edits = Some(fields);
        cx.notify();
    }

    /// Switching away from unsaved changes nudges the save bar instead.
    pub fn select(&mut self, next: Selected, cx: &mut Context<Self>) {
        if Some(next) == self.selected {
            return;
        }
        if self.is_dirty(cx) {
            self.nudge += 1;
            cx.notify();
            return;
        }
        self.edits = None;
        self.error = None;
        self.selected = Some(next);
        self.holders = None;
        self.add_picker = None;
        let name = self.stored(cx).name;
        self.set_name_field(&name, cx);
        cx.notify();
    }

    pub fn start_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_dirty(cx) {
            self.nudge += 1;
            cx.notify();
            return;
        }
        self.selected = Some(Selected::New);
        self.edits = Some(RoleFields::default());
        self.section = Section::Display;
        self.error = None;
        self.holders = None;
        self.set_name_field("", cx);
        self.name.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        if self.is_new() {
            self.edits = None;
            let roles = self.roles(cx);
            let ranked = Self::ranked(&roles);
            self.selected = ranked.first().or(roles.iter().find(|r| r.is_default)).map(|r| Selected::Role(r.id));
        } else {
            self.edits = None;
        }
        self.nudge = 0;
        let name = self.stored(cx).name;
        self.set_name_field(&name, cx);
        cx.notify();
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        let role = self.role(cx);
        if self.save.saving() || (!self.is_new() && role.is_none()) {
            return;
        }
        let sent = self.fields(cx).trimmed();
        self.error = None;
        self.created_id = None;
        let server = self.ctx.server;
        let request = match &role {
            None => Request::CreateRole {
                server,
                name: sent.name.clone(),
                color: sent.color,
                permissions: sent.permissions,
                manage_each_other: sent.manage_each_other,
            },
            Some(r) => Request::UpdateRole {
                server,
                role_id: r.id,
                name: sent.name.clone(),
                color: sent.color,
                permissions: sent.permissions,
                position: r.position,
                manage_each_other: sent.manage_each_other,
            },
        };
        self.save.start(SentRole { role_id: role.map(|r| r.id), fields: sent }, cx, |this| &mut this.save);
        let weak = cx.weak_entity();
        requests::send(request, cx, move |result, cx| {
            if let Err(e) = result {
                let _ = weak.update(cx, |this, cx| {
                    this.error = Some(e.js_string());
                    this.save.stop();
                    cx.notify();
                });
            }
        });
    }

    pub(crate) fn run_delete(&mut self, r: Role, cx: &mut Context<Self>) {
        self.error = None;
        let weak = cx.weak_entity();
        requests::send(Request::DeleteRole { server: self.ctx.server, role_id: r.id }, cx, move |result, cx| {
            let _ = weak.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.edits = None;
                        this.selected = None;
                    }
                    Err(e) => this.error = Some(e.js_string()),
                }
                this.confirm_delete = None;
                cx.notify();
            });
        });
    }

    /// Reorder to an absolute position; the server clamps and keeps positions dense. Non-owners
    /// can't lift a role to or above their own.
    pub fn move_to(&mut self, r: &Role, position: i32, cx: &mut Context<Self>) {
        let roles = self.roles(cx);
        let ranked = Self::ranked(&roles);
        let (is_owner, level) = self.hierarchy(&roles, cx);
        let max = if is_owner { ranked.len() as i32 } else { level - 1 };
        let pos = position.min(max).max(1);
        if pos == r.position {
            return;
        }
        self.error = None;
        let weak = cx.weak_entity();
        requests::send(
            Request::UpdateRole {
                server: self.ctx.server,
                role_id: r.id,
                name: r.name.to_string(),
                color: r.color,
                permissions: r.permissions,
                position: pos,
                manage_each_other: r.manage_each_other,
            },
            cx,
            move |result, cx| {
                if let Err(e) = result {
                    let _ = weak.update(cx, |this, cx| {
                        this.error = Some(e.js_string());
                        cx.notify();
                    });
                }
            },
        );
    }

    fn drop_role(&mut self, cx: &mut Context<Self>) {
        let Some((id, Some(drop))) = self.drag_drop.take() else {
            self.drag_drop = None;
            cx.notify();
            return;
        };
        let roles = self.roles(cx);
        let ranked = Self::ranked(&roles);
        cx.notify();
        let Some(from) = ranked.iter().position(|r| r.id == id) else { return };
        // `drop` is an insertion index into the list as drawn (dragged row included); index 0 =
        // top = position N.
        let to = if drop > from { drop - 1 } else { drop };
        let r = ranked[from].clone();
        self.move_to(&r, (ranked.len() - to) as i32, cx);
    }

    fn set_role_holding(&mut self, m: &Member, role: i64, on: bool, cx: &mut Context<Self>) {
        let mut ids: Vec<i64> = m.role_ids.to_vec();
        if on && !ids.contains(&role) {
            ids.push(role);
        }
        if !on {
            ids.retain(|id| *id != role);
        }
        self.busy = Some(m.uid);
        cx.notify();
        let name = user_name(m.uid, &m.username, cx).to_string();
        let weak = cx.weak_entity();
        requests::send(Request::SetMemberRoles { server: self.ctx.server, username: name, uid: m.uid, role_ids: ids }, cx, move |result, cx| {
            if let Err(e) = &result {
                log::error!("set_member_roles: {e}");
            }
            let _ = weak.update(cx, |this, cx| {
                this.busy = None;
                cx.notify();
            });
        });
    }

    fn open_color_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.color_picker.is_some() {
            self.color_picker = None;
            cx.notify();
            return;
        }
        let color = self.fields(cx).color;
        let picker = cx.new(|cx| ColorPicker::new(color, window, cx));
        let sub = cx.subscribe(&picker, |this: &mut Self, _, e: &ColorPickerEvent, cx| match e {
            ColorPickerEvent::Change(c) => {
                let c = *c;
                this.change(cx, |f| f.color = c);
            }
            ColorPickerEvent::Done => {
                this.color_picker = None;
                cx.notify();
            }
        });
        let tile = self.custom_tile.get();
        self.color_picker = Some((picker, point(tile.right() - px(PICKER_W), tile.bottom() + px(8.)), sub));
        cx.notify();
    }

    fn open_add_picker(&mut self, role: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_picker.is_some() {
            self.add_picker = None;
            cx.notify();
            return;
        }
        let search = cx.new(|cx| MemberSearch::new(self.ctx.server, "", 0, cx));
        let query = SearchBox::new("Find members", window, cx, |this: &mut Self, text, cx| {
            if let Some(p) = &this.add_picker {
                p.search.update(cx, |s, cx| s.set_query(text, cx));
            }
        });
        query.focus(window, cx);
        let observe = cx.observe(&search, |_, _, cx| cx.notify());
        let b = self.add_button.get();
        // Right-aligned under the button: shift the anchor left by our width.
        self.add_picker = Some(AddPicker { search, query, picked: Vec::new(), anchor: point(b.right() - px(288.), b.bottom() + px(4.)), _observe: observe });
        let _ = role;
        cx.notify();
    }
}

#[cfg(feature = "fixtures")]
impl RolesTab {
    /// The roles screens: `role` selected on `section`; `unsaved` renames and recolours it.
    pub fn fixture_select(&mut self, role: i64, section: Section, unsaved: bool, cx: &mut Context<Self>) {
        self.selected = Some(Selected::Role(role));
        self.section = section;
        self.edits = None;
        let name = self.stored(cx).name;
        self.set_name_field(&name, cx);
        if unsaved {
            self.set_name_field("Streamers", cx);
            let mut fields = self.stored(cx);
            fields.name = "Streamers".into();
            fields.color = 0xe91e63;
            self.edits = Some(fields);
        }
        cx.notify();
    }

    /// The `color-picker` screen: the custom colour picker open.
    pub fn fixture_color_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_color_picker(window, cx);
    }
}

impl Render for RolesTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let roles = self.roles(cx);
        let theme = cx.theme();
        if roles.is_empty() {
            return div().child(EmptyState::new(IconName::SettingsShield, "No roles here", "This server doesn't support roles yet."));
        }
        let ranked = Self::ranked(&roles);
        let everyone = roles.iter().find(|r| r.is_default).cloned();
        let can_manage = self.can_manage_roles(cx);
        let role = self.role(cx);
        let is_new = self.is_new();
        let fields = self.fields(cx);
        let dirty = self.is_dirty(cx);
        // A nudge belongs to the changes it guarded; the next ones start calm.
        if !dirty {
            self.nudge = 0;
        }
        let editable = is_new || role.as_ref().is_some_and(|r| self.can_edit_role(r, &roles, cx));
        let sections: Vec<Section> = match (&role, is_new) {
            (Some(r), _) if r.is_default => vec![Section::Permissions],
            (_, true) => vec![Section::Display, Section::Permissions],
            _ => vec![Section::Display, Section::Permissions, Section::Members],
        };
        let section = if sections.contains(&self.section) { self.section } else { sections[0] };
        let total_members = model.community(self.ctx.server, cx).and_then(|c| c.roster.read(cx).meta().map(|m| m.total_members)).unwrap_or(0);
        // Counts come from the server (role-list pushes), so they include offline members the
        // sidebar never loaded.
        let count_of = |r: &Role| if r.is_default { total_members } else { r.member_count };
        let roles_for_edit = roles.clone();
        let can_edit = |r: &Role| self.can_edit_role(r, &roles_for_edit, cx);

        // ── hierarchy ──
        let selected = self.selected;
        let edits = self.edits.clone();
        let drag = self.drag_drop;
        let weak = cx.weak_entity();
        let rows: Rc<Vec<(Role, bool)>> = Rc::new(ranked.iter().map(|r| (r.clone(), can_edit(r))).collect());
        let n = rows.len();
        let list = {
            let rows = rows.clone();
            let edits = edits.clone();
            PaneRows::uniform(&self.ctx.scroll, n, px(ROLE_ROW), move |range, _, cx| {
                range
                    .map(|i| {
                        let (r, editable) = &rows[i];
                        let is_selected = selected == Some(Selected::Role(r.id));
                        let (name, color) = match (&edits, is_selected) {
                            (Some(e), true) => (if e.name.is_empty() { r.name.clone() } else { e.name.clone().into() }, e.color),
                            _ => (r.name.clone(), r.color),
                        };
                        let drop_line = match drag {
                            Some((id, Some(d))) if id != r.id => {
                                if d == i {
                                    Some(true)
                                } else if d == i + 1 && i == n - 1 {
                                    Some(false)
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        };
                        role_row(
                            r,
                            name,
                            color,
                            Regional::get().group_signed(if r.is_default { 0 } else { r.member_count }),
                            is_selected,
                            !editable,
                            *editable,
                            drag.is_some_and(|(id, _)| id == r.id),
                            drop_line,
                            i,
                            &weak,
                            cx,
                        )
                    })
                    .collect()
            })
        };
        let aside = div()
            .w(px(224.))
            .flex_none()
            .child(
                div()
                    .mb_2()
                    .flex()
                    .h_7()
                    .items_center()
                    .justify_between()
                    .pl_2()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_1()
                            .child(SectionLabel::new("Roles"))
                            .child(SectionLabel::new(Regional::get().group(ranked.len() as u64))),
                    )
                    .when(can_manage, |this| {
                        let (muted, primary, hover) = (theme.color_text_muted, theme.color_text_primary, theme.color_surface_hover);
                        this.child(
                            div()
                                .id("role-create")
                                .group("role-create")
                                .flex()
                                .size_7()
                                .items_center()
                                .justify_center()
                                .rounded(theme.radius_sm)
                                .cursor_pointer()
                                .hover(move |s| s.bg(hover))
                                .tooltip(decibell_ui::title_tooltip("Create role"))
                                .when(ranked.len() >= MAX_ROLES, |this| this.opacity(0.4))
                                .child(Icon::new(IconName::SettingsPlus).size(px(15.)).stroke_width(2.25).color(muted).group_hover("role-create", primary))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start_create(window, cx))),
                        )
                    }),
            )
            .child(
                div()
                    .id("role-list")
                    .flex()
                    .flex_col()
                    .when(is_new, |this| {
                        let draft_name: SharedString = if fields.name.trim().is_empty() { "New role".into() } else { fields.name.trim().to_string().into() };
                        this.child(role_row_plain(draft_name, fields.color, "Draft".into(), true, fields.name.trim().is_empty(), cx).mb(px(2.)))
                    })
                    .child(list)
                    .on_drop(cx.listener(|this, _: &RoleDrag, _, cx| this.drop_role(cx))),
            )
            .when_some(everyone.clone(), |this, everyone| {
                let id = everyone.id;
                this.child(div().mx_2().my_2().h(px(1.)).bg(theme.color_border_divider)).child(
                    role_row_plain("@everyone".into(), 0, "All".into(), selected == Some(Selected::Role(id)), false, cx)
                        .id("role-everyone")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.select(Selected::Role(id), cx))),
                )
            })
            .child(
                div().mt_4().px_2().child(
                    Label::new(if can_manage {
                        "Higher roles outrank lower ones, and members wear the color of their highest role. Drag to reorder."
                    } else {
                        "Higher roles outrank lower ones, and members wear the color of their highest role."
                    })
                    .size(12.)
                    .line_height(12. * 1.55)
                    .color(theme.color_text_muted),
                ),
            );

        // ── editor ──
        let role_name: SharedString = if is_new {
            if fields.name.trim().is_empty() { "New role".into() } else { fields.name.trim().to_string().into() }
        } else if role.as_ref().is_some_and(|r| r.is_default) {
            "@everyone".into()
        } else if !fields.name.is_empty() {
            fields.name.clone().into()
        } else {
            role.as_ref().map(|r| r.name.clone()).unwrap_or_default()
        };
        let tile_color = role_color(fields.color, cx);
        let meta: String = match (&role, is_new) {
            (Some(r), _) if r.is_default => "Every member · the baseline under all roles".into(),
            (_, true) => "Not created yet".into(),
            (Some(r), _) => format!("{} · {}", plural(count_of(r), "member"), plural(perm_count(fields.permissions), "permission")),
            _ => String::new(),
        };
        let header = div()
            .flex()
            .items_center()
            .gap_4()
            .pb_5()
            .child(
                div()
                    .flex()
                    .size_12()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_lg)
                    .bg(tile_color.opacity(0.16))
                    .child(
                        Icon::new(if role.as_ref().is_some_and(|r| r.is_default) { IconName::SettingsUsers } else { IconName::SettingsShield })
                            .size(px(22.))
                            .color(tile_color),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .when(is_new && fields.name.trim().is_empty(), |this| this.opacity(0.6))
                            .child(crate::ui::editor_title(role_name.clone(), theme.color_text_primary)),
                    )
                    .child(crate::ui::meta(meta, cx)),
            )
            .when(role.as_ref().is_some_and(|r| !r.is_default) && editable, |this| {
                let r = role.clone().expect("checked");
                let (muted, error) = (theme.color_text_muted, theme.color_error);
                this.child(
                    div()
                        .id("role-delete")
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1p5()
                        .rounded(theme.radius_sm)
                        .px_3()
                        .py_2()
                        .text_color(muted)
                        .cursor_pointer()
                        .hover(move |s| s.bg(error.opacity(0.1)).text_color(error))
                        .child(Icon::new(IconName::SettingsTrash).size(px(14.)).color(muted).group_hover("role-delete-g", error))
                        .child(Label::new("Delete").size(13.).weight(600.))
                        .group("role-delete-g")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.confirm_delete = Some(r.clone());
                            cx.notify();
                        })),
                )
            });

        let mut editor = div().min_w_0().flex_1().child(header);
        if !editable && role.is_some() {
            editor = editor.child(
                div().mb_5().child(notice(
                    IconName::SettingsLock,
                    if can_manage {
                        "This role is at or above your highest role. Only the owner or a higher role can change it."
                    } else {
                        "You need Manage Roles to change roles."
                    },
                    cx,
                )),
            );
        }
        // Section tabs.
        let mut tabs = div().flex().gap_6().border_b_1().border_color(theme.color_border_divider);
        for s in &sections {
            let on = *s == section;
            let s = *s;
            let count = match s {
                Section::Permissions => Some(perm_count(fields.permissions)),
                Section::Members => role.as_ref().map(count_of),
                Section::Display => None,
            };
            let (primary, muted, secondary, accent) = (theme.color_text_primary, theme.color_text_muted, theme.color_text_secondary, theme.color_accent);
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("role-section-{}", s.label())))
                    .mb(px(-1.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .border_b_2()
                    .border_color(if on { accent } else { gpui::transparent_black() })
                    .pb_2p5()
                    .cursor_pointer()
                    .text_color(if on { primary } else { muted })
                    .when(!on, |this| this.hover(move |st| st.text_color(secondary)))
                    .child(Label::new(s.label()).size(13.).weight(600.))
                    .when_some(count, |this, n| {
                        this.child(
                            div()
                                .rounded(theme.radius_sm)
                                .bg(theme.color_surface_active)
                                .px_1p5()
                                .child(Label::new(Regional::get().group_signed(n)).font(FontRole::Meta).size(11.).line_height(18.).color(theme.color_text_muted)),
                        )
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.section = s;
                        cx.notify();
                    })),
            );
        }
        editor = editor.child(tabs);
        if let Some(error) = &self.error {
            editor = editor.child(div().mt_5().child(error_banner("role-error", error.clone(), None, cx)));
        }
        let body: AnyElement = match section {
            Section::Display => self.display_section(&fields, editable, window, cx),
            Section::Permissions => self.permissions_section(&fields, editable, window, cx),
            Section::Members => match &role {
                Some(r) => self.members_section(r, editable && !r.is_default, window, cx),
                None => div().into_any_element(),
            },
        };
        let theme = cx.theme();
        let _ = theme;
        editor = editor.child(div().pt_6().child(body)).child(
            SaveBar::new("roles-save", &self.ctx.scroll, dirty, self.nudge + self.ctx.nudge.get())
                .saving(self.save.saving())
                .message(if is_new { "This role doesn't exist until you create it." } else { "You have unsaved changes." })
                .nudge_message(if is_new { "Create the role or cancel it first." } else { "Save or reset your changes first." })
                .labels(if is_new { "Cancel" } else { "Reset" }, if is_new { "Create role" } else { "Save changes" })
                .save_disabled(!role.as_ref().is_some_and(|r| r.is_default) && fields.name.trim().is_empty())
                .on_reset(cx.listener(|this, _: &ClickEvent, _, cx| this.reset(cx)))
                .on_save(cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx))),
        );

        let confirm_title = format!("Delete {}?", self.confirm_delete.as_ref().map_or("role".to_string(), |r| r.name.to_string()));
        // Its right edge on the tile's, under it, as last laid out.
        let tile = self.custom_tile.get();
        let color_picker = self.color_picker.as_ref().map(|(picker, at, _)| {
            let at = if tile.size.width > px(0.) { point(tile.right() - px(PICKER_W), tile.bottom() + px(8.)) } else { *at };
            Popover::new("role-color-picker", at).over_modal()
                .on_dismiss(cx.listener(|this, _: &DismissEvent, _, cx| {
                    this.color_picker = None;
                    cx.notify();
                }))
                .child(div().w(px(PICKER_W)).p_3().child(picker.clone()))
        });
        let add_picker = self.add_picker.as_ref().map(|p| self.render_add_picker(p, cx));
        // The dialogs and popovers sit outside the row: as flex items they'd take a gap each.
        div()
            .child(div().flex().items_start().gap_8().child(aside).child(editor))
            .child(
                ConfirmModal::new("role-delete-confirm", self.confirm_delete.is_some(), confirm_title, "Delete role")
                    .tone(ConfirmTone::Danger)
                    .message("Members holding it lose its permissions and color right away. This can't be undone.")
                    .on_confirm(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(r) = this.confirm_delete.clone() {
                            this.run_delete(r, cx);
                        }
                    }))
                    .on_cancel(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.confirm_delete = None;
                        cx.notify();
                    })),
            )
            .children(color_picker)
            .children(add_picker)
    }
}

/// A hierarchy row (`RoleRow`): grip (draggable rows, on hover), colour dot, name, a lock when it
/// can't be edited, the member count.
#[allow(clippy::too_many_arguments)]
fn role_row(
    r: &Role,
    name: SharedString,
    color: u32,
    meta: String,
    selected: bool,
    locked: bool,
    draggable: bool,
    dragging: bool,
    drop_line: Option<bool>,
    index: usize,
    weak: &gpui::WeakEntity<RolesTab>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let id = r.id;
    let group: SharedString = format!("role-row-{id}").into();
    let w = weak.clone();
    let w2 = weak.clone();
    let accent = theme.color_accent;
    let row = role_row_body(name.clone(), color, meta.into(), selected, false, Some((group.clone(), draggable, locked)), cx)
        .id(("role-row", id as u64))
        .group(group)
        .when(dragging, |this| this.opacity(0.4))
        .when_some(drop_line, |this, above| {
            this.child(div().absolute().left_1().right_1().h(px(2.)).rounded_full().bg(accent).map(|d| if above { d.top(px(-1.)) } else { d.bottom(px(-1.)) }))
        })
        .on_click(move |_, _, cx| {
            let _ = w.update(cx, |this, cx| this.select(Selected::Role(id), cx));
        })
        .when(draggable, |this| {
            this.tooltip(decibell_ui::title_tooltip("Drag to reorder (Alt+↑/↓)"))
                .on_drag(RoleDrag { id, name: name.clone(), color }, move |d, _, _, cx| {
                    let (name, color) = (d.name.clone(), d.color);
                    let _ = w2.update(cx, |this, cx| {
                        this.drag_drop = Some((d.id, None));
                        cx.notify();
                    });
                    cx.new(|_| DragGhost { name, color })
                })
        });
    let w3 = weak.clone();
    div()
        .h(px(ROLE_ROW))
        .on_drag_move(move |e: &DragMoveEvent<RoleDrag>, _, cx| {
            if !e.bounds.contains(&e.event.position) {
                return;
            }
            let half = e.bounds.origin.y + e.bounds.size.height / 2.;
            let drop = if e.event.position.y < half { index } else { index + 1 };
            let _ = w3.update(cx, |this, cx| {
                if let Some((dragged, current)) = this.drag_drop
                    && current != Some(drop)
                {
                    this.drag_drop = Some((dragged, Some(drop)));
                    cx.notify();
                }
            });
        })
        .child(row)
        .into_any_element()
}

fn role_row_plain(name: SharedString, color: u32, meta: SharedString, selected: bool, italic: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    role_row_body(name, color, meta, selected, italic, None, cx).id(SharedString::from("role-row-plain"))
}

fn role_row_body(
    name: SharedString,
    color: u32,
    meta: SharedString,
    selected: bool,
    italic: bool,
    grip: Option<(SharedString, bool, bool)>,
    cx: &App,
) -> gpui::Div {
    let theme = cx.theme();
    let (bg, fg, weight) = if selected {
        (Some(theme.color_surface_active), theme.color_text_primary, 600.)
    } else {
        (None, theme.color_text_secondary, 500.)
    };
    let (hover_bg, hover_fg) = (theme.color_surface_hover, theme.color_text_primary);
    let (draggable, locked, group) = match &grip {
        Some((g, d, l)) => (*d, *l, Some(g.clone())),
        None => (false, false, None),
    };
    div()
        .relative()
        .flex()
        .h_9()
        .w_full()
        .items_center()
        .gap_2()
        .rounded(theme.radius_md)
        .pl_1()
        .pr_2p5()
        .cursor_pointer()
        .when_some(bg, |this, bg| this.bg(bg))
        .text_color(fg)
        .when(!selected, |this| this.hover(move |s| s.bg(hover_bg).text_color(hover_fg)))
        .child(
            div()
                .flex()
                .w_4()
                .flex_none()
                .justify_center()
                .invisible()
                .when(draggable, |this| match &group {
                    Some(g) => this.group_hover(g.clone(), |s| s.visible()).cursor_grab(),
                    None => this,
                })
                .child(Icon::new(IconName::SettingsGrip).size(px(14.)).color(theme.color_text_muted)),
        )
        .child(dot(role_color(color, cx), 10.))
        .child(
            div().min_w_0().flex_1().child(
                Label::new(name).size(13.).weight(weight).color(if italic { theme.color_text_muted } else { fg }).truncate(),
            ),
        )
        .when(locked, |this| this.child(Icon::new(IconName::SettingsLock).size(px(12.)).color(theme.color_text_muted)))
        .child(Label::new(meta).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted).nowrap())
}

impl RolesTab {
    fn display_section(&mut self, fields: &RoleFields, editable: bool, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let model = Model::global(cx).clone();
        let me = model.auth.read(cx).uid;
        let me_name: SharedString = model.auth.read(cx).username.clone().unwrap_or_else(|| "you".into());
        let nick = model
            .community(self.ctx.server, cx)
            .and_then(|c| c.roster.read(cx).nickname(me).cloned());
        let preview_name = nick.unwrap_or_else(|| me_name.clone());
        let pref = model.settings.read(cx).time_format();
        let clock = Regional::get().clock(crate::now_secs(), pref);
        let theme = cx.theme();
        let custom_active = fields.color != 0 && !ROLE_COLOR_PRESETS.contains(&fields.color);
        let mut swatches = div().flex().flex_wrap().items_center().gap_2p5().when(!editable, |this| this.opacity(0.6));
        for c in ROLE_COLOR_PRESETS {
            let on = fields.color == c;
            let title = if c == 0 { "No color".to_string() } else { format!("#{c:06x}") };
            let mark = if c == 0 { theme.color_text_primary } else { on_swatch(c) };
            let tile = div()
                .relative()
                .flex()
                .size_8()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .map(|this| if c == 0 { this.border_1().border_color(theme.color_border).bg(theme.color_bg_lighter) } else { this.bg(crate::server::color_picker::rgb(c)) })
                .when(c == 0 && !on, |this| {
                    // The diagonal of "no colour": `M9 23 23 9` in a 32-unit box.
                    let muted = theme.color_text_muted;
                    this.child(
                        gpui::canvas(|_, _, _| {}, move |b, _, window, _| {
                            let mut path = gpui::PathBuilder::stroke(px(2.));
                            let k = f32::from(b.size.width) / 32.;
                            path.move_to(b.origin + point(px(9. * k), px(23. * k)));
                            path.line_to(b.origin + point(px(23. * k), px(9. * k)));
                            if let Ok(path) = path.build() {
                                window.paint_path(path, muted);
                            }
                        })
                        .absolute()
                        .inset_0(),
                    )
                })
                .when(on, |this| this.child(Icon::new(IconName::SettingsCheck).size(px(15.)).stroke_width(3.).color(mark)));
            swatches = swatches.child(
                div()
                    .id(("role-color", c as u64))
                    .flex_none()
                    .rounded(theme.radius_md + px(4.))
                    .p(px(if on { 2. } else { 0. }))
                    .m(px(if on { -4. } else { 0. }))
                    .when(on, |this| this.border_2().border_color(theme.color_text_primary).bg(theme.color_bg_dark))
                    .tooltip(decibell_ui::title_tooltip(title))
                    .when(editable, |this| {
                        this.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.change(cx, |f| f.color = c)))
                    })
                    .child(tile),
            );
        }
        swatches = swatches.child(div().mx_0p5().h_5().w(px(1.)).bg(theme.color_border_divider)).child(
            div()
                .id("role-color-custom")
                .relative()
                .flex_none()
                .child(self.custom_tile.probe())
                .child(custom_tile(custom_active, self.color_picker.is_some(), fields.color, false, cx))
                .tooltip(decibell_ui::title_tooltip(if custom_active { format!("Custom color #{}", crate::server::color_picker::hex_of(fields.color)) } else { "Custom color".into() }))
                .when(editable, |this| this.cursor_pointer().on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_color_picker(window, cx)))),
        );
        let theme = cx.theme();
        let name_color = if fields.color != 0 { role_color(fields.color, cx) } else { theme.color_text_primary };
        let chip_name: SharedString = if fields.name.trim().is_empty() { "New role".into() } else { fields.name.trim().to_string().into() };
        div()
            .flex()
            .flex_col()
            .gap_7()
            .child(
                div()
                    .child(div().mb_2p5().child(SectionLabel::new("Role name")))
                    .child(div().max_w(px(384.)).child(TextField::new("role-name").editor(&self.name).disabled(!editable))),
            )
            .child(div().child(div().mb_2p5().child(SectionLabel::new("Color"))).child(swatches))
            .child(
                div()
                    .child(div().mb_2p5().child(SectionLabel::new("Preview")))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .rounded(theme.radius_lg)
                            .border_1()
                            .border_color(theme.color_border_divider)
                            .bg(theme.color_bg_mid)
                            .px_4()
                            .py(px(14.))
                            .child(UserAvatar::new(me, me_name.clone(), 40.))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .flex()
                                            .items_baseline()
                                            .gap_2()
                                            .child(
                                                Label::new(preview_name)
                                                    .font(FontRole::Channel)
                                                    .size(theme.scaled.text_sender)
                                                    .weight(theme.palette.font_weight_emphasis)
                                                    .color(name_color)
                                                    .truncate(),
                                            )
                                            .child(Label::new(format!("Today at {clock}")).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted).nowrap()),
                                    )
                                    .child(
                                        Label::new("This is how my name looks in chat.")
                                            .size(theme.scaled.text_body)
                                            .line_height(theme.scaled.text_body * theme.palette.leading_body)
                                            .color(theme.color_text_primary),
                                    )
                                    .child(div().mt_2p5().flex().child(RoleChip::new("role-preview-chip", chip_name, fields.color))),
                            ),
                    )
                    .child(div().mt_2().child(Label::new("How a member whose highest role this is shows up in chat.").size(12.).line_height(12. * 1.55).color(theme.color_text_muted))),
            )
            .into_any_element()
    }

    fn permissions_section(&mut self, fields: &RoleFields, editable: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let query = self.perm_query.text(cx);
        let q = query.trim().to_lowercase();
        let matches = |label: &str, description: &str| q.is_empty() || label.to_lowercase().contains(&q) || description.to_lowercase().contains(&q);
        let admin = has_bits(fields.permissions, perm::ADMINISTRATOR);
        let any_match = EDITABLE_PERMISSIONS.iter().any(|p| matches(p.label, p.description));
        let theme = cx.theme();
        let mut out = div().flex().flex_col().gap_7().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().min_w_0().flex_1().child(self.perm_query.field("perm-search")))
                .when(editable && fields.permissions != 0, |this| {
                    this.child(
                        Button::new("perm-clear", "Clear all")
                            .variant(ButtonVariant::Ghost)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.change(cx, |f| f.permissions = 0))),
                    )
                }),
        );
        if admin && q.is_empty() {
            out = out.child(
                div()
                    .mt(px(-12.))
                    .flex()
                    .gap_2p5()
                    .rounded(theme.radius_md)
                    .border_1()
                    .border_color(theme.color_warning.opacity(0.3))
                    .bg(theme.color_warning.opacity(0.1))
                    .px(px(14.))
                    .py_3()
                    .child(div().mt_0p5().child(Icon::new(IconName::SettingsAlert).size(px(15.)).color(theme.color_warning)))
                    .child(crate::sentence::Sentence::new(&crate::sentence::Shaped::new(crate::sentence::build(
                        &[
                            crate::sentence::Run::Strong("Administrator is on.".into()),
                            crate::sentence::Run::Text(" This role can do everything below, whatever the switches say. Hierarchy still applies.".into()),
                        ],
                        crate::sentence::Base { font: FontRole::Sans, size: 13., line_height: 13. * 1.55, color: theme.color_text_secondary, strong: theme.color_text_primary },
                        window.scale_factor(),
                        cx,
                    )))),
            );
        }
        let theme = cx.theme();
        if q.is_empty() {
            out = out.child(
                div().child(div().mb_2p5().child(SectionLabel::new("Hierarchy"))).child(
                    div().overflow_hidden().rounded(theme.radius_lg).border_1().border_color(theme.color_border_divider).bg(theme.color_bg_light).child(perm_row(
                        "perm-manage-each-other",
                        "Members with this role can manage each other",
                        "When it's their highest role, they can change each other's nicknames and use voice moderation on each other. Kicks, bans, timeouts and roles still need a higher role.",
                        fields.manage_each_other,
                        !editable,
                        {
                            let weak = cx.weak_entity();
                            move |_, cx| {
                                let _ = weak.update(cx, |this, cx| this.change(cx, |f| f.manage_each_other = !f.manage_each_other));
                            }
                        },
                        cx,
                    )),
                ),
            );
        }
        for group in PERMISSION_GROUPS {
            let rows: Vec<_> = EDITABLE_PERMISSIONS.iter().filter(|p| p.group == group && matches(p.label, p.description)).collect();
            if rows.is_empty() {
                continue;
            }
            let mut card = div().flex().flex_col().overflow_hidden().rounded(theme.radius_lg).border_1().border_color(theme.color_border_divider).bg(theme.color_bg_light);
            let n = rows.len();
            for (i, p) in rows.into_iter().enumerate() {
                let on = has_bits(fields.permissions, p.bit);
                let bit = p.bit;
                let weak = cx.weak_entity();
                card = card.child(perm_row(
                    ("perm", bit),
                    p.label,
                    p.description,
                    on,
                    !editable,
                    move |_, cx| {
                        let _ = weak.update(cx, |this, cx| this.change(cx, |f| f.permissions = toggle_bit(f.permissions, bit, !on)));
                    },
                    cx,
                ));
                if i + 1 < n {
                    card = card.child(div().h(px(1.)).bg(theme.color_border_divider));
                }
            }
            out = out.child(div().child(div().mb_2p5().child(SectionLabel::new(group.label()))).child(card));
        }
        if !any_match {
            out = out.child(
                div().py_6().flex().justify_center().child(Label::new(format!("No permission matches \u{201c}{}\u{201d}.", query.trim())).size(13.).color(theme.color_text_muted)),
            );
        }
        out.into_any_element()
    }

    fn members_section(&mut self, role: &Role, can_assign: bool, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // The role's holders, searched on the server.
        if self.holders.as_ref().is_none_or(|(id, _, _)| *id != role.id) {
            let server = self.ctx.server;
            let query = self.holders_query.text(cx);
            let role_id = role.id;
            let search = cx.new(|cx| MemberSearch::new(server, &query, role_id, cx));
            let observe = cx.observe(&search, |_, _, cx| cx.notify());
            self.holders = Some((role.id, search, observe));
        }
        let search = self.holders.as_ref().map(|(_, s, _)| s.clone()).expect("set above");
        let found = search.read(cx);
        let (loaded, error, has_more, loading, total) = (found.loaded, found.error.clone(), found.has_more, found.loading, found.total);
        let members = found.members.clone();
        let query = self.holders_query.text(cx);
        let theme = cx.theme();
        let mut out = div().flex().flex_col().gap_4().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().min_w_0().flex_1().child(self.holders_query.field("holders-search")))
                .when(can_assign, |this| {
                    let role_id = role.id;
                    this.child(
                        div()
                            .id("holders-add")
                            .relative()
                            .flex()
                            .h_10()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .rounded(theme.radius_sm)
                            .bg(theme.color_accent)
                            .px_4()
                            .cursor_pointer()
                            .hover({
                                let h = theme.color_accent_hover;
                                move |s| s.bg(h)
                            })
                            .child(self.add_button.probe())
                            .child(Icon::new(IconName::SettingsUserPlus).size(px(15.)).color(theme.color_on_accent))
                            .child(Label::new("Add members").size(13.).weight(600.).color(theme.color_on_accent))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_add_picker(role_id, window, cx))),
                    )
                }),
        );
        if !loaded {
            return out.child(div().px_3().py_4().child(Label::new("Loading members…").size(13.).color(theme.color_text_muted))).into_any_element();
        }
        if let Some(error) = error.clone().filter(|_| members.is_empty()) {
            return out.child(error_banner("holders-error", error, None, cx)).into_any_element();
        }
        if members.is_empty() && query.trim().is_empty() {
            return out.child(EmptyState::new(IconName::SettingsUsers, "Nobody has this role yet", "Add members here, or from the Members tab.")).into_any_element();
        }
        let rows = Rc::new(members.clone());
        let weak = cx.weak_entity();
        let busy = self.busy;
        let (role_id, role_name) = (role.id, role.name.clone());
        let n = rows.len();
        let list = PaneRows::uniform(&self.ctx.scroll, n, px(HOLDER_ROW), move |range, _, cx| {
            let theme = cx.theme();
            range
                .map(|i| {
                    let m = rows[i].clone();
                    let name = user_name(m.uid, &m.username, cx);
                    let shown: SharedString = if m.nickname.is_empty() { name.clone() } else { m.nickname.clone() };
                    let group: SharedString = format!("holder-{}", m.uid).into();
                    let hover = theme.color_surface_hover;
                    let w = weak.clone();
                    let title = format!("Remove {role_name} from {shown}");
                    div()
                        .id(("holder", m.uid as u64))
                        .group(group.clone())
                        .flex()
                        .h(px(HOLDER_ROW))
                        .items_center()
                        .gap_3()
                        .rounded(theme.radius_md)
                        .px_3()
                        .hover(move |s| s.bg(hover))
                        .child(UserAvatar::new(m.uid, name.clone(), 28.).dot(AvatarDot::new(presence_of(m.uid, m.is_online, cx), theme.color_bg_dark, 13.)))
                        .child(
                            div()
                                .flex()
                                .min_w_0()
                                .flex_1()
                                .items_baseline()
                                .gap_2()
                                .child(Label::new(shown.clone()).size(14.).weight(500.).color(theme.color_text_primary).truncate())
                                .when(!m.nickname.is_empty(), |this| this.child(Label::new(name.clone()).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted).nowrap())),
                        )
                        .when(can_assign, |this| {
                            let (muted, error) = (theme.color_text_muted, theme.color_error);
                            let off = busy == Some(m.uid);
                            this.child(
                                div()
                                    .id(("holder-remove", m.uid as u64))
                                    .flex()
                                    .size_8()
                                    .items_center()
                                    .justify_center()
                                    .rounded(theme.radius_sm)
                                    .invisible()
                                    .group_hover(group.clone(), |s| s.visible())
                                    .when(off, |this| this.opacity(0.4))
                                    .tooltip(decibell_ui::title_tooltip(title))
                                    .hover(move |s| s.bg(error.opacity(0.1)))
                                    .child(Icon::new(IconName::SettingsX).size(px(14.)).stroke_width(2.5).color(muted).group_hover(group.clone(), error))
                                    .when(!off, |this| {
                                        this.cursor_pointer().on_click(move |_, _, cx| {
                                            let m = m.clone();
                                            let _ = w.update(cx, |this, cx| this.set_role_holding(&m, role_id, false, cx));
                                        })
                                    }),
                            )
                        })
                        .into_any_element()
                })
                .collect()
        });
        let mut list_box = div().flex().flex_col().child(list);
        if members.is_empty() {
            list_box = list_box.child(div().py_6().flex().justify_center().child(Label::new(format!("No one with this role matches \u{201c}{}\u{201d}.", query.trim())).size(13.).color(theme.color_text_muted)));
        }
        if has_more {
            let more = (total - members.len() as i64).max(0);
            let s2 = search.clone();
            list_box = list_box.child(
                div().flex().justify_center().pt_3().child(
                    outline_button("holders-more", if loading { "Loading…".to_string() } else { format!("Show {} more", Regional::get().group_signed(more)) }, loading, false, cx)
                        .when(!loading, |b| b.on_click(move |_, _, cx| s2.update(cx, |s, cx| s.load_more(cx)))),
                ),
            );
        }
        out = out.child(list_box);
        out.into_any_element()
    }

    fn render_add_picker(&self, p: &AddPicker, cx: &mut Context<Self>) -> AnyElement {
        let role_id = match self.selected {
            Some(Selected::Role(id)) => id,
            _ => 0,
        };
        let found = p.search.read(cx);
        let shown: Vec<Member> = found.members.iter().filter(|m| !m.is_owner && !m.role_ids.contains(&role_id)).cloned().collect();
        let (loaded, has_more, loading) = (found.loaded, found.has_more, found.loading);
        let query_blank = found.query().trim().is_empty();
        let picked: Vec<Uid> = p.picked.iter().map(|m| m.uid).collect();
        let theme = cx.theme();
        let mut list = div().id("add-members-list").flex().flex_col().max_h(px(256.)).overflow_y_scroll();
        for m in shown.iter().cloned() {
            let on = picked.contains(&m.uid);
            let name = user_name(m.uid, &m.username, cx);
            let primary: SharedString = if m.nickname.is_empty() { name.clone() } else { m.nickname.clone() };
            let hover = theme.color_surface_hover;
            list = list.child(
                div()
                    .id(("add-member", m.uid as u64))
                    .flex()
                    .w_full()
                    .items_center()
                    .gap_2p5()
                    .rounded(theme.radius_md)
                    .px_2()
                    .py_1p5()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(UserAvatar::new(m.uid, name.clone(), 24.))
                    .child(
                        div()
                            .flex()
                            .min_w_0()
                            .flex_1()
                            .items_baseline()
                            .gap_1p5()
                            .child(Label::new(primary).size(13.).weight(500.).color(theme.color_text_primary).truncate())
                            .when(!m.nickname.is_empty(), |this| this.child(Label::new(name.clone()).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted).nowrap())),
                    )
                    .child(check_square(on, cx))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(p) = &mut this.add_picker {
                            if let Some(i) = p.picked.iter().position(|x| x.uid == m.uid) {
                                p.picked.remove(i);
                            } else {
                                p.picked.push(m.clone());
                            }
                            cx.notify();
                        }
                    })),
            );
        }
        let theme = cx.theme();
        if has_more {
            let search = p.search.clone();
            let (muted, primary, hover) = (theme.color_text_muted, theme.color_text_primary, theme.color_surface_hover);
            list = list.child(
                div()
                    .id("add-members-more")
                    .rounded(theme.radius_md)
                    .px_2()
                    .py_1p5()
                    .text_color(muted)
                    .when(loading, |this| this.opacity(0.6))
                    .when(!loading, |this| this.cursor_pointer().hover(move |s| s.bg(hover).text_color(primary)).on_click(move |_, _, cx| search.update(cx, |s, cx| s.load_more(cx))))
                    .child(Label::new(if loading { "Loading…" } else { "Show more" }).size(12.).weight(500.)),
            );
        }
        if loaded && shown.is_empty() && !has_more {
            list = list.child(
                div().px_2().py_4().flex().justify_center().child(
                    Label::new(if query_blank { "Everyone here already has it." } else { "No one else matches." }).size(12.).color(theme.color_text_muted),
                ),
            );
        }
        let count = picked.len();
        let label = if count > 0 { format!("Add {count} member{}", if count == 1 { "" } else { "s" }) } else { "Add members".to_string() };
        let b = self.add_button.get();
        let anchor = if b.size.width > px(0.) { point(b.right() - px(288.), b.bottom() + px(4.)) } else { p.anchor };
        Popover::new("add-members", anchor).over_modal()
            .on_dismiss(cx.listener(|this, _: &DismissEvent, _, cx| {
                this.add_picker = None;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w(px(288.))
                    .p_2()
                    .child(p.query.field("add-members-search"))
                    .child(div().mt_2().child(list))
                    .child(
                        div().mt_2().child(Button::new("add-members-run", label).full_width().disabled(count == 0).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let Some(p) = this.add_picker.take() else { return };
                            for m in p.picked {
                                this.set_role_holding(&m, role_id, true, cx);
                            }
                            cx.notify();
                        }))),
                    ),
            )
            .into_any_element()
    }
}

#[allow(dead_code)]
fn _unused(_: Hsla, _: MouseButton) {}
