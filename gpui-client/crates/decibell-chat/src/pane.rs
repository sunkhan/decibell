//! [`ChatPane`]: a server text channel's chat (`ChatPanel.tsx` without its composer): the
//! message list, what stands in for it while there is nothing to list (loading, retry, the
//! welcome banner), the composer slot under it, and the delete confirmation.
//!
//! The pane observes exactly what it draws: its slice's flags, the channel cache's events
//! (re-sync, outside jumps) and, when it follows the navigation, the active channel.

use std::rc::Rc;
use std::time::{Duration, Instant};

use decibell_model::chat::{CacheEvent, MessageSlice};
use decibell_model::stores::navigation::NavEvent;
use decibell_model::{ChannelId, Model, ServerId, actions};
use decibell_ui::motion::{self, Transition};
use decibell_ui::{ActiveTheme as _, Button, ConfirmModal, ConfirmTone, FontRole, Icon, IconName, Label};
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, Render, StyleRefinement, Subscription, Task, Window, canvas, div,
    prelude::*, px,
};

use crate::list::{ListEvent, MessageList};
use crate::media::slots;
use crate::source::ChannelSource;

/// The composer card at rest (`COMPOSER_REST_PX`): media is sized against the pane less this.
const COMPOSER_REST: f32 = 70.;

/// The list of the chat pane that was shown last (the live scroll benchmark drives it).
pub(crate) struct ShownList(pub gpui::WeakEntity<MessageList>);

impl gpui::Global for ShownList {}

/// The chat pane that showed a channel last (self-tests read its delete confirmation).
pub(crate) struct ShownPane(pub gpui::WeakEntity<ChatPane>);

impl gpui::Global for ShownPane {}

struct Shown {
    server: ServerId,
    channel: ChannelId,
    slice: Entity<MessageSlice>,
    list: Entity<MessageList>,
    composer: Option<crate::media::ComposerParts>,
    /// The registered welcome banner, built the first time this channel shows empty.
    welcome: Option<gpui::AnyView>,
    /// The newest-page load got no answer: Retry shows.
    failed: bool,
    _load: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// "Loading history…" stays blank this long, then fades up (`animate-[fadeUp_0.3s_ease_0.3s_both]`:
/// most pages land within the delay and the pane goes straight from blank to the channel).
const LOADING_DELAY: Duration = Duration::from_millis(300);
const LOADING_FADE: Transition = Transition::new(300, motion::EASE);
const LOADING_LIFT: f32 = 6.;

/// What stands in the list's place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// The newest page hasn't come yet (or a load runs with nothing to show).
    Loading,
    /// The newest page got no answer: "Couldn't load messages." and Retry.
    Failed,
    /// Nobody wrote here yet.
    Welcome,
    List,
}

pub struct ChatPane {
    shown: Option<Shown>,
    /// The message whose deletion waits for a confirmation.
    confirm_delete: Option<i64>,
    /// While the loading state lasts: its 300 ms delay, then whether the text shows.
    loading_timer: Option<Task<()>>,
    loading_shown: bool,
    _subscriptions: Vec<Subscription>,
}

impl ChatPane {
    /// A pane on one channel.
    pub fn new(server: ServerId, channel: ChannelId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut pane = Self { shown: None, confirm_delete: None, loading_timer: None, loading_shown: false, _subscriptions: Vec::new() };
        pane.watch_cache(cx);
        pane.show(Some((server, channel)), window, cx);
        pane
    }

    /// The pane the shell puts in `Slot::Chat`: it shows the navigation's active text channel
    /// and follows it (a fresh list per channel, as the renderer mounts one per channel key).
    pub fn for_active_channel(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut pane = Self { shown: None, confirm_delete: None, loading_timer: None, loading_shown: false, _subscriptions: Vec::new() };
        pane.watch_cache(cx);
        pane._subscriptions.push(cx.subscribe_in(&model.nav, window, |this, nav, event: &NavEvent, window, cx| {
            let NavEvent::ChannelActivated { .. } = event;
            let active = nav.read(cx).active_channel_key().map(|(server, channel)| (server, channel.clone()));
            this.show(active, window, cx);
        }));
        let active = model.nav.read(cx).active_channel_key().map(|(server, channel)| (server, channel.clone()));
        pane.show(active, window, cx);
        pane
    }

