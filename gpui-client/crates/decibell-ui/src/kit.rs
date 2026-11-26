//! The component kit's gallery: one screen per family of components, drawn from fixture state.
//! `tools/electron-captures/pages/<screen>.tsx` draws the renderer's own components in the same
//! places, so `pairs.mjs` can put the two side by side. Usable live: `decibell --screen kit-forms`.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, Context, IntoElement, Render, SharedString, Window, div,
    point, prelude::*, px,
};

use crate::TITLEBAR_HEIGHT;
use crate::popover::visible_window_bounds;

use crate::modal::ConfirmTone;
use crate::scroll::RowHeights;
use crate::{
    ActiveTheme as _, AssumeClientDecorations, Avatar, Badge, BadgeTone, Banner, BellMark, Button,
    ButtonSize, ButtonVariant, Checkbox, ConfirmModal, ContextMenu, EmptyState, ErrorCard, Field,
    IconButton, IconName, Label, LetterAvatar, LinkButton, MenuEntry, MenuItem, NavItem, Notice,
    Presence, SaveBar, ScrollArea, ScrollHandle, Segment, SegmentedControl, Select, 
    SelectOption, SelectSize, SelectVariant, SettingsWindow, Slider, Spinner,
    StatusDot, StatusPill, Switch, TextField, Toast, ToastStack, Toasts, Tooltip,
};

/// Captures render one frame on a clock that does not run: entrances and transitions are drawn
/// at their end (reduced motion). `decibell-capture` is recognised by the global it sets.
fn settle_motion_for_capture(cx: &mut App) {
    if cx.has_global::<AssumeClientDecorations>() {
        cx.set_reduce_motion(true);
    }
}

/// A titled group: `flex flex-col gap-2`, the title `text-[12px] text-text-muted`.
fn section(title: &'static str, content: impl IntoElement, cx: &App) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(Label::new(title).size(12.).color(cx.theme().color_text_muted))
        .child(content)
        .into_any_element()
}

fn row() -> gpui::Div {
    div().flex().flex_wrap().items_center().gap_3()
}

fn page(cx: &App) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    div()
        .id("kit")
        .relative()
        .size_full()
        .bg(theme.color_bg_primary)
        .text_color(theme.color_text_primary)
        .p_6()
        .flex()
        .flex_col()
        .gap_6()
}

// ── kit-buttons ──────────────────────────────────────────────────────────────────────────────

pub struct KitButtons;

impl Render for KitButtons {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let variants = [
            (ButtonVariant::Primary, "Save changes"),
            (ButtonVariant::Secondary, "Cancel"),
            (ButtonVariant::Ghost, "Reset"),
            (ButtonVariant::Danger, "Delete"),
            (ButtonVariant::DangerSoft, "Remove"),
            (ButtonVariant::DangerOutline, "Leave"),
            (ButtonVariant::Soft, "Copy invite"),
        ];
        let theme = cx.theme();
        page(cx)
            .child(section(
                "Buttons · md",
                row().children(variants.iter().enumerate().map(|(i, (variant, label))| {
                    Button::new(("md", i), *label).variant(*variant)
                })),
                cx,
            ))
            .child(section(
                "Buttons · sm",
                row().children(variants.iter().enumerate().map(|(i, (variant, label))| {
                    Button::new(("sm", i), *label).variant(*variant).size(ButtonSize::Sm)
                })),
                cx,
            ))
            .child(section(
                "States · disabled, loading, icon",
                row()
                    .child(Button::new("disabled", "Save changes").disabled(true))
                    .child(
                        Button::new("disabled-danger", "Delete")
                            .variant(ButtonVariant::Danger)
                            .disabled(true),
                    )
                    .child(Button::new("loading", "Saving…").loading(true))
                    .child(Button::new("icon", "Add role").icon(IconName::SettingsPlus)),
                cx,
            ))
            .child(section(
                "Full width",
                div().w(px(320.)).child(Button::new("wide", "Log In").full_width()),
                cx,
            ))
            .child(section(
                "Icon buttons · links",
                row()
                    .child(IconButton::new("close", IconName::Close))
                    .child(IconButton::new("dismiss", IconName::Close).small())
                    .child(LinkButton::new("undo", "Undo"))
                    .child(LinkButton::new("reset-link", "Reset").size(11.).weight(500.).recolor()),
                cx,
            ))
            .child(section(
                "Spinners",
                row()
                    .child(Spinner::new().color(theme.color_accent))
                    .child(Spinner::new().size(px(20.)).color(theme.color_text_primary))
                    .child(Spinner::new().size(px(32.)).color(theme.color_accent))
                    .child(Spinner::ring().size(px(14.)).color(theme.color_text_secondary)),
                cx,
            ))
            .child(section(
                "Badges",
                row()
                    .child(Badge::new("New"))
                    .child(Badge::new("this window").tone(BadgeTone::AccentMid))
                    .child(Badge::new("Blocked").tone(BadgeTone::Error))
                    .child(Badge::new("120 ms").tone(BadgeTone::Value)),
                cx,
            ))
    }
}

