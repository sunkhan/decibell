//! Settings → Devices (`DevicesTab.tsx`; spec `2026-10-19-devices-and-password-design.md`): every
//! install signed in to the account by password and not signed out since — this one first — with
//! its OS name (never the hostname), app version, address and activity; sign out one device or
//! every other one, each behind a confirmation.
//!
//! The list is the model's (`live.devices`): loaded when the tab shows and again when central says
//! it changed while the tab is open (`actions::devices::watch`). "Last active 5 min ago" moves on
//! while the tab is shown: a 60 s timer that exists only while a row says "Last active".

use std::time::Duration;

use decibell_common::Watch;
use decibell_model::live::devices::{Confirming, Device, device_title, platform_label};
use decibell_model::{Model, actions};
use decibell_ui::{
    ActiveTheme as _, Button, ButtonVariant, ConfirmModal, FontRole, Icon, IconName, Label, ScrollHandle,
};
use gpui::{AnyElement, App, Context, IntoElement, Render, SharedString, Subscription, Task, Window, div, prelude::*, px};

use super::TabId;
use crate::hooks::SettingsHooks;
use crate::rows::{Btn, TextLink, danger_soft_look, format_ago, local_date, section_label};
use crate::window::CloseLink;

pub struct DevicesTab {
    scroll: ScrollHandle,
    close: CloseLink,
    /// "now" for the relative times, moved on by the ticker.
    now: i64,
    ticker: Option<Task<()>>,
    watching: bool,
    _devices: Subscription,
    _release: Subscription,
    _watch: Watch,
}

impl DevicesTab {
    pub fn new(close: CloseLink, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let watch = Watch::new();
        // Every change redraws; a list with a "Last active" row (re)arms the minute ticker.
        let devices = cx.observe(&model.live.devices, |this, _, cx| {
            this.arm_ticker(cx);
            cx.notify();
        });
        // The renderer loads on mount and on every `devices_changed`; without a core (screens)
        // the list is the fixture's.
        let watching = decibell_model::core::is_running();
        if watching {
            cx.defer(|cx| actions::devices::watch(true, cx));
        }
        let release = cx.on_release(|this: &mut DevicesTab, cx| {
            if this.watching {
                actions::devices::watch(false, cx);
            }
        });
        let mut tab = Self {
            scroll: ScrollHandle::new(),
            close,
            now: SettingsHooks::now(cx),
            ticker: None,
            watching,
            _devices: devices,
            _release: release,
            _watch: watch,
        };
        tab.arm_ticker(cx);
        tab
    }

    /// The minute ticker: only while some row shows a relative time, and not on a fixed clock.
    fn arm_ticker(&mut self, cx: &mut Context<Self>) {
        let relative = Model::global(cx).live.devices.read(cx).list.iter().flatten().any(|d| !d.online);
        let fixed = cx.try_global::<SettingsHooks>().is_some_and(|h| h.now.is_some());
        if !relative || fixed {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(60)).await;
                let alive = this.update(cx, |this, cx| {
                    this.now = SettingsHooks::now(cx);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
    }
}

fn confirm(c: Option<Confirming>, cx: &mut App) {
    let devices = Model::global(cx).live.devices.clone();
    devices.update(cx, |d, cx| d.confirm(c, cx));
}

/// "Decibell 0.9.4 · Linux": the version, and the platform when the title isn't already it.
fn details(d: &Device) -> String {
    let platform = platform_label(&d.platform);
    let title = device_title(&d.name, &d.platform);
    let mut parts = Vec::new();
    if !d.app_version.is_empty() {
        parts.push(format!("Decibell {}", d.app_version));
    }
    if title.as_ref() != platform {
        parts.push(platform.to_string());
    }
    parts.join(" · ")
}

fn device_row(d: &Device, now: i64, busy: bool, sign_out: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let id = d.id;
    let details = details(d);
    let muted = theme.color_text_muted;
    let mut activity = div().mt_0p5().flex().flex_wrap().items_center().gap_x_1p5();
    activity = if d.online {
        activity.child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().size_2().rounded_full().bg(theme.color_success))
                .child(Label::new("Active now").size(12.).color(theme.color_success)),
        )
    } else {
        activity.child(
            Label::new(SharedString::from(format!("Last active {}", format_ago(d.last_active_at, now)))).size(12.).color(muted),
        )
    };
    if !d.address.is_empty() {
        activity = activity.child(Label::new("·").size(12.).color(muted)).child(Label::new(d.address.clone()).size(12.).color(muted));
    }
    activity = activity
        .child(Label::new("·").size(12.).color(muted))
        .child(Label::new(SharedString::from(format!("Signed in {}", local_date(d.signed_in_at, true)))).size(12.).color(muted));

    div()
        .flex()
        .items_center()
        .gap_3p5()
        .rounded(theme.radius_md)
        .border_1()
        .border_color(theme.color_border_divider)
        .bg(theme.color_bg_light)
        .px_4()
        .py_3p5()
        .child(
            div()
                .flex()
                .size_10()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .bg(theme.color_bg_darkest)
                .child(Icon::new(IconName::DevicesMonitor).size(px(18.)).color(theme.color_text_secondary)),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div().min_w_0().child(
                                Label::new(device_title(&d.name, &d.platform))
                                    .size(14.)
                                    .weight(500.)
                                    .color(theme.color_text_primary)
                                    .truncate(),
                            ),
                        )
                        .when(d.current, |this| {
                            this.child(
                                div().flex_none().rounded(theme.radius_sm).bg(theme.color_accent_soft).px_1p5().py(px(1.)).child(
                                    Label::new("THIS DEVICE")
                                        .font(FontRole::Channel)
                                        .size(10.)
                                        .weight(600.)
                                        .tracking(0.06)
                                        .color(theme.color_accent_bright),
                                ),
                            )
                        }),
                )
                .when(!details.is_empty(), |this| {
                    this.child(div().mt_0p5().child(Label::new(details).size(12.).color(theme.color_text_secondary).truncate()))
                })
                .child(activity),
        )
        .when(sign_out, |this| {
            this.child(
                // `rounded-sm border border-error/20 bg-error/10 px-3 py-1.5 text-[12px] font-medium`.
                Btn::new(("device-sign-out", id as usize), "Sign out", danger_soft_look(cx), theme.radius_sm)
                    .text(12., 500.)
                    .pad(12., 6.)
                    .disabled(busy)
                    .on_click(move |_, _, cx| confirm(Some(Confirming::One(id)), cx)),
            )
        })
        .into_any_element()
}

