//! The create dialog for channels and categories (`features/channels/CreateChannelModal.tsx`),
//! opened from the category header's "+" and the channel list's context menu (MANAGE_CHANNELS
//! only: every entry point is gated). The server slugs the name into the id; success confirms
//! itself through the `channel_list_updated` push (a new text channel opens: the model's
//! `channel_action_responded`), a refusal through that answer's toast.
//!
//! A text channel's encryption is chosen here and fixed for its life (owner decision
//! 2026-10-14); off by default, so server-side search keeps working.

use decibell_common::Watch;
use decibell_core::api;
use decibell_model::Model;
use decibell_model::error::UiError;
use decibell_model::stores::community::ChannelKind;
use decibell_shell::{CreateChannelRequest, ShellRequests};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, CloseRequest, FontRole, Label, Modal, Switch};
use gpui::{App, ClickEvent, Context, Entity, IntoElement, Render, SharedString, Window, div, prelude::*, px};

use crate::ui::{self, HEADING_TRACKING, InputLook};

pub struct CreateChannelOverlay {
    model: Model,
    request: Option<CreateChannelRequest>,
    name: Entity<InputState>,
    kind: ChannelKind,
    encrypted: bool,
    creating: bool,
    focus_name: bool,
    _subscriptions: Vec<gpui::Subscription>,
    _watch: Watch,
}