// ── kit-forms ────────────────────────────────────────────────────────────────────────────────

pub struct KitForms {
    switches: [bool; 2],
    checks: [bool; 2],
    quality: &'static str,
    volume: f32,
    bitrate: f32,
    device: Option<u32>,
    codec: Option<u32>,
}

impl KitForms {
    /// The screen's state (the self-tests drive it too).
    pub fn fixture() -> Self {
        Self::new()
    }

    /// Whether switch `index` (0: the first, off at start) is on.
    pub fn switch(&self, index: usize) -> bool {
        self.switches[index]
    }

    fn new() -> Self {
        Self {
            switches: [false, true],
            checks: [false, true],
            quality: "medium",
            volume: 30.,
            bitrate: 60.,
            device: Some(2),
            codec: Some(1),
        }
    }
}

fn devices() -> Vec<SelectOption<u32>> {
    vec![
        SelectOption::new(1, "System default"),
        SelectOption::new(2, "Default device"),
        SelectOption::new(3, "Headset (USB)").hint("Stereo · 48 kHz"),
        SelectOption::new(4, "HDMI output").disabled(true),
        SelectOption::new(5, "Speakers"),
    ]
}

impl Render for KitForms {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let quality = Rc::new(vec![
            Segment::new("low", "Low"),
            Segment::new("medium", "Medium"),
            Segment::new("high", "High"),
        ]);
        let segments = |quality: &Rc<Vec<Segment<&'static str>>>| {
            quality
                .iter()
                .map(|segment| Segment::new(segment.value, segment.label.clone()))
                .collect::<Vec<_>>()
        };
        page(cx)
            .child(section(
                "Switch · off, on, disabled",
                row()
                    .gap_4()
                    .child(Switch::new("switch-off", self.switches[0]).on_toggle(cx.listener(
                        |this, on: &bool, _, cx| {
                            this.switches[0] = *on;
                            cx.notify();
                        },
                    )))
                    .child(Switch::new("switch-on", self.switches[1]).on_toggle(cx.listener(
                        |this, on: &bool, _, cx| {
                            this.switches[1] = *on;
                            cx.notify();
                        },
                    )))
                    .child(Switch::new("switch-disabled-off", false).disabled(true))
                    .child(Switch::new("switch-disabled-on", true).disabled(true)),
                cx,
            ))
            .child(section(
                "Checkbox",
                row()
                    .gap_2()
                    .child(div().w(px(200.)).child(
                        Checkbox::new("check-a", self.checks[0]).label("Discord").on_toggle(
                            cx.listener(|this, on: &bool, _, cx| {
                                this.checks[0] = *on;
                                cx.notify();
                            }),
                        ),
                    ))
                    .child(div().w(px(200.)).child(
                        Checkbox::new("check-b", self.checks[1]).label("Firefox").on_toggle(
                            cx.listener(|this, on: &bool, _, cx| {
                                this.checks[1] = *on;
                                cx.notify();
                            }),
                        ),
                    ))
                    .child(div().w(px(200.)).child(Checkbox::new("check-c", true).label("Disabled").disabled(true)))
                    .child(Checkbox::new("check-bare", true)),
                cx,
            ))
            .child(section(
                "Segmented control",
                row()
                    .gap_6()
                    .child(div().w(px(360.)).child(
                        SegmentedControl::new("quality", segments(&quality), self.quality).on_change(
                            cx.listener(|this, value: &&'static str, _, cx| {
                                this.quality = value;
                                cx.notify();
                            }),
                        ),
                    ))
                    .child(div().w(px(360.)).child(
                        SegmentedControl::new("quality-disabled", segments(&quality), "high").disabled(true),
                    )),
                cx,
            ))
            .child(section(
                "Slider · plain, filled with ticks, disabled",
                row()
                    .gap_8()
                    .child(div().w(px(320.)).py(px(5.)).child(
                        Slider::new("volume", self.volume, 0., 100.).on_change(cx.listener(
                            |this, value: &f32, _, cx| {
                                this.volume = *value;
                                cx.notify();
                            },
                        )),
                    ))
                    .child(div().w(px(320.)).py(px(5.)).child(
                        Slider::new("bitrate", self.bitrate, 0., 100.)
                            .fill()
                            .ticks([25., 50., 75.])
                            .on_change(cx.listener(|this, value: &f32, _, cx| {
                                this.bitrate = *value;
                                cx.notify();
                            })),
                    ))
                    .child(div().w(px(320.)).py(px(5.)).child(Slider::new("disabled", 40., 0., 100.).fill().disabled(true))),
                cx,
            ))
            .child(section(
                "Select · md, placeholder, sm, sunken, disabled",
                row()
                    .items_start()
                    .child(div().w(px(240.)).child(
                        Select::options("device", self.device, devices())
                            .icon(IconName::SettingsSpeaker)
                            .on_change(cx.listener(|this, value: &u32, _, cx| {
                                this.device = Some(*value);
                                cx.notify();
                            })),
                    ))
                    .child(div().w(px(200.)).child(
                        Select::options("placeholder", None, devices()).placeholder("Choose a device…"),
                    ))
                    .child(div().w(px(140.)).child(
                        Select::options(
                            "codec",
                            self.codec,
                            vec![SelectOption::new(1, "Opus"), SelectOption::new(2, "PCM")],
                        )
                        .size(SelectSize::Sm)
                        .on_change(cx.listener(|this, value: &u32, _, cx| {
                            this.codec = Some(*value);
                            cx.notify();
                        })),
                    ))
                    .child(div().w(px(160.)).child(
                        Select::options(
                            "sunken",
                            Some(1),
                            vec![SelectOption::new(1, "Rust"), SelectOption::new(2, "TypeScript")],
                        )
                        .size(SelectSize::Sm)
                        .variant(SelectVariant::Sunken),
                    ))
                    .child(div().w(px(200.)).child(
                        Select::options("disabled-select", Some(1), devices()).disabled(true),
                    )),
                cx,
            ))
            .child(section(
                "Fields · value, hint, error, dialog label",
                row()
                    .items_start()
                    .gap_6()
                    .child(div().w(px(260.)).child(
                        Field::new("Server name").child(TextField::new("name").value("Daily Gaming")),
                    ))
                    .child(div().w(px(260.)).child(
                        Field::new("Topic")
                            .hint("Shown under the channel's name.")
                            .child(TextField::new("topic").placeholder("What's this channel about?")),
                    ))
                    .child(div().w(px(260.)).child(
                        Field::new("Invite code")
                            .error(Some("That code is taken.".into()))
                            .child(TextField::new("code").value("gaming").error(true)),
                    ))
                    .child(div().w(px(260.)).child(
                        Field::plain("Max uses").child(
                            TextField::new("search").leading_icon(IconName::SettingsSearch).placeholder("Search members"),
                        ),
                    )),
                cx,
            ))
    }
}

