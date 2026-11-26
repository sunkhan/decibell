//! The right panel beside a text channel (`MainLayout`'s server branch): a search's results while
//! a search of this community is open, else the member list while the members panel is on.
//! Below 1024 px the results float over the chat from the right edge (`max-lg:absolute … w-[360px]
//! shadow-float`). Closing the search brings the member list back as the renderer remounts it.
//!
//! Registered for `Slot::RightPanel`, so the shell draws it uncached and it sizes itself: its own
//! render is a wrapper; the results and the member list are cached views inside it.

use decibell_common::Watch;
use decibell_model::stores::layout::LayoutEvent;
use decibell_model::{Model, ServerId};
use decibell_shell::{MEMBERS_WIDTH, MembersList};
use decibell_ui::ActiveTheme as _;
use gpui::{AnyElement, AppContext as _, Context, Entity, IntoElement, Render, StyleRefinement, Window, div, prelude::*, px};

use crate::results::{FLOATING_WIDTH, PANEL_WIDTH, ResultsView, is_narrow};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Shown {
    /// The community whose search is open and shows (the active one).
    search: Option<ServerId>,
    members_panel: bool,
}

pub struct RightPanel {
    model: Model,
    shown: Shown,
    members: Option<Entity<MembersList>>,
    results: Option<Entity<ResultsView>>,
    _watch: Watch,
}

fn shown(model: &Model, cx: &gpui::App) -> Shown {
    let active = model.nav.read(cx).active_server;
    let store = model.chat.search.read(cx);
    // Search results take the member list's place on the community they're for.
    let search = active.filter(|server| store.open && store.server == Some(*server));
    Shown { search, members_panel: model.layout.read(cx).prefs().members_panel }
}

impl RightPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        let check = |this: &mut RightPanel, cx: &gpui::App| {
            let next = shown(&this.model, cx);
            std::mem::replace(&mut this.shown, next) != next
        };
        watch
            .store_if(&model.chat.search, cx, move |this, _, cx| check(this, cx))
            .store_if(&model.nav, cx, move |this, _, cx| check(this, cx))
            .event(&model.layout, cx, |_, e: &LayoutEvent| *e == LayoutEvent::Panels);
        let shown = shown(&model, cx);
        Self { model, shown, members: None, results: None, _watch: watch }
    }

    /// The results view on screen, if any (tests).
    pub fn results(&self) -> Option<&Entity<ResultsView>> {
        self.results.as_ref()
    }
}

impl Render for RightPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shown = shown(&self.model, cx);
        if let Some(server) = self.shown.search {
            self.members = None;
            if self.results.as_ref().is_none_or(|r| r.read(cx).server() != server) {
                self.results = Some(cx.new(|cx| ResultsView::new(server, cx)));
            }
            let results = self.results.clone().expect("just made").cached(StyleRefinement::default().size_full());
            let panel: AnyElement = if is_narrow(window, cx) {
                // Floating over the chat from the row's right edge.
                let shadow = cx.theme().shadow_float.clone();
                div().absolute().top_0().bottom_0().right_0().w(px(FLOATING_WIDTH)).shadow(shadow).child(results).into_any_element()
            } else {
                div().flex_none().w(px(PANEL_WIDTH)).h_full().child(results).into_any_element()
            };
            return panel;
        }
        self.results = None;
        if self.shown.members_panel {
            let members = self.members.get_or_insert_with(|| cx.new(MembersList::new)).clone();
            return members.cached(StyleRefinement::default().w(px(MEMBERS_WIDTH)).h_full()).into_any_element();
        }
        self.members = None;
        div().into_any_element()
    }
}