    fn watch_cache(&mut self, cx: &mut Context<Self>) {
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        self._subscriptions.push(cx.subscribe(&model.chat.channels, |this, _, event: &CacheEvent, cx| match event {
            CacheEvent::Resync(server) => {
                // The channel on screen catches up after its community reconnected.
                if let Some(shown) = this.shown.as_ref().filter(|shown| shown.server == *server) {
                    let at_bottom = shown.list.read(cx).at_bottom();
                    actions::chat::resync_displayed(shown.server, shown.channel.clone(), at_bottom, cx);
                }
            }
            CacheEvent::JumpRequested => this.take_jump(cx),
        }));
    }

    pub fn list(&self) -> Option<&Entity<MessageList>> {
        self.shown.as_ref().map(|shown| &shown.list)
    }

    /// The message whose delete confirmation is open.
    pub fn confirming_delete(&self) -> Option<i64> {
        self.confirm_delete
    }

    /// What the pane shows in the list's place (`ChatPanel.tsx`'s conditions):
    /// - loading while the newest page isn't in and hasn't failed, or a load runs with no rows;
    /// - Retry when that page failed and there are no rows or the page never came (live rows that
    ///   arrived meanwhile don't hide the failure);
    /// - the welcome banner with no rows; else the list.
    pub fn body(&self, cx: &App) -> Option<Body> {
        let shown = self.shown.as_ref()?;
        let slice = shown.slice.read(cx);
        Some(body_of(slice.rows().len(), slice.fetched, slice.loading, shown.failed))
    }

    /// Whether "Loading history…" is drawn now (after its delay).
    pub fn loading_text_shown(&self) -> bool {
        self.loading_shown
    }