// ── kit-overlays ─────────────────────────────────────────────────────────────────────────────

pub struct KitOverlays {
    menu_open: bool,
    move_open: bool,
    volume: f32,
}

impl Render for KitOverlays {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Window coordinates of the content's top-left (under the title bar).
        let origin = visible_window_bounds(window, cx).origin + point(px(0.), TITLEBAR_HEIGHT);
        let theme = cx.theme();
        let anchor_box = gpui::Bounds {
            origin: origin + point(px(340.), px(358.)),
            size: gpui::size(px(40.), px(40.)),
        };
        page(cx)
            .child(
                div()
                    .absolute()
                    .left(px(24.))
                    .top(px(24.))
                    .w(px(260.))
                    .child(Label::new("Select · open").size(12.).color(theme.color_text_muted))
                    .child(
                        div().mt_2().child(
                            Select::options("open-select", Some(2u32), devices())
                                .initially_open(),
                        ),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left(px(920.))
                    .top(px(24.))
                    .child(Label::new("Menu (settings popover)").size(12.).color(theme.color_text_muted)),
            )
            .when(self.menu_open, |this| {
                this.child(
                    ContextMenu::new("menu", origin + point(px(920.), px(52.)))
                        .width(px(220.))
                        .entry(MenuItem::new("Edit nickname").icon(IconName::SettingsPencil))
                        .entry(MenuItem::new("Copy user ID").icon(IconName::SettingsKey))
                        .entry(MenuEntry::Separator)
                        .entry(MenuItem::new("Kick").icon(IconName::SettingsUserX).danger())
                        .entry(MenuItem::new("Ban").icon(IconName::SettingsBan).danger().disabled(true))
                        .on_dismiss(cx.listener(|this, _, _, cx| {
                            this.menu_open = false;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .absolute()
                    .left(px(340.))
                    .top(px(330.))
                    .child(Label::new("Rail label").size(12.).color(theme.color_text_muted)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(340.))
                    .top(px(358.))
                    .size_10()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_md)
                    .bg(theme.color_surface_hover)
                    .child(crate::Icon::new(IconName::SettingsTabAudio).color(theme.color_text_secondary))
                    .child(Tooltip::new("rail", "Audio", anchor_box).shown(true)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(920.))
                    .top(px(330.))
                    .child(Label::new("Menu rows · label, checkable, unfolding, slider").size(12.).color(theme.color_text_muted)),
            )
            .child(
                ContextMenu::new("rows", origin + point(px(920.), px(358.)))
                    .width(px(220.))
                    .entry(MenuEntry::Label("Output".into()))
                    .entry(MenuItem::new("Default").checked(true).keep_open())
                    .entry(MenuItem::new("Headset (USB)").checked(false).keep_open())
                    .entry(MenuEntry::Separator)
                    .entry(
                        MenuItem::new("Move to…")
                            .expander(self.move_open)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.move_open = !this.move_open;
                                cx.notify();
                            })),
                    )
                    .entry(MenuEntry::Custom(
                        div()
                            .px_3p5()
                            .py_3()
                            .child(
                                div()
                                    .mb_2p5()
                                    .flex()
                                    .justify_between()
                                    .child(Label::new("USER VOLUME").size(11.).weight(600.).color(theme.color_text_muted))
                                    .child(LinkButton::new("volume-reset", "Reset").size(11.).weight(500.).recolor()),
                            )
                            .child(
                                div().py(px(5.)).child(Slider::new("menu-volume", self.volume, 0., 100.).ticks([50.]).on_change(
                                    cx.listener(|this, value: &f32, _, cx| {
                                        this.volume = *value;
                                        cx.notify();
                                    }),
                                )),
                            )
                            .into_any_element(),
                    )),
            )
            .child(ToastStack::new())
    }
}

// ── kit-dialogs ──────────────────────────────────────────────────────────────────────────────

pub struct KitDialogs {
    confirm: bool,
}

impl Render for KitDialogs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        page(cx)
            .items_start()
            .child(Label::new("A message list would be under this dialog.").size(13.).color(theme.color_text_secondary))
            .child(
                Button::new("open-confirm", "Delete message…")
                    .variant(ButtonVariant::Danger)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm = true;
                        cx.notify();
                    })),
            )
            .child(
                ConfirmModal::new("confirm", self.confirm, "Delete message", "Delete")
                    .tone(ConfirmTone::Danger)
                    .message("Delete this message? This cannot be undone.")
                    .child(
                        div()
                            .mt_3()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .child(Label::new("Tip: hold").size(11.).color(theme.color_text_muted))
                            .child(
                                div()
                                    .rounded(theme.radius_sm)
                                    .border_1()
                                    .border_color(theme.color_border)
                                    .bg(theme.color_bg_darkest)
                                    .px_1p5()
                                    .child(Label::new("Shift").font(crate::FontRole::Mono).size(10.).color(theme.color_text_secondary)),
                            )
                            .child(
                                Label::new("while clicking the trash icon to skip this confirmation next time.")
                                    .size(11.)
                                    .color(theme.color_text_muted),
                            ),
                    )
                    .on_confirm(cx.listener(|this, _, _, cx| {
                        this.confirm = false;
                        cx.notify();
                    }))
                    .on_cancel(cx.listener(|this, _, _, cx| {
                        this.confirm = false;
                        cx.notify();
                    })),
            )
    }
}

