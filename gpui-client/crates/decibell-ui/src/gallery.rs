//! The design-token sheet: the active theme's surfaces, colours, type, radii, elevation and
//! icons, drawn with the real tokens and fonts. It proves the theme and font plumbing end to
//! end, and it is the first capture target. Not part of the product.

use decibell_theme::ThemeId;
use gpui::{
    AnyElement, App, BoxShadow, Context, Div, Hsla, IntoElement, Pixels, Render, Window, div,
    prelude::*, px,
};

use crate::{ActiveTheme as _, FontRole, Icon, IconName, Label, Theme};

pub struct Gallery;

impl Render for Gallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id("gallery")
            .size_full()
            .overflow_y_scroll()
            .bg(theme.color_bg_primary)
            .text_color(theme.color_text_primary)
            .p_6()
            .flex()
            .flex_col()
            .gap_5()
            .child(header(cx))
            .child(
                div()
                    .flex()
                    .gap_6()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_5()
                            .child(surfaces(cx))
                            .child(colors(cx))
                            .child(radii(cx))
                            .child(elevation(cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_5()
                            .child(type_roles(cx))
                            .child(icons(cx)),
                    ),
            )
    }
}

fn header(cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_3()
                .child(
                    Label::new(theme.id.display_name())
                        .font(FontRole::Display)
                        .size(theme.scaled.text_title * 1.5)
                        .weight(700.),
                )
                .child(
                    Label::new(format!("{} · {:?}", theme.id, theme.id.mode()))
                        .font(FontRole::Meta)
                        .size(theme.scaled.text_meta)
                        .color(theme.color_text_muted),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(ThemeId::ALL.into_iter().map(|id| theme_chip(id, cx))),
        )
}

/// A button per theme, drawn in that theme's own colours.
fn theme_chip(id: ThemeId, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let palette = id.palette();
    let active = id == theme.id;
    div()
        .id(id.as_str())
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .rounded(theme.radius_md)
        .bg(crate::hsla(palette.color_bg_mid))
        .border_1()
        .border_color(if active {
            theme.color_accent
        } else {
            theme.color_border
        })
        .cursor_pointer()
        .on_click(move |_, _, cx| Theme::set(cx, id))
        .child(
            div()
                .size_2()
                .rounded_full()
                .bg(crate::hsla(palette.color_accent)),
        )
        .child(
            Label::new(id.display_name())
                .size(12.)
                .weight(500.)
                .color(crate::hsla(palette.color_text_primary)),
        )
}

fn section(title: &'static str, cx: &App) -> Div {
    let theme = cx.theme();
    div().flex().flex_col().gap_2().child(
        Label::new(title)
            .font(FontRole::Section)
            .size(theme.scaled.text_section)
            .weight(theme.palette.font_weight_emphasis)
            .color(theme.color_text_muted),
    )
}

fn swatch(name: &'static str, color: Hsla, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .w(px(92.))
        .child(
            div()
                .h_8()
                .rounded(theme.radius_sm)
                .border_1()
                .border_color(theme.color_border)
                .bg(color),
        )
        .child(
            Label::new(name)
                .font(FontRole::Meta)
                .size(theme.scaled.text_micro)
                .color(theme.color_text_secondary)
                .truncate(),
        )
}