    /// Starts the loading text's delay when the pane enters the loading state, and forgets it when
    /// it leaves (each wait starts blank).
    fn follow_loading(&mut self, cx: &mut Context<Self>) {
        let loading = self.body(cx) == Some(Body::Loading);
        if !loading {
            self.loading_timer = None;
            self.loading_shown = false;
            return;
        }
        if self.loading_timer.is_none() && !self.loading_shown {
            self.loading_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LOADING_DELAY).await;
                let _ = this.update(cx, |this, cx| {
                    this.loading_timer = None;
                    this.loading_shown = true;
                    cx.notify();
                });
            }));
        }
    }

    fn show(&mut self, target: Option<(ServerId, ChannelId)>, window: &mut Window, cx: &mut Context<Self>) {
        if let (Some(shown), Some((server, channel))) = (&self.shown, &target)
            && shown.server == *server
            && shown.channel == *channel
        {
            return;
        }
        self.confirm_delete = None;
        self.loading_timer = None;
        self.loading_shown = false;
        // Dropping the old list saves its position in its slice.
        self.shown = None;
        let model = Model::global(cx).clone();
        let Some((server, channel)) = target else {
            model.chat.channels.update(cx, |cache, _| cache.set_displayed(None));
            cx.notify();
            return;
        };
        model.chat.channels.update(cx, |cache, _| cache.set_displayed(Some((server, channel.clone()))));
        let source = Rc::new(ChannelSource { server, channel: channel.clone() });
        let list = cx.new(|cx| MessageList::new(source.clone(), cx));
        let slice = crate::source::ListSource::slice(&*source, cx);
        let subscriptions = vec![
            // The flags that pick loading / retry / welcome / the list.
            cx.observe(&slice, |this, _, cx| {
                this.take_jump(cx);
                this.follow_loading(cx);
                cx.notify();
            }),
            cx.subscribe_in(&list, window, |this, list, event: &ListEvent, _, cx| this.list_event(list, event, cx)),
        ];
        let composer = slots(cx).composer.clone().map(|build| build(server, channel.clone(), &list, window, cx));
        // First visit: the newest page (shared with any other caller; `false`: no answer).
        let load = actions::chat::open_channel(server, channel.clone(), cx).map(|load| {
            cx.spawn(async move |this, cx| {
                let ok = load.await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(shown) = this.shown.as_mut().filter(|shown| shown.server == server) {
                        shown.failed = !ok;
                        this.follow_loading(cx);
                        cx.notify();
                    }
                });
            })
        });
        cx.set_global(ShownList(list.downgrade()));
        cx.set_global(ShownPane(cx.entity().downgrade()));
        self.shown = Some(Shown { server, channel, slice, list, composer, welcome: None, failed: false, _load: load, _subscriptions: subscriptions });
        self.take_jump(cx);
        self.follow_loading(cx);
        cx.notify();
    }

    /// A jump asked for from outside (the Transfers panel, search): run it once this channel's
    /// first page is in.
    fn take_jump(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = &self.shown else { return };
        if !shown.slice.read(cx).fetched {
            return;
        }
        let model = Model::global(cx).clone();
        let (server, channel) = (shown.server, shown.channel.clone());
        let id = model.chat.channels.update(cx, |cache, _| cache.take_jump_request(server, &channel, Instant::now()));
        if let Some(id) = id {
            shown.list.update(cx, |list, cx| list.jump_to_message(id, cx));
        }
    }

    fn list_event(&mut self, list: &Entity<MessageList>, event: &ListEvent, cx: &mut Context<Self>) {
        match event {
            ListEvent::Reply(id) => list.update(cx, |list, cx| list.set_reply_target(Some(*id), cx)),
            ListEvent::Delete { id, skip_confirm: true } => self.delete(*id, cx),
            ListEvent::Delete { id, .. } => {
                self.confirm_delete = Some(*id);
                cx.notify();
            }
            ListEvent::OpenLink(url) => cx.open_url(url),
            // The image viewer is the attachments crate's.
            ListEvent::OpenMedia(_) | ListEvent::AtBottom(_) => {}
        }
    }

    fn delete(&mut self, id: i64, cx: &mut Context<Self>) {
        if let Some(shown) = &self.shown {
            actions::chat::delete_message(shown.server, shown.channel.clone(), id, cx);
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = self.shown.as_mut() else { return };
        shown.failed = false;
        let (server, load) = (shown.server, actions::chat::load_latest(shown.server, shown.channel.clone(), cx));
        shown._load = Some(cx.spawn(async move |this, cx| {
            let ok = load.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(shown) = this.shown.as_mut().filter(|shown| shown.server == server) {
                    shown.failed = !ok;
                    this.follow_loading(cx);
                    cx.notify();
                }
            });
        }));
        self.follow_loading(cx);
        cx.notify();
    }
}

/// `ChatPanel.tsx`: `awaitingFirstPage || (loading && messages.length === 0)` → loading;
/// `(messages.length === 0 || !firstPageIn) && failedKey === activeKey` → Retry; no rows → the
/// welcome; else the list.
fn body_of(rows: usize, fetched: bool, loading: bool, failed: bool) -> Body {
    let awaiting_first_page = !fetched && !failed;
    if awaiting_first_page || (loading && rows == 0) {
        Body::Loading
    } else if (rows == 0 || !fetched) && failed {
        Body::Failed
    } else if rows == 0 {
        Body::Welcome
    } else {
        Body::List
    }
}

#[cfg(test)]
mod body_tests {
    use super::{Body, body_of};

    #[test]
    fn what_stands_in_the_lists_place() {
        // (rows, fetched, loading, failed)
        assert_eq!(body_of(0, false, false, false), Body::Loading, "the frame before the load starts");
        assert_eq!(body_of(0, false, true, false), Body::Loading);
        assert_eq!(body_of(3, false, true, false), Body::Loading, "live rows before the first page: still waiting for it");
        assert_eq!(body_of(0, true, true, false), Body::Loading, "a reload with nothing to show");
        assert_eq!(body_of(5, true, true, false), Body::List, "a page loading above rows");
        assert_eq!(body_of(0, false, false, true), Body::Failed);
        assert_eq!(body_of(4, false, false, true), Body::Failed, "live rows don't hide a first page that never came");
        assert_eq!(body_of(0, true, false, true), Body::Failed);
        assert_eq!(body_of(4, true, false, true), Body::List, "a later failure with the page in keeps the list");
        assert_eq!(body_of(0, true, false, false), Body::Welcome);
        assert_eq!(body_of(2, true, false, false), Body::List);
    }
}

