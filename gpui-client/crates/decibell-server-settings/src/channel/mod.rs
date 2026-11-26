//! Channel and category settings (`ChannelSettingsModal.tsx` + `channelSettings/`), in the
//! shared settings frame. Overview, Retention and Permissions edits are one draft (the settings
//! part and the permission part, which a different call and permission send); the save bar
//! commits all of it, and leaving with unsaved changes shakes the bar instead of dropping them
//! (owner decision 2026-10-14). Delete sits at the sidebar's foot, like Leave Server.

pub mod overview;
pub mod permissions;
pub mod retention;

use decibell_common::Watch;
use decibell_core::events::CoreEvent;
use decibell_model::save_echo::EchoSave;
use decibell_model::stores::community::{Channel, ChannelKind};
use decibell_model::stores::permissions::perm;
use decibell_model::{ChannelId, Model, ServerId};
use decibell_text::input::{Enter, InputEvent, InputState};
use decibell_ui::{
    ActiveTheme as _, CloseRequest, ConfirmModal, ConfirmTone, Icon, IconName, Label, NavItem, SaveBar, ScrollHandle,
    SettingsWindow,
};
use gpui::{AnyElement, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*, px};

use crate::requests::{self, Request};
use crate::server::overview::toast_error;
use crate::ui::error_banner;
use permissions::PermDraft;

pub const CHANNEL_NAME_MAX_BYTES: usize = 64;
/// The server's cap on a channel topic, in UTF-8 bytes.
pub const TOPIC_MAX_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Retention,
    Permissions,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Retention => "Retention",
            Tab::Permissions => "Permissions",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Tab::Overview => IconName::SettingsSliders,
            Tab::Retention => IconName::SettingsClock,
            Tab::Permissions => IconName::SettingsShield,
        }
    }
}

pub fn channel_icon(kind: ChannelKind) -> IconName {
    match kind {
        ChannelKind::Voice => IconName::SettingsSpeaker,
        ChannelKind::Category => IconName::SettingsFolder,
        _ => IconName::SettingsHash,
    }
}

fn type_label(kind: ChannelKind) -> &'static str {
    match kind {
        ChannelKind::Text => "Text channel",
        ChannelKind::Voice => "Voice channel",
        ChannelKind::Category => "Category",
        ChannelKind::Unknown => "Channel",
    }
}

/// Everything the Overview and Retention tabs edit, held as one draft until the save bar
/// commits it (`draft.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelDraft {
    pub name: String,
    pub topic: String,
    pub slowmode: i32,
    /// kbps; 0 = the client default.
    pub bitrate: i32,
    /// Text, image, video, document, audio.
    pub retention: [i32; 5],
}

impl ChannelDraft {
    pub fn of(c: &Channel) -> ChannelDraft {
        ChannelDraft {
            name: c.name.to_string(),
            topic: c.topic.to_string(),
            slowmode: c.slowmode_seconds,
            bitrate: c.voice_bitrate_kbps,
            retention: [
                c.retention_days_text.max(0),
                c.retention_days_image.max(0),
                c.retention_days_video.max(0),
                c.retention_days_document.max(0),
                c.retention_days_audio.max(0),
            ],
        }
    }
}

/// Which parts of a draft differ from the channel as stored. Only the fields the channel's type
/// shows count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    pub name: bool,
    pub topic: bool,
    pub slowmode: bool,
    pub bitrate: bool,
    pub retention: bool,
}

impl Changes {
    pub fn of(c: &Channel, d: &ChannelDraft) -> Changes {
        let s = ChannelDraft::of(c);
        let text = c.kind == ChannelKind::Text;
        Changes {
            name: !d.name.trim().is_empty() && d.name.trim() != s.name,
            topic: text && d.topic.trim() != s.topic,
            slowmode: text && d.slowmode != s.slowmode,
            bitrate: c.kind == ChannelKind::Voice && d.bitrate != s.bitrate,
            retention: text && d.retention != s.retention,
        }
    }

