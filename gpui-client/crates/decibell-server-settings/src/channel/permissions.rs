//! Channel settings → Permissions (`channelSettings/PermissionsTab.tsx`, permissions v2 / v3):
//! the targets with overrides on the left (@everyone always, at the bottom), the selected one's
//! allow / inherit / deny controls on the right, only for the bits the channel's type uses (a
//! category keeps all). Edits are staged with the rest of the channel settings and go out with
//! the save bar; "Sync now" / "Sync all" are actions and run at once. Mirrors the server's guards
//! so the UI doesn't offer what will be refused: only bits the local user holds in this channel
//! are toggleable, and roles at or above the user's level are not offered.

use std::collections::HashMap;

use decibell_common::{BoundsCell, UserAvatar, user_name};
use decibell_model::stores::community::{Channel, ChannelKind, Member, Overwrite, OverwriteTarget, Role};
use decibell_model::stores::permissions::{PermissionGroup, channel_overwrite_permissions, has_bits, perm, toggle_bit};
use decibell_model::{Model, Uid};
use decibell_ui::{ActiveTheme as _, DismissEvent, EmptyState, FontRole, Icon, IconName, Label, Popover, SectionLabel, Switch};
use gpui::{AnyElement, App, AppContext as _, ClickEvent, Context, Entity, Hsla, IntoElement, Pixels, Point, SharedString, Subscription, Window, div, point, prelude::*, px};

use super::ChannelSettingsWindow;
use crate::member_search::MemberSearch;
use crate::requests::{self, Request};
use crate::server::overview::toast_error;
use crate::ui::{SearchBox, dot, notice, outline_button, role_color};

/// Allow / deny masks of one overwrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bits {
    pub allow: u64,
    pub deny: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriState {
    Allow,
    Inherit,
    Deny,
}

pub fn state_of(b: Bits, bit: u64) -> TriState {
    if has_bits(b.deny, bit) {
        TriState::Deny
    } else if has_bits(b.allow, bit) {
        TriState::Allow
    } else {
        TriState::Inherit
    }
}

/// The draft key of a stored overwrite: members by account, never by name.
pub fn overwrite_key(o: &Overwrite) -> String {
    match o.target_type {
        OverwriteTarget::Member => format!("member:{}", o.target_uid),
        OverwriteTarget::Role => format!("role:{}", o.target_id),
    }
}

/// "role:<id>" | "member:<uid>" → its two halves.
fn split_target(key: &str) -> (&str, &str) {
    key.split_once(':').unwrap_or(("role", key))
}

/// Which overwrite groups a channel type shows. A text channel has no use for Connect or Speak,
/// a voice channel none for Send Messages; a category governs both kinds. Hidden bits keep
/// whatever they're set to.
pub fn groups_for(kind: ChannelKind) -> &'static [PermissionGroup] {
    match kind {
        ChannelKind::Text => &[PermissionGroup::General, PermissionGroup::Text],
        ChannelKind::Voice => &[PermissionGroup::General, PermissionGroup::Voice],
        _ => &[PermissionGroup::General, PermissionGroup::Text, PermissionGroup::Voice],
    }
}

fn group_label(g: PermissionGroup) -> &'static str {
    match g {
        PermissionGroup::General => "Access and management",
        PermissionGroup::Members => "Members",
        PermissionGroup::Text => "Text",
        PermissionGroup::Voice => "Voice",
    }
}

struct AddTarget {
    search: Entity<MemberSearch>,
    query: SearchBox,
    anchor: Point<Pixels>,
    _observe: Subscription,
}

/// Staged permission edits for one channel: overwrites per target (full allow / deny), a
/// category's sync switch (none = unchanged), and targets added in this session that have no
/// overwrite yet. Lives in the window so switching tabs keeps it.
#[derive(Default)]
pub struct PermDraft {
    /// In the order they were staged (the order the save sends them).
    pub pending: Vec<(String, Bits)>,
    pub sync_draft: Option<bool>,
    pub added: Vec<String>,
    selected: String,
    picker: Option<AddTarget>,
    /// Names of members picked this session, for one the roster never loaded.
    picked_names: HashMap<Uid, SharedString>,
    busy: bool,
    add_button: BoundsCell,
}

impl PermDraft {
    pub fn pending_bits(&self, key: &str) -> Option<Bits> {
        self.pending.iter().find(|(k, _)| k == key).map(|(_, b)| *b)
    }

    pub fn dirty(&self, window: &ChannelSettingsWindow, cx: &App) -> bool {
        let category_sync = window.channel(cx).is_some_and(|c| c.category_sync);
        !self.pending.is_empty() || self.sync_draft.is_some_and(|s| s != category_sync)
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.sync_draft = None;
        self.added.clear();
    }

    /// Staged rows the server now holds drop out of the draft: that's how a save settles (the
    /// re-pushed overwrites arrive after the calls return).
    pub fn settle(&mut self, overwrites: &[Overwrite], category_sync: bool) {
        self.pending.retain(|(key, bits)| {
            let stored = overwrites.iter().find(|o| overwrite_key(o) == *key);
            !(stored.map_or(0, |o| o.allow) == bits.allow && stored.map_or(0, |o| o.deny) == bits.deny)
        });
        self.added.retain(|k| !overwrites.iter().any(|o| overwrite_key(o) == *k));
        if self.sync_draft == Some(category_sync) {
            self.sync_draft = None;
        }
    }

