//! Settings → Account (`AccountTab.tsx`): the profile card (our banner gradient, the picture, the
//! name, "Logged in · Change username · Change password"), the picture's buttons, and Log Out.
//!
//! - The picture and "Change picture" / "Add a picture" ask for the picture editor
//!   ([`crate::PictureRequests`]), and so does a picture dropped anywhere on the card (the veil
//!   says so while a file is dragged over it): the editor is another feature's; it opens over
//!   Settings. A saved picture shows here until the avatar store has fetched it.
//! - "Remove" sends the empty upload central takes as "remove" (`actions::presence::remove_avatar`);
//!   the avatar store follows `avatar_changed`.
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
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, ExternalPaths, Hsla, Image, ImageFormat, ImageSource, IntoElement, ObjectFit, Render,
    SharedString, Subscription, Task, Window, div, img, linear_color_stop, linear_gradient, prelude::*, px,
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
    /// The picture just saved and the avatar version it replaced (`justSaved`), with the 10 s
    /// that bound how long it stands in.
    just_saved: Option<(Arc<Image>, SharedString)>,
    _just_saved_expiry: Option<Task<()>>,
    _watch: Watch,
    _pictures: Subscription,
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
        let pictures = SettingsHooks::get(cx).pictures.clone();
        let pictures = cx.subscribe(&pictures, |this: &mut AccountTab, _, event: &PictureEvent, cx| {
            if let PictureEvent::AvatarSaved { jpeg, before } = event {
                this.just_saved = Some((Arc::new(Image::from_bytes(ImageFormat::Jpeg, (**jpeg).clone())), before.clone()));
                this._just_saved_expiry = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(10)).await;
                    let _ = this.update(cx, |this, cx| {
                        this.just_saved = None;
                        this._just_saved_expiry = None;
                        cx.notify();
                    });
                }));
                cx.notify();
            }
        });
        Self {
            scroll: ScrollHandle::new(),
            close,
            shown,
            removing: false,
            rename,
            just_saved: None,
            _just_saved_expiry: None,
            _watch: watch,
            _pictures: pictures,
        }
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
        let remove = actions::presence::remove_avatar();
        cx.spawn(async move |this, cx| {
            remove.await;
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

/// Files dropped on the card (`usePictureDrop`): the editor's host picks the picture among them.
fn dropped(paths: &ExternalPaths, cx: &mut App) {
    let paths = paths.paths().to_vec();
    if paths.is_empty() {
        return;
    }
    let pictures = SettingsHooks::get(cx).pictures.clone();
    pictures.update(cx, |p, cx| p.request(PictureEvent::AvatarDropped(paths), cx));
}

/// `userGradient`: `linear-gradient(135deg, av-N, color-mix(av-N 72%, #000))`.
fn banner(base: Hsla) -> gpui::Background {
    let mut dark: gpui::Rgba = base.into();
    dark.r *= 0.72;
    dark.g *= 0.72;
    dark.b *= 0.72;
    linear_gradient(135., linear_color_stop(base, 0.), linear_color_stop(Hsla::from(dark), 1.))
}

/// The picture as a button (`PictureButton` with `badge`): a tab stop that Enter and Space press, a
/// scrim saying "Change" over a camera while hovered or focused from the keyboard
/// (`group-hover` / `group-focus-visible`), and the pencil badge at the corner.
fn picture_button(uid: Uid, name: SharedString, saved: Option<Arc<Image>>, window: &mut Window, cx: &mut App) -> AnyElement {
    let id: gpui::ElementId = "account-picture".into();
    let hover = Hover::new(window, cx, id.clone(), Some(Transition::TAILWIND));
    let focus = window.use_keyed_state((id.clone(), "focus"), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
    let focus_visible = focus.is_focused(window) && window.last_input_was_keyboard();
    let (h, listener) = (if focus_visible { 1. } else { hover.progress }, hover.listener());
    let theme = cx.theme();
    let radius = theme.avatar_radius(px(80.));
    let ring = theme.shadow_ring.clone();
    // `bg-black/55 text-white`: the renderer's literals.
    let scrim = gpui::black().opacity(0.55 * h);
    div()
        .id(id)
        .track_focus(&focus)
        .relative()
        .flex()
        .flex_none()
        .cursor_pointer()
        .rounded(radius)
        .focus_visible(move |s| s.shadow(ring))
        .on_hover(listener)
        .on_click(|_, _, cx| request_picture(cx))
        .tooltip(decibell_ui::title_tooltip("Change picture"))
        .child(match saved {
            // `<img src={justSaved.url} className="size-20 object-cover" style={{ borderRadius }}>`.
            Some(image) => img(ImageSource::Image(image)).size_20().rounded(radius).object_fit(ObjectFit::Cover).into_any_element(),
            None => UserAvatar::new(uid, name, 80.).into_any_element(),
        })
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
        let avatars = model.live.presence.avatars.read(cx);
        let has_avatar = avatars.image(uid).is_some();
        // The saved picture stands in until the store has fetched one of another version.
        let fetched_other = |before: &SharedString| avatars.get(uid).is_some_and(|e| e.image.is_some() && e.version != *before);
        let saved = self.just_saved.as_ref().filter(|(_, before)| !fetched_other(before)).map(|(image, _)| image.clone());
        let theme = cx.theme();
        let base = avatar_color(theme, person_slot(uid, username.as_deref().unwrap_or("")));
        let avatar_radius = theme.avatar_radius(px(80.));

        let picture: AnyElement = match &username {
            Some(name) => picture_button(uid, name.clone(), saved, window, cx),
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
                    .probe("account-remove-picture")
                    .disabled(self.removing)
                    .on_click(cx.listener(|this, _, _, cx| this.remove_picture(cx))),
            );
        }
        buttons = buttons.child(
            Button::new("account-change-picture", if has_avatar { "Change picture" } else { "Add a picture" })
                .probe("account-change-picture")
                .on_click(|_, _, cx| request_picture(cx)),
        );

        let card = div()
            .id("account-card")
            .group("account-card")
            .relative()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_light)
            .when(username.is_some(), |this| this.on_drop(|paths: &ExternalPaths, _, cx| dropped(paths, cx)))
            .children(decibell_harness::probes::probe("account-card"))
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
            )
            .when(username.is_some(), |this| {
                // `DropVeil`: `absolute inset-0 z-10 rounded-[inherit] border-2 border-dashed
                // border-accent bg-bg-light/90 text-[13px] font-semibold text-text-primary`, while a
                // file is dragged over the card.
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .invisible()
                        .group_drag_over::<ExternalPaths>("account-card", |s| s.visible())
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius_lg)
                        .border_2()
                        .border_dashed()
                        .border_color(theme.color_accent)
                        .bg(theme.color_bg_light.opacity(0.9))
                        .child(Label::new("Drop to use as your picture").size(13.).weight(600.).color(theme.color_text_primary)),
                )
            });

        // `mt-5 rounded-md border border-error/20 bg-error/10 px-5 py-2.5 text-[13px] font-medium`.
        let log_out_button = div().mt_5().flex().child(
            Button::new("account-log-out", "Log Out")
                .variant(ButtonVariant::DangerSoft)
                .size(ButtonSize::Lg)
                .probe("account-log-out")
                .on_click(|_, _, cx| actions::auth::log_out(cx)),
        );

        let dialog = rename::render(&self.rename, |t: &mut AccountTab| &mut t.rename, window, cx);
        super::pane(TabId::Account, &self.scroll, &self.close)
            .child(div().child(card).child(log_out_button))
            .child(dialog)
    }
}
