//! Overview (`tabs/OverviewTab.tsx`): the server's identity (name and description, editable with
//! MANAGE_SERVER), public listing, the picture (through the picture editor), and ownership
//! transfer (owner only, typed confirmation). The identity draft follows the server until it is
//! edited, so someone else's change shows and never drops your typing; after Save it stays until
//! the server's meta push shows what was sent (`save_echo`).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use decibell_common::{BoundsCell, UserAvatar, Watch, user_name};
use decibell_core::events::CoreEvent;
use decibell_model::regional::Regional;
use decibell_model::save_echo::EchoSave;
use decibell_model::stores::community::Member;
use decibell_model::stores::permissions::perm;
use decibell_model::{Model, actions};
use decibell_picture_editor::{EditorOptions, PictureEditor, PictureEditorEvent, PictureSource};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{
    ActiveTheme as _, Button, ButtonVariant, ConfirmModal, ConfirmTone, DismissEvent, FontRole, Icon, IconName, Label,
    Popover, RowHeights, SaveBar, ScrollArea, Switch,
};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, ExternalPaths, Image, ImageFormat, ImageSource,
    IntoElement, ObjectFit, PathPromptOptions, Point, Pixels, Render, SharedString, Subscription, Task, Window, div,
    img, point, prelude::*, px,
};

use super::{SettingsTab, TabContext};
use crate::member_search::MemberSearch;
use crate::requests::{self, Request};
use crate::ui::{InputBox, SearchBox, caps_label, heading};

pub const SERVER_NAME_MAX_BYTES: usize = 64;
pub const SERVER_DESCRIPTION_MAX_BYTES: usize = 512;

/// `data:image/<type>;base64,<bytes>` → an image GPUI decodes when drawn.
pub fn decode_data_url(url: &str) -> Option<Image> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    let format = match mime {
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::Webp,
        "image/gif" => ImageFormat::Gif,
        _ => return None,
    };
    let bytes = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    Some(Image::from_bytes(format, bytes))
}

#[derive(Clone, PartialEq)]
struct Meta {
    name: String,
    description: String,
}

/// The new owner, searched for on the server (MEMBER_SEARCH_REQ): every member, whether the
/// roster has paged them in or not. Accounts only, and not yourself.
struct OwnerPicker {
    search: Entity<MemberSearch>,
    query: SearchBox,
    anchor: Point<Pixels>,
    scroll: decibell_ui::ScrollHandle,
    _observe: Subscription,
}

pub struct OverviewTab {
    ctx: TabContext,
    name: Entity<InputState>,
    description: Entity<InputState>,
    /// Edited (name, description); `None` follows the server.
    draft: Option<Meta>,
    syncing: bool,
    meta_save: EchoSave<Meta>,
    public_save: EchoSave<bool>,
    /// The picture as central sent it, decoded once per version.
    picture: Option<(SharedString, Option<Arc<Image>>)>,
    asked: Option<SharedString>,
    /// The picture just saved, shown until the tile's copy arrives (with the data it replaces).
    just_saved: Option<(Arc<Image>, Option<SharedString>)>,
    editor: Option<Entity<PictureEditor>>,
    _editor_events: Option<Subscription>,
    _picture_wait: Option<Task<()>>,
    _picture_answer: Option<Subscription>,
    confirm_remove: bool,
    transfer_target: Option<Member>,
    transfer_confirm: Entity<InputState>,
    transferring: bool,
    owner_picker: Option<OwnerPicker>,
    picker_trigger: BoundsCell,
    _watch: Watch,
    _subs: Vec<Subscription>,
}

impl SettingsTab for OverviewTab {
    fn dirty(&self, cx: &App) -> bool {
        self.meta_dirty(cx)
    }
}