fn swatches(items: Vec<(&'static str, Hsla)>, cx: &App) -> impl IntoElement {
    div().flex().flex_wrap().gap_2().children(
        items
            .into_iter()
            .map(|(name, color)| swatch(name, color, cx)),
    )
}

fn surfaces(cx: &App) -> impl IntoElement {
    let t = cx.theme();
    section("SURFACES", cx)
        .child(swatches(
            vec![
                ("bg-darkest", t.color_bg_darkest),
                ("bg-dark", t.color_bg_dark),
                ("bg-mid", t.color_bg_mid),
                ("bg-light", t.color_bg_light),
                ("bg-lighter", t.color_bg_lighter),
                ("bg-surface", t.color_bg_surface),
                ("bg-primary", t.color_bg_primary),
                ("bg-secondary", t.color_bg_secondary),
                ("bg-tertiary", t.color_bg_tertiary),
                ("bg-input", t.color_bg_input),
                ("bg-dmbar", t.color_bg_dmbar),
                ("border", t.color_border),
            ],
            cx,
        ))
        .child(swatches(
            vec![
                ("chrome titlebar", t.chrome.color_bg_titlebar),
                ("chrome darkest", t.chrome.color_bg_darkest),
                ("chrome tertiary", t.chrome.color_bg_tertiary),
                ("chrome border", t.chrome.color_border),
                ("chrome text", t.chrome.color_text_primary),
            ],
            cx,
        ))
}

fn colors(cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let ramp = |colors: [Hsla; 8]| {
        div().flex().gap_1().children(
            colors
                .into_iter()
                .map(|color| div().size_5().rounded(t.avatar_radius(px(20.))).bg(color)),
        )
    };
    section("COLOURS", cx)
        .child(swatches(
            vec![
                ("accent", t.color_accent),
                ("accent-bright", t.color_accent_bright),
                ("accent-hover", t.color_accent_hover),
                ("accent-soft", t.color_accent_soft),
                ("accent-mid", t.color_accent_mid),
                ("success", t.color_success),
                ("warning", t.color_warning),
                ("error", t.color_error),
                ("live", t.color_live),
                ("text-primary", t.color_text_primary),
                ("text-secondary", t.color_text_secondary),
                ("text-muted", t.color_text_muted),
            ],
            cx,
        ))
        .child(
            div()
                .flex()
                .gap_4()
                .child(ramp([
                    t.color_av_1,
                    t.color_av_2,
                    t.color_av_3,
                    t.color_av_4,
                    t.color_av_5,
                    t.color_av_6,
                    t.color_av_7,
                    t.color_av_8,
                ]))
                .child(ramp([
                    t.color_name_1,
                    t.color_name_2,
                    t.color_name_3,
                    t.color_name_4,
                    t.color_name_5,
                    t.color_name_6,
                    t.color_name_7,
                    t.color_name_8,
                ])),
        )
}

fn radii(cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let sample = |name: &'static str, radius: Pixels| -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .child(
                div()
                    .size_12()
                    .rounded(radius)
                    .bg(t.color_bg_surface)
                    .border_1()
                    .border_color(t.color_accent),
            )
            .child(
                Label::new(format!("{name} {}", f32::from(radius)))
                    .font(FontRole::Meta)
                    .size(t.scaled.text_micro)
                    .color(t.color_text_secondary),
            )
            .into_any_element()
    };
    section("RADII", cx).child(div().flex().gap_4().children([
        sample("sm", t.radius_sm),
        sample("md", t.radius_md),
        sample("lg", t.radius_lg),
        sample("xl", t.radius_xl),
        sample("avatar", t.avatar_radius(px(48.))),
    ]))
}

fn elevation(cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let card = |name: &'static str, shadow: &Vec<BoxShadow>| -> AnyElement {
        div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(96.))
            .h_16()
            .rounded(t.radius_lg)
            .bg(t.color_bg_surface)
            .shadow(shadow.clone())
            .child(
                Label::new(name)
                    .font(FontRole::Meta)
                    .size(t.scaled.text_meta)
                    .color(t.color_text_secondary),
            )
            .into_any_element()
    };
    section("ELEVATION", cx).child(div().flex().gap_6().p_4().children([
        card("raised", &t.shadow_raised),
        card("float", &t.shadow_float),
        card("modal", &t.shadow_modal),
        card("ring", &t.shadow_ring),
        card("glow", &t.shadow_glow),
    ]))
}