impl Render for DevicesTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let devices = model.live.devices.read(cx);
        let list = devices.list.clone();
        let (error, busy, confirming) = (devices.error.clone(), devices.busy, devices.confirming.clone());
        let now = self.now;
        let theme = cx.theme();

        let current = list.as_ref().and_then(|l| l.iter().find(|d| d.current).cloned());
        let others: Vec<Device> = list.iter().flatten().filter(|d| !d.current).cloned().collect();

        let mut body = div().flex().flex_col().gap_5().child(
            Label::new(
                "These devices are signed in to your account. Sign out any you don't use or don't recognize: a device you sign out needs your password to sign in again. If you don't recognize one, change your password too (Account).",
            )
            .size(12.)
            .line_height(12. * 1.55)
            .color(theme.color_text_muted),
        );
        if let Some(error) = &error {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .rounded(theme.radius_md)
                    .border_1()
                    .border_color(theme.color_error.opacity(0.2))
                    .bg(theme.color_error.opacity(0.1))
                    .px_4()
                    .py_3()
                    .child(div().min_w_0().child(Label::new(error.clone()).size(12.).color(theme.color_error)))
                    .child(
                        TextLink::new("devices-retry", "Try again", theme.color_error, theme.color_error)
                            .text(12., 600.)
                            .on_click(|_, _, cx| actions::devices::load(cx)),
                    ),
            );
        }
        if list.is_none() && error.is_none() {
            body = body.child(Label::new("Loading your devices…").size(12.).color(theme.color_text_muted));
        }
        if let Some(current) = &current {
            body = body.child(div().child(section_label("This device", true, cx)).child(device_row(current, now, busy, false, cx)));
        }
        if list.is_some() {
            let theme = cx.theme();
            let mut section = div().child(section_label("Other devices", true, cx));
            section = if others.is_empty() {
                section.child(
                    div()
                        .rounded(theme.radius_md)
                        .border_1()
                        .border_color(theme.color_border_divider)
                        .bg(theme.color_bg_light)
                        .px_4()
                        .py_3p5()
                        .child(Label::new("No other devices are signed in.").size(12.).color(theme.color_text_muted)),
                )
            } else {
                section.child(div().flex().flex_col().gap_2().children(others.iter().map(|d| device_row(d, now, busy, true, cx))))
            };
            if others.len() > 1 {
                let count = others.len();
                section = section.child(
                    div().mt_3().flex().child(
                        Button::new("devices-sign-out-all", "Sign out of all other devices")
                            .variant(ButtonVariant::DangerSoft)
                            .disabled(busy)
                            .on_click(move |_, _, cx| confirm(Some(Confirming::AllOthers { count }), cx)),
                    ),
                );
            }
            body = body.child(section);
        }

        let (title, label, message) = match &confirming {
            Some(Confirming::AllOthers { count }) => (
                "Sign out of all other devices?",
                "Sign out all",
                format!("Every device but this one ({count}) will be signed out and need your password to sign in again."),
            ),
            Some(Confirming::One(id)) => {
                let d = list.iter().flatten().find(|d| d.id == *id);
                let named = d.map_or(String::new(), |d| {
                    let title = device_title(&d.name, &d.platform);
                    if d.address.is_empty() { title.to_string() } else { format!("{title} ({})", d.address) }
                });
                (
                    "Sign out this device?",
                    "Sign out",
                    format!("{named} will be signed out and need your password to sign in again. If you don't recognize it, change your password too."),
                )
            }
            None => ("Sign out this device?", "Sign out", String::new()),
        };
        let dialog = ConfirmModal::new("devices-confirm", confirming.is_some(), title, label)
            .message(message)
            .confirm_disabled(busy)
            .on_cancel(move |_, _, cx| {
                if !busy {
                    confirm(None, cx);
                }
            })
            .on_confirm(move |_, _, cx| match Model::global(cx).live.devices.read(cx).confirming.clone() {
                Some(Confirming::AllOthers { .. }) => actions::devices::sign_out(Vec::new(), true, cx),
                Some(Confirming::One(id)) => actions::devices::sign_out(vec![id], false, cx),
                None => {}
            });

        super::pane(TabId::Devices, &self.scroll, &self.close).child(body).child(dialog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, platform: &str, version: &str) -> Device {
        Device {
            id: 1,
            name: name.into(),
            platform: platform.into(),
            app_version: version.into(),
            address: "".into(),
            signed_in_at: 0,
            last_active_at: 0,
            online: false,
            current: false,
        }
    }

    #[test]
    fn details_skip_the_platform_the_title_already_says() {
        assert_eq!(details(&device("CachyOS", "linux", "0.9.4")), "Decibell 0.9.4 · Linux");
        assert_eq!(details(&device("", "windows", "0.9.4")), "Decibell 0.9.4");
        assert_eq!(details(&device("", "macos", "")), "");
    }
}