impl OverviewTab {
    pub fn new(ctx: TabContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let server = ctx.server;
        let mut watch = Watch::new();
        watch.store(&model.servers, cx).store(&model.auth, cx).people(cx, |_, _| true);
        if let Some(community) = model.community(server, cx) {
            watch.store(&community.roles, cx);
            // Our own roles decide what may be edited.
            watch.store(&community.roster, cx);
        }
        let (server_name, server_description) = Self::stored_meta(server, cx);
        let name = cx.new(|cx| InputState::single_line(window, cx).max_length(64).max_bytes(SERVER_NAME_MAX_BYTES).with_value(&server_name));
        let description = cx.new(|cx| {
            InputState::multi_line(window, cx).max_length(512).max_bytes(SERVER_DESCRIPTION_MAX_BYTES).with_value(&server_description)
        });
        let transfer_confirm = cx.new(|cx| InputState::single_line(window, cx));
        let mut subs = Vec::new();
        for input in [&name, &description] {
            subs.push(cx.subscribe(input, |this: &mut Self, _, event: &InputEvent, cx| {
                if *event == InputEvent::Change && !this.syncing {
                    this.draft = Some(Meta {
                        name: this.name.read(cx).value().to_string(),
                        description: this.description.read(cx).value().to_string(),
                    });
                    cx.notify();
                }
            }));
        }
        if let Some(community) = model.community(server, cx) {
            subs.push(cx.observe(&community.meta, |this: &mut Self, _, cx| {
                this.on_meta(cx);
                cx.notify();
            }));
        }
        subs.push(cx.subscribe(&transfer_confirm, |_: &mut Self, _, event: &InputEvent, cx| {
            if *event == InputEvent::Change {
                cx.notify();
            }
        }));
        // A refusal ends both waits. Always listening, so a quick reply can't beat the listener.
        subs.push(cx.subscribe(&model.answers, move |this: &mut Self, _, event: &CoreEvent, cx| {
            if let CoreEvent::ServerUpdateResponded(p) = event
                && decibell_model::ServerId::from_wire(&p.server_id) == server
                && !p.success
            {
                this.meta_save.stop();
                this.public_save.stop();
                let body = if p.message.is_empty() { "Unknown error.".to_string() } else { p.message.clone() };
                toast_error("Couldn't update server", body, cx);
                cx.notify();
            }
        }));
        OverviewTab {
            ctx,
            name,
            description,
            draft: None,
            syncing: false,
            meta_save: EchoSave::default(),
            public_save: EchoSave::default(),
            picture: None,
            asked: None,
            just_saved: None,
            editor: None,
            _editor_events: None,
            _picture_wait: None,
            _picture_answer: None,
            confirm_remove: false,
            transfer_target: None,
            transfer_confirm,
            transferring: false,
            owner_picker: None,
            picker_trigger: BoundsCell::new(),
            _watch: watch,
            _subs: subs,
        }
    }

    fn stored_meta(server: decibell_model::ServerId, cx: &App) -> (String, String) {
        let model = Model::global(cx);
        let fallback = model.servers.read(cx).get(server).cloned();
        match model.community(server, cx).map(|c| c.meta.read(cx).clone()).filter(|m| !m.name.is_empty()) {
            Some(meta) => (meta.name.to_string(), meta.description.to_string()),
            None => (
                fallback.as_ref().map_or("Server".into(), |s| s.name.to_string()),
                fallback.map_or(String::new(), |s| s.description.to_string()),
            ),
        }
    }

    fn can_manage(&self, cx: &App) -> bool {
        Model::global(cx).has_permission(self.ctx.server, perm::MANAGE_SERVER, cx)
    }

    /// (name, description) as shown: the draft, else the server's.
    fn shown(&self, cx: &App) -> Meta {
        match &self.draft {
            Some(d) => d.clone(),
            None => {
                let (name, description) = Self::stored_meta(self.ctx.server, cx);
                Meta { name, description }
            }
        }
    }

    #[cfg(feature = "fixtures")]
    /// Type `text` into the description box, as a person would (the self-tests).
    pub(crate) fn type_description(&mut self, text: &str, cx: &mut Context<Self>) {
        self.description.update(cx, |s, cx| s.set_value(text, cx));
    }

    #[cfg(feature = "fixtures")]
    /// A save of the name or description is waiting for the server's echo.
    pub(crate) fn saving_meta(&self) -> bool {
        self.meta_save.saving()
    }

    pub fn meta_dirty(&self, cx: &App) -> bool {
        let (name, description) = Self::stored_meta(self.ctx.server, cx);
        let shown = self.shown(cx);
        self.can_manage(cx) && (shown.name.trim() != name || shown.description.trim() != description)
    }

    /// The server's meta changed: settle a save, and let fields that aren't being edited follow.
    fn on_meta(&mut self, cx: &mut Context<Self>) {
        let (name, description) = Self::stored_meta(self.ctx.server, cx);
        if let Some(sent) = self.meta_save.settle(|sent| sent.name == name && sent.description == description) {
            // Edits made while the save was out stay.
            if self
                .draft
                .as_ref()
                .is_none_or(|d| d.name.trim() == sent.name && d.description.trim() == sent.description)
            {
                self.draft = None;
            }
        }
        let public = Model::global(cx).community(self.ctx.server, cx).is_some_and(|c| c.meta.read(cx).public_listing);
        self.public_save.settle(|sent| *sent == public);
        if self.draft.is_none() {
            self.set_inputs(&name, &description, cx);
        }
    }