fn centered() -> gpui::Div {
    div().flex_1().flex().flex_col().items_center().justify_center()
}

/// `WelcomeState`: the banner of a channel nobody wrote in yet.
fn welcome(server: ServerId, channel: &ChannelId, cx: &mut Context<ChatPane>) -> impl IntoElement + use<> {
    let model = Model::global(cx).clone();
    let name = model
        .community(server, cx)
        .and_then(|community| community.channels.read(cx).get(channel).map(|channel| channel.name.clone()))
        .unwrap_or_else(|| channel.to_string().into());
    // `canCreateInvite || canManageInvites` / `canManageChannel || canManageChannelRoles`.
    use decibell_model::stores::permissions::perm as perms;
    let can = |bits: &[u64], cx: &App| bits.iter().any(|bit| model.channel_permission(server, channel, *bit, cx));
    let invite = can(&[perms::CREATE_INVITE, perms::MANAGE_INVITES], cx);
    let manage = can(&[perms::MANAGE_CHANNELS, perms::MANAGE_ROLES], cx);
    let theme = cx.theme();
    let button = |id: &'static str, icon: IconName, label: &'static str, primary: bool| {
        let (background, hover, text) = if primary {
            (theme.color_accent, theme.color_accent_hover, theme.color_on_accent)
        } else {
            (theme.color_bg_light, theme.color_bg_lighter, theme.color_text_muted)
        };
        // `.welcome-btn`: `padding: 9px 16px; gap: 7px; radius-md; 13px / 500`.
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(7.))
            .px_4()
            .py(px(9.))
            .rounded(theme.radius_md)
            .bg(background)
            .hover(move |style| style.bg(hover))
            .cursor_pointer()
            .child(Icon::new(icon).size(px(15.)).color(text))
            .child(Label::new(label).size(13.).weight(500.).color(text).nowrap())
    };
    let channel_id = channel.clone();
    centered().child(
        div()
            .flex()
            .flex_col()
            .items_center()
            .max_w(px(420.))
            .child(
                // `.welcome-icon-wrap`: a 72 px tile with the hash.
                div()
                    .size(px(72.))
                    .mb_5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_xl)
                    .bg(theme.color_accent_soft)
                    .child(Label::new("#").font(FontRole::Display).size(34.).weight(700.).color(theme.color_accent_bright)),
            )
            .child(div().mb_2().child(Label::new(format!("Welcome to #{name}")).font(FontRole::Display).size(26.).weight(700.).tracking(-0.01).nowrap()))
            .child(
                div().mb(px(28.)).child(
                    Label::new(format!("This is the start of the #{name} channel. Invite your squad and get the conversation going."))
                        .size(14.)
                        .line_height(14. * 1.6)
                        .color(theme.color_text_muted),
                ),
            )
            .when(invite || manage, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap(px(10.))
                        .when(invite, |this| {
                            this.child(button("welcome-invite", IconName::LucideUserPlus, "Invite friends", true).on_click(move |_, _, cx| {
                                let model = Model::global(cx).clone();
                                model.overlays.update(cx, |overlays, cx| overlays.open_modal("invite-friends", cx));
                            }))
                        })
                        .when(manage, |this| {
                            this.child(button("welcome-manage", IconName::LucideSettings, "Manage channel", false).on_click(move |_, _, cx| {
                                let model = Model::global(cx).clone();
                                model.overlays.update(cx, |overlays, cx| overlays.open_channel_settings(channel_id.clone(), cx));
                            }))
                        })
                )
            }),
    )
}

