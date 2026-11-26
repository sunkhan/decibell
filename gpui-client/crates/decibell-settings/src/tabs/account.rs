//! Settings → Account (`AccountTab.tsx`): the profile card (our banner gradient, the picture, the
//! name, "Logged in · Change username · Change password"), the picture's buttons, and Log Out.
//!
//! - The picture and "Change picture" / "Add a picture" ask for the picture editor
//!   ([`crate::PictureRequests`]): the editor is another feature's; it opens over Settings.
//! - "Remove" sends the empty upload central takes as "remove" (`upload_avatar`, zero bytes); the
//!   avatar store follows `avatar_changed`.
//! - Change username: the rename dialog ([`crate::dialogs::rename`]), drawn here. Change password:
//!   the model's flow (`live.devices.open_password`), its own overlay.
//! - Log Out (`actions::auth::log_out`): hang up first (the renderer's `endCall(null)`), then the
//!   core forgets this device at central and drops every connection; `logged_out` clears the
//!   stores and closes Settings.

use decibell_common::{UserAvatar, Watch};
use decibell_model::live::presence::avatars::AvatarsChanged;
use decibell_model::stores::users::NamesChanged;
use decibell_model::{Model, Uid, actions};
use decibell_ui::identity::{avatar_color, person_slot};
use decibell_ui::motion::Transition;
use decibell_ui::{ActiveTheme as _, Button, ButtonSize, ButtonVariant, FontRole, Hover, Icon, IconName, Label, ScrollHandle};
use gpui::{
    AnyElement, App, Context, Hsla, IntoElement, Render, SharedString, Window, div, linear_color_stop, linear_gradient,
    prelude::*, px,
};

use super::TabId;
use crate::dialogs::rename::{self, RenameForm};
use crate::hooks::{PictureEvent, SettingsHooks};
use crate::rows::TextLink;
use crate::window::CloseLink;

#[derive(Clone, PartialEq)]
struct Shown {
    uid: Uid,
    username: Option<SharedString>,
    renames: bool,
    devices: bool,
}

pub struct AccountTab {
    scroll: ScrollHandle,
    close: CloseLink,
    shown: Shown,
    removing: bool,
    pub rename: RenameForm,
    _watch: Watch,
}

impl AccountTab {
    pub fn new(close: CloseLink, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let shown_of = |a: &decibell_model::stores::auth::AuthStore| Shown {
            uid: a.uid,
            username: a.username.clone(),
            renames: a.renames,
            devices: a.devices,
        };
        let mut watch = Watch::new();
        watch
            .store_if(&model.auth, cx, move |this: &mut AccountTab, a, _| {
                let next = shown_of(a);
                std::mem::replace(&mut this.shown, next.clone()) != next
            })
            // Our name and picture (not our status: the tab doesn't show it).
            .event(&model.users, cx, |this: &mut AccountTab, e: &NamesChanged| e.0.contains(&this.shown.uid))
            .event(&model.live.presence.avatars, cx, |this: &mut AccountTab, e: &AvatarsChanged| e.0.contains(&this.shown.uid));
        let shown = shown_of(model.auth.read(cx));
        let mut rename = RenameForm::new(|t: &mut AccountTab| &mut t.rename, window, cx);
        rename.open = SettingsHooks::get(cx).staged.open_rename;
        Self { scroll: ScrollHandle::new(), close, shown, removing: false, rename, _watch: watch }
    }

    pub fn open_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rename.open(window, cx);
        cx.notify();
    }

    fn remove_picture(&mut self, cx: &mut Context<Self>) {
        if self.shown.username.is_none() || self.removing {
            return;
        }
        self.removing = true;
        cx.notify();
        if !decibell_model::core::is_running() {
            self.removing = false;
            return;
        }
        // Central treats a zero-byte upload as "remove"; the picture goes with `avatar_changed`.
        cx.spawn(async move |this, cx| {
            match decibell_core::api::auth::upload_avatar(Vec::<u8>::new()).await {
                Ok(r) if !r.success => log::error!("Avatar removal failed: {}", r.message),
                Ok(_) => {}
                Err(e) => log::error!("Avatar removal failed: {e}"),
            }
            let _ = this.update(cx, |this, cx| {
                this.removing = false;
                cx.notify();
            });
        })
        .detach();
    }
}

fn request_picture(cx: &mut App) {
    let pictures = SettingsHooks::get(cx).pictures.clone();
    pictures.update(cx, |p, cx| p.request(PictureEvent::ChangeAvatar, cx));
}

/// `userGradient`: `linear-gradient(135deg, av-N, color-mix(av-N 72%, #000))`.
fn banner(base: Hsla) -> gpui::Background {
    let mut dark: gpui::Rgba = base.into();
    dark.r *= 0.72;
    dark.g *= 0.72;
    dark.b *= 0.72;
    linear_gradient(135., linear_color_stop(base, 0.), linear_color_stop(Hsla::from(dark), 1.))
}