    pub fn any(self) -> bool {
        self.name || self.topic || self.slowmode || self.bitrate || self.retention
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Wipe,
    Delete,
}

/// Send requests one after another (each waits for the previous to be sent: the renderer's
/// `await invoke` chain); `done` gets the first failure.
pub fn send_in_order(mut requests: Vec<Request>, cx: &mut App, done: impl FnOnce(Result<(), decibell_model::UiError>, &mut App) + 'static) {
    if requests.is_empty() {
        cx.defer(move |cx| done(Ok(()), cx));
        return;
    }
    let first = requests.remove(0);
    requests::send(first, cx, move |result, cx| match result {
        Ok(()) => send_in_order(requests, cx, done),
        Err(e) => done(Err(e), cx),
    });
}

pub struct ChannelSettingsWindow {
    pub(crate) server: ServerId,
    pub(crate) channel_id: ChannelId,
    open: bool,
    tab: Tab,
    pub(crate) draft: Option<ChannelDraft>,
    pub(crate) name: Entity<InputState>,
    pub(crate) topic: Entity<InputState>,
    syncing: bool,
    pub(crate) perms: PermDraft,
    scroll: ScrollHandle,
    nudge: u32,
    error: Option<SharedString>,
    confirm: Option<Confirm>,
    working: bool,
    save: EchoSave<()>,
    _watch: Watch,
    _subs: Vec<Subscription>,
}

impl ChannelSettingsWindow {
    pub fn new(server: ServerId, channel_id: ChannelId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.auth, cx).store(&model.servers, cx).people(cx, |_, _| true);
        let mut subs = Vec::new();
        if let Some(community) = model.community(server, cx) {
            watch.store(&community.meta, cx).store(&community.roles, cx).store(&community.roster, cx);
            subs.push(cx.observe(&community.channels, |this: &mut Self, _, cx| {
                this.on_channels(cx);
                cx.notify();
            }));
        }
        let stored = model.community(server, cx).and_then(|c| c.channels.read(cx).get(&channel_id).cloned());
        let name = cx.new(|cx| {
            InputState::single_line(window, cx)
                .max_length(64)
                .max_bytes(CHANNEL_NAME_MAX_BYTES)
                .with_value(stored.as_ref().map_or("", |c| &c.name))
        });
        // One line: Enter would only become a space.
        let topic = cx.new(|cx| {
            InputState::multi_line(window, cx)
                .max_bytes(TOPIC_MAX_BYTES)
                .enter(Enter::Submit)
                .placeholder("Let everyone know what this channel is for")
                .with_value(stored.as_ref().map_or("", |c| &c.topic))
        });
        for input in [&name, &topic] {
            subs.push(cx.subscribe(input, |this: &mut Self, _, e: &InputEvent, cx| {
                if *e == InputEvent::Change && !this.syncing {
                    let name = this.name.read(cx).value().to_string();
                    let topic = this.topic.read(cx).value().to_string();
                    this.change(cx, |d| {
                        d.name = name;
                        d.topic = topic;
                    });
                }
            }));
        }
        // A refusal ends the wait at once and leaves the draft. Renames and overwrites answer with
        // `channel_action_responded` (toasted by the router); the settings update answers with
        // `channel_updated`, which nothing else surfaces.
        let channel = channel_id.clone();
        subs.push(cx.subscribe(&model.answers, move |this: &mut Self, _, event: &CoreEvent, cx| match event {
            CoreEvent::ChannelActionResponded(p) if ServerId::from_wire(&p.server_id) == server && !p.success => {
                this.save.stop();
                cx.notify();
            }
            CoreEvent::ChannelUpdated(p) if ServerId::from_wire(&p.server_id) == server && !p.success => {
                this.error = Some(if p.message.is_empty() { "Couldn't save the channel settings.".into() } else { p.message.clone().into() });
                this.save.stop();
                cx.notify();
            }
            // CHANNEL_WIPE_RES → toast. The CHANNEL_WIPED broadcast that follows clears the
            // local messages.
            CoreEvent::ChannelWipeResponded(p) if ServerId::from_wire(&p.server_id) == server && p.channel_id == *channel => {
                this.working = false;
                let regional = decibell_model::regional::Regional::get();
                if p.success {
                    let body = format!(
                        "Removed {} message(s) and {} attachment(s).",
                        regional.group_signed(p.deleted_message_count),
                        regional.group_signed(p.deleted_attachment_count)
                    );
                    let model = Model::global(cx).clone();
                    model.toasts.update(cx, |t, cx| t.success("Channel history wiped", Some(body.into()), cx));
                    this.confirm = None;
                } else {
                    toast_error("Wipe failed", if p.message.is_empty() { "Unknown error.".to_string() } else { p.message.clone() }, cx);
                }
                cx.notify();
            }
            _ => {}
        }));
        let can_manage = model.channel_permission(server, &channel_id, perm::MANAGE_CHANNELS, cx);
        let can_roles = model.channel_permission(server, &channel_id, perm::MANAGE_ROLES, cx);
        let tab = if can_manage || !can_roles { Tab::Overview } else { Tab::Permissions };
        let perms = PermDraft::default();
        requests::fire(Request::ListOverwrites { server, channel: channel_id.clone() }, cx);
        ChannelSettingsWindow {
            server,
            channel_id,
            open: true,
            tab,
            draft: None,
            name,
            topic,
            syncing: false,
            perms,
            scroll: ScrollHandle::new(),
            nudge: 0,
            error: None,
            confirm: None,
            working: false,
            save: EchoSave::default(),
            _watch: watch,
            _subs: subs,
        }
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.open != open {
            self.open = open;
            cx.notify();
        }
    }