    /// Overwrites first, then the category switch (so turning sync off hands its followers the
    /// rows as edited). The draft clears as the server's updates arrive, not here.
    pub fn commit(&self, window: &ChannelSettingsWindow, channel: &Channel, _: &App) -> Vec<Request> {
        let mut out = Vec::new();
        for (key, bits) in &self.pending {
            let (kind, id) = split_target(key);
            let member = kind == "member";
            out.push(Request::SetOverwrite {
                server: window.server,
                channel: window.channel_id.clone(),
                target_type: if member { "member" } else { "role" },
                target_id: if member { String::new() } else { id.to_string() },
                target_uid: if member { id.parse().unwrap_or(0) } else { 0 },
                allow: bits.allow,
                deny: bits.deny,
            });
        }
        if let Some(sync) = self.sync_draft
            && sync != channel.category_sync
        {
            out.push(Request::SetCategorySync { server: window.server, channel: window.channel_id.clone(), enabled: sync });
        }
        out
    }
}

/// What the permissions view reads, once per render.
struct Facts {
    roles: Vec<Role>,
    members: HashMap<Uid, Member>,
    overwrites: Vec<Overwrite>,
    parent: Option<Channel>,
    children: Vec<Channel>,
    is_owner: bool,
    my_bits: u64,
    my_level: i32,
    everyone_key: String,
    can_edit: bool,
}

/// Who or what a row is.
struct Info {
    member: Option<Uid>,
    role: Option<Role>,
    username: SharedString,
    name: SharedString,
    sub: SharedString,
}

impl Facts {
    fn stored(&self, key: &str) -> Bits {
        self.overwrites.iter().find(|o| overwrite_key(o) == key).map_or(Bits::default(), |o| Bits { allow: o.allow, deny: o.deny })
    }

    fn describe(&self, key: &str, picked: &HashMap<Uid, SharedString>, cx: &App) -> Info {
        let (kind, id) = split_target(key);
        if kind == "role" {
            let role = self.roles.iter().find(|r| r.id.to_string() == id).cloned();
            let name: SharedString = match &role {
                Some(r) if r.is_default => "@everyone".into(),
                Some(r) => r.name.clone(),
                None => "Deleted role".into(),
            };
            return Info { member: None, role, username: SharedString::default(), name, sub: SharedString::default() };
        }
        let uid: Uid = id.parse().unwrap_or(0);
        let m = self.members.get(&uid);
        // A community placeholder (no account) is named only by its rows.
        let fallback: SharedString = m
            .map(|m| m.username.clone())
            .or_else(|| self.overwrites.iter().find(|o| o.target_type == OverwriteTarget::Member && o.target_uid == uid).map(|o| o.target_id.clone()))
            .or_else(|| picked.get(&uid).cloned())
            .unwrap_or_else(|| "Unknown user".into());
        let username = user_name(uid, &fallback, cx);
        let nickname = m.map(|m| m.nickname.clone()).filter(|n| !n.is_empty());
        Info {
            member: Some(uid),
            role: None,
            name: nickname.clone().unwrap_or_else(|| username.clone()),
            sub: if nickname.is_some() { username.clone() } else { SharedString::default() },
            username,
        }
    }

    /// Role rows at or above your highest role are the server's to refuse (everyone is always
    /// editable; member rows are any member's).
    fn locked(&self, key: &str) -> bool {
        let (kind, id) = split_target(key);
        if self.is_owner || kind != "role" {
            return false;
        }
        self.roles.iter().find(|r| r.id.to_string() == id).is_some_and(|r| !r.is_default && r.position >= self.my_level)
    }
}

impl ChannelSettingsWindow {
    fn facts(&self, channel: &Channel, cx: &App) -> Facts {
        let model = Model::global(cx);
        let community = model.community(self.server, cx);
        let me = model.auth.read(cx).uid;
        let roles: Vec<Role> = community.as_ref().map(|c| c.roles.read(cx).list().to_vec()).unwrap_or_default();
        let members = community.as_ref().map(|c| c.roster.read(cx).members().clone()).unwrap_or_default();
        let list: Vec<Channel> = community.as_ref().map(|c| c.channels.read(cx).list().to_vec()).unwrap_or_default();
        let owner = community.as_ref().map_or(0, |c| c.meta.read(cx).owner);
        let is_owner = owner != 0 && owner == me;
        let is_category = channel.kind == ChannelKind::Category;
        // Nearest category above the channel in the flat list.
        let parent = if is_category {
            None
        } else {
            let mut category = None;
            let mut found = None;
            for c in &list {
                if c.id == channel.id {
                    found = category.clone();
                    break;
                }
                if c.kind == ChannelKind::Category {
                    category = Some(c.clone());
                }
            }
            found
        };
        let mut children = Vec::new();
        if is_category {
            let mut inside = false;
            for c in &list {
                if c.kind == ChannelKind::Category {
                    if inside {
                        break;
                    }
                    inside = c.id == channel.id;
                } else if inside {
                    children.push(c.clone());
                }
            }
        }
        let mine: Vec<i64> = members.get(&me).map(|m| m.role_ids.to_vec()).unwrap_or_default();
        let my_level = if is_owner { i32::MAX } else { mine.iter().map(|id| roles.iter().find(|r| r.id == *id).map_or(0, |r| r.position)).fold(0, i32::max) };
        let everyone_key = roles.iter().find(|r| r.is_default).map_or(String::new(), |r| format!("role:{}", r.id));
        Facts {
            my_bits: if is_owner { perm::ALL } else { channel.my_permissions },
            overwrites: self.overwrites(cx),
            can_edit: model.channel_permission(self.server, &self.channel_id, perm::MANAGE_ROLES, cx),
            roles,
            members,
            parent,
            children,
            is_owner,
            my_level,
            everyone_key,
        }
    }