    fn set_inputs(&mut self, name: &str, description: &str, cx: &mut Context<Self>) {
        self.syncing = true;
        if self.name.read(cx).value() != name {
            self.name.update(cx, |s, cx| s.set_value(name, cx));
        }
        if self.description.read(cx).value() != description {
            self.description.update(cx, |s, cx| s.set_value(description, cx));
        }
        self.syncing = false;
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.draft = None;
        let (name, description) = Self::stored_meta(self.ctx.server, cx);
        self.set_inputs(&name, &description, cx);
        cx.notify();
    }

    pub(crate) fn save_meta(&mut self, cx: &mut Context<Self>) {
        let shown = self.shown(cx);
        if !self.can_manage(cx) || !self.meta_dirty(cx) || shown.name.trim().is_empty() || self.meta_save.saving() {
            return;
        }
        let sent = Meta { name: shown.name.trim().to_string(), description: shown.description.trim().to_string() };
        self.meta_save.start(sent.clone(), cx, |this| &mut this.meta_save);
        let server = self.ctx.server;
        let weak = cx.weak_entity();
        requests::send(
            Request::UpdateServer { server, name: sent.name, description: sent.description, public_listing: None },
            cx,
            move |result, cx| {
                if let Err(e) = result {
                    let _ = weak.update(cx, |this, cx| {
                        this.meta_save.stop();
                        cx.notify();
                    });
                    toast_error("Couldn't update server", e.js_string(), cx);
                }
            },
        );
    }

    fn public_listing(&self, cx: &App) -> bool {
        Model::global(cx).community(self.ctx.server, cx).is_some_and(|c| c.meta.read(cx).public_listing)
    }

    /// Public listing: a flip shows at once and holds until the push confirms it. The current
    /// authoritative name / description go with it, so flipping visibility never commits an
    /// unsaved rename.
    fn toggle_public(&mut self, cx: &mut Context<Self>) {
        if !self.can_manage(cx) || self.public_save.saving() {
            return;
        }
        let next = !self.public_listing(cx);
        self.public_save.start(next, cx, |this| &mut this.public_save);
        let (name, description) = Self::stored_meta(self.ctx.server, cx);
        let weak = cx.weak_entity();
        requests::send(
            Request::UpdateServer { server: self.ctx.server, name, description, public_listing: Some(next) },
            cx,
            move |result, cx| {
                if let Err(e) = result {
                    let _ = weak.update(cx, |this, cx| {
                        this.public_save.stop();
                        cx.notify();
                    });
                    toast_error("Couldn't update visibility", e.js_string(), cx);
                }
            },
        );
    }

    // ── picture ──

    /// The decoded picture at the current version, fetched (once per version) when central
    /// hasn't sent the bytes yet.
    fn picture_image(&mut self, cx: &mut Context<Self>) -> (bool, Option<Arc<Image>>) {
        let model = Model::global(cx).clone();
        let server = self.ctx.server;
        let version: SharedString = model.servers.read(cx).picture_version(server).to_string().into();
        if version.is_empty() {
            return (false, None);
        }
        let data = model.servers.read(cx).picture(server).cloned();
        let Some(data) = data else {
            if self.asked.as_ref() != Some(&version) {
                self.asked = Some(version.clone());
                cx.defer(move |cx| actions::servers::fetch_server_picture(server, cx));
            }
            return (true, None);
        };
        if let Some((v, image)) = &self.picture
            && *v == version
        {
            return (true, image.clone());
        }
        let image = decode_data_url(&data).map(Arc::new);
        self.picture = Some((version, image.clone()));
        (true, image)
    }

