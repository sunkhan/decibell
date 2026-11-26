//! The DM view ([`decibell_shell::Slot::Dm`]): the open conversation ([`Conversation`], one per
//! peer, as the renderer mounts a fresh panel per `activeDmUser`) and the Friends rail beside it
//! while `Layout::prefs().dm_friends_panel` is on (`MainLayout.tsx`' `dm` branch).
//!
//! **The composer slot.** The composer is another crate's; it registers once with
//! [`set_dm_composer`] and is built per conversation with the peer and the conversation's
//! [`MessageList`] (it subscribes to the list's `ListEvent::Reply`, shows the rail with
//! `set_reply_target`, starts an edit with `start_edit`). Until one registers, the pane draws
//! [`composer::FallbackComposer`]: the renderer's card with a working one-line field (Enter sends
//! through `actions::dm::send_dm`), no emoji picker, no reply bar.
//!
//! **The call stage slot.** [`decibell_shell::Slot::DmCallStage`]: the voice crate registers the
//! stage; the conversation builds it while a call with its peer is outgoing, connecting or active
//! (`CallStage.tsx`' own condition) and draws it above the list, uncached (it sizes itself).

mod banner;
pub mod composer;
mod conversation;
mod header;
mod source;

use std::rc::Rc;

use decibell_chat::MessageList;
use decibell_common::Watch;
use decibell_model::dm::DmEvent;
use decibell_model::stores::layout::LayoutEvent;
use decibell_model::{Model, Uid};
use decibell_ui::{ActiveTheme as _, Label};
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, Global, IntoElement, Render, StyleRefinement, Window, div,
    prelude::*, px,
};

pub use banner::{Notice, notice};
pub use conversation::{Body, Conversation};
pub use source::DmSource;

use crate::friends::FriendsRail;
use crate::friends::RAIL_WIDTH;

/// Builds the DM composer for a conversation: the peer and the conversation's message list.
pub type DmComposerBuilder = Rc<dyn Fn(Uid, Entity<MessageList>, &mut Window, &mut App) -> AnyView>;

#[derive(Default)]
struct DmSlots {
    composer: Option<DmComposerBuilder>,
}

impl Global for DmSlots {}

/// Register the DM composer (once at start-up, from the composer crate's `init`).
pub fn set_dm_composer(cx: &mut App, build: impl Fn(Uid, Entity<MessageList>, &mut Window, &mut App) -> AnyView + 'static) {
    cx.default_global::<DmSlots>().composer = Some(Rc::new(build));
}

pub(crate) fn dm_composer(cx: &App) -> Option<DmComposerBuilder> {
    cx.try_global::<DmSlots>().and_then(|slots| slots.composer.clone())
}

pub struct DmPane {
    shown: Option<(Uid, Entity<Conversation>)>,
    rail: Option<Entity<FriendsRail>>,
    rail_on: bool,
    _watch: Watch,
}

impl DmPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .event(&model.dm.list, cx, |_, event: &DmEvent| matches!(event, DmEvent::ActiveChanged))
            .event(&model.layout, cx, |_, event: &LayoutEvent| *event == LayoutEvent::Panels);
        let _ = window;
        Self { shown: None, rail: None, rail_on: false, _watch: watch }
    }

    /// The conversation on screen (tests).
    pub fn conversation(&self) -> Option<&Entity<Conversation>> {
        self.shown.as_ref().map(|(_, c)| c)
    }
}

impl Render for DmPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let active = model.dm.list.read(cx).active_peer;
        // A fresh conversation per peer (the renderer keys its state on `activeDmUser`).
        if self.shown.as_ref().map(|(peer, _)| *peer) != active {
            self.shown = active.map(|peer| (peer, cx.new(|cx| Conversation::new(peer, window, cx))));
        }
        self.rail_on = model.layout.read(cx).prefs().dm_friends_panel;
        if self.rail_on {
            if self.rail.is_none() {
                self.rail = Some(cx.new(|cx| FriendsRail::new(window, cx)));
            }
        } else {
            self.rail = None;
        }
        let theme = cx.theme();
        let main = match &self.shown {
            Some((_, conversation)) => div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(conversation.clone().cached(StyleRefinement::default().size_full()))
                .into_any_element(),
            None => div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .bg(theme.color_bg_mid)
                .child(Label::new("Select a conversation or start a new one").size(14.).color(theme.color_text_muted))
                .into_any_element(),
        };
        div().size_full().flex().child(main).children(self.rail.clone().map(|rail| {
            div().flex_none().w(px(RAIL_WIDTH)).h_full().child(rail.cached(StyleRefinement::default().size_full()))
        }))
    }
}
