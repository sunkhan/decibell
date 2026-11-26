//! Small structural pieces: dividers, section labels, badges, notices, the error card, the empty
//! state and the window-wide banners.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClickEvent, ElementId, IntoElement, RenderOnce,
    SharedString, Window, div, prelude::*, px,
};

use crate::{ActiveTheme as _, FontRole, Icon, IconButton, IconName, Label, motion};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A 1 px `bg-border-divider` line across its parent (`h-px`); style it further as a div.
///
/// ```ignore
/// divider(cx).mx_2p5().my_1()   // a menu's separator
/// ```
pub fn divider(cx: &App) -> gpui::Div {
    div().h(px(1.)).w_full().flex_none().bg(cx.theme().color_border_divider)
}

/// A 1 px `bg-border-divider` line down its parent (`w-px`).
pub fn vertical_divider(cx: &App) -> gpui::Div {
    div().w(px(1.)).h_full().flex_none().bg(cx.theme().color_border_divider)
}

/// The uppercase group label (`SectionLabel` in settingsUi: `font-section text-section
/// font-medium uppercase leading-none tracking-section text-text-muted`). GPUI has no
/// letter-spacing: the tracking is not drawn.
///
/// ```ignore
/// SectionLabel::new("Members")
/// ```
#[derive(IntoElement)]
pub struct SectionLabel {
    text: SharedString,
}

impl SectionLabel {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl RenderOnce for SectionLabel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let size = theme.scaled.text_section;
        Label::new(SharedString::from(self.text.to_uppercase()))
            .font(FontRole::Section)
            .size(size)
            .line_height(size)
            .weight(500.)
            .color(theme.color_text_muted)
    }
}

/// A heading: the settings pane's title (`font-display text-[18px] font-semibold`) or a dialog's
/// (`text-[16px] font-semibold`).
#[derive(IntoElement)]
pub struct Heading {
    text: SharedString,
    dialog: bool,
}

impl Heading {
    /// The pane title.
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            dialog: false,
        }
    }

    /// A dialog's title.
    pub fn dialog(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            dialog: true,
        }
    }
}

impl RenderOnce for Heading {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        if self.dialog {
            Label::new(self.text)
                .font(FontRole::Display)
                .size(16.)
                .weight(600.)
                .color(theme.color_text_primary)
        } else {
            Label::new(self.text)
                .font(FontRole::Display)
                .size(18.)
                .weight(600.)
                .color(theme.color_text_primary)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeTone {
    /// `bg-accent-soft px-1.5 py-px text-[11px] font-semibold text-accent-bright`.
    Accent,
    /// `bg-accent-mid px-1.5 py-0.5 text-[11px] font-semibold text-accent-bright` ("this window").
    AccentMid,
    /// `bg-error/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-error`.
    Error,
    /// `bg-accent-soft px-2 py-0.5 text-[12px] font-medium text-accent-bright` (a value readout:
    /// "120 ms").
    Value,
}

/// A small `rounded-sm` pill.
///
/// ```ignore
/// Badge::new("this window").tone(BadgeTone::AccentMid)
/// ```
#[derive(IntoElement)]
pub struct Badge {
    text: SharedString,
    tone: BadgeTone,
}

impl Badge {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            tone: BadgeTone::Accent,
        }
    }

    pub fn tone(mut self, tone: BadgeTone) -> Self {
        self.tone = tone;
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, color, size, weight, py, px_, upper) = match self.tone {
            BadgeTone::Accent => (theme.color_accent_soft, theme.color_accent_bright, 11., 600., 1., 6., false),
            BadgeTone::AccentMid => (theme.color_accent_mid, theme.color_accent_bright, 11., 600., 2., 6., false),
            BadgeTone::Error => (theme.color_error.opacity(0.15), theme.color_error, 10., 600., 2., 6., true),
            BadgeTone::Value => (theme.color_accent_soft, theme.color_accent_bright, 12., 500., 2., 8., false),
        };
        let text = if upper {
            SharedString::from(self.text.to_uppercase())
        } else {
            self.text
        };
        div()
            .flex_none()
            .rounded(theme.radius_sm)
            .bg(bg)
            .px(px(px_))
            .py(px(py))
            .child(Label::new(text).size(size).weight(weight).color(color))
    }
}

/// An inline notice (`flex items-center gap-2 rounded-md border border-error/30 bg-error/10 px-3
/// py-2.5 text-[13px] text-error` with settingsUi's 15 px `alert` icon), the settings tabs'
/// failure line.
#[derive(IntoElement)]
pub struct Notice {
    text: SharedString,
}

impl Notice {
    pub fn error(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl RenderOnce for Notice {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .items_center()
            .gap_2()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_error.opacity(0.3))
            .bg(theme.color_error.opacity(0.1))
            .px_3()
            .py_2p5()
            .child(Icon::new(IconName::SettingsAlert).size(px(15.)).color(theme.color_error))
            .child(Label::new(self.text).size(13.).color(theme.color_error))
    }
}

/// `components/ErrorCard.tsx`: `flex max-w-[520px] items-start gap-3 rounded-md border
/// border-warning/10 bg-warning/[0.06] px-4 py-3.5`, a 32 px `bg-warning/[0.12]` disc with the
/// warning triangle, the text `text-[13px] leading-[1.55] text-text-secondary`; enters with
/// `fadeUp` (0.4 s `ease`, from 6 px down).
///
/// ```ignore
/// ErrorCard::new(("load-error", channel_id), "Couldn't load this channel's messages.")
/// ```
#[derive(IntoElement)]
pub struct ErrorCard {
    id: ElementId,
    text: SharedString,
    icon: IconName,
}

