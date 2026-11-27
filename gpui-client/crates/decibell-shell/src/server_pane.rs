//! A text channel's pane (`MainLayout`'s server branch, `features/chat/ChannelHeader.tsx`): the
//! channel header spanning the chat and the panel on its right (members, or a search's results),
//! which start under it, as in Discord (owner, 2026-10-14). The chat, the right panel and the
//! header's search box are slots.

use decibell_common::Watch;
use decibell_model::stores::layout::LayoutEvent;
use decibell_model::{ChannelId, Model, ServerId};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, title_tooltip};
use gpui::{
    AnyView, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, Render, SharedString, StyleRefinement,
    Subscription, Window, div, prelude::*, px,
};

use crate::members::MembersList;
use crate::slots::{PaneHost, Slot};

/// The members toggle's place and the search box: `w-[244px]`.
pub const SEARCH_BOX_WIDTH: f32 = 244.;

pub struct ServerPane {
    model: Model,
    channel: Option<(ServerId, ChannelId)>,
    members_panel: bool,
    chat: PaneHost,
    right: PaneHost,
    search: PaneHost,
    members: Option<Entity<MembersList>>,
    community: Option<(ServerId, Subscription)>,
    _watch: Watch,
}

impl ServerPane {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .store_if(&model.nav, cx, |this, nav, _| {
                let next = nav.active_channel_key().map(|(s, c)| (s, c.clone()));
                std::mem::replace(&mut this.channel, next.clone()) != next
            })
            .store(&model.communities, cx)
            .event(&model.layout, cx, |_, e: &LayoutEvent| *e == LayoutEvent::Panels);
        let channel = model.nav.read(cx).active_channel_key().map(|(s, c)| (s, c.clone()));
        let members_panel = model.layout.read(cx).prefs().members_panel;
        Self {
            model,
            channel,
            members_panel,
            chat: PaneHost::default(),
            right: PaneHost::default(),
            search: PaneHost::default(),
            members: None,
            community: None,
            _watch: watch,
        }
    }

    /// The default right panel: the member list while the members panel is on.
    fn members(&mut self, cx: &mut App) -> AnyView {
        self.members.get_or_insert_with(|| cx.new(MembersList::new)).clone().into()
    }
}