    fn choose_picture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
            filters: decibell_picture_editor::picture_filters(),
        });
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| this.open_editor(PictureSource::Path(path), window, cx));
            });
        })
        .detach();
    }

    pub fn open_editor(&mut self, source: PictureSource, window: &mut Window, cx: &mut Context<Self>) {
        let model = Model::global(cx).clone();
        let server = self.ctx.server;
        let name = Self::stored_meta(server, cx).0;
        let members = model.servers.read(cx).get(server).map_or(0, |s| s.member_count);
        let members = (members > 0).then(|| format!("{} members", Regional::get().group(members as u64)).into());
        let editor = cx.new(|cx| PictureEditor::new(source, EditorOptions::server(name, members), window, cx));
        self._editor_events = Some(cx.subscribe(&editor, |this: &mut Self, editor, event: &PictureEditorEvent, cx| match event {
            PictureEditorEvent::Save(jpeg) => this.save_picture(jpeg.clone(), editor, cx),
            PictureEditorEvent::Closed { saved } => {
                if let Some(jpeg) = saved {
                    let data = Model::global(cx).servers.read(cx).picture(this.ctx.server).cloned();
                    let image = Arc::new(Image::from_bytes(ImageFormat::Jpeg, (**jpeg).clone()));
                    this.just_saved = Some((image, data));
                }
                this.editor = None;
                this._editor_events = None;
                cx.notify();
            }
        }));
        self.editor = Some(editor);
        cx.notify();
    }

    /// Sends the picture and waits for the community's answer (`server_picture_update_responded`):
    /// the editor stays on "Saving…" until then, and shows a refusal itself (the router doesn't
    /// toast for a community in `picture_saves_in_flight`).
    fn save_picture(&mut self, jpeg: Arc<Vec<u8>>, editor: Entity<PictureEditor>, cx: &mut Context<Self>) {
        let server = self.ctx.server;
        let model = Model::global(cx).clone();
        model.servers.update(cx, |s, _| {
            s.picture_saves_in_flight.insert(server);
        });
        let finish = move |result: Result<(), SharedString>, editor: &Entity<PictureEditor>, cx: &mut App| {
            if let Some(model) = cx.try_global::<Model>().cloned() {
                model.servers.update(cx, |s, _| {
                    s.picture_saves_in_flight.remove(&server);
                });
            }
            editor.update(cx, |e, cx| e.save_done(result, cx));
        };
        // Listening before the request goes out, so a quick answer can't beat it.
        let answered = editor.clone();
        let sub = cx.subscribe(&model.answers, move |this: &mut Self, _, event: &CoreEvent, cx| {
            if let CoreEvent::ServerPictureUpdateResponded(p) = event
                && decibell_model::ServerId::from_wire(&p.server_id) == server
            {
                this._picture_wait = None;
                this._picture_answer = None;
                let result = if p.success {
                    Ok(())
                } else {
                    Err(if p.message.is_empty() { "The server refused the picture.".into() } else { p.message.clone().into() })
                };
                finish(result, &answered, cx);
            }
        });
        self._picture_answer = Some(sub);
        let timed_out = editor.clone();
        self._picture_wait = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(10)).await;
            let _ = this.update(cx, |this, cx| {
                this._picture_wait = None;
                this._picture_answer = None;
                finish(Err("The server didn't answer. Try again.".into()), &timed_out, cx);
            });
        }));
        let failed = editor.clone();
        let weak = cx.weak_entity();
        requests::send(Request::UpdateServerPicture { server, bytes: std::rc::Rc::new((*jpeg).clone()) }, cx, move |result, cx| {
            if let Err(e) = result {
                let _ = weak.update(cx, |this, _| {
                    this._picture_wait = None;
                    this._picture_answer = None;
                });
                finish(Err(e.plain()), &failed, cx);
            }
        });
    }

    fn remove_picture(&mut self, cx: &mut Context<Self>) {
        self.confirm_remove = false;
        cx.notify();
        requests::send(Request::UpdateServerPicture { server: self.ctx.server, bytes: Default::default() }, cx, |result, cx| {
            if let Err(e) = result {
                log::error!("update_server_picture: {e}");
                toast_error("Failed to remove", "Please try again.", cx);
            }
        });
    }

    // ── ownership ──

    fn target_name(&self, cx: &App) -> SharedString {
        self.transfer_target.as_ref().map_or(SharedString::default(), |m| user_name(m.uid, &m.username, cx))
    }

    fn open_owner_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.owner_picker.is_some() {
            self.owner_picker = None;
            cx.notify();
            return;
        }
        let server = self.ctx.server;
        let search = cx.new(|cx| MemberSearch::new(server, "", 0, cx));
        let query = SearchBox::new("Find a member", window, cx, |this: &mut Self, text, cx| {
            if let Some(picker) = &this.owner_picker {
                picker.search.update(cx, |s, cx| s.set_query(text, cx));
            }
        });
        query.focus(window, cx);
        let bounds = self.picker_trigger.get();
        let observe = cx.observe(&search, |_, _, cx| cx.notify());
        self.owner_picker = Some(OwnerPicker {
            search,
            query,
            anchor: point(bounds.left(), bounds.bottom() + px(4.)),
            scroll: decibell_ui::ScrollHandle::new(),
            _observe: observe,
        });
        cx.notify();
    }

    fn pick_owner(&mut self, member: Member, cx: &mut Context<Self>) {
        self.owner_picker = None;
        self.transfer_target = Some(member);
        self.transfer_confirm.update(cx, |s, cx| s.clear(cx));
        cx.notify();
    }

    fn run_transfer(&mut self, cx: &mut Context<Self>) {
        let model = Model::global(cx).clone();
        let is_owner = model.is_owner(self.ctx.server, cx);
        let Some(target) = self.transfer_target.clone() else { return };
        let name = self.target_name(cx);
        if !is_owner || name.is_empty() || self.transfer_confirm.read(cx).value() != name.as_ref() {
            return;
        }
        self.transferring = true;
        cx.notify();
        let weak = cx.weak_entity();
        requests::send(
            Request::TransferOwnership { server: self.ctx.server, new_owner: name.to_string(), new_owner_uid: target.uid },
            cx,
            move |result, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.transferring = false;
                    if result.is_ok() {
                        this.transfer_target = None;
                        this.transfer_confirm.update(cx, |s, cx| s.clear(cx));
                    }
                    cx.notify();
                });
                if let Err(e) = result {
                    toast_error("Transfer failed", e.js_string(), cx);
                }
            },
        );
    }
}