impl ErrorCard {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            icon: IconName::AlertTriangle,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = icon;
        self
    }
}

impl RenderOnce for ErrorCard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .max_w(px(520.))
            .items_start()
            .gap_3()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_warning.opacity(0.1))
            .bg(theme.color_warning.opacity(0.06))
            .px_4()
            .py_3p5()
            .child(
                div()
                    .mt(px(1.))
                    .flex()
                    .flex_none()
                    .size_8()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(theme.color_warning.opacity(0.12))
                    .child(Icon::new(self.icon).size(px(16.)).color(theme.color_warning)),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Label::new(self.text)
                        .size(13.)
                        .line_height(13. * 1.55)
                        .color(theme.color_text_secondary),
                ),
            )
            .with_animation(
                self.id,
                Animation::new(Duration::from_millis(400)).with_easing(motion::EASE.easing()),
                |card, t| card.opacity(t).top(px(6. * (1. - t))),
            )
    }
}

/// The empty state of a settings list (`AuditLogTab` `EmptyState`): a 48 px `rounded-lg
/// bg-bg-light shadow-raised` tile with a 20 px muted icon, a `font-display text-[16px]
/// font-semibold` title, a `max-w-sm text-[13px] leading-[1.55] text-text-muted` line, and an
/// optional action under it.
///
/// ```ignore
/// EmptyState::new(IconName::SettingsBan, "No bans", "Members you ban show up here.")
/// ```
#[derive(IntoElement)]
pub struct EmptyState {
    icon: IconName,
    title: SharedString,
    body: SharedString,
    action: Option<AnyElement>,
}

impl EmptyState {
    pub fn new(icon: IconName, title: impl Into<SharedString>, body: impl Into<SharedString>) -> Self {
        Self {
            icon,
            title: title.into(),
            body: body.into(),
            action: None,
        }
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .items_center()
            .px_6()
            .py(px(56.))
            .child(
                div()
                    .mb_4()
                    .flex()
                    .size_12()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_lg)
                    .bg(theme.color_bg_light)
                    .shadow(theme.shadow_raised.clone())
                    .child(Icon::new(self.icon).size(px(20.)).color(theme.color_text_muted)),
            )
            .child(
                div().mb_1().child(
                    Label::new(self.title)
                        .font(FontRole::Display)
                        .size(16.)
                        .weight(600.)
                        .color(theme.color_text_primary),
                ),
            )
            .child(
                div().mb_4().max_w(px(384.)).text_center().child(
                    Label::new(self.body)
                        .size(13.)
                        .line_height(13. * 1.55)
                        .color(theme.color_text_muted),
                ),
            )
            .children(self.action)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    /// "Connection lost. Reconnecting...": `h-8 bg-warning text-xs font-semibold text-bg-primary`,
    /// centred.
    Reconnecting,
    /// The crash-reporting disclosure: `h-9 border-b border-border-divider bg-accent-soft px-4
    /// text-[12px] text-accent-bright`, with a dismiss button.
    Notice,
}

/// A strip across the top of the window (`layouts/MainLayout.tsx`, `CrashReportingBanner.tsx`).
///
/// ```ignore
/// Banner::reconnecting()
/// Banner::notice("banner", "Decibell sends anonymous crash reports…").on_dismiss(…)
/// ```
#[derive(IntoElement)]
pub struct Banner {
    id: ElementId,
    kind: BannerKind,
    text: SharedString,
    on_dismiss: Option<ClickHandler>,
}

impl Banner {
    pub fn reconnecting() -> Self {
        Self {
            id: ElementId::Name("reconnecting".into()),
            kind: BannerKind::Reconnecting,
            text: SharedString::new_static("Connection lost. Reconnecting..."),
            on_dismiss: None,
        }
    }

    pub fn notice(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            kind: BannerKind::Notice,
            text: text.into(),
            on_dismiss: None,
        }
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Banner {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        match self.kind {
            BannerKind::Reconnecting => div()
                .flex()
                .flex_none()
                .h_8()
                .items_center()
                .justify_center()
                .bg(theme.color_warning)
                .child(
                    Label::new(self.text)
                        .size(12.)
                        .line_height(16.)
                        .weight(600.)
                        .color(theme.color_bg_primary),
                )
                .into_any_element(),
            BannerKind::Notice => {
                let on_dismiss = self.on_dismiss;
                div()
                    .flex()
                    .flex_none()
                    .h_9()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.color_border_divider)
                    .bg(theme.color_accent_soft)
                    .px_4()
                    .child(Label::new(self.text).size(12.).color(theme.color_accent_bright))
                    .child(
                        div().ml_3().child(
                            IconButton::new((self.id, "dismiss"), IconName::Close)
                                .small()
                                .color(theme.color_accent_bright, theme.color_accent_bright)
                                .hover_bg(theme.color_accent_mid)
                                .on_click(move |event, window, cx| {
                                    if let Some(handler) = &on_dismiss {
                                        handler(event, window, cx);
                                    }
                                }),
                        ),
                    )
                    .into_any_element()
            }
        }
    }
}