    #[cfg(feature = "fixtures")]
    /// Stage `bits` for `key` (`role:<id>` / `member:<uid>`), as the tri-state controls do.
    pub(crate) fn stage_bits(&mut self, key: &str, bits: Bits, cx: &mut Context<Self>) {
        let Some(channel) = self.channel(cx) else { return };
        let f = self.facts(&channel, cx);
        self.stage(&f, key, bits);
        cx.notify();
    }

    #[cfg(feature = "fixtures")]
    /// The category switch, staged.
    pub(crate) fn stage_category_sync(&mut self, on: bool, cx: &mut Context<Self>) {
        self.perms.sync_draft = Some(on);
        cx.notify();
    }

    #[cfg(feature = "fixtures")]
    /// "Sync now" on a channel, "Sync all" on a category.
    pub(crate) fn sync_now(&mut self, cx: &mut Context<Self>) {
        let Some(channel) = self.channel(cx) else { return };
        let f = self.facts(&channel, cx);
        self.run_sync(&f, cx);
    }

    fn bits_of(&self, f: &Facts, key: &str) -> Bits {
        self.perms.pending_bits(key).unwrap_or_else(|| f.stored(key))
    }

    /// Targets listed: every stored overwrite, anything staged, anything just added. Roles by
    /// rank, then members by key; @everyone apart.
    fn targets(&self, f: &Facts) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for k in f.overwrites.iter().map(overwrite_key).chain(self.perms.pending.iter().map(|(k, _)| k.clone())).chain(self.perms.added.iter().cloned()) {
            if k != f.everyone_key && !keys.contains(&k) {
                keys.push(k);
            }
        }
        let rank = |k: &str| {
            let (kind, id) = split_target(k);
            if kind == "member" {
                return -1;
            }
            f.roles.iter().find(|r| r.id.to_string() == id).map_or(0, |r| r.position)
        };
        keys.sort_by(|a, b| rank(b).cmp(&rank(a)).then_with(|| a.cmp(b)));
        keys
    }

    fn stage(&mut self, f: &Facts, key: &str, next: Bits) {
        let saved = f.stored(key);
        self.perms.pending.retain(|(k, _)| k != key);
        if next != saved {
            self.perms.pending.push((key.to_string(), next));
        }
    }

    fn run_sync(&mut self, f: &Facts, cx: &mut Context<Self>) {
        if !f.can_edit || self.perms.busy || self.perms.dirty(self, cx) {
            return;
        }
        self.perms.busy = true;
        cx.notify();
        let weak = cx.weak_entity();
        let request = Request::SyncChannelPermissions { server: self.server, channel: self.channel_id.clone(), category: f.parent.as_ref().map(|p| p.id.clone()) };
        requests::send(request, cx, move |result, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.perms.busy = false;
                cx.notify();
            });
            if let Err(e) = result {
                toast_error("Couldn't sync permissions", e.js_string(), cx);
            }
        });
    }

    fn open_add_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.perms.picker.is_some() {
            self.perms.picker = None;
            cx.notify();
            return;
        }
        let search = cx.new(|cx| MemberSearch::new(self.server, "", 0, cx));
        let query = SearchBox::new("Find a role or member", window, cx, |this: &mut Self, text, cx| {
            if let Some(p) = &this.perms.picker {
                p.search.update(cx, |s, cx| s.set_query(text, cx));
            }
            cx.notify();
        });
        query.focus(window, cx);
        let observe = cx.observe(&search, |_, _, cx| cx.notify());
        let b = self.perms.add_button.get();
        self.perms.picker = Some(AddTarget { search, query, anchor: point(b.left(), b.bottom() + px(4.)), _observe: observe });
        cx.notify();
    }

    fn add_target(&mut self, f_targets: &[String], everyone: &str, key: String, name: Option<SharedString>, cx: &mut Context<Self>) {
        let (kind, id) = split_target(&key);
        if kind == "member"
            && let Some(name) = name
        {
            self.perms.picked_names.insert(id.parse().unwrap_or(0), name);
        }
        self.perms.picker = None;
        if key != everyone && !f_targets.contains(&key) && !self.perms.added.contains(&key) {
            self.perms.added.push(key.clone());
        }
        self.perms.selected = key;
        cx.notify();
    }

    pub(crate) fn permissions_tab(&mut self, channel: &Channel, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let f = self.facts(channel, cx);
        let theme = cx.theme();
        let is_category = channel.kind == ChannelKind::Category;
        let synced = f.parent.is_some() && channel.permissions_synced;
        let followers = f.children.iter().filter(|c| c.permissions_synced).count();
        let draft_dirty = self.perms.dirty(self, cx);
        let category_sync_shown = self.perms.sync_draft.unwrap_or(channel.category_sync);
        let plain_group = is_category && !category_sync_shown;
        let sync_title = draft_dirty.then_some("Save or reset your changes first.");
        let busy = self.perms.busy;
        let mut out = div().flex().flex_col().gap_6();

        // ── sync with the category ──
        let strip = |icon: IconName, text: AnyElement, action: Option<&'static str>, cx: &mut Context<Self>| {
            let theme = cx.theme();
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_3()
                .child(Icon::new(icon).size(px(15.)).color(theme.color_text_muted))
                .child(div().min_w_0().flex_1().child(text))
                .when_some(action, |this, action| {
                    let disabled = busy || draft_dirty;
                    let mut b = outline_button("perm-sync", action, disabled, true, cx);
                    if let Some(t) = sync_title {
                        b = b.tooltip(decibell_ui::title_tooltip(t));
                    }
                    if !disabled {
                        b = b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if let Some(channel) = this.channel(cx) {
                                let f = this.facts(&channel, cx);
                                this.run_sync(&f, cx);
                            }
                        }));
                    }
                    this.child(b)
                })
        };
        let sentence = |runs: Vec<(String, bool)>, cx: &App| -> AnyElement {
            let theme = cx.theme();
            let mut d = div().flex().flex_wrap();
            for (text, strong) in runs {
                d = d.child(
                    Label::new(text).size(13.).line_height(13. * 1.55).weight(if strong { 600. } else { 400. }).color(if strong { theme.color_text_primary } else { theme.color_text_secondary }),
                );
            }
            d.into_any_element()
        };
        if is_category {
            let mut card = div().flex().flex_col().overflow_hidden().rounded(theme.radius_lg).border_1().border_color(theme.color_border_divider).bg(theme.color_bg_light);
            let hover = theme.color_bg_lighter;
            let can_edit = f.can_edit;
            let stored_sync = channel.category_sync;
            card = card.child(
                div()
                    .id("category-sync-row")
                    .flex()
                    .items_center()
                    .gap_4()
                    .px_4()
                    .py(px(14.))
                    .when(can_edit, |this| {
                        this.cursor_pointer().hover(move |s| s.bg(hover)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let next = !this.perms.sync_draft.unwrap_or(stored_sync);
                            this.perms.sync_draft = if next == stored_sync { None } else { Some(next) };
                            cx.notify();
                        }))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(Label::new("Channels follow this category's permissions").size(14.).weight(500.).color(theme.color_text_primary))
                            .child(
                                div().mt_0p5().child(
                                    Label::new("Off, the category only groups channels. Switching it never changes what a channel allows: a channel follows the category once it's synced.")
                                        .size(12.)
                                        .line_height(12. * 1.55)
                                        .color(theme.color_text_muted),
                                ),
                            ),
                    )
                    .child(Switch::new("category-sync", category_sync_shown).disabled(!can_edit).on_toggle(cx.listener(move |this, _: &bool, _, cx| {
                        cx.stop_propagation();
                        let next = !this.perms.sync_draft.unwrap_or(stored_sync);
                        this.perms.sync_draft = if next == stored_sync { None } else { Some(next) };
                        cx.notify();
                    }))),
            );
            if channel.category_sync && category_sync_shown && !f.children.is_empty() {
                let n = f.children.len();
                let text = sentence(
                    vec![
                        (format!("{followers} of {n}"), true),
                        (format!("\u{00a0}channel{} follow these permissions.", if n == 1 { "" } else { "s" }), false),
                    ],
                    cx,
                );
                let action = (followers < n && f.can_edit).then_some("Sync all");
                card = card.child(div().h(px(1.)).bg(cx.theme().color_border_divider)).child(strip(IconName::SettingsLink, text, action, cx));
            }
            out = out.child(card);
        }
        if let Some(parent) = f.parent.as_ref().filter(|p| p.category_sync) {
            let theme = cx.theme();
            let text = if synced {
                sentence(
                    vec![
                        ("Synced with\u{00a0}".into(), false),
                        (parent.name.to_string(), true),
                        (". Changing anything here gives this channel its own permissions.".into(), false),
                    ],
                    cx,
                )
            } else {
                sentence(
                    vec![
                        ("Not synced with\u{00a0}".into(), false),
                        (parent.name.to_string(), true),
                        (". This channel has its own permissions.".into(), false),
                    ],
                    cx,
                )
            };
            let action = (!synced && f.can_edit).then_some("Sync now");
            let card = div().overflow_hidden().rounded(theme.radius_lg).border_1().border_color(theme.color_border_divider).bg(theme.color_bg_light);
            out = out.child(card.child(strip(if synced { IconName::SettingsLink } else { IconName::SettingsLinkOff }, text, action, cx)));
        }
        if !f.can_edit {
            out = out.child(notice(IconName::SettingsLock, "You need Manage Roles in this channel to change permissions.", cx));
        }
        if plain_group {
            out = out.child(EmptyState::new(
                IconName::SettingsFolder,
                "This category only groups channels",
                "Each channel in it keeps its own permissions. Turn on the switch above to set permissions here that its channels can follow.",
            ));
            return out.into_any_element();
        }

        // ── targets ──
        let targets = self.targets(&f);
        let everyone_key = f.everyone_key.clone();
        let target = if self.perms.selected == everyone_key || targets.contains(&self.perms.selected) { self.perms.selected.clone() } else { everyone_key.clone() };
        let visible: Vec<_> = channel_overwrite_permissions().filter(|p| groups_for(channel.kind).contains(&p.group)).collect();
        let counts = |b: Bits| {
            (
                visible.iter().filter(|p| state_of(b, p.bit) == TriState::Allow).count(),
                visible.iter().filter(|p| state_of(b, p.bit) == TriState::Deny).count(),
            )
        };
        let theme = cx.theme();
        let mut list = div().flex().flex_col().gap_0p5();
        for k in &targets {
            let info = f.describe(k, &self.perms.picked_names, cx);
            let c = counts(self.bits_of(&f, k));
            let key = k.clone();
            list = list.child(
                target_row(k, &info, c, self.perms.pending_bits(k).is_some(), f.locked(k), *k == target, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.perms.selected = key.clone();
                        cx.notify();
                    })),
            );
        }
        if targets.is_empty() {
            list = list.child(div().px_2().py_1p5().child(Label::new("No role or member has its own permissions here yet.").size(12.).line_height(12. * 1.55).color(theme.color_text_muted)));
        }
        let mut aside = div()
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
                    .relative()
                    .child(self.perms.add_button.probe())
                    .child(SectionLabel::new("Roles and members"))
                    .when(f.can_edit, |this| {
                        let (muted, primary, hover) = (theme.color_text_muted, theme.color_text_primary, theme.color_surface_hover);
                        this.child(
                            div()
                                .id("perm-add-target")
                                .group("perm-add-target")
                                .flex()
                                .size_7()
                                .items_center()
                                .justify_center()
                                .rounded(theme.radius_sm)
                                .cursor_pointer()
                                .hover(move |s| s.bg(hover))
                                .tooltip(decibell_ui::title_tooltip("Add a role or member"))
                                .child(Icon::new(IconName::SettingsPlus).size(px(15.)).stroke_width(2.25).color(muted).group_hover("perm-add-target", primary))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_add_target(window, cx))),
                        )
                    }),
            )
            .child(list);
        if !everyone_key.is_empty() {
            let info = f.describe(&everyone_key, &self.perms.picked_names, cx);
            let c = counts(self.bits_of(&f, &everyone_key));
            let key = everyone_key.clone();
            aside = aside.child(div().mx_2().my_2().h(px(1.)).bg(theme.color_border_divider)).child(
                target_row(&everyone_key, &info, c, self.perms.pending_bits(&everyone_key).is_some(), false, target == everyone_key, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.perms.selected = key.clone();
                        cx.notify();
                    },
                )),
            );
        }
        let theme = cx.theme();
        aside = aside.child(
            div().mt_4().px_2().child(
                Label::new("Overrides stack: @everyone first, then roles, then the member. When one role allows and another denies, allow wins.")
                    .size(12.)
                    .line_height(12. * 1.55)
                    .color(theme.color_text_muted),
            ),
        );

        // ── editor ──
        let t = f.describe(&target, &self.perms.picked_names, cx);
        let current = self.bits_of(&f, &target);
        let (allowed, denied) = counts(current);
        let is_everyone = target == everyone_key;
        let locked = f.locked(&target);
        let can_edit_target = f.can_edit && !locked;
        // Clearing a row changes every bit it sets, so you must hold them all.
        let can_clear = can_edit_target && has_bits(f.my_bits, current.allow) && has_bits(f.my_bits, current.deny);
        let tile_color = match &t.role {
            Some(r) if !r.is_default => role_color(r.color, cx),
            _ => theme.color_text_secondary,
        };
        let summary = if allowed + denied == 0 {
            "follows the server roles".to_string()
        } else {
            [(allowed > 0).then(|| format!("{allowed} allowed")), (denied > 0).then(|| format!("{denied} denied"))].into_iter().flatten().collect::<Vec<_>>().join(", ")
        };
        let kind_text = if is_everyone { "Every member · applies first" } else if t.member.is_some() { "Member" } else { "Role" };
        let show_clear = can_clear && (allowed + denied > 0 || (!is_everyone && self.perms.added.contains(&target)));
        let header = div()
            .flex()
            .items_center()
            .gap_4()
            .pb_7()
            .child(match t.member {
                Some(uid) => UserAvatar::new(uid, t.username.clone(), 48.).into_any_element(),
                None => div()
                    .flex()
                    .size_12()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_lg)
                    .bg(tile_color.opacity(0.16))
                    .child(Icon::new(if is_everyone { IconName::SettingsUsers } else { IconName::SettingsShield }).size(px(22.)).color(tile_color))
                    .into_any_element(),
            })
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(crate::ui::editor_title(t.name.clone(), theme.color_text_primary))
                            .when(!t.sub.is_empty(), |this| this.child(Label::new(t.sub.clone()).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted).nowrap())),
                    )
                    .child(crate::ui::meta(format!("{kind_text} · {summary}"), cx)),
            )
            .when(show_clear, |this| {
                let theme = cx.theme();
                let (muted, primary, hover) = (theme.color_text_muted, theme.color_text_primary, theme.color_surface_hover);
                let key = target.clone();
                this.child(
                    div()
                        .id("perm-clear-target")
                        .group("perm-clear-target")
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1p5()
                        .rounded(theme.radius_sm)
                        .px_3()
                        .py_2()
                        .text_color(muted)
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover).text_color(primary))
                        .child(Icon::new(if is_everyone { IconName::SettingsRefresh } else { IconName::SettingsX }).size(px(14.)).color(muted).group_hover("perm-clear-target", primary))
                        .child(Label::new(if is_everyone { "Reset all" } else { "Remove" }).size(13.).weight(600.))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let Some(channel) = this.channel(cx) else { return };
                            let f = this.facts(&channel, cx);
                            this.stage(&f, &key, Bits::default());
                            if key != f.everyone_key {
                                this.perms.added.retain(|k| *k != key);
                                if !f.overwrites.iter().any(|o| overwrite_key(o) == key) {
                                    this.perms.selected = f.everyone_key.clone();
                                }
                            }
                            cx.notify();
                        })),
                )
            });
        let mut editor = div().min_w_0().flex_1().child(header);
        let mut groups = div().flex().flex_col().gap_7();
        if locked && f.can_edit {
            groups = groups.child(notice(
                IconName::SettingsLock,
                "This role is at or above your highest role. Only the owner or a higher role can change its permissions here.",
                cx,
            ));
        }
        for group in groups_for(channel.kind) {
            let rows: Vec<_> = visible.iter().filter(|p| p.group == *group).collect();
            if rows.is_empty() {
                continue;
            }
            let theme = cx.theme();
            let mut card = div().flex().flex_col().overflow_hidden().rounded(theme.radius_lg).border_1().border_color(theme.color_border_divider).bg(theme.color_bg_light);
            let n = rows.len();
            for (i, p) in rows.into_iter().enumerate() {
                let editable = can_edit_target && has_bits(f.my_bits, p.bit);
                let title = (!editable && can_edit_target).then_some("You can only change permissions you hold in this channel.");
                let theme = cx.theme();
                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .px_4()
                        .py_3()
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .child(Label::new(p.label).size(14.).weight(500.).color(theme.color_text_primary))
                                .child(div().mt_0p5().child(Label::new(p.description).size(12.).line_height(12. * 1.55).color(theme.color_text_muted))),
                        )
                        .child(self.tri_state(p.bit, state_of(current, p.bit), !editable, title, &target, cx)),
                );
                if i + 1 < n {
                    card = card.child(div().h(px(1.)).bg(cx.theme().color_border_divider));
                }
            }
            groups = groups.child(div().child(div().mb_2p5().child(SectionLabel::new(group_label(*group)))).child(card));
        }
        editor = editor.child(groups);
        let picker = self.perms.picker.as_ref().map(|p| self.render_add_target(p, &f, &targets, cx));
        out = out.child(div().flex().items_start().gap_8().child(aside).child(editor));
        div().child(out).children(picker).into_any_element()
    }

    /// Deny / inherit / allow as one segmented control (`TriStateControl`).
    fn tri_state(&self, bit: u64, value: TriState, disabled: bool, title: Option<&'static str>, target: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let states: [(TriState, IconName, &str, Hsla, Hsla, Hsla); 3] = [
            (TriState::Deny, IconName::SettingsX, "Deny", theme.color_error.opacity(0.15), theme.color_error, theme.color_error.opacity(0.4)),
            (TriState::Inherit, IconName::SettingsSlash, "Inherit", theme.color_bg_lighter, theme.color_text_primary, theme.color_border),
            (TriState::Allow, IconName::SettingsCheck, "Allow", theme.color_success.opacity(0.15), theme.color_success, theme.color_success.opacity(0.4)),
        ];
        let mut group = div()
            .id(SharedString::from(format!("tri-{bit}")))
            .flex()
            .flex_none()
            .gap_0p5()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_dark)
            .p_0p5()
            .when(disabled, |this| this.opacity(0.5))
            .when_some(title, |this, t| this.tooltip(decibell_ui::title_tooltip(t)));
        for (state, icon, label, bg, fg, ring) in states {
            let on = value == state;
            let (muted, hover, primary) = (theme.color_text_muted, theme.color_surface_hover, theme.color_text_primary);
            let target = target.to_string();
            let gname: SharedString = format!("tri-{bit}-{label}").into();
            group = group.child(
                div()
                    .id(SharedString::from(format!("tri-{bit}-{label}")))
                    .group(gname.clone())
                    .flex()
                    .h_7()
                    .w_8()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_sm)
                    .map(|this| if on { this.bg(bg).border_1().border_color(ring) } else { this })
                    .when(!on && !disabled, |this| this.hover(move |s| s.bg(hover)))
                    .when(title.is_none(), |this| this.tooltip(decibell_ui::title_tooltip(label)))
                    .child(
                        Icon::new(icon)
                            .size(px(14.))
                            .stroke_width(2.75)
                            .color(if on { fg } else { muted })
                            .when(!on && !disabled, |i| i.group_hover(gname.clone(), primary)),
                    )
                    .when(!disabled, |this| {
                        this.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let Some(channel) = this.channel(cx) else { return };
                            let f = this.facts(&channel, cx);
                            if !f.can_edit || f.locked(&target) {
                                return;
                            }
                            let current = this.bits_of(&f, &target);
                            let next = Bits {
                                allow: toggle_bit(current.allow, bit, state == TriState::Allow),
                                deny: toggle_bit(current.deny, bit, state == TriState::Deny),
                            };
                            this.stage(&f, &target, next);
                            cx.notify();
                        }))
                    }),
            );
        }
        group.into_any_element()
    }

    /// The + picker: roles below yours, and members found on the server (MEMBER_SEARCH, not
    /// just the loaded roster).
    fn render_add_target(&self, p: &AddTarget, f: &Facts, targets: &[String], cx: &mut Context<Self>) -> AnyElement {
        let mut taken: Vec<String> = targets.to_vec();
        taken.push(f.everyone_key.clone());
        let q = p.query.text(cx).trim().to_lowercase();
        let mut roles: Vec<Role> = f
            .roles
            .iter()
            .filter(|r| !r.is_default && r.position < f.my_level && !taken.contains(&format!("role:{}", r.id)) && (q.is_empty() || r.name.to_lowercase().contains(&q)))
            .cloned()
            .collect();
        roles.sort_by_key(|r| std::cmp::Reverse(r.position));
        let found = p.search.read(cx);
        let members: Vec<Member> = found.members.iter().filter(|m| !m.is_owner && !taken.contains(&format!("member:{}", m.uid))).cloned().collect();
        let (loaded, has_more, loading) = (found.loaded, found.has_more, found.loading);
        let theme = cx.theme();
        let hover = theme.color_surface_hover;
        let mut list = div().id("perm-target-list").flex().flex_col().max_h(px(288.)).overflow_y_scroll();
        let everyone = f.everyone_key.clone();
        let targets_owned: Vec<String> = targets.to_vec();
        if !roles.is_empty() {
            list = list.child(div().px_2().pb_1p5().pt_2().child(SectionLabel::new("Roles")));
            for r in roles.iter() {
                let key = format!("role:{}", r.id);
                let (everyone, targets_owned) = (everyone.clone(), targets_owned.clone());
                list = list.child(
                    div()
                        .id(("perm-target-role", r.id as u64))
                        .flex()
                        .w_full()
                        .items_center()
                        .gap_2p5()
                        .rounded(theme.radius_md)
                        .px_2()
                        .py_1p5()
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .child(div().flex().size_6().flex_none().items_center().justify_center().child(dot(role_color(r.color, cx), 10.)))
                        .child(div().min_w_0().flex_1().child(Label::new(r.name.clone()).size(13.).weight(500.).color(theme.color_text_primary).truncate()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.add_target(&targets_owned, &everyone, key.clone(), None, cx))),
                );
            }
        }
        if !members.is_empty() {
            list = list.child(div().px_2().pb_1p5().pt_3().child(SectionLabel::new("Members")));
            for m in members.iter() {
                let name = user_name(m.uid, &m.username, cx);
                let primary: SharedString = if m.nickname.is_empty() { name.clone() } else { m.nickname.clone() };
                let key = format!("member:{}", m.uid);
                let (everyone, targets_owned, picked) = (everyone.clone(), targets_owned.clone(), name.clone());
                list = list.child(
                    div()
                        .id(("perm-target-member", m.uid as u64))
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
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.add_target(&targets_owned, &everyone, key.clone(), Some(picked.clone()), cx))),
                );
            }
        }
        if has_more {
            let search = p.search.clone();
            let (muted, primary) = (theme.color_text_muted, theme.color_text_primary);
            list = list.child(
                div()
                    .id("perm-target-more")
                    .rounded(theme.radius_md)
                    .px_2()
                    .py_1p5()
                    .text_color(muted)
                    .when(loading, |this| this.opacity(0.6))
                    .when(!loading, |this| this.cursor_pointer().hover(move |s| s.bg(hover).text_color(primary)).on_click(move |_, _, cx| search.update(cx, |s, cx| s.load_more(cx))))
                    .child(Label::new(if loading { "Loading…" } else { "Show more" }).size(12.).weight(500.)),
            );
        }
        if loaded && roles.is_empty() && members.is_empty() && !has_more {
            list = list.child(
                div().px_2().py_4().flex().justify_center().child(
                    Label::new(if q.is_empty() { "Every role and member already has a row." } else { "No role or member matches." }).size(12.).color(theme.color_text_muted),
                ),
            );
        }
        let b = self.perms.add_button.get();
        let anchor = if b.size.width > px(0.) { point(b.left(), b.bottom() + px(4.)) } else { p.anchor };
        Popover::new("perm-add-target-popover", anchor)
            .on_dismiss(cx.listener(|this, _: &DismissEvent, _, cx| {
                this.perms.picker = None;
                cx.notify();
            }))
            .child(div().flex().flex_col().w(px(288.)).p_2().child(p.query.field("perm-target-search")).child(div().mt_2().child(list)))
            .into_any_element()
    }
}