impl Render for ChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (background, muted) = {
            let theme = cx.theme();
            (theme.color_bg_mid, theme.color_text_muted)
        };
        // A registered welcome banner, built the first time this channel shows empty.
        if let Some(shown) = self.shown.as_mut()
            && shown.welcome.is_none()
            && let Some(build) = slots(cx).welcome.clone()
        {
            let empty = {
                let slice = shown.slice.read(cx);
                slice.rows().is_empty() && slice.fetched && !slice.loading
            };
            if empty && !shown.failed {
                shown.welcome = Some(build(shown.server, shown.channel.clone(), window, cx));
            }
        }
        let root = div().size_full().min_w_0().flex().flex_col().bg(background);
        let Some(shown) = &self.shown else {
            return root.child(centered().child(Label::new("Pick a channel.").size(14.).color(muted)));
        };
        let state = self.body(cx).unwrap_or(Body::Loading);
        // The pane's size is what media is scaled against (`chatViewSize`).
        let view_size = shown.list.read(cx).view_size_cell();
        let probe = canvas(
            move |bounds, _, _| {
                let (width, height) = (f32::from(bounds.size.width).round(), f32::from(bounds.size.height).round());
                view_size.set((width, (height - COMPOSER_REST).max(0.)));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let body = match state {
            Body::Loading if !self.loading_shown => centered().into_any_element(),
            Body::Loading => {
                // Each wait fades in from blank: the transition's state goes with the text (GPUI
                // drops an element's state in the first frame that doesn't draw it), and a channel
                // switch restarts the delay.
                let t = motion::transition_from(window, cx, "loading-history", 0., 1., LOADING_FADE);
                centered()
                    .child(
                        div()
                            .relative()
                            .top(px(LOADING_LIFT * (1. - t)))
                            .opacity(t)
                            .child(Label::new("Loading history…").size(14.).color(muted)),
                    )
                    .into_any_element()
            }
            Body::Failed => centered()
                .gap_3()
                .child(Label::new("Couldn't load messages.").size(14.).color(muted))
                .child(Button::new("retry-history", "Retry").on_click(cx.listener(|this, _, _, cx| this.retry(cx))))
                .into_any_element(),
            Body::Welcome => match &shown.welcome {
                Some(view) => centered().child(view.clone()).into_any_element(),
                None => welcome(shown.server, &shown.channel, cx).into_any_element(),
            },
            Body::List => {
                div().flex_1().min_h_0().child(shown.list.clone().cached(StyleRefinement::default().size_full())).into_any_element()
            }
        };
        let confirm = self.confirm_delete;
        // "Type anywhere to compose": a key that reaches the pane from the list (or with nothing
        // focused) goes to the composer (`useTypeToFocusComposer`).
        let type_to_focus = shown.composer.as_ref().map(|composer| composer.type_to_focus.clone());
        let list_focus = shown.list.read(cx).focus_handle().clone();
        root.relative()
            .when_some(type_to_focus, |root, hook| {
                root.on_key_down(move |event, window, cx| {
                    let focused = window.focused(cx);
                    if (focused.is_none() || focused.as_ref() == Some(&list_focus)) && hook(event, window, cx) {
                        cx.stop_propagation();
                    }
                })
            })
            .child(probe)
            .child(div().flex_1().min_h_0().flex().flex_col().overflow_hidden().child(body))
            .children(shown.composer.as_ref().map(|composer| composer.view.clone()))
            .child(
                ConfirmModal::new("delete-message", confirm.is_some(), "Delete message", "Delete")
                    .message("Delete this message? This cannot be undone.")
                    .child(decibell_ui::delete_message_tip(cx))
                    .tone(ConfirmTone::Danger)
                    .on_confirm(cx.listener(|this, _, _, cx| {
                        if let Some(id) = this.confirm_delete.take() {
                            this.delete(id, cx);
                        }
                        cx.notify();
                    }))
                    .on_cancel(cx.listener(|this, _, _, cx| {
                        this.confirm_delete = None;
                        cx.notify();
                    })),
            )
    }
}