// ── kit-settings ─────────────────────────────────────────────────────────────────────────────

pub struct KitSettings {
    tab: usize,
    open: bool,
    scroll: ScrollHandle,
    dirty: bool,
    nudge: u32,
    notifications: bool,
    delay: f32,
    device: Option<u32>,
}

const TABS: [(IconName, &str); 9] = [
    (IconName::SettingsTabAccount, "Account"),
    (IconName::SettingsTabDevices, "Devices"),
    (IconName::SettingsTabAppearance, "Appearance"),
    (IconName::SettingsTabPrivacy, "Privacy"),
    (IconName::SettingsTabAudio, "Audio"),
    (IconName::SettingsTabKeybinds, "Keybinds"),
    (IconName::SettingsTabNetwork, "Network"),
    (IconName::SettingsTabCodecs, "Codecs"),
    (IconName::SettingsTabAbout, "About"),
];

impl Render for KitSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut window = SettingsWindow::new("settings", self.open, &self.scroll)
            .nav_label("Settings")
            .title(TABS[self.tab].1)
            .context("Daily Gaming")
            .on_close(cx.listener(|this, _, _, cx| {
                // Unsaved changes hold the window: shake the save bar instead.
                if this.dirty {
                    this.nudge += 1;
                } else {
                    this.open = false;
                }
                cx.notify();
            }));
        for (index, (icon, label)) in TABS.iter().enumerate() {
            window = window.item(
                NavItem::new(("tab", index), *icon, *label)
                    .active(index == self.tab)
                    .dot(index == 4 && self.dirty)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.dirty && index != this.tab {
                            this.nudge += 1;
                        } else {
                            this.tab = index;
                            this.scroll.scroll_to_top();
                        }
                        cx.notify();
                    })),
            );
        }
        window = window.foot_item(
            NavItem::new("leave", IconName::SettingsLogout, "Leave Server").danger(),
        );
        let setting_row = |title: &'static str, description: &'static str, control: AnyElement| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .py_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(Label::new(title).size(14.).weight(500.).color(theme.color_text_primary))
                        .child(Label::new(description).size(12.).color(theme.color_text_muted)),
                )
                .child(control)
        };
        let content = div()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                Field::new("Output device").child(
                    Select::options("output", self.device, devices()).on_change(cx.listener(|this, value: &u32, _, cx| {
                        this.device = Some(*value);
                        this.dirty = true;
                        cx.notify();
                    })),
                ),
            )
            .child(setting_row(
                "Notification sounds",
                "Play a sound when a message arrives.",
                Switch::new("sounds", self.notifications)
                    .on_toggle(cx.listener(|this, on: &bool, _, cx| {
                        this.notifications = *on;
                        this.dirty = true;
                        cx.notify();
                    }))
                    .into_any_element(),
            ))
            .child(
                Field::new("Push to talk release delay")
                    .aside(Badge::new(SharedString::from(format!("{} ms", self.delay as i32))).tone(BadgeTone::Value))
                    .hint("Keeps your mic open briefly after you let go, so the end of a word isn't cut off.")
                    .child(div().py(px(5.)).child(Slider::new("delay", self.delay, 0., 1000.).step(10.).on_change(
                        cx.listener(|this, value: &f32, _, cx| {
                            this.delay = *value;
                            this.dirty = true;
                            cx.notify();
                        }),
                    ))),
            )
            .children((0..6).map(|i| {
                setting_row(
                    ["Show link previews", "Unfiltered GIF search", "Compact mode", "Show timestamps", "Reduce motion", "Developer mode"][i],
                    "A setting that makes the pane tall enough to scroll.",
                    Switch::new(("extra", i), i % 2 == 0).into_any_element(),
                )
            }))
            .child(
                SaveBar::new("save", &self.scroll, self.dirty, self.nudge)
                    .on_reset(cx.listener(|this, _, _, cx| {
                        this.dirty = false;
                        this.nudge = 0;
                        cx.notify();
                    }))
                    .on_save(cx.listener(|this, _, _, cx| {
                        this.dirty = false;
                        this.nudge = 0;
                        cx.notify();
                    })),
            );
        div()
            .id("kit-settings")
            .size_full()
            .flex()
            .items_start()
            .bg(theme.color_bg_primary)
            .p_6()
            .child(
                Button::new("reopen", "Open settings").on_click(cx.listener(|this, _, _, cx| {
                    this.open = true;
                    cx.notify();
                })),
            )
            .child(window.child(content))
    }
}

