//! The two overlays the shell draws over the main layout (`register_overlay`): they hold nothing
//! while their modal is closed. Opening builds the window (and its open tab) for the community or
//! channel in question; closing lets the window fade out (300 ms, the modal's), then drops it with
//! everything it held: tab state, answer subscriptions, search results, pictures.

use std::time::Duration;

use decibell_common::Watch;
use decibell_model::stores::overlays::modal;
use decibell_model::{ChannelId, Model, ServerId};
use gpui::{App, AppContext as _, Context, Entity, Global, IntoElement, ParentElement as _, Render, Task, WeakEntity, Window, div};

use crate::channel::ChannelSettingsWindow;
use crate::server::{ServerSettingsWindow, TabId};
use decibell_model::stores::navigation::ActiveView;

/// How long a closing window keeps drawing (the modal's fade).
const FADE: Duration = Duration::from_millis(300);

/// Every live host, weakly (the self-tests find the open window through them).
#[derive(Default)]
struct Hosts {
    server: Vec<WeakEntity<ServerSettingsHost>>,
    channel: Vec<WeakEntity<ChannelSettingsHost>>,
}

impl Global for Hosts {}

/// The Server Settings window open (or fading out) in any app window.
pub fn server_window(cx: &App) -> Option<Entity<ServerSettingsWindow>> {
    cx.try_global::<Hosts>()?.server.iter().filter_map(WeakEntity::upgrade).find_map(|h| h.read(cx).window.clone())
}

/// The Channel Settings window open (or fading out) in any app window.
pub fn channel_window(cx: &App) -> Option<Entity<ChannelSettingsWindow>> {
    cx.try_global::<Hosts>()?.channel.iter().filter_map(WeakEntity::upgrade).find_map(|h| h.read(cx).window.clone())
}

/// `ServerSettingsModal`'s mount point.
pub struct ServerSettingsHost {
    window: Option<Entity<ServerSettingsWindow>>,
    open: bool,
    /// The tab the window was on when it last closed. The renderer's modal stays mounted (its
    /// `activeTab` state with it) while the server sidebar is: reopened, on this community or
    /// another, it shows the same tab when the permissions allow it, else Overview. Leaving the
    /// server view unmounts it there; here that forgets the tab.
    last_tab: Option<TabId>,
    _closing: Option<Task<()>>,
    _watch: Watch,
    _nav: gpui::Subscription,
}

impl ServerSettingsHost {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store_if(&model.overlays, cx, |this: &mut Self, overlays, _| this.open != overlays.is_open(modal::SERVER_SETTINGS));
        let me = cx.weak_entity();
        let hosts = cx.default_global::<Hosts>();
        hosts.server.retain(|h| h.upgrade().is_some());
        hosts.server.push(me);
        let nav = cx.observe(&model.nav, |this: &mut Self, nav, cx| {
            if nav.read(cx).active_view != ActiveView::Server {
                this.last_tab = None;
            }
        });
        let mut this = ServerSettingsHost { window: None, open: false, last_tab: None, _closing: None, _watch: watch, _nav: nav };
        this.sync(window, cx);
        this
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = Model::global(cx).clone();
        let open = model.overlays.read(cx).is_open(modal::SERVER_SETTINGS);
        if open == self.open {
            return;
        }
        self.open = open;
        if open {
            self._closing = None;
            match &self.window {
                Some(w) => w.update(cx, |w, cx| w.set_open(true, cx)),
                None => {
                    let Some(server) = model.nav.read(cx).active_server else { return };
                    let last = self.last_tab;
                    self.window = Some(cx.new(|cx| {
                        let mut w = ServerSettingsWindow::new(server, window, cx);
                        // `show` opens only a tab the permissions allow (else Overview stays).
                        if let Some(tab) = last {
                            w.show(tab, window, cx);
                        }
                        w
                    }));
                }
            }
        } else if let Some(w) = &self.window {
            self.last_tab = Some(w.read(cx).tab().id());
            w.update(cx, |w, cx| w.set_open(false, cx));
            let delay = if cx.reduce_motion() { Duration::ZERO } else { FADE };
            self._closing = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| {
                    this.window = None;
                    this._closing = None;
                    cx.notify();
                });
            }));
        }
    }

    /// The window while it is open or fading out (tests).
    pub fn window(&self) -> Option<&Entity<ServerSettingsWindow>> {
        self.window.as_ref()
    }
}

impl Render for ServerSettingsHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(window, cx);
        div().children(self.window.clone())
    }
}

/// `ChannelSettingsModal`'s mount point.
pub struct ChannelSettingsHost {
    window: Option<Entity<ChannelSettingsWindow>>,
    open: bool,
    target: Option<(ServerId, ChannelId)>,
    _closing: Option<Task<()>>,
    _watch: Watch,
}

impl ChannelSettingsHost {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store_if(&model.overlays, cx, |this: &mut Self, overlays, _| {
            this.open != overlays.is_open(modal::CHANNEL_SETTINGS)
                || (overlays.is_open(modal::CHANNEL_SETTINGS) && this.target.as_ref().map(|t| &t.1) != overlays.channel_settings_channel.as_ref())
        });
        let me = cx.weak_entity();
        let hosts = cx.default_global::<Hosts>();
        hosts.channel.retain(|h| h.upgrade().is_some());
        hosts.channel.push(me);
        let mut this = ChannelSettingsHost { window: None, open: false, target: None, _closing: None, _watch: watch };
        this.sync(window, cx);
        this
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = Model::global(cx).clone();
        let overlays = model.overlays.read(cx);
        let open = overlays.is_open(modal::CHANNEL_SETTINGS);
        let channel = overlays.channel_settings_channel.clone();
        let server = model.nav.read(cx).active_server;
        let target = match (server, channel) {
            (Some(s), Some(c)) if open => Some((s, c)),
            _ => None,
        };
        if open == self.open && (!open || target == self.target) {
            return;
        }
        self.open = open;
        if open {
            self._closing = None;
            let Some((server, channel)) = target.clone() else { return };
            // Pointing at another channel opens fresh state for it.
            if self.target.as_ref() != Some(&(server, channel.clone())) {
                self.window = None;
            }
            self.target = target;
            match &self.window {
                Some(w) => w.update(cx, |w, cx| w.set_open(true, cx)),
                None => self.window = Some(cx.new(|cx| ChannelSettingsWindow::new(server, channel, window, cx))),
            }
        } else if let Some(w) = &self.window {
            w.update(cx, |w, cx| w.set_open(false, cx));
            let delay = if cx.reduce_motion() { Duration::ZERO } else { FADE };
            self._closing = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| {
                    this.window = None;
                    this.target = None;
                    this._closing = None;
                    cx.notify();
                });
            }));
        }
    }

    pub fn window(&self) -> Option<&Entity<ChannelSettingsWindow>> {
        self.window.as_ref()
    }
}

impl Render for ChannelSettingsHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(window, cx);
        div().children(self.window.clone())
    }
}