    pub fn channel(&self, cx: &App) -> Option<Channel> {
        Model::global(cx).community(self.server, cx).and_then(|c| c.channels.read(cx).get(&self.channel_id).cloned())
    }

    pub fn can_manage(&self, cx: &App) -> bool {
        Model::global(cx).channel_permission(self.server, &self.channel_id, perm::MANAGE_CHANNELS, cx)
    }

    pub fn can_roles(&self, cx: &App) -> bool {
        Model::global(cx).channel_permission(self.server, &self.channel_id, perm::MANAGE_ROLES, cx)
    }

    pub fn fields(&self, cx: &App) -> Option<ChannelDraft> {
        self.draft.clone().or_else(|| self.channel(cx).map(|c| ChannelDraft::of(&c)))
    }

    pub fn changes(&self, cx: &App) -> Changes {
        match (self.channel(cx), self.fields(cx)) {
            (Some(c), Some(d)) => Changes::of(&c, &d),
            _ => Changes::default(),
        }
    }

    fn settings_dirty(&self, cx: &App) -> bool {
        self.changes(cx).any() && self.can_manage(cx)
    }

    pub fn dirty(&self, cx: &App) -> bool {
        self.settings_dirty(cx) || self.perms.dirty(self, cx)
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn nudges(&self) -> u32 {
        self.nudge
    }

    pub fn saving(&self) -> bool {
        self.save.saving()
    }

    pub(crate) fn change(&mut self, cx: &mut Context<Self>, patch: impl FnOnce(&mut ChannelDraft)) {
        let Some(mut d) = self.fields(cx) else { return };
        patch(&mut d);
        self.draft = Some(d);
        cx.notify();
    }

    /// The store changed: the permission draft drops what the server now holds, and a save
    /// settles once nothing differs.
    fn on_channels(&mut self, cx: &mut Context<Self>) {
        let channel = self.channel(cx);
        if let Some(c) = &channel {
            let overwrites = self.overwrites(cx);
            self.perms.settle(&overwrites, c.category_sync);
        }
        let clean = !self.changes(cx).any() && !self.perms.dirty(self, cx);
        if self.save.settle(|_| clean).is_some() {
            self.draft = None;
        }
        if self.draft.is_none()
            && let Some(c) = channel
        {
            self.sync_inputs(&c.name, &c.topic, cx);
        }
    }

    fn sync_inputs(&mut self, name: &str, topic: &str, cx: &mut Context<Self>) {
        self.syncing = true;
        if self.name.read(cx).value() != name {
            self.name.update(cx, |s, cx| s.set_value(name, cx));
        }
        if self.topic.read(cx).value() != topic {
            self.topic.update(cx, |s, cx| s.set_value(topic, cx));
        }
        self.syncing = false;
    }

    pub(crate) fn overwrites(&self, cx: &App) -> Vec<decibell_model::stores::community::Overwrite> {
        Model::global(cx)
            .community(self.server, cx)
            .and_then(|c| c.channels.read(cx).overwrites(&self.channel_id).map(|o| o.to_vec()))
            .unwrap_or_default()
    }

    /// Close, unless there are unsaved changes: then shake the save bar.
    pub fn request_close(&mut self, cx: &mut Context<Self>) {
        if self.dirty(cx) {
            self.nudge += 1;
            cx.notify();
        } else {
            let overlays = Model::global(cx).overlays.clone();
            overlays.update(cx, |o, cx| o.close_modal(cx));
        }
    }

    pub fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        // Tabs share one draft: switching keeps it (no guard needed).
        self.tab = tab;
        self.scroll.scroll_to_top();
        cx.notify();
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.draft = None;
        self.perms.reset();
        self.error = None;
        if let Some(c) = self.channel(cx) {
            self.sync_inputs(&c.name, &c.topic, cx);
        }
        cx.notify();
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        let (Some(channel), Some(fields)) = (self.channel(cx), self.fields(cx)) else { return };
        let changes = Changes::of(&channel, &fields);
        let settings_dirty = changes.any() && self.can_manage(cx);
        let perms_dirty = self.perms.dirty(self, cx);
        if !(settings_dirty || perms_dirty) || self.save.saving() {
            return;
        }
        self.save.start((), cx, |this| &mut this.save);
        self.error = None;
        let (server, id) = (self.server, self.channel_id.clone());
        let mut out = Vec::new();
        if settings_dirty && changes.name {
            out.push(Request::RenameChannel { server, channel: id.clone(), name: fields.name.trim().to_string() });
        }
        if settings_dirty && (changes.retention || changes.topic || changes.bitrate || changes.slowmode) {
            out.push(Request::UpdateChannel {
                server,
                channel: id.clone(),
                retention: fields.retention,
                // Explicit presence on the wire: none = leave unchanged.
                voice_bitrate_kbps: (channel.kind == ChannelKind::Voice).then_some(fields.bitrate),
                slowmode_seconds: changes.slowmode.then_some(fields.slowmode),
                topic: changes.topic.then(|| fields.topic.trim().to_string()),
            });
        }
        if perms_dirty {
            out.extend(self.perms.commit(self, &channel, cx));
        }
        let weak = cx.weak_entity();
        send_in_order(out, cx, move |result, cx| {
            if let Err(e) = result {
                let _ = weak.update(cx, |this, cx| {
                    this.error = Some(e.js_string());
                    this.save.stop();
                    cx.notify();
                });
            }
        });
    }