// ── kit-identity ─────────────────────────────────────────────────────────────────────────────

pub struct KitIdentity;

impl Render for KitIdentity {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let people: [(i64, &str); 8] = [
            (1, "aurora"),
            (2, "basil"),
            (3, "cedar"),
            (4, "dahlia"),
            (5, "ember"),
            (6, "fern"),
            (7, "gale"),
            (0, "guest"),
        ];
        let ring = theme.color_bg_primary;
        let left = div()
            .flex()
            .flex_col()
            .gap_6()
            .w(px(600.))
            .child(section(
                "Letter avatars · 16 20 24 32 40 48 80",
                row().items_end().children([16., 20., 24., 32., 40., 48., 80.].map(|size| LetterAvatar::new(1, "aurora", size))),
                cx,
            ))
            .child(section(
                "Colours by uid (0: by name)",
                row().children(people.map(|(uid, name)| Avatar::new(uid, name, 32.))),
                cx,
            ))
            .child(section(
                "With presence · online, away, do not disturb, offline",
                row().gap_4().children(
                    [Presence::Online, Presence::Away, Presence::Dnd, Presence::Offline]
                        .into_iter()
                        .enumerate()
                        .map(|(i, presence)| {
                            div()
                                .relative()
                                .child(LetterAvatar::new(people[i].0, people[i].1, 40.))
                                .child(
                                    div()
                                        .absolute()
                                        .bottom(px(-1.))
                                        .right(px(-1.))
                                        .child(StatusDot::new(presence, ring).size(13.)),
                                )
                        }),
                ),
                cx,
            ))
            .child(section(
                "Status dots · 10 13 16 24",
                row().children([10., 13., 16., 24.].map(|size| {
                    row().gap_1().children(
                        [Presence::Online, Presence::Away, Presence::Dnd, Presence::Offline]
                            .map(|presence| StatusDot::new(presence, ring).size(size)),
                    )
                })),
                cx,
            ))
            .child(section(
                "Status pills",
                row().children([Presence::Online, Presence::Away, Presence::Dnd, Presence::Offline].map(StatusPill::new)),
                cx,
            ))
            .child(section(
                "Bell mark",
                row().items_end().gap_8().child(BellMark::new("mark")).child(BellMark::new("mark-big").height(56.).bar_width(5.).gap(3.)),
                cx,
            ));
        let right = div()
            .flex()
            .flex_col()
            .gap_6()
            .w(px(580.))
            .child(section(
                "Error card · notice",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(ErrorCard::new("error-card", "Couldn't reach Daily Gaming. Check your connection; Decibell keeps trying."))
                    .child(Notice::error("You don't have permission to send messages here.")),
                cx,
            ))
            .child(section(
                "Banners",
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Banner::reconnecting())
                    .child(Banner::notice("crash-banner", "Decibell sends anonymous crash reports to help us fix bugs.")),
                cx,
            ))
            .child(section(
                "Empty state",
                EmptyState::new(IconName::SettingsBan, "No bans", "Members you ban show up here, with who banned them and why."),
                cx,
            ));
        page(cx).flex_row().gap_8().child(left).child(right)
    }
}

