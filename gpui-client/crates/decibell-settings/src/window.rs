//! The Settings window (`SettingsModal.tsx`): open while `model.overlays.active_modal` is
//! `"settings"` (the user panel's gear, the new sign-in toast's "Review devices"), on the kit's
//! `SettingsWindow` frame with the renderer's tabs in its order, the open tab from
//! `model.nav.settings_tab` (unknown → the first), Devices only when central serves the list
//! (`LoginResponse.devices`). Esc, ✕ and a press on the backdrop close it. The renderer's user
//! Settings hold no drafts (every control applies at once; Network's custom speed on blur), so
//! nothing holds the window: leaving a tab or closing applies what Network's box holds first, as the
//! blur of the renderer's leaving press does. Below 1024 px the frame folds its sidebar into the icon
//! rail.
//!
//! The window is uncached (an overlay of the main layout, it renders with it); the open tab is
//! its own view drawn cached at the pane's size, so a change in a tab redraws that tab only.
//! The renderer keeps the closing window mounted through its 300 ms fade: so does this, then the
//! tab's view goes (and with it what it held: a device list load, the mic meter).

use std::time::Duration;

use decibell_model::Model;
use decibell_model::stores::overlays::modal;
use decibell_ui::{NavItem, ScrollHandle, SettingsWindow};
use gpui::{
    App, Context, IntoElement, Render, StyleRefinement, Styled as _, Subscription, Task, WeakEntity, Window, div,
};

use crate::tabs::{TabId, TabView};

/// How a tab asks the window to close (its pane's ✕).
#[derive(Clone)]
pub struct CloseLink(pub(crate) WeakEntity<SettingsOverlay>);

impl CloseLink {
    pub fn request(&self, cx: &mut App) {
        if let Some(window) = self.0.upgrade() {
            window.update(cx, |w, cx| w.request_close(cx));
        }
    }

    /// For views made outside a window (unit tests).
    pub fn none() -> CloseLink {
        CloseLink(WeakEntity::new_invalid())
    }
}

/// The renderer's fade-out (`duration-300`): the tab's view stays this long after closing.
const FADE_OUT: Duration = Duration::from_millis(300);

pub struct SettingsOverlay {
    open: bool,
    /// Central serves the device list (`auth.devices`).
    devices: bool,
    /// `nav.settings_tab` as last seen.
    wanted: gpui::SharedString,
    view: Option<TabView>,
    /// Drops the view once the fade-out ends (exists only during it).
    fade: Option<Task<()>>,
    /// The frame's built-in pane is not used (each tab draws its own): a handle for its API.
    unused_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsOverlay {
    pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let weak = cx.weak_entity();
        crate::handles::update(cx, |h| h.settings = Some(weak));
        let subscriptions = vec![
            cx.observe(&model.overlays, |this, overlays, cx| {
                let open = overlays.read(cx).is_open(modal::SETTINGS);
                if open != this.open {
                    this.set_open(open, cx);
                }
            }),
            cx.observe(&model.nav, |this, nav, cx| {
                let tab = nav.read(cx).settings_tab.clone();
                if tab != this.wanted {
                    this.wanted = tab;
                    cx.notify();
                }
            }),
            cx.observe(&model.auth, |this, auth, cx| {
                let devices = auth.read(cx).devices;
                if devices != this.devices {
                    this.devices = devices;
                    cx.notify();
                }
            }),
        ];
        Self {
            open: model.overlays.read(cx).is_open(modal::SETTINGS),
            devices: model.auth.read(cx).devices,
            wanted: model.nav.read(cx).settings_tab.clone(),
            view: None,
            fade: None,
            unused_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.open = open;
        if open {
            self.fade = None;
            // A fresh window: the tab as it was when it opened (the renderer mounts it anew).
            self.view = None;
        } else {
            self.fade = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(FADE_OUT).await;
                let _ = this.update(cx, |this, cx| {
                    this.fade = None;
                    if !this.open {
                        this.view = None;
                        cx.notify();
                    }
                });
            }));
        }
        cx.notify();
    }

    /// The tabs shown (Devices only with central's support).
    pub fn tabs(&self) -> Vec<TabId> {
        TabId::ALL.into_iter().filter(|t| *t != TabId::Devices || self.devices).collect()
    }

    /// The tab shown: the one asked for, else the first.
    pub fn active(&self) -> TabId {
        TabId::from_id(&self.wanted)
            .filter(|t| self.tabs().contains(t))
            .unwrap_or(TabId::Account)
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn view(&self) -> Option<&TabView> {
        self.view.as_ref()
    }

    /// Esc, ✕, the backdrop: close (what the open tab holds unapplied applies first).
    pub fn request_close(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = &self.view {
            view.commit_typed(cx);
        }
        let overlays = Model::global(cx).overlays.clone();
        overlays.update(cx, |o, cx| o.close_modal(cx));
    }

    /// A tab in the sidebar: it opens (what the open tab holds unapplied applies first).
    pub fn select(&mut self, tab: TabId, cx: &mut Context<Self>) {
        if let Some(view) = &self.view
            && view.tab() != tab
        {
            view.commit_typed(cx);
        }
        let nav = Model::global(cx).nav.clone();
        nav.update(cx, |n, cx| n.set_settings_tab(tab.id(), cx));
    }
}

impl Render for SettingsOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open && self.view.is_none() {
            return div().into_any_element();
        }
        let active = self.active();
        // The open tab's view, built when it first shows (the renderer mounts the tab's
        // component); another tab's goes.
        if self.open && self.view.as_ref().is_none_or(|v| v.tab() != active) {
            let link = CloseLink(cx.weak_entity());
            self.view = Some(TabView::build(active, link, window, cx));
        }
        let shown = self.view.as_ref().map_or(active, TabView::tab);

        let mut frame = SettingsWindow::new("settings", self.open, &self.unused_scroll)
            .nav_label("Settings")
            .title(shown.label())
            .on_close(cx.listener(|this, _, _, cx| this.request_close(cx)));
        for tab in self.tabs() {
            frame = frame.item(
                NavItem::new(("settings-tab", tab as usize), tab.icon(), tab.label())
                    .probe(format!("settings-tab:{}", tab.id()))
                    .active(tab == shown)
                    .on_click(cx.listener(move |this, _, _, cx| this.select(tab, cx))),
            );
        }
        if let Some(view) = &self.view {
            frame = frame.pane(view.view().cached(StyleRefinement::default().flex_1().min_w_0().h_full()));
        }
        frame.into_any_element()
    }
}
