//! The signed-in layout (`layouts/MainLayout.tsx`):
//!
//! ```text
//!   [Connection lost. Reconnecting...]          central's link is down
//!   [crash-report notice]                       until dismissed once
//!   ServerBar (home · unread DMs · tiles · +)
//!   ╭─ sidebar group ─┬─ pane ──────────────────────────╮   in an 8 px chrome gutter, rounded,
//!   │ channels / DMs  │ server: header + chat + right   │   bordered; browse fills it alone
//!   │ [user panel]    │ home · dm · voice               │
//!   ╰─────────────────┴─────────────────────────────────╯
//! ```
//!
//! Every part is its own cached view; this one re-renders only when the view (home, server,
//! voice, dm, browse) changes, the banner comes or goes, or the user panel's height moves.

use decibell_common::Watch;
use decibell_model::stores::connections::CentralStatus;
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{Model, actions};
use decibell_ui::{Icon, IconName, Label, title_tooltip};
use gpui::{
    AnyView, AppContext as _, ClickEvent, Context, Entity, IntoElement, Render, StyleRefinement, Window, div,
    prelude::*, px,
};

use crate::channels::ChannelSidebar;
use crate::conversations::ConversationSidebar;
use crate::friends::FriendsPage;
use crate::overlays::{LeaveServerOverlay, RevokedNoticeOverlay};
use crate::server_bar::{BAR_HEIGHT, ServerBar};
use crate::server_pane::ServerPane;
use crate::slots::{PaneHost, Slot, overlay_builders};
use crate::user_panel::{PanelHeight, UserPanel};

/// What of the stores this view shows.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Shown {
    view: ActiveView,
    reconnecting: bool,
    consent_shown: bool,
}

pub struct MainView {
    shown: Shown,
    panel: Entity<PanelHeight>,
    server_bar: Entity<ServerBar>,
    user_panel: Entity<UserPanel>,
    server_sidebar: Option<Entity<ChannelSidebar>>,
    dm_sidebar: Option<Entity<ConversationSidebar>>,
    server_pane: Option<Entity<ServerPane>>,
    pane: PaneHost,
    overlays: Vec<AnyView>,
    _watch: Watch,
}

fn shown(model: &Model, cx: &gpui::App) -> Shown {
    Shown {
        view: model.nav.read(cx).active_view,
        reconnecting: model.connections.read(cx).central == CentralStatus::Reconnecting,
        consent_shown: model.settings.read(cx).get().crash_reporting_consent_shown,
    }
}