pub(crate) fn toast_error(title: &'static str, body: impl Into<SharedString>, cx: &mut App) {
    if let Some(model) = cx.try_global::<Model>().cloned() {
        model.toasts.update(cx, |t, cx| t.error(SharedString::new_static(title), Some(body.into()), cx));
    }
}

/// The preview is the tile at 2×: its corner too.
fn preview_radius(cx: &App) -> Pixels {
    cx.theme().radius_md * 2.
}

impl Render for OverviewTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let server = self.ctx.server;
        let can_manage = self.can_manage(cx);
        let (server_name, server_description) = Self::stored_meta(server, cx);
        let owner = model.community(server, cx).map_or(0, |c| c.meta.read(cx).owner);
        let is_owner = model.is_owner(server, cx);
        let (has_picture, picture) = self.picture_image(cx);
        // The saved picture shows until the tile's copy (a newer data URL) arrives.
        if let Some((_, before)) = &self.just_saved
            && model.servers.read(cx).picture(server).is_some_and(|now| Some(now) != before.as_ref())
        {
            self.just_saved = None;
        }
        let shown_image = self.just_saved.as_ref().map(|(i, _)| i.clone()).or(if has_picture { picture } else { None });
        let meta_dirty = self.meta_dirty(cx);
        let name_blank = self.shown(cx).name.trim().is_empty();
        let public = self.public_save.sent().copied().unwrap_or_else(|| self.public_listing(cx));
        let owner_name = if owner != 0 {
            let fallback: SharedString = model
                .community(server, cx)
                .and_then(|c| c.roster.read(cx).member(owner).map(|m| m.username.clone()))
                .unwrap_or_else(|| "Unknown user".into());
            Some(user_name(owner, &fallback, cx))
        } else {
            None
        };
        if owner != 0 {
            decibell_common::want_member(server, owner, cx);
        }
        let target_name = self.target_name(cx);
        if !target_name.is_empty() {
            let placeholder: SharedString = format!("Type {target_name} to confirm").into();
            self.transfer_confirm.update(cx, |s, cx| s.set_placeholder(placeholder, cx));
        }
        let confirm_text = self.transfer_confirm.read(cx).value().to_string();
        let theme = cx.theme();

        // ── identity ──
        let mut card = div()
            .flex()
            .flex_col()
            .gap_2()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_light)
            .p(px(14.));
        if can_manage {
            card = card
                .child(
                    div()
                        .child(div().mb_1().child(caps_label("Name", cx)))
                        .child(InputBox::new("server-name", &self.name).text(14., 500).pad(12., 8.)),
                )
                .child(
                    div().child(div().mb_1().child(caps_label("Description", cx))).child(
                        InputBox::new("server-description", &self.description)
                            .text(13., 400)
                            .leading(1.5)
                            .color(theme.color_text_secondary)
                            .pad(12., 8.)
                            .rows(2),
                    ),
                )
                .child(
                    div()
                        .mt_1()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap_3()
                        .border_t_1()
                        .border_color(theme.color_border_divider)
                        .pt_3()
                        .child(
                            div()
                                .min_w_0()
                                .child(Label::new("Public listing").size(13.).weight(500.).color(theme.color_text_primary))
                                .child(
                                    div().mt_0p5().child(
                                        Label::new(
                                            "List this server in Discover so anyone can find and join it without an invite. Off keeps it hidden and invite-only.",
                                        )
                                        .size(12.)
                                        .line_height(12. * 1.45)
                                        .color(theme.color_text_muted),
                                    ),
                                ),
                        )
                        .child(
                            Switch::new("server-public", public)
                                .disabled(self.public_save.saving())
                                .on_toggle(cx.listener(|this, _: &bool, _, cx| this.toggle_public(cx))),
                        ),
                );
        } else {
            card = card.child(
                div()
                    .child(caps_label("Name", cx))
                    .child(div().mt_0p5().child(Label::new(server_name.clone()).size(14.).weight(500.).color(theme.color_text_primary))),
            );
            if !server_description.is_empty() {
                card = card.child(
                    div().child(caps_label("Description", cx)).child(
                        div().mt_0p5().child(Label::new(server_description.clone()).size(13.).line_height(19.5).color(theme.color_text_secondary)),
                    ),
                );
            }
        }
        if let Some(owner_name) = owner_name {
            card = card.child(
                div()
                    .child(caps_label("Owner", cx))
                    .child(div().mt_0p5().child(Label::new(owner_name).size(13.).color(theme.color_text_secondary))),
            );
        }
        let identity = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(heading("Server identity", theme.color_text_primary))
            .child(card)
            .when(!can_manage, |this| {
                this.child(
                    Label::new("Members with Manage Server can change the name and description.")
                        .size(12.)
                        .line_height(18.)
                        .color(theme.color_text_muted),
                )
            });

        // ── picture ──
        let radius = preview_radius(cx);
        let preview: AnyElement = match shown_image {
            Some(image) => img(ImageSource::Image(image))
                .w(px(256.))
                .h(px(88.))
                .object_fit(ObjectFit::Cover)
                .rounded(radius)
                .into_any_element(),
            None => div()
                .flex()
                .flex_col()
                .w(px(256.))
                .h(px(88.))
                .items_center()
                .justify_center()
                .gap_1p5()
                .rounded(radius)
                .border_2()
                .border_dashed()
                .border_color(theme.color_border)
                .child(Icon::new(IconName::PictureImage).size(px(18.)).color(theme.color_text_muted))
                .child(
                    Label::new(if can_manage { "Drop a picture or click" } else { "No picture" })
                        .size(12.)
                        .weight(500.)
                        .color(theme.color_text_muted),
                )
                .into_any_element(),
        };
        let picture_button = if can_manage {
            div()
                .id("server-picture")
                .group("server-picture")
                .relative()
                .flex()
                .flex_none()
                .rounded(radius)
                .cursor_pointer()
                .tooltip(decibell_ui::title_tooltip(if has_picture { "Change picture" } else { "Upload picture" }))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.choose_picture(window, cx)))
                .child(preview)
                .child(
                    // The hover scrim: `bg-black/55` with a white "Change" (renderer literals).
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_1()
                        .rounded(radius)
                        .bg(gpui::black().opacity(0.55))
                        .invisible()
                        .group_hover("server-picture", |s| s.visible())
                        .child(Icon::new(IconName::PictureCamera).size(px(18.)).color(gpui::white()))
                        .child(Label::new("Change").size(11.).weight(600.).color(gpui::white())),
                )
                .into_any_element()
        } else {
            div().relative().flex().flex_none().rounded(radius).child(preview).into_any_element()
        };
        let picture_card = div()
            .id("server-picture-card")
            .group("server-picture-card")
            .relative()
            .flex()
            .items_center()
            .gap_5()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_light)
            .p(px(14.))
            .when(can_manage, |this| {
                this.on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                    let Some(path) = paths
                        .paths()
                        .iter()
                        .find(|p| p.to_str().is_some_and(decibell_picture_editor::picture::looks_like_picture))
                    else {
                        return;
                    };
                    this.open_editor(PictureSource::Path(path.clone()), window, cx);
                }))
            })
            .child(picture_button)
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(Label::new("On the server bar, invite cards and Discover").size(13.).weight(500.).color(theme.color_text_primary))
                    .child(
                        div().mt_0p5().child(
                            Label::new(if can_manage {
                                "Any JPEG, PNG, WebP or GIF. You'll crop it to the bar's shape next."
                            } else {
                                "Members with Manage Server can change it."
                            })
                            .size(12.)
                            .line_height(12. * 1.45)
                            .color(theme.color_text_muted),
                        ),
                    )
                    .when(can_manage, |this| {
                        this.child(
                            div()
                                .mt_3()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("picture-upload", if has_picture { "Change picture" } else { "Upload picture" })
                                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.choose_picture(window, cx))),
                                )
                                .when(has_picture, |this| {
                                    this.child(Button::new("picture-remove", "Remove").variant(ButtonVariant::Ghost).on_click(cx.listener(
                                        |this, _: &ClickEvent, _, cx| {
                                            this.confirm_remove = true;
                                            cx.notify();
                                        },
                                    )))
                                }),
                        )
                    }),
            )
            .when(can_manage, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .invisible()
                        .group_drag_over::<ExternalPaths>("server-picture-card", |s| s.visible())
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius_md)
                        .border_2()
                        .border_dashed()
                        .border_color(theme.color_accent)
                        .bg(theme.color_bg_light.opacity(0.9))
                        .child(Label::new("Drop to use as the server picture").size(13.).weight(600.).color(theme.color_text_primary)),
                )
            });
        let picture_section = div().flex().flex_col().gap_2().child(heading("Server picture", theme.color_text_primary)).child(picture_card);

        // ── ownership ──
        let transfer = is_owner.then(|| {
            let trigger_label: AnyElement = match &self.transfer_target {
                Some(m) => {
                    let label: SharedString =
                        if m.nickname.is_empty() { target_name.clone() } else { format!("{} ({})", m.nickname, target_name).into() };
                    div()
                        .flex()
                        .min_w_0()
                        .flex_1()
                        .items_center()
                        .gap_2()
                        .child(UserAvatar::new(m.uid, target_name.clone(), 18.))
                        .child(div().min_w_0().flex_1().child(Label::new(label).size(13.).color(theme.color_text_primary).truncate()))
                        .into_any_element()
                }
                None => div()
                    .min_w_0()
                    .flex_1()
                    .child(Label::new("Choose a member…").size(13.).color(theme.color_text_muted).truncate())
                    .into_any_element(),
            };
            let open = self.owner_picker.is_some();
            let trigger = div()
                .id("owner-picker-trigger")
                .relative()
                .mt_3()
                .flex()
                .h_10()
                .w_full()
                .items_center()
                .gap_2()
                .rounded(theme.radius_md)
                .border_1()
                .border_color(if open { theme.color_accent } else { theme.color_border })
                .bg(theme.color_bg_lighter)
                .px_3()
                .cursor_pointer()
                .child(self.picker_trigger.probe())
                .child(trigger_label)
                .child(Icon::new(IconName::SelectChevron).size(px(14.)).color(theme.color_text_muted))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_owner_picker(window, cx)));
            let can_run = !target_name.is_empty() && confirm_text == target_name.as_ref() && !self.transferring;
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(heading("Transfer ownership", theme.color_error))
                .child(
                    div()
                        .rounded(theme.radius_md)
                        .border_1()
                        .border_color(theme.color_error.opacity(0.25))
                        .bg(theme.color_error.opacity(0.05))
                        .p(px(14.))
                        .child(
                            Label::new(
                                "The new owner gets every permission and outranks every role; you keep only the roles you hold. This can't be undone by you.",
                            )
                            .size(12.)
                            .line_height(12. * 1.55)
                            .color(theme.color_text_muted),
                        )
                        .child(trigger)
                        .when(!target_name.is_empty(), |this| {
                            this.child(
                                div().mt_2().child(
                                    InputBox::new("transfer-confirm", &self.transfer_confirm).text(13., 400).pad(12., 8.).focus_border(theme.color_error).no_ring(),
                                ),
                            )
                            .child(
                                div().mt_2().child(
                                    Button::new(
                                        "transfer-run",
                                        if self.transferring { "Transferring…".to_string() } else { format!("Transfer ownership to {target_name}") },
                                    )
                                    .variant(ButtonVariant::Danger)
                                    .full_width()
                                    .weight(500.)
                                    .disabled(!can_run)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.run_transfer(cx))),
                                ),
                            )
                        }),
                )
        });

        let picker = self.owner_picker.as_ref().map(|picker| self.render_owner_picker(picker, window, cx));
        let theme = cx.theme();
        // The picker and the dialog sit outside the column: as flex items they'd take a gap each.
        let content = div()
            .child(div().flex().flex_col().gap_6().child(identity).child(picture_section).children(transfer))
            .children(picker)
            .child(
                ConfirmModal::new("remove-picture", self.confirm_remove, "Remove the server picture?", "Remove")
                    .tone(ConfirmTone::Danger)
                    .message("Members will see the server's letter and colour on the server bar again.")
                    .on_confirm(cx.listener(|this, _: &ClickEvent, _, cx| this.remove_picture(cx)))
                    .on_cancel(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.confirm_remove = false;
                        cx.notify();
                    })),
            )
            .children(self.editor.clone());
        let _ = theme;
        div().flex().flex_col().child(content).when(can_manage, |this| {
            this.child(
                SaveBar::new("overview-save", &self.ctx.scroll, meta_dirty, self.ctx.nudge.get())
                    .saving(self.meta_save.saving())
                    .save_disabled(name_blank)
                    .on_reset(cx.listener(|this, _: &ClickEvent, _, cx| this.reset(cx)))
                    .on_save(cx.listener(|this, _: &ClickEvent, _, cx| this.save_meta(cx))),
            )
        })
    }
}