impl CreateChannelOverlay {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        // `maxLength={64}` (UTF-16 units).
        let name = cx.new(|cx| InputState::single_line(window, cx).max_length(64));
        cx.subscribe(&name, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::Submit => this.create(cx),
            // `useEscapeToClose`: Escape in the field closes too.
            InputEvent::Escape => this.close(cx),
            _ => {}
        })
        .detach();
        let requests = ShellRequests::entity(cx);
        let watch = Watch::new();
        let mut subscriptions: Vec<gpui::Subscription> = Vec::new();
        subscriptions.push(cx.observe(&requests, |this: &mut Self, requests, cx| {
            let next = requests.read(cx).create_channel.clone();
            if next == this.request {
                return;
            }
            this.request = next;
            this.mounted(cx);
            cx.notify();
        }));
        let request = requests.read(cx).create_channel.clone();
        let mut this = Self { model, request, name, kind: ChannelKind::Text, encrypted: false, creating: false, focus_name: false, _subscriptions: subscriptions, _watch: watch };
        this.mounted(cx);
        cx.set_global(crate::Current::<CreateChannelOverlay>(cx.weak_entity()));
        this
    }

    /// A new request: the dialog mounts afresh (`createTarget && <CreateChannelModal …/>`).
    fn mounted(&mut self, cx: &mut App) {
        let Some(request) = self.request.clone() else { return };
        self.kind = request.kind;
        self.encrypted = false;
        self.creating = false;
        let name = self.name.clone();
        name.update(cx, |n, cx| n.clear(cx));
        if let Some((text, encrypted)) = crate::screen_state(cx).and_then(|s| s.create_fields.clone()) {
            name.update(cx, |n, cx| n.set_value(&text, cx));
            self.encrypted = encrypted;
        }
        // `autoFocus`: on the next render (it has the window).
        self.focus_name = true;
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        ShellRequests::entity(cx).update(cx, |r, cx| r.clear_create_channel(cx));
    }

    fn can_create(&self, cx: &App) -> bool {
        !self.name.read(cx).value().trim().is_empty() && !self.creating
    }

    /// `handleCreate`.
    fn create(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.request.clone() else { return };
        if !self.can_create(cx) {
            return;
        }
        self.creating = true;
        cx.notify();
        let category = self.kind == ChannelKind::Category;
        let args = api::channels::CreateChannelArgs {
            server_id: request.server.wire(),
            name: self.name.read(cx).value().trim().to_string(),
            channel_type: kind_name(self.kind).to_string(),
            voice_bitrate_kbps: 0,
            category_id: if category { None } else { request.category.as_ref().map(|c| c.to_string()) },
            encrypted: Some(self.kind == ChannelKind::Text && self.encrypted),
        };
        cx.spawn(async move |this, cx| {
            let result = if decibell_model::core::is_running() {
                api::channels::create_channel(args).await.map_err(UiError::from)
            } else {
                Err(UiError::not_running())
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.close(cx),
                Err(e) => {
                    this.creating = false;
                    let model = this.model.clone();
                    model.toasts.update(cx, |t, cx| t.error("Create failed", Some(e.js_string()), cx));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn is_open(&self) -> bool {
        self.request.is_some()
    }

    /// For the screens and self-tests: type the name, flip the switch.
    pub fn set_fields(&mut self, name: &str, encrypted: bool, cx: &mut Context<Self>) {
        self.name.update(cx, |n, cx| n.set_value(name, cx));
        self.encrypted = encrypted;
        cx.notify();
    }
}

fn kind_name(kind: ChannelKind) -> &'static str {
    match kind {
        ChannelKind::Voice => "voice",
        ChannelKind::Category => "category",
        _ => "text",
    }
}

impl Render for CreateChannelOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.request.is_some();
        if std::mem::take(&mut self.focus_name) && open {
            // After the Modal takes focus as it opens (it keeps focus that is already inside it).
            let name = self.name.clone();
            window.defer(cx, move |window, cx| name.update(cx, |n, cx| n.focus(window, cx)));
        }
        let card = self.request.clone().map(|request| self.card(&request, window, cx));
        Modal::new("create-channel", open)
            .on_close(cx.listener(|this, _: &CloseRequest, _, cx| this.close(cx)))
            .children(card)
    }
}

impl CreateChannelOverlay {
    fn card(&mut self, request: &CreateChannelRequest, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let kind = self.kind;
        let category = kind == ChannelKind::Category;
        let title: SharedString = if category { "Create category".into() } else { format!("Create {} channel", kind_name(kind)).into() };
        let description = if category {
            "A collapsible group for channels. Drag channels into it to organize the sidebar."
        } else if kind == ChannelKind::Text {
            "A new place for messages. The channel id is derived from the name."
        } else {
            "A new place to talk. The channel id is derived from the name."
        };
        let placeholder = if category {
            "Gaming"
        } else if kind == ChannelKind::Text {
            "bug-reports"
        } else {
            "Game Night"
        };
        self.name.update(cx, |n, cx| n.set_placeholder(placeholder, cx));
        let can_create = self.can_create(cx);
        let t = cx.theme();
        let name_box = ui::input_box("create-channel-name", &self.name, InputLook::new(t.color_bg_light, t.radius_md).plain_placeholder(), window, cx);
        let t = cx.theme();
        let (bg_light, bg_lighter, accent, accent_hover, on_accent, accent_soft, accent_bright, muted, secondary, primary) = (
            t.color_bg_light,
            t.color_bg_lighter,
            t.color_accent,
            t.color_accent_hover,
            t.color_on_accent,
            t.color_accent_soft,
            t.color_accent_bright,
            t.color_text_muted,
            t.color_text_secondary,
            t.color_text_primary,
        );
        let type_choice = (request.allow_type_choice && !category).then(|| {
            let choice = |value: ChannelKind, label: &'static str, cx: &mut Context<Self>| {
                let active = kind == value;
                div()
                    .id(label)
                    .flex_1()
                    .flex()
                    .justify_center()
                    .rounded(cx.theme().radius_sm)
                    .py_1p5()
                    .cursor_pointer()
                    .when(active, |this| this.bg(accent_soft))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.kind = value;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_color(if active { accent_bright } else { muted })
                            .when(!active, |this| this.hover(move |s| s.text_color(secondary)))
                            .child(Label::new(label).size(12.).line_height(18.).weight(500.)),
                    )
            };
            div()
                .mb_4()
                .flex()
                .gap_1()
                .rounded(cx.theme().radius_md)
                .bg(bg_light)
                .p_1()
                .child(choice(ChannelKind::Text, "Text", cx))
                .child(choice(ChannelKind::Voice, "Voice", cx))
        });
        let t = cx.theme();
        let encrypted = (kind == ChannelKind::Text).then(|| {
            div()
                .id("create-channel-encrypted")
                .relative()
                .children(decibell_shell::probes::probe("create-channel-encrypted"))
                .mt_4()
                .flex()
                .items_start()
                .justify_between()
                .gap_4()
                .rounded(t.radius_md)
                .border_1()
                .border_color(t.color_border_divider)
                .bg(bg_light)
                .px_3()
                .py_2p5()
                .cursor_pointer()
                // A `<label>` around the switch: a click anywhere on it toggles.
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.encrypted = !this.encrypted;
                    cx.notify();
                }))
                .child(
                    div()
                        .min_w_0()
                        .child(Label::new("End-to-end encrypted").size(13.).line_height(19.5).weight(500.).color(primary))
                        .child(
                            div().mt_0p5().child(
                                Label::new(
                                    "Messages and files are sealed on members' devices, so the server can't read them. Members need encryption set up, and search won't cover it. You can't change this later.",
                                )
                                .size(12.)
                                .line_height(12. * 1.55)
                                .color(muted),
                            ),
                        ),
                )
                // The label's click toggles it (a handler here too would toggle twice).
                .child(Switch::new("create-channel-encrypted-switch", self.encrypted))
        });
        let t = cx.theme();
        div()
            .id("create-channel-card")
            .occlude()
            .w_full()
            .max_w(px(400.))
            .rounded(t.radius_xl)
            .border_1()
            .border_color(t.color_border)
            .bg(t.color_bg_dark)
            .p_6()
            .shadow(t.shadow_modal.clone())
            .child(div().mb_1().child(Label::new(title).font(FontRole::Display).size(18.).line_height(27.).weight(600.).tracking(HEADING_TRACKING).color(primary)))
            .child(div().mb_4().child(Label::new(description).size(12.).line_height(12. * 1.55).color(muted)))
            .children(type_choice)
            .child(div().mb_1().child(Label::new("NAME").size(11.).line_height(16.5).weight(600.).tracking(0.07).color(muted)))
            .child(name_box)
            .children(encrypted)
            .child(
                div()
                    .mt_5()
                    .flex()
                    .gap_2p5()
                    .child(
                        div()
                            .id("create-channel-cancel")
                            .flex_1()
                            .flex()
                            .justify_center()
                            .rounded(t.radius_md)
                            .bg(bg_light)
                            .py_2p5()
                            .cursor_pointer()
                            .hover(move |s| s.bg(bg_lighter))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)))
                            .child(Label::new("Cancel").size(13.).line_height(19.5).weight(500.).color(primary)),
                    )
                    .child(
                        div()
                            .id("create-channel-create")
                            .relative()
                            .children(decibell_shell::probes::probe("create-channel-create"))
                            .flex_1()
                            .flex()
                            .justify_center()
                            .rounded(t.radius_md)
                            .bg(accent)
                            .py_2p5()
                            .when(can_create, |this| this.cursor_pointer().hover(move |s| s.bg(accent_hover)))
                            .when(!can_create, |this| this.opacity(0.5))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.create(cx)))
                            .child(Label::new(if self.creating { "Creating..." } else { "Create" }).size(13.).line_height(19.5).weight(600.).color(on_accent)),
                    ),
            )
    }
}