/// The picture as a button (`PictureButton` with `badge`): a hover scrim saying "Change" over a
/// camera, and the pencil badge at the corner.
fn picture_button(uid: Uid, name: SharedString, window: &mut Window, cx: &mut App) -> AnyElement {
    let id: gpui::ElementId = "account-picture".into();
    let hover = Hover::new(window, cx, id.clone(), Some(Transition::TAILWIND));
    let (h, listener) = (hover.progress, hover.listener());
    let theme = cx.theme();
    let radius = theme.avatar_radius(px(80.));
    let ring = theme.shadow_ring.clone();
    // `bg-black/55 text-white`: the renderer's literals.
    let scrim = gpui::black().opacity(0.55 * h);
    div()
        .id(id)
        .relative()
        .flex()
        .flex_none()
        .cursor_pointer()
        .rounded(radius)
        .focus_visible(move |s| s.shadow(ring))
        .on_hover(listener)
        .on_click(|_, _, cx| request_picture(cx))
        .tooltip(decibell_ui::title_tooltip("Change picture"))
        .child(UserAvatar::new(uid, name, 80.))
        .when(h > 0., |this| {
            this.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .rounded(radius)
                    .bg(scrim)
                    .child(Icon::new(IconName::PictureCamera).size(px(18.)).color(gpui::white().opacity(h)))
                    .child(Label::new("Change").size(11.).weight(600.).color(gpui::white().opacity(h))),
            )
        })
        .child(
            // `absolute -bottom-1 -right-1 size-6 rounded-full bg-accent text-on-accent ring-3
            // ring-bg-light`.
            div()
                .absolute()
                .bottom(px(-7.))
                .right(px(-7.))
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.color_bg_light)
                .child(
                    div()
                        .size_6()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(theme.color_accent)
                        .child(Icon::new(IconName::PicturePencil).size(px(12.)).color(theme.color_on_accent)),
                ),
        )
        .into_any_element()
}

impl Render for AccountTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let Shown { uid, username, renames, devices } = self.shown.clone();
        let has_avatar = model.live.presence.avatars.read(cx).image(uid).is_some();
        let theme = cx.theme();
        let base = avatar_color(theme, person_slot(uid, username.as_deref().unwrap_or("")));
        let avatar_radius = theme.avatar_radius(px(80.));

        let picture: AnyElement = match &username {
            Some(name) => picture_button(uid, name.clone(), window, cx),
            None => div().size_20().rounded(avatar_radius).bg(cx.theme().color_bg_darkest).into_any_element(),
        };
        let theme = cx.theme();
        let link = |id: &'static str, label: &'static str| {
            TextLink::new(id, label, theme.color_text_secondary, theme.color_text_primary).text(12., 400.)
        };
        let dot = || Label::new("·").size(12.).color(theme.color_text_muted);
        let mut meta = div()
            .mt_0p5()
            .flex()
            .items_center()
            .gap_2()
            .child(Label::new("Logged in").size(12.).color(theme.color_text_muted));
        if renames {
            meta = meta.child(dot()).child(link("account-rename", "Change username").probe("account-rename").on_click(cx.listener(|this, _, window, cx| {
                this.open_rename(window, cx);
            })));
        }
        if devices {
            meta = meta.child(dot()).child(link("account-password", "Change password").probe("account-password").on_click(|_, _, cx| {
                let devices = Model::global(cx).live.devices.clone();
                devices.update(cx, |d, cx| d.open_password(cx));
            }));
        }

        let mut buttons = div().flex().flex_none().gap_2();
        if has_avatar {
            buttons = buttons.child(
                Button::new("account-remove-picture", if self.removing { "Removing…" } else { "Remove" })
                    .variant(ButtonVariant::Ghost)
                    .disabled(self.removing)
                    .on_click(cx.listener(|this, _, _, cx| this.remove_picture(cx))),
            );
        }
        buttons = buttons.child(
            Button::new("account-change-picture", if has_avatar { "Change picture" } else { "Add a picture" })
                .on_click(|_, _, cx| request_picture(cx)),
        );

        let card = div()
            .relative()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_light)
            .child(div().h_20().rounded_t(theme.radius_lg - px(1.)).bg(banner(base)))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_4()
                    .px_5()
                    .pb_4()
                    .child(
                        // `-mt-10 flex shrink-0 bg-bg-light ring-4 ring-bg-light`: the ring is a
                        // 4 px frame of the card's colour around the picture.
                        div()
                            .m(px(-4.))
                            .mt(px(-44.))
                            .flex()
                            .flex_none()
                            .rounded(avatar_radius + px(4.))
                            .bg(theme.color_bg_light)
                            .p_1()
                            .child(picture),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .pt_3()
                            .child(
                                Label::new(username.clone().unwrap_or_default())
                                    .font(FontRole::Display)
                                    .size(18.)
                                    .weight(600.)
                                    .color(theme.color_text_primary)
                                    .truncate(),
                            )
                            .child(meta),
                    )
                    .child(buttons),
            );

        // `mt-5 rounded-md border border-error/20 bg-error/10 px-5 py-2.5 text-[13px] font-medium`.
        let log_out_button = div().mt_5().flex().child(
            Button::new("account-log-out", "Log Out")
                .variant(ButtonVariant::DangerSoft)
                .size(ButtonSize::Lg)
                .on_click(|_, _, cx| actions::auth::log_out(cx)),
        );

        let dialog = rename::render(&self.rename, |t: &mut AccountTab| &mut t.rename, window, cx);
        super::pane(TabId::Account, &self.scroll, &self.close)
            .child(div().child(card).child(log_out_button))
            .child(dialog)
    }
}