impl MainView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let panel = cx.new(|_| PanelHeight(PanelHeight::RESTING));
        let mut watch = Watch::new();
        let check = |this: &mut MainView, cx: &gpui::App| {
            let next = shown(&Model::global(cx).clone(), cx);
            std::mem::replace(&mut this.shown, next) != next
        };
        watch
            .store_if(&model.nav, cx, move |this, _, cx| check(this, cx))
            .store_if(&model.connections, cx, move |this, _, cx| check(this, cx))
            .store_if(&model.settings, cx, move |this, _, cx| check(this, cx))
            // The sidebar's and the user panel's sizes (their cached views are laid out at them).
            .event(&model.layout, cx, |_, e: &decibell_model::stores::layout::LayoutEvent| {
                *e == decibell_model::stores::layout::LayoutEvent::Sidebar
            });
        let mut overlays: Vec<AnyView> = vec![
            cx.new(LeaveServerOverlay::new).into(),
            cx.new(RevokedNoticeOverlay::new).into(),
        ];
        for build in overlay_builders(cx) {
            overlays.push(build(window, cx));
        }
        // The renderer asks for these as the main layout mounts.
        cx.defer(|cx| {
            actions::friends::request_friend_list(cx);
            actions::servers::request_server_list(cx);
        });
        watch.store(&panel, cx);
        Self {
            shown: shown(&model, cx),
            server_bar: cx.new(|cx| ServerBar::new(window, cx)),
            user_panel: cx.new(|cx| UserPanel::new(panel.clone(), cx)),
            panel,
            server_sidebar: None,
            dm_sidebar: None,
            server_pane: None,
            pane: PaneHost::default(),
            overlays,
            _watch: watch,
        }
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Shown { view, reconnecting, consent_shown } = self.shown;
        let sidebar_width = Model::global(cx).layout.read(cx).prefs().sidebar_width as f32;
        let panel_height = self.panel.read(cx).0;
        let ink = crate::ink::base(cx);
        let chrome = crate::ink::chrome(cx);
        // Inside the workspace's 1 px border: corners that paint must be rounded themselves
        // (GPUI clips to the rectangle, not the rounded border).
        let inner = ink.radius_lg - px(1.);

        // The sidebar for the view, built on first show and dropped when it goes.
        let sidebar: Option<AnyView> = match view {
            ActiveView::Server | ActiveView::Voice => {
                self.dm_sidebar = None;
                let panel = self.panel.clone();
                Some(self.server_sidebar.get_or_insert_with(|| cx.new(|cx| ChannelSidebar::new(panel, cx))).clone().into())
            }
            ActiveView::Home | ActiveView::Dm => {
                self.server_sidebar = None;
                let panel = self.panel.clone();
                Some(self.dm_sidebar.get_or_insert_with(|| cx.new(|cx| ConversationSidebar::new(panel, cx))).clone().into())
            }
            ActiveView::Browse => {
                self.server_sidebar = None;
                self.dm_sidebar = None;
                None
            }
        };
        let pane: AnyView = match view {
            ActiveView::Server => {
                self.pane.clear();
                self.server_pane.get_or_insert_with(|| cx.new(ServerPane::new)).clone().into()
            }
            other => {
                self.server_pane = None;
                let slot = match other {
                    ActiveView::Home => Slot::Home,
                    ActiveView::Dm => Slot::Dm,
                    ActiveView::Voice => Slot::Voice,
                    _ => Slot::Browse,
                };
                let home = |_: &mut Window, cx: &mut gpui::App| -> AnyView { cx.new(FriendsPage::new).into() };
                let fallback: Option<&crate::slots::PaneFallback> = if slot == Slot::Home { Some(&home) } else { None };
                self.pane.view(slot, window, cx, fallback)
            }
        };

        let workspace = div()
            .flex()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .rounded(ink.radius_lg)
            .border_1()
            .border_color(ink.color_border)
            .when_some(sidebar, |this, sidebar| {
                this.child(
                    div()
                        .relative()
                        .flex()
                        .flex_none()
                        .h_full()
                        .rounded_l(inner)
                        .overflow_hidden()
                        // A cached view is laid out at the size it is given: the sidebar's width,
                        // the user panel's measured height.
                        .child(sidebar.cached(StyleRefinement::default().w(px(sidebar_width)).h_full()))
                        .child(
                            div()
                                .absolute()
                                .bottom_2()
                                .left_2()
                                .right_2()
                                .h(px(panel_height))
                                .child(self.user_panel.clone().cached(StyleRefinement::default().size_full())),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when(view == ActiveView::Browse, |this| this.rounded(inner))
                    .when(view != ActiveView::Browse, |this| this.rounded_r(inner))
                    .overflow_hidden()
                    .child(pane.cached(StyleRefinement::default().size_full())),
            );

        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .when(reconnecting, |this| {
                this.child(
                    div().flex().flex_none().h_8().items_center().justify_center().bg(ink.color_warning).child(
                        Label::new("Connection lost. Reconnecting...").size(12.).weight(600.).color(ink.color_bg_primary),
                    ),
                )
            })
            .when(!consent_shown, |this| this.child(crash_reporting_banner(&ink)))
            .child(self.server_bar.clone().cached(StyleRefinement::default().w_full().h(px(BAR_HEIGHT))))
            .child(
                // `chrome-ground`: the gutter takes the chrome palette's ground colour only.
                div().flex().flex_1().min_h_0().px_2().pb_2().bg(chrome.color_bg_darkest).child(workspace),
            )
            // Overlays size and place themselves (absolute, deferred): drawn uncached, they render
            // with this view, which is rare, and when they notify.
            .children(self.overlays.iter().map(|overlay| overlay.clone().into_any_element()))
    }
}

/// The first-launch notice (`CrashReportingBanner`): shown until dismissed, never again after.
fn crash_reporting_banner(ink: &crate::ink::Ink) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .h_9()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(ink.color_border_divider)
        .bg(ink.color_accent_soft)
        .px_4()
        .child(
            Label::new("Decibell sends anonymous crash reports to help us fix bugs. You can disable this in Settings → Privacy.")
                .size(12.)
                .color(ink.color_accent_bright),
        )
        .child(
            div()
                .id("crash-banner-dismiss")
                .ml_3()
                .flex()
                .flex_none()
                .size_6()
                .items_center()
                .justify_center()
                .rounded(ink.radius_sm)
                .cursor_pointer()
                .hover(|s| s.bg(ink.color_accent_mid))
                .tooltip(title_tooltip("Dismiss"))
                .on_click(|_: &ClickEvent, _, cx| {
                    let settings = Model::global(cx).settings.clone();
                    settings.update(cx, |s, cx| s.change(|v| v.crash_reporting_consent_shown = true, cx));
                })
                .child(Icon::new(IconName::SettingsX).size(px(12.)).stroke_width(2.5).color(ink.color_accent_bright)),
        )
}