/// A target in the list (`TargetRow`): avatar or role dot, name, lock, unsaved dot, counts.
fn target_row(key: &str, info: &Info, counts: (usize, usize), unsaved: bool, locked: bool, selected: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    let (bg, fg, weight) = if selected { (Some(theme.color_surface_active), theme.color_text_primary, 600.) } else { (None, theme.color_text_secondary, 500.) };
    let (hover_bg, hover_fg) = (theme.color_surface_hover, theme.color_text_primary);
    div()
        .id(SharedString::from(format!("perm-target-{key}")))
        .flex()
        .h_9()
        .w_full()
        .items_center()
        .gap_2p5()
        .rounded(theme.radius_md)
        .px_2p5()
        .cursor_pointer()
        .when_some(bg, |this, bg| this.bg(bg))
        .text_color(fg)
        .when(!selected, |this| this.hover(move |s| s.bg(hover_bg).text_color(hover_fg)))
        .child(match info.member {
            Some(uid) => UserAvatar::new(uid, info.username.clone(), 18.).into_any_element(),
            None => div()
                .flex()
                .size(px(18.))
                .flex_none()
                .items_center()
                .justify_center()
                .child(dot(
                    match &info.role {
                        Some(r) if !r.is_default => role_color(r.color, cx),
                        _ => theme.color_text_muted,
                    },
                    10.,
                ))
                .into_any_element(),
        })
        .child(div().min_w_0().flex_1().child(Label::new(info.name.clone()).size(13.).weight(weight).truncate()))
        .when(locked, |this| this.child(Icon::new(IconName::SettingsLock).size(px(12.)).color(theme.color_text_muted)))
        .when(unsaved, |this| this.child(div().id("unsaved").size_1p5().flex_none().rounded_full().bg(theme.color_accent).tooltip(decibell_ui::title_tooltip("Unsaved changes"))))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1p5()
                .when(counts.0 > 0, |this| {
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_0p5()
                            .child(Icon::new(IconName::SettingsCheck).size(px(11.)).stroke_width(3.).color(theme.color_success))
                            .child(Label::new(counts.0.to_string()).font(FontRole::Meta).size(theme.scaled.text_meta).weight(500.).color(theme.color_success)),
                    )
                })
                .when(counts.1 > 0, |this| {
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_0p5()
                            .child(Icon::new(IconName::SettingsX).size(px(11.)).stroke_width(3.).color(theme.color_error))
                            .child(Label::new(counts.1.to_string()).font(FontRole::Meta).size(theme.scaled.text_meta).weight(500.).color(theme.color_error)),
                    )
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tri_states_read_deny_first() {
        let b = Bits { allow: perm::SEND_MESSAGES | perm::VIEW_CHANNEL, deny: perm::SEND_MESSAGES };
        assert_eq!(state_of(b, perm::SEND_MESSAGES), TriState::Deny);
        assert_eq!(state_of(b, perm::VIEW_CHANNEL), TriState::Allow);
        assert_eq!(state_of(b, perm::SPEAK), TriState::Inherit);
    }

    #[test]
    fn channel_types_list_their_groups() {
        assert_eq!(groups_for(ChannelKind::Text), &[PermissionGroup::General, PermissionGroup::Text]);
        assert_eq!(groups_for(ChannelKind::Voice), &[PermissionGroup::General, PermissionGroup::Voice]);
        assert_eq!(groups_for(ChannelKind::Category).len(), 3);
    }

    #[test]
    fn a_settled_draft_drops_what_the_server_holds() {
        let mut d = PermDraft::default();
        d.pending.push(("role:3".into(), Bits { allow: 0, deny: perm::SEND_MESSAGES }));
        d.pending.push(("member:9".into(), Bits { allow: perm::STREAM, deny: 0 }));
        d.added.push("member:9".into());
        d.sync_draft = Some(true);
        let stored = vec![Overwrite { target_type: OverwriteTarget::Role, target_id: "3".into(), target_uid: 0, allow: 0, deny: perm::SEND_MESSAGES }];
        d.settle(&stored, true);
        assert_eq!(d.pending.len(), 1, "the role row is stored now");
        assert_eq!(d.pending[0].0, "member:9");
        assert_eq!(d.added, vec!["member:9".to_string()]);
        assert_eq!(d.sync_draft, None);
    }
}