impl Render for ServerPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        self.members_panel = model.layout.read(cx).prefs().members_panel;
        let community = self.channel.as_ref().and_then(|(s, _)| model.community(*s, cx));
        if self.community.as_ref().map(|(id, _)| *id) != community.as_ref().map(|c| c.id) {
            self.community = community.as_ref().map(|c| (c.id, cx.observe(&c.channels, |_, _, cx| cx.notify())));
        }
        let empty = |_: &mut Window, cx: &mut App| -> AnyView { cx.new(|_| Empty).into() };
        let search = self.search.view(Slot::HeaderSearch, window, cx, Some(&empty));
        let chat = self.chat.view(Slot::Chat, window, cx, None);
        // The right panel: a registered one decides itself; the default shows the members
        // while the panel is on.
        // (A cached view is laid out at the size its parent gives it: the member list has its fixed
        // width; a registered panel sizes itself and is drawn uncached.)
        let right: Option<gpui::AnyElement> = if crate::slots::pane_builder(cx, Slot::RightPanel).is_some() {
            Some(self.right.view(Slot::RightPanel, window, cx, None).into_any_element())
        } else if self.members_panel {
            Some(self.members(cx).cached(StyleRefinement::default().w(px(crate::members::MEMBERS_WIDTH)).h_full()).into_any_element())
        } else {
            None
        };
        let theme = cx.theme();
        let channel = self
            .channel
            .as_ref()
            .and_then(|(_, id)| community.as_ref().and_then(|c| c.channels.read(cx).get(id).cloned()));

        let header = self.channel.as_ref().map(|(_, id)| {
            let name = channel.as_ref().map_or_else(|| id.clone(), |c| c.name.clone());
            let topic = channel.as_ref().map(|c| c.topic.clone()).filter(|t| !t.is_empty());
            decibell_harness::probes::text("channel-header-topic", &topic.clone().unwrap_or_default());
            let encrypted = channel.as_ref().is_some_and(|c| c.encrypted);
            let title = |text: SharedString, color| {
                Label::new(text)
                    .font(FontRole::Display)
                    .size(theme.scaled.text_title)
                    .weight(theme.palette.font_weight_emphasis)
                    .tracking(theme.palette.tracking_title)
                    .color(color)
                    .truncate()
            };
            let visible = self.members_panel;
            div()
                .flex()
                .flex_none()
                .h_12()
                .items_center()
                .border_b_1()
                .border_color(theme.color_border_divider)
                .pl_4()
                .pr_2()
                .child(div().mr_1p5().flex_none().child(title("#".into(), theme.color_text_muted)))
                .child(div().min_w_0().flex_shrink(1.).child(title(name, theme.color_text_bright)))
                .when(encrypted, |this| {
                    // `ChannelEncryptionBadge`: `rounded-sm bg-success/15 px-2 py-0.5 font-channel
                    // text-[11px] font-medium text-success`, the 11 px lock and "E2EE".
                    this.child(
                        div().ml_2p5().flex().flex_none().items_center().child(
                            div()
                                .id("channel-encrypted")
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(px(5.))
                                .rounded(theme.radius_sm)
                                .bg(theme.color_success.opacity(0.15))
                                .px_2()
                                .py_0p5()
                                .tooltip(title_tooltip(
                                    "End-to-end encrypted channel — messages and attachments are sealed on members' devices; the server never holds the keys",
                                ))
                                .child(Icon::new(IconName::ChatLock).size(px(11.)).color(theme.color_success))
                                .child(Label::new("E2EE").font(FontRole::Channel).size(11.).weight(500.).color(theme.color_success).nowrap()),
                        ),
                    )
                })
                .when_some(topic, |this, topic| {
                    this.child(div().mx_3().flex_none().h_5().w(px(1.)).bg(theme.color_border_divider)).child(
                        div()
                            .id("channel-topic")
                            .mr_3()
                            .flex_1()
                            .flex_basis(px(0.))
                            .min_w_0()
                            .tooltip(title_tooltip(topic.clone()))
                            .child(
                                Label::new(topic)
                                    .font(FontRole::Channel)
                                    .size(13.)
                                    // `tracking-normal` (the header's `tracking-title` stops here).
                                    .tracking(0.)
                                    .color(theme.color_text_muted)
                                    .truncate(),
                            ),
                    )
                })
                .child(
                    div()
                        .id("members-toggle")
                        .group("members-toggle")
                        .ml_auto()
                        .flex()
                        .flex_none()
                        .size_8()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius_sm)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.color_surface_hover))
                        .tooltip(title_tooltip(if visible { "Hide members" } else { "Show members" }))
                        .on_click(|_: &ClickEvent, _, cx| {
                            let layout = Model::global(cx).layout.clone();
                            layout.update(cx, |l, cx| l.toggle_members_panel(cx));
                        })
                        .child(
                            Icon::new(IconName::ShellPanel)
                                .size(px(18.))
                                .color(theme.color_text_muted)
                                .group_hover("members-toggle", theme.color_text_secondary),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(SEARCH_BOX_WIDTH))
                        .h_full()
                        // `ml-4`: the members toggle stays 8 px clear of the members column.
                        .ml_4()
                        .child(search.cached(StyleRefinement::default().size_full())),
                )
        });

        // A view's root fills the box its cached view was given.
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.color_bg_mid)
            .children(header)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().flex_1().min_w_0().h_full().child(chat.cached(StyleRefinement::default().size_full())))
                    .children(right),
            )
    }
}

/// The search box's place until message search registers it.
struct Empty;

impl Render for Empty {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
