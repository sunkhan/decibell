//! The app's content under the title bar (`App.tsx`'s routes + `AppLayout`): nothing until the
//! core has read the configuration (and started a saved sign-in), then the sign-in page or the
//! main layout by whether we are signed in, with the certificate prompt over either.

use decibell_common::Watch;
use decibell_model::Model;
use decibell_ui::ActiveTheme as _;
use gpui::{AnyView, AppContext as _, Context, Entity, IntoElement, Render, SharedString, StyleRefinement, Window, div, prelude::*};

use crate::auth::{AuthMode, AuthView};
use crate::main_view::MainView;
use crate::overlays::CertMismatchOverlay;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Booting,
    SignIn,
    Main,
}

fn page(model: &Model, cx: &gpui::App) -> Page {
    let auth = model.auth.read(cx);
    if !auth.booted {
        Page::Booting
    } else if auth.is_authenticated {
        Page::Main
    } else {
        Page::SignIn
    }
}

pub struct Shell {
    page: Page,
    auth: Option<Entity<AuthView>>,
    main: Option<Entity<MainView>>,
    cert: Entity<CertMismatchOverlay>,
    _watch: Watch,
}

impl Shell {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store_if(&model.auth, cx, |this, _, cx| {
            let next = page(&Model::global(cx).clone(), cx);
            std::mem::replace(&mut this.page, next) != next
        });
        Self { page: page(&model, cx), auth: None, main: None, cert: cx.new(CertMismatchOverlay::new), _watch: watch }
    }

    /// The sign-in page's view, when it shows (tests and the dev sign-in).
    pub fn auth_view(&self) -> Option<&Entity<AuthView>> {
        self.auth.as_ref()
    }

    pub fn main_view(&self) -> Option<&Entity<MainView>> {
        self.main.as_ref()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::probes::flag("page-sign-in", self.page == Page::SignIn);
        let content: Option<AnyView> = match self.page {
            Page::Booting => {
                self.auth = None;
                self.main = None;
                None
            }
            Page::SignIn => {
                // Nothing of the signed-in layout carries over to the next account.
                self.main = None;
                Some(self.auth.get_or_insert_with(|| cx.new(|cx| AuthView::new(AuthMode::Login, cx))).clone().into())
            }
            Page::Main => {
                self.auth = None;
                Some(self.main.get_or_insert_with(|| cx.new(|cx| MainView::new(window, cx))).clone().into())
            }
        };
        div()
            .relative()
            .size_full()
            .bg(cx.theme().color_bg_primary)
            .children(content.map(|view| view.cached(StyleRefinement::default().size_full())))
            .child(self.cert.clone())
    }
}

/// What the title bar shows (`Titlebar.tsx`): the community's name in a server or voice view,
/// the open conversation's peer in a DM, else "Decibell".
pub fn titlebar_title(cx: &gpui::App) -> SharedString {
    let Some(model) = cx.try_global::<Model>() else { return SharedString::new_static("Decibell") };
    let nav = model.nav.read(cx);
    match nav.active_view {
        decibell_model::stores::navigation::ActiveView::Server | decibell_model::stores::navigation::ActiveView::Voice => nav
            .active_server
            .and_then(|id| model.servers.read(cx).get(id).map(|s| s.name.clone()))
            .unwrap_or_else(|| SharedString::new_static("Decibell")),
        decibell_model::stores::navigation::ActiveView::Dm => {
            let list = model.dm.list.read(cx);
            list.active_peer
                .map(|peer| {
                    let fallback = list.get(peer).map(|c| c.username.clone()).unwrap_or_default();
                    model.name_of(peer, &fallback, cx)
                })
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| SharedString::new_static("Decibell"))
        }
        _ => SharedString::new_static("Decibell"),
    }
}

/// The OS window's title (`useWindowTitle`): "<community> — Decibell", "<name> — Decibell" in a
/// DM, else "Decibell".
pub fn window_title(cx: &gpui::App) -> SharedString {
    match cx.try_global::<Model>() {
        Some(model) => decibell_model::live::window_title(model, cx),
        None => SharedString::new_static("Decibell"),
    }
}