    #[cfg(feature = "fixtures")]
    /// Open the wipe or delete confirmation (the Retention tab's button, the nav's Delete).
    pub(crate) fn ask(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        self.confirm = Some(confirm);
        cx.notify();
    }

    #[cfg(feature = "fixtures")]
    pub(crate) fn asking(&self) -> Option<Confirm> {
        self.confirm
    }

    pub(crate) fn run_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.confirm else { return };
        if self.working {
            return;
        }
        self.working = true;
        cx.notify();
        let (server, channel) = (self.server, self.channel_id.clone());
        let weak = cx.weak_entity();
        match confirm {
            Confirm::Wipe => {
                // `channel_wipe_responded` resets `working` and closes the dialog.
                requests::send(Request::WipeChannel { server, channel }, cx, move |result, cx| {
                    if let Err(e) = result {
                        let _ = weak.update(cx, |this, cx| {
                            this.working = false;
                            cx.notify();
                        });
                        toast_error("Wipe failed", e.js_string(), cx);
                    }
                });
            }
            Confirm::Delete => {
                // Success is the channel list push; denials come back as the router's
                // `channel_action_responded` toast.
                requests::send(Request::DeleteChannel { server, channel }, cx, move |result, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.working = false;
                        if result.is_ok() {
                            this.confirm = None;
                            let overlays = Model::global(cx).overlays.clone();
                            overlays.update(cx, |o, cx| o.close_modal(cx));
                        }
                        cx.notify();
                    });
                    if let Err(e) = result {
                        toast_error("Delete failed", e.js_string(), cx);
                    }
                });
            }
        }
    }
}

