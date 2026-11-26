//! The content screens, drawn from [`crate::fixture`] (no network): `content-attachments`,
//! `content-embeds` (and the viewer's and the Transfers panel's, registered by their modules).

use std::rc::Rc;

use decibell_chat::{ListEvent, MessageList};
use decibell_harness::Screen;
use decibell_ui::{ActiveTheme as _, AssumeClientDecorations};
use gpui::{AnyView, App, AppContext as _, Context, Entity, IntoElement, Render, StyleRefinement, Subscription, Window, canvas, div, prelude::*, px};

use crate::fixture;

/// A channel of the content fixture in the chat pane's frame (the list over the resting composer's
/// box, as the chat crate's fixture pane and the Electron captures draw it), with the overlays.
pub struct ContentPane {
    pub list: Entity<MessageList>,
    overlays: Vec<AnyView>,
    _subscription: Subscription,
}

impl ContentPane {
    pub fn new(channel: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        if cx.has_global::<AssumeClientDecorations>() {
            cx.set_reduce_motion(true);
        }
        let fetcher = fixture::fetcher(cx);
        let payloads = match channel {
            fixture::EMBEDS => fixture::embed_payloads(),
            _ => fixture::attachment_payloads(&fetcher),
        };
        let source: Rc<dyn decibell_chat::ListSource> = fixture::open(channel, payloads, cx);
        let list = cx.new(|cx| MessageList::new(source, cx));
        let subscription = cx.subscribe(&list, |_, _, event: &ListEvent, _| log::info!("[content fixture] {event:?}"));
        // What the shell draws over the main layout: the menu, the viewer.
        let overlays: Vec<AnyView> = vec![cx.new(crate::menu::MediaMenu::new).into(), crate::viewer::overlay(window, cx)];
        Self { list, overlays, _subscription: subscription }
    }
}

impl Render for ContentPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, card, border, radius) = (theme.color_bg_mid, theme.color_bg_light, theme.color_border, theme.radius_lg);
        let view_size = self.list.read(cx).view_size_cell();
        let probe = canvas(
            move |bounds, _, _| view_size.set((f32::from(bounds.size.width).round(), (f32::from(bounds.size.height).round() - 70.).max(0.))),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .child(probe)
            .child(div().flex_1().min_h_0().child(self.list.clone().cached(StyleRefinement::default().size_full())))
            .child(div().flex_none().px_3().py_2().child(div().h(px(56.)).rounded(radius).border_1().border_color(border).bg(card)))
            .children(self.overlays.iter().cloned())
    }
}

fn pane(channel: &'static str, window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| ContentPane::new(channel, window, cx)).into()
}

pub fn screens() -> Vec<Screen> {
    let mut screens = vec![
        Screen {
            name: "content-attachments",
            about: "Attachments of every kind and grid shape: pictures (loaded, waiting, failed), a GIF, videos, audio, documents, a tombstone",
            build: |window, cx| pane(fixture::ATTACHMENTS, window, cx),
        },
        Screen {
            name: "content-embeds",
            about: "Link previews (site cards with a thumbnail, a large picture, none), picture and GIF links, invite cards",
            build: |window, cx| pane(fixture::EMBEDS, window, cx),
        },
    ];
    screens.extend(crate::viewer::screens());
    screens
}