// ── kit-scroll ───────────────────────────────────────────────────────────────────────────────

/// Mirrors `pages/kit-scroll.tsx` box for box (the Chromium scrollbar measurements).
pub struct KitScroll {
    handles: [ScrollHandle; 6],
    switches: [bool; 2],
}

impl KitScroll {
    /// The screen's state (the self-tests open it too).
    pub fn fixture() -> Self {
        Self::new()
    }

    fn new() -> Self {
        let handles: [ScrollHandle; 6] = Default::default();
        handles[0].scroll_to(px(200.));
        handles[2].scroll_to_bottom();
        handles[4].scroll_to(px(200.));
        Self {
            handles,
            switches: [false, true],
        }
    }
}

fn scroll_rows(count: usize, cx: &App) -> Vec<AnyElement> {
    let theme = cx.theme();
    (0..count)
        .map(|i| {
            div()
                .flex()
                .flex_none()
                .h_8()
                .items_center()
                .px_3()
                .when(i % 2 == 0, |this| this.bg(theme.color_bg_secondary))
                .child(Label::new(SharedString::from(format!("Row {}", i + 1))).size(13.).color(theme.color_text_secondary))
                .into_any_element()
        })
        .collect()
}

impl Render for KitScroll {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let note = |x: f32, y: f32, text: &'static str| {
            div().absolute().left(px(x)).top(px(y)).child(Label::new(text).size(12.).color(theme.color_text_muted))
        };
        // `outline outline-1 outline-border`: a 1 px frame outside the box.
        let frame = |x: f32, y: f32, h: f32| {
            div()
                .absolute()
                .left(px(x - 1.))
                .top(px(y - 1.))
                .w(px(302.))
                .h(px(h + 2.))
                .border_1()
                .border_color(theme.color_border)
                .bg(theme.color_bg_primary)
        };
        let box_area = |id: &'static str, handle: &ScrollHandle| ScrollArea::new(id).track_scroll(handle).size_full();
        let stripe = theme.color_bg_secondary;
        div()
            .id("kit-scroll")
            .relative()
            .size_full()
            .bg(theme.color_bg_primary)
            .child(div().absolute().left_4().top_3().child(Label::new("kit-scroll · Chromium scrollbars under globals.css · Switch").size(14.).weight(600.)))
            .child(note(16., 40., "A · overflow-y-auto · scrollTop 200"))
            .child(frame(16., 64., 400.).child(box_area("a", &self.handles[0]).children(scroll_rows(40, cx))))
            .child(note(336., 40., "B · gutter stable · no overflow"))
            .child(frame(336., 64., 400.).child(box_area("b", &self.handles[1]).gutter_stable().children(scroll_rows(5, cx))))
            .child(note(656., 40., "C · at the bottom · thumb hovered"))
            .child(frame(656., 64., 400.).child(box_area("c", &self.handles[2]).children(scroll_rows(40, cx))))
            .child(note(976., 40., "D · SETTINGS_PANE · scrollTop 0"))
            .child(frame(976., 64., 400.).child(box_area("d", &self.handles[3]).gutter_stable().children(scroll_rows(40, cx))))
            .child(note(16., 488., "Switch (off, on)"))
            .child(
                div()
                    .absolute()
                    .left_4()
                    .top(px(512.))
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(Switch::new("off", self.switches[0]).on_toggle(cx.listener(|this, on: &bool, _, cx| {
                        this.switches[0] = *on;
                        cx.notify();
                    })))
                    .child(Switch::new("on", self.switches[1]).on_toggle(cx.listener(|this, on: &bool, _, cx| {
                        this.switches[1] = *on;
                        cx.notify();
                    }))),
            )
            .child(note(336., 488., "E · control: scrollbar-width/color auto"))
            .child(frame(336., 512., 240.).child(box_area("e", &self.handles[4]).children(scroll_rows(40, cx))))
            .child(note(656., 488., "F · 24 000 px of content (minimum thumb)"))
            .child(frame(656., 512., 240.).child(box_area("f", &self.handles[5]).windowed(
                750,
                RowHeights::Uniform(px(32.)),
                move |range, _, _| {
                    range
                        .map(|i| div().size_full().when(i % 2 == 0, |this| this.bg(stripe)).into_any_element())
                        .collect()
                },
            )))
    }
}