#[cfg(feature = "fixtures")]
impl OverviewTab {
    /// The `transfer-ownership` screen: bob chosen, half his name typed.
    pub fn fixture_transfer(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let bob = crate::fixture::members().into_iter().find(|m| m.uid == 2).map(|m| Member::from(&m));
        self.transfer_target = bob;
        self.transfer_confirm.update(cx, |s, cx| s.set_value("bo", cx));
        cx.notify();
    }

    /// The `picture-editor` screen: an editor already open.
    pub fn fixture_editor(&mut self, editor: Entity<PictureEditor>, cx: &mut Context<Self>) {
        self.editor = Some(editor);
        cx.notify();
    }
}

impl OverviewTab {
    fn render_owner_picker(&self, picker: &OwnerPicker, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let me = Model::global(cx).auth.read(cx).uid;
        let found = picker.search.read(cx);
        let shown: Vec<Member> = found.members.iter().filter(|m| m.uid > 0 && m.uid != me).cloned().collect();
        let (loaded, error, has_more, loading) = (found.loaded, found.error.clone(), found.has_more, found.loading);
        let query_blank = found.query().trim().is_empty();
        let theme = cx.theme();
        let entity = cx.weak_entity();
        let search = picker.search.clone();
        let rows = shown.len();
        let mut extra: Vec<(&'static str, SharedString, gpui::Hsla)> = Vec::new();
        if !loaded {
            extra.push(("searching", "Searching…".into(), theme.color_text_muted));
        } else if let Some(error) = error {
            extra.push(("error", error, theme.color_error));
        } else if shown.is_empty() && !has_more {
            extra.push((
                "empty",
                if query_blank { "No one else is a member yet.".into() } else { "No one else matches.".into() },
                theme.color_text_muted,
            ));
        }
        let count = rows + has_more as usize + extra.len();
        let list = ScrollArea::new("owner-picker-list").track_scroll(&picker.scroll).h(px(256.)).windowed(
            count,
            RowHeights::Uniform(px(36.)),
            move |range, _, cx| {
                let theme = cx.theme();
                range
                    .map(|i| {
                        if i < rows {
                            let m = shown[i].clone();
                            let name = user_name(m.uid, &m.username, cx);
                            let primary: SharedString = if m.nickname.is_empty() { name.clone() } else { m.nickname.clone() };
                            let weak = entity.clone();
                            let hover = theme.color_surface_hover;
                            div()
                                .id(("owner-option", m.uid as u64))
                                .flex()
                                .h_full()
                                .w_full()
                                .items_center()
                                .gap_2p5()
                                .rounded(theme.radius_md)
                                .px_2()
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
                                        .when(!m.nickname.is_empty(), |this| {
                                            this.child(Label::new(name.clone()).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted).truncate())
                                        }),
                                )
                                .on_click(move |_, _, cx| {
                                    let m = m.clone();
                                    let _ = weak.update(cx, |this, cx| this.pick_owner(m, cx));
                                })
                                .into_any_element()
                        } else if has_more && i == rows {
                            let search = search.clone();
                            let (muted, primary, hover) = (theme.color_text_muted, theme.color_text_primary, theme.color_surface_hover);
                            div()
                                .id("owner-more")
                                .flex()
                                .h_full()
                                .items_center()
                                .rounded(theme.radius_md)
                                .px_2()
                                .text_color(muted)
                                .when(loading, |this| this.opacity(0.6))
                                .when(!loading, |this| {
                                    this.cursor_pointer()
                                        .hover(move |s| s.bg(hover).text_color(primary))
                                        .on_click(move |_, _, cx| search.update(cx, |s, cx| s.load_more(cx)))
                                })
                                .child(Label::new(if loading { "Loading…" } else { "Show more" }).size(12.).weight(500.))
                                .into_any_element()
                        } else {
                            let (_, text, color) = &extra[i - rows - has_more as usize];
                            div()
                                .flex()
                                .h_full()
                                .items_center()
                                .justify_center()
                                .px_2()
                                .child(Label::new(text.clone()).size(12.).line_height(18.).color(*color))
                                .into_any_element()
                        }
                    })
                    .collect()
            },
        );
        // Under the trigger as last laid out.
        let b = self.picker_trigger.get();
        let anchor = if b.size.width > px(0.) { point(b.left(), b.bottom() + px(4.)) } else { picker.anchor };
        Popover::new("owner-picker", anchor)
            .on_dismiss(cx.listener(|this, _: &DismissEvent, _, cx| {
                this.owner_picker = None;
                cx.notify();
            }))
            .child(div().flex().flex_col().w(px(320.)).p_2().child(picker.query.field("owner-search")).child(div().mt_2().child(list)))
            .into_any_element()
    }
}