fn type_roles(cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let s = &t.scaled;
    let weights = |font: FontRole, size: f32| {
        div()
            .flex()
            .flex_wrap()
            .gap_x_4()
            .children([400., 500., 600., 700.].map(|weight| {
                Label::new(format!("Hamburg {weight}"))
                    .font(font)
                    .size(size)
                    .weight(weight)
            }))
    };
    section("TYPE", cx)
        .child(type_row(
            "title · display",
            Label::new("The quick brown fox jumps over the lazy dog")
                .font(FontRole::Display)
                .size(s.text_title)
                .weight(600.)
                .color(t.color_text_bright),
            cx,
        ))
        .child(type_row(
            "body · sans",
            Label::new("Sphinx of black quartz, judge my vow: 0123456789 ıİğüşöç ÀÉÎõü")
                .size(s.text_body)
                .line_height(s.text_body * t.palette.leading_body),
            cx,
        ))
        .child(type_row(
            "sender · sans 600",
            Label::new("aurora")
                .size(s.text_sender)
                .weight(t.palette.font_weight_emphasis)
                .color(t.color_name_3),
            cx,
        ))
        .child(type_row(
            "channel",
            Label::new("# general")
                .font(FontRole::Channel)
                .size(s.text_channel)
                .weight(500.)
                .color(t.color_text_secondary),
            cx,
        ))
        .child(type_row(
            "member",
            Label::new("basil · In voice")
                .font(FontRole::Channel)
                .size(s.text_member),
            cx,
        ))
        .child(type_row(
            "section",
            Label::new("TEXT CHANNELS")
                .font(FontRole::Section)
                .size(s.text_section)
                .weight(t.palette.font_weight_emphasis)
                .color(t.color_text_muted),
            cx,
        ))
        .child(type_row(
            "meta",
            Label::new("Today at 14:32 · 1,204 messages")
                .font(FontRole::Meta)
                .size(s.text_meta)
                .color(t.color_text_muted),
            cx,
        ))
        .child(type_row(
            "micro",
            Label::new("NEW · 12")
                .font(FontRole::Meta)
                .size(s.text_micro)
                .weight(600.)
                .color(t.color_accent),
            cx,
        ))
        .child(type_row(
            "reply",
            Label::new("replying to a message that is cut when it gets too long")
                .size(s.text_reply)
                .line_height(s.text_reply_line_height)
                .color(t.color_text_secondary)
                .truncate(),
            cx,
        ))
        .child(type_row(
            "mono",
            Label::new("fn main() { let x = 0x2a; } // 0O 1lI")
                .font(FontRole::Mono)
                .size(s.text_body * 0.9),
            cx,
        ))
        .child(type_row(
            "tile",
            Label::new("Daily Gaming")
                .font(FontRole::Tile)
                .size(13.)
                .weight(600.),
            cx,
        ))
        .child(type_row(
            "wordmark",
            Label::new("DECIBELL")
                .font(FontRole::Wordmark)
                .size(13.)
                .weight(600.)
                .color(t.color_text_muted),
            cx,
        ))
        .child(type_row(
            "sans weights",
            weights(FontRole::Sans, s.text_body),
            cx,
        ))
        .child(type_row(
            "meta weights",
            weights(FontRole::Meta, s.text_body),
            cx,
        ))
        .child(type_row(
            "tile weights",
            weights(FontRole::Tile, s.text_body),
            cx,
        ))
        .child(type_row(
            "mono weights",
            weights(FontRole::Mono, s.text_meta),
            cx,
        ))
}

fn type_row(role: &'static str, sample: impl IntoElement, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    div()
        .flex()
        .items_baseline()
        .gap_3()
        .child(
            div().w(px(132.)).flex_none().child(
                Label::new(role)
                    .font(FontRole::Meta)
                    .size(t.scaled.text_micro)
                    .color(t.color_text_muted),
            ),
        )
        .child(div().flex_1().min_w_0().child(sample))
}

fn icons(cx: &App) -> impl IntoElement {
    let t = cx.theme();
    const ICONS: [IconName; 6] = [
        IconName::LucideSettings,
        IconName::LucideUserPlus,
        IconName::WindowMinimize,
        IconName::WindowMaximize,
        IconName::WindowRestore,
        IconName::WindowClose,
    ];
    let line = |size: f32, color: Hsla, stroke_width: Option<f32>| {
        div()
            .flex()
            .items_center()
            .gap_4()
            .children(ICONS.map(|path| {
                let icon = Icon::new(path).size(px(size)).color(color);
                match stroke_width {
                    Some(width) => icon.stroke_width(width),
                    None => icon,
                }
            }))
    };
    section("ICONS", cx)
        .child(line(16., t.color_text_secondary, None))
        .child(line(20., t.color_text_primary, Some(1.5)))
        .child(line(24., t.color_accent, None))
}