/// The kit's screens (see `decibell_harness::Screen`).
pub fn screens() -> Vec<decibell_harness::Screen> {
    use decibell_harness::Screen;
    vec![
        Screen {
            name: "kit-buttons",
            about: "Buttons in every variant and size, icon buttons, links, spinners, badges",
            build: |_, cx| {
                settle_motion_for_capture(cx);
                cx.new(|_| KitButtons).into()
            },
        },
        Screen {
            name: "kit-forms",
            about: "Switch, checkbox, segmented control, slider, select, fields",
            build: |_, cx| {
                settle_motion_for_capture(cx);
                cx.new(|_| KitForms::new()).into()
            },
        },
        Screen {
            name: "kit-overlays",
            about: "An open select, menus, a rail tooltip and the toast stack",
            build: |_, cx| {
                settle_motion_for_capture(cx);
                push_gallery_toasts(cx);
                cx.new(|_| KitOverlays {
                    menu_open: true,
                    move_open: true,
                    volume: 50.,
                })
                .into()
            },
        },
        Screen {
            name: "kit-dialogs",
            about: "The confirmation dialog over the window",
            build: |_, cx| {
                settle_motion_for_capture(cx);
                cx.new(|_| KitDialogs { confirm: true }).into()
            },
        },
        Screen {
            name: "kit-settings",
            about: "The settings window with a draft (capture at 1280 wide)",
            build: build_settings,
        },
        Screen {
            name: "kit-settings-rail",
            about: "The settings window below 1024 px: the icon rail (capture at 900 wide)",
            build: build_settings,
        },
        Screen {
            name: "kit-identity",
            about: "Letter avatars, presence dots and pills, the bell mark, error card, empty state, banners",
            build: |_, cx| {
                settle_motion_for_capture(cx);
                cx.new(|_| KitIdentity).into()
            },
        },
        Screen {
            name: "kit-scroll",
            about: "Scroll areas with Chromium's scrollbar, as pages/kit-scroll.tsx measures it",
            build: |_, cx| {
                settle_motion_for_capture(cx);
                cx.new(|_| KitScroll::new()).into()
            },
        },
    ]
}

fn build_settings(_: &mut Window, cx: &mut App) -> gpui::AnyView {
    settle_motion_for_capture(cx);
    cx.new(|_| KitSettings {
        tab: 4,
        open: true,
        scroll: ScrollHandle::new(),
        dirty: true,
        nudge: 0,
        notifications: true,
        delay: 120.,
        device: Some(2),
    })
    .into()
}

/// The overlays screen's toasts: one of each kind, held for an hour so the screen stays put.
fn push_gallery_toasts(cx: &mut App) {
    let hour = Duration::from_secs(3600);
    Toasts::push(
        cx,
        Toast::error("Couldn't send the message").body("The server refused it: you are sending messages too quickly.").duration(hour),
    );
    Toasts::push(cx, Toast::success("Invite link copied").duration(hour));
    Toasts::push(cx, Toast::info("A new version is ready").action("Restart", |_, _| {}).duration(hour));
}

