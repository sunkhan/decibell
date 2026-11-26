//! The chat screens, drawn from [`crate::fixture`] (no network): `decibell --screen chat-fixture`
//! and `chat-features`, and the pane the self-tests and the benchmark drive.

use std::rc::Rc;

use decibell_harness::Screen;
use decibell_ui::{ActiveTheme as _, AssumeClientDecorations, ConfirmModal, ConfirmTone};
use gpui::{App, AppContext as _, Context, Entity, IntoElement, Render, StyleRefinement, Subscription, Window, canvas, div, prelude::*, px};

use crate::fixture::{self, FixtureSource, Which};
use crate::list::{ListEvent, MessageList};
use crate::source::ListSource;

/// A chat pane over a fixture channel: the list in the pane's frame, the delete confirmation,
/// and (for the tests) a column of adjustable width beside it, standing in for the member list.
pub struct FixturePane {
    pub source: Rc<FixtureSource>,
    pub list: Entity<MessageList>,
    /// px taken from the list's right (the member list opening and closing).
    pub side: f32,
    /// Draw the resting composer's box under the list (the composer itself is another crate's;
    /// the box keeps the list the height it has in the app and in the Electron captures).
    pub composer: bool,
    confirm_delete: Option<i64>,
    _subscription: Subscription,
}

impl FixturePane {
    pub fn new(which: Which, cx: &mut Context<Self>) -> Self {
        Self::with_pages(which, 1, cx)
    }

    /// `#general` with its `pages` newest pages loaded.
    pub fn with_pages(which: Which, pages: usize, cx: &mut Context<Self>) -> Self {
        // A capture renders one frame on a clock that does not run: entrances show their end.
        if cx.has_global::<AssumeClientDecorations>() {
            cx.set_reduce_motion(true);
        }
        let source = fixture::open_pages(which, pages, cx);
        let (list, subscription) = Self::mount(&source, cx);
        Self { source, list, side: 0., composer: true, confirm_delete: None, _subscription: subscription }
    }

    fn mount(source: &Rc<FixtureSource>, cx: &mut Context<Self>) -> (Entity<MessageList>, Subscription) {
        let list_source: Rc<dyn ListSource> = source.clone();
        let list = cx.new(|cx| MessageList::new(list_source, cx));
        let subscription = cx.subscribe(&list, |this, list, event: &ListEvent, cx| match event {
            ListEvent::Reply(id) => list.update(cx, |list, cx| list.set_reply_target(Some(*id), cx)),
            ListEvent::Delete { id, skip_confirm: true } => this.source.delete(*id, cx),
            ListEvent::Delete { id, .. } => {
                this.confirm_delete = Some(*id);
                cx.notify();
            }
            ListEvent::OpenLink(url) => log::info!("[chat fixture] open {url}"),
            ListEvent::OpenMedia(id) => log::info!("[chat fixture] open the media of {id}"),
        });
        (list, subscription)
    }

    /// Shows another fixture channel (or the same again): the old list is dropped, which saves
    /// its position in its slice, and a fresh one mounts, as a channel switch does.
    pub fn switch(&mut self, which: Which, cx: &mut Context<Self>) {
        let source = fixture::open(which, cx);
        self.remount(source, cx);
    }

    /// Mounts a fresh list on a source that is already loaded.
    pub fn remount(&mut self, source: Rc<FixtureSource>, cx: &mut Context<Self>) {
        let (list, subscription) = Self::mount(&source, cx);
        self.source = source;
        self.list = list;
        self._subscription = subscription;
        cx.notify();
    }

    pub fn set_side(&mut self, side: f32, cx: &mut Context<Self>) {
        self.side = side;
        cx.notify();
    }
}

impl Render for FixturePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, card, border, radius) = (theme.color_bg_mid, theme.color_bg_light, theme.color_border, theme.radius_lg);
        let confirm = self.confirm_delete;
        // The pane's size less the resting composer is what media is scaled against.
        let view_size = self.list.read(cx).view_size_cell();
        let probe = canvas(
            move |bounds, _, _| view_size.set((f32::from(bounds.size.width).round(), (f32::from(bounds.size.height).round() - 70.).max(0.))),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let column = div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(probe)
            .child(div().flex_1().min_h_0().child(self.list.clone().cached(StyleRefinement::default().size_full())))
            // ChatPanel's composer at rest: `px-3 py-2` around a `min-h-[54px] rounded-lg border bg-bg-light` card.
            .when(self.composer, |this| {
                this.child(div().flex_none().px_3().py_2().child(div().h(px(56.)).rounded(radius).border_1().border_color(border).bg(card)))
            });
        div()
            .size_full()
            .flex()
            .bg(background)
            .child(column)
            .when(self.side > 0., |this| this.child(div().w(px(self.side)).h_full().flex_none()))
            .child(
                ConfirmModal::new("delete-message", confirm.is_some(), "Delete message", "Delete")
                    .message("Delete this message? This cannot be undone.\n\nTip: hold Shift when clicking delete to skip this confirmation.")
                    .tone(ConfirmTone::Danger)
                    .on_confirm(cx.listener(|this, _, _, cx| {
                        if let Some(id) = this.confirm_delete.take() {
                            this.source.delete(id, cx);
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

fn pane(which: Which, cx: &mut App) -> gpui::AnyView {
    cx.new(|cx| FixturePane::new(which, cx)).into()
}

pub fn screens() -> Vec<Screen> {
    vec![
        Screen {
            name: "chat-fixture",
            about: "The message list on the 10,000-message fixture channel, at its newest page",
            build: |_, cx| pane(Which::General, cx),
        },
        Screen {
            name: "chat-features",
            about: "The message list on a hand-written channel: every row type and body feature",
            build: |_, cx| pane(Which::Features, cx),
        },
    ]
}