impl Render for ChannelSettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(channel) = self.channel(cx) else { return div() };
        let model = Model::global(cx).clone();
        let can_manage = self.can_manage(cx);
        let can_roles = self.can_roles(cx);
        let is_text = channel.kind == ChannelKind::Text;
        // The landing channel after auth must always exist: the server refuses to delete the
        // last text channel, so don't offer it.
        let last_text = is_text
            && model.community(self.server, cx).is_some_and(|c| c.channels.read(cx).list().iter().filter(|c| c.kind == ChannelKind::Text).count() <= 1);
        let changes = self.changes(cx);
        let perms_dirty = self.perms.dirty(self, cx);
        let dirty = (changes.any() && can_manage) || perms_dirty;
        // A nudge belongs to the changes it guarded; the next ones start calm.
        if !dirty {
            self.nudge = 0;
        }
        let mut tabs = vec![Tab::Overview];
        if is_text {
            tabs.push(Tab::Retention);
        }
        if can_manage || can_roles {
            tabs.push(Tab::Permissions);
        }
        let active = if tabs.contains(&self.tab) { self.tab } else { Tab::Overview };
        let tab_dirty = |t: Tab| match t {
            Tab::Overview => can_manage && (changes.name || changes.topic || changes.slowmode || changes.bitrate),
            Tab::Retention => can_manage && changes.retention,
            Tab::Permissions => perms_dirty,
        };
        let server_name: SharedString = model
            .community(self.server, cx)
            .map(|c| c.meta.read(cx).name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| model.servers.read(cx).get(self.server).map(|s| s.name.clone()))
            .unwrap_or_default();
        let prefix = if is_text { "#" } else { "" };
        let theme = cx.theme();
        let tile = || {
            div()
                .flex()
                .size_8()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .bg(theme.color_bg_light)
                .shadow(theme.shadow_raised.clone())
                .child(Icon::new(channel_icon(channel.kind)).size(px(15.)).color(theme.color_text_secondary))
        };
        let header = div()
            .mb_3()
            .flex()
            .items_center()
            .gap_2p5()
            .px_3()
            .child(tile())
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(div().min_w_0().child(Label::new(channel.name.clone()).size(14.).weight(600.).color(theme.color_text_primary).truncate()))
                            .when(channel.encrypted, |this| this.child(Icon::new(IconName::SettingsLock).size(px(12.)).color(theme.color_success))),
                    )
                    .child(
                        Label::new(if server_name.is_empty() {
                            type_label(channel.kind).to_string()
                        } else {
                            format!("{} · {}", type_label(channel.kind), server_name)
                        })
                        .size(12.)
                        .color(theme.color_text_muted)
                        .truncate(),
                    ),
            );
        let rail_header = div().mb_3().flex().justify_center().child(tile());
        let mut frame = SettingsWindow::new("channel-settings", self.open, &self.scroll)
            .nav_header(header, rail_header)
            .title(active.label())
            .context(format!("{prefix}{}", channel.name))
            .on_close(cx.listener(|this, _: &CloseRequest, _, cx| this.request_close(cx)));
        for t in tabs {
            frame = frame.item(
                NavItem::new(SharedString::from(format!("channel-tab-{}", t.label())), t.icon(), t.label())
                    .active(t == active)
                    .dot(tab_dirty(t))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.select_tab(t, cx))),
            );
        }
        if can_manage && !last_text {
            frame = frame.foot_item(
                NavItem::new("channel-delete", IconName::SettingsTrash, if channel.kind == ChannelKind::Category { "Delete Category" } else { "Delete Channel" })
                    .danger()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.confirm = Some(Confirm::Delete);
                        cx.notify();
                    })),
            );
        }
        let body: AnyElement = match active {
            Tab::Overview => self.overview_tab(&channel, can_manage, cx),
            Tab::Retention => self.retention_tab(&channel, can_manage, cx),
            Tab::Permissions => self.permissions_tab(&channel, window, cx),
        };
        let theme = cx.theme();
        let _ = theme;
        let content = div()
            .flex()
            .flex_col()
            .when_some(self.error.clone(), |this, error| this.child(div().mb_6().child(error_banner("channel-error", error, None, cx))))
            .child(body)
            .when(can_manage || can_roles, |this| {
                this.child(
                    SaveBar::new("channel-save", &self.scroll, dirty, self.nudge)
                        .saving(self.save.saving())
                        .nudge_message("Save or reset your changes before closing.")
                        .on_reset(cx.listener(|this, _: &ClickEvent, _, cx| this.reset(cx)))
                        .on_save(cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx))),
                )
            });
        let confirm = self.confirm;
        let (title, label, body_text) = match confirm {
            Some(Confirm::Wipe) => (
                format!("Wipe #{}?", channel.name),
                if self.working { "Wiping…" } else { "Wipe history" },
                "Every message and attachment in this channel is deleted for everyone. The channel, its settings and permissions stay. This can't be undone.".to_string(),
            ),
            _ => (
                format!("Delete {prefix}{}?", channel.name),
                if self.working {
                    "Deleting…"
                } else if channel.kind == ChannelKind::Category {
                    "Delete category"
                } else {
                    "Delete channel"
                },
                match channel.kind {
                    ChannelKind::Category => format!(
                        "Its channels stay and move up to the group above{}.",
                        if channel.category_sync { ", keeping the permissions they follow" } else { "" }
                    ),
                    ChannelKind::Voice => "The channel must be empty first. This can't be undone.".to_string(),
                    _ => "Every message and attachment in it goes too. This can't be undone.".to_string(),
                },
            ),
        };
        div().child(frame.child(content)).child(
            ConfirmModal::new("channel-confirm", confirm.is_some() && self.open, title, label)
                .tone(ConfirmTone::Danger)
                .message(body_text)
                .confirm_disabled(self.working)
                .on_confirm(cx.listener(|this, _: &ClickEvent, _, cx| this.run_confirm(cx)))
                .on_cancel(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if !this.working {
                        this.confirm = None;
                        cx.notify();
                    }
                })),
        )
    }
}
