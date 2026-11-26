//! Settings → About (`AboutTab.tsx`): the app card (name, tagline, version) and the updater's
//! status with its one button. The version is the app's own (`SettingsHooks::app_version`) unless
//! the updater reports one; the buttons go to the app's [`crate::UpdateHooks`] (the updater is a
//! later task: without it the model's `UpdateStore` says "disabled").

use decibell_common::Watch;
use decibell_model::Model;
use decibell_model::stores::update::{UpdateMode, UpdateStatus};
use decibell_ui::{ActiveTheme as _, Button, ButtonSize, FontRole, Icon, IconName, Label, ScrollHandle};
use gpui::{
    AnyElement, App, Context, IntoElement, Render, SharedString, Window, div, linear_color_stop, linear_gradient,
    prelude::*, px,
};

use super::TabId;
use crate::hooks::SettingsHooks;
use crate::rows::card;
use crate::window::CloseLink;

pub struct AboutTab {
    scroll: ScrollHandle,
    close: CloseLink,
    _watch: Watch,
}

impl AboutTab {
    pub fn new(close: CloseLink, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.update, cx);
        Self { scroll: ScrollHandle::new(), close, _watch: watch }
    }
}

fn hook(cx: &mut App, run: impl FnOnce(&dyn crate::UpdateHooks, &mut App)) {
    let hooks = SettingsHooks::get(cx).update.clone();
    match hooks {
        Some(hooks) => run(&*hooks, cx),
        None => log::info!("[update] no updater in this build"),
    }
}

fn button(id: &'static str, label: &'static str, disabled: bool, on: fn(&dyn crate::UpdateHooks, &mut App)) -> AnyElement {
    Button::new(id, label)
        .size(ButtonSize::Sm)
        .disabled(disabled)
        .on_click(move |_, _, cx| hook(cx, |h, cx| on(h, cx)))
        .into_any_element()
}

impl Render for AboutTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let update = model.update.read(cx);
        let (status, mode) = (update.status.clone(), update.mode);
        let version = if update.current_version.is_empty() {
            SettingsHooks::get(cx).app_version.clone()
        } else {
            update.current_version.clone()
        };
        let theme = cx.theme();

        let app_card = card(cx)
            .px_5()
            .py_5()
            .child(
                div()
                    .mb_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        // `bg-gradient-to-br from-accent to-accent-bright`, the note drawn in
                        // `stroke="white"` (the TSX's literal, not a token).
                        div()
                            .flex()
                            .size_10()
                            .items_center()
                            .justify_center()
                            .rounded(theme.radius_md)
                            .bg(linear_gradient(
                                135.,
                                linear_color_stop(theme.color_accent, 0.),
                                linear_color_stop(theme.color_accent_bright, 1.),
                            ))
                            .child(Icon::new(IconName::SettingsUiAppNote).size(px(20.)).color(gpui::white())),
                    )
                    .child(
                        div()
                            .child(
                                Label::new("Decibell")
                                    .font(FontRole::Display)
                                    .size(14.)
                                    .weight(600.)
                                    .color(theme.color_text_primary),
                            )
                            .child(Label::new("Decentralized game chat").size(12.).color(theme.color_text_muted)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .child(Label::new("Version\u{a0}").size(12.).color(theme.color_text_secondary))
                    .child(Label::new(version).size(12.).weight(500.).color(theme.color_text_primary)),
            );

        let row = status_row(&status, mode, cx);
        let theme = cx.theme();
        let updates_card = card(cx)
            .px_5()
            .py_5()
            .child(div().mb_3().child(Label::new("Updates").size(13.).weight(600.).color(theme.color_text_primary)))
            .child(row);

        super::pane(TabId::About, &self.scroll, &self.close)
            .child(div().flex().flex_col().gap_4().child(app_card).child(updates_card))
    }
}

/// `UpdateStatusRow`.
fn status_row(status: &UpdateStatus, mode: UpdateMode, cx: &App) -> AnyElement {
    let theme = cx.theme();
    if mode == UpdateMode::Disabled {
        return Label::new("Updates are disabled in development builds.")
            .size(12.)
            .color(theme.color_text_muted)
            .into_any_element();
    }
    let line = |text: SharedString, color| Label::new(text).size(12.).color(color).into_any_element();
    let check: fn(&dyn crate::UpdateHooks, &mut App) = |h, cx| h.check(cx);
    let (line, button) = match status {
        UpdateStatus::Idle | UpdateStatus::NotAvailable { .. } => (
            line("You're up to date.".into(), theme.color_text_secondary),
            button("update-check", "Check now", false, check),
        ),
        UpdateStatus::Checking => (
            line("Checking for updates…".into(), theme.color_text_secondary),
            button("update-check", "Check now", true, check),
        ),
        UpdateStatus::Available { version } if mode == UpdateMode::NotifyOnly => (
            line(format!("{version} is available.").into(), theme.color_text_primary),
            button("update-release-page", "Open release page", false, |h, cx| h.open_release_page(cx)),
        ),
        UpdateStatus::Available { .. } => (
            line("Update available — preparing download…".into(), theme.color_text_secondary),
            button("update-check", "Check now", true, check),
        ),
        UpdateStatus::Downloading { pct, version } => (
            line(format!("Downloading {version}… {}%", pct.round() as i64).into(), theme.color_text_secondary),
            button("update-check", "Check now", true, check),
        ),
        UpdateStatus::Downloaded { version } => (
            line(format!("Update ready: {version}").into(), theme.color_text_primary),
            button("update-restart", "Restart", false, |h, cx| h.quit_and_install(cx)),
        ),
        UpdateStatus::Error { message } => (
            line(format!("Couldn't check: {message}").into(), theme.color_error),
            button("update-check", "Try again", false, check),
        ),
    };
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(div().min_w_0().flex_1().child(line))
        .child(div().flex_none().child(button))
        .into_any_element()
}
