//! The live previews beside the stage: [`Crop`] (the renderer's `<Crop>`: the crop scaled into a
//! box) and the two sets the renderer ships, the profile picture's (`AvatarPreviews`) and the
//! server picture's (`ServerPreviews`).

use decibell_ui::identity::{avatar_color, person_slot};
use decibell_ui::{ActiveTheme as _, FontRole, Label, Presence, StatusDot};
use gpui::{
    AnyElement, App, Bounds, Corners, Hsla, IntoElement, Pixels, RenderOnce, SharedString, Window, canvas, div, fill,
    linear_color_stop, linear_gradient, point, prelude::*, px, size,
};

use crate::geometry::{Size, View};
use crate::picture::{Fill, Picture};
use crate::{TILE_HEIGHT, TILE_WIDTH};

/// What a preview draws: the picture, where the crop puts it, and the fill. Cheap to clone.
#[derive(Clone)]
pub struct CropState {
    pub picture: Option<Picture>,
    pub view: View,
    pub frame: Option<Size>,
    pub fill: Fill,
}

pub(crate) fn rgb(c: [u8; 3]) -> Hsla {
    gpui::rgb(((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32).into()
}

/// Paints the crop of `state` into `bounds` (with `radius` corners), as the renderer's `<Crop>`:
/// a box of another aspect shows the middle of the crop, as `object-cover` will.
pub(crate) fn paint_crop(state: &CropState, bounds: Bounds<Pixels>, radius: Pixels, placeholder: Hsla, window: &mut Window) {
    let corners = Corners::all(radius);
    let (Some(p), Some(frame)) = (&state.picture, state.frame) else {
        window.paint_quad(fill(bounds, placeholder).corner_radii(corners));
        return;
    };
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let k = (width / frame.w).max(height / frame.h);
    let ox = (width - frame.w * k) / 2.;
    let oy = (height - frame.h * k) / 2.;
    let bg = match state.fill {
        Fill::Color(c) => rgb(c),
        Fill::Blur => rgb(p.palette[1]),
    };
    window.paint_quad(fill(bounds, bg).corner_radii(corners));
    let at = |x: f32, y: f32, w: f32, h: f32| Bounds {
        origin: bounds.origin + point(px(x), px(y)),
        size: size(px(w), px(h)),
    };
    if state.fill == Fill::Blur {
        let (w, h) = (frame.w * k, frame.h * k);
        let s = (w / p.nat.w).max(h / p.nat.h) * 1.1;
        let rect = at(ox + (w - p.nat.w * s) / 2., oy + (h - p.nat.h * s) / 2., p.nat.w * s, p.nat.h * s);
        let _ = window.paint_image(bounds, rect, corners, p.blur.clone(), 0, false);
    }
    let v = state.view;
    let rect = at(ox + v.x * k, oy + v.y * k, p.nat.w * v.scale * k, p.nat.h * v.scale * k);
    let _ = window.paint_image(bounds, rect, corners, p.stage.clone(), 0, false);
}

/// A live preview: the crop scaled into a `width` × `height` box (`<Crop>`).
#[derive(IntoElement)]
pub struct Crop {
    state: CropState,
    width: f32,
    height: f32,
    radius: Pixels,
}

impl Crop {
    pub fn new(state: &CropState, width: f32, height: f32) -> Self {
        Crop { state: state.clone(), width, height, radius: px(0.) }
    }

    pub fn radius(mut self, radius: Pixels) -> Self {
        self.radius = radius;
        self
    }
}

impl RenderOnce for Crop {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let placeholder = cx.theme().color_bg_lighter;
        let (state, radius) = (self.state, self.radius);
        div()
            .flex_none()
            .w(px(self.width))
            .h(px(self.height))
            .child(canvas(|_, _, _| {}, move |bounds, _, window, _| paint_crop(&state, bounds, radius, placeholder, window)).size_full())
    }
}

/// The small uppercase label over each preview (`PreviewLabel`).
fn preview_label(text: &'static str, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let size = theme.scaled.text_section;
    div().mb_2().child(
        Label::new(text.to_uppercase())
            .font(FontRole::Section)
            .size(size)
            .line_height(size)
            .weight(500.)
            .tracking(theme.palette.tracking_section)
            .color(theme.color_text_muted),
    )
}

/// `userGradient(uid, name)`: `linear-gradient(135deg, av-N, color-mix(av-N 72%, #000))`.
fn user_gradient(uid: i64, name: &str, cx: &App) -> gpui::Background {
    let base = avatar_color(cx.theme(), person_slot(uid, name));
    let mut dark: gpui::Rgba = base.into();
    dark.r *= 0.72;
    dark.g *= 0.72;
    dark.b *= 0.72;
    linear_gradient(135., linear_color_stop(base, 0.), linear_color_stop(dark, 1.))
}

/// The picture where people meet it: your profile card, a message, the member list. Same sizes,
/// rings and status cut-outs as the real ones (`AvatarPreviews`).
pub(crate) fn avatar_previews(state: &CropState, uid: i64, name: &SharedString, status: Presence, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let radius = |s: f32| theme.avatar_radius(px(s));
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .child(preview_label("Profile", cx))
                .child(
                    div()
                        .overflow_hidden()
                        .rounded(theme.radius_lg)
                        .border_1()
                        .border_color(theme.color_border)
                        .bg(theme.color_bg_light)
                        .child(div().h_12().rounded_t(theme.radius_lg - px(1.)).bg(user_gradient(uid, name, cx)))
                        .child(
                            div().relative().mx(px(14.)).h_7().child(
                                // `ring-4 ring-bg-light` around the 64 px picture.
                                div()
                                    .absolute()
                                    .top(px(-32. - 4.))
                                    .left(px(-4.))
                                    .p_1()
                                    .rounded(radius(72.))
                                    .bg(theme.color_bg_light)
                                    .child(
                                        div()
                                            .relative()
                                            .child(Crop::new(state, 64., 64.).radius(radius(64.)))
                                            .child(
                                                div()
                                                    .absolute()
                                                    .bottom(px(-1.))
                                                    .right(px(-1.))
                                                    .child(StatusDot::new(status, theme.color_bg_light).size(16.).ring_width(3.)),
                                            ),
                                    ),
                            ),
                        )
                        .child(
                            div().px(px(14.)).pb_3().pt_2().child(
                                Label::new(name.clone())
                                    .font(FontRole::Display)
                                    .size(16.)
                                    .weight(600.)
                                    .color(theme.color_text_primary)
                                    .truncate(),
                            ),
                        ),
                ),
        )
        .child(
            div().child(preview_label("Chat", cx)).child(
                div()
                    .flex()
                    .gap_3()
                    .rounded(theme.radius_lg)
                    .bg(theme.color_bg_mid)
                    .px_3()
                    .py_2p5()
                    .child(Crop::new(state, 40., 40.).radius(radius(40.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_2()
                                    .child(
                                        Label::new(name.clone())
                                            .font(FontRole::Channel)
                                            .size(theme.scaled.text_sender)
                                            .weight(theme.palette.font_weight_emphasis)
                                            .color(theme.color_text_primary)
                                            .truncate(),
                                    )
                                    .child(
                                        Label::new("18:24")
                                            .font(FontRole::Meta)
                                            .size(theme.scaled.text_meta)
                                            .color(theme.color_text_muted)
                                            .nowrap(),
                                    ),
                            )
                            .child(
                                Label::new("new picture, who dis")
                                    .size(theme.scaled.text_body)
                                    .line_height(theme.scaled.text_body * theme.palette.leading_body)
                                    .color(theme.color_text_primary),
                            ),
                    ),
            ),
        )
        .child(
            div().child(preview_label("Member list", cx)).child(
                div()
                    .flex()
                    .h(px(44.))
                    .items_center()
                    .gap_2p5()
                    .rounded(theme.radius_lg)
                    .bg(theme.color_bg_tertiary)
                    .px_2p5()
                    .child(
                        div()
                            .relative()
                            .flex()
                            .child(Crop::new(state, 34., 34.).radius(radius(34.)))
                            .child(
                                div()
                                    .absolute()
                                    .bottom(px(-1.))
                                    .right(px(-1.))
                                    .child(StatusDot::new(status, theme.color_bg_tertiary).size(13.)),
                            ),
                    )
                    .child(
                        div().min_w_0().child(
                            Label::new(name.clone())
                                .font(FontRole::Channel)
                                .size(theme.scaled.text_member)
                                .weight(500.)
                                .color(theme.color_text_secondary)
                                .truncate(),
                        ),
                    ),
            ),
        )
        .into_any_element()
}

/// The tile as members see it, at its real size, resting and selected; and an invite card's
/// banner, which shows the middle of a wider band (`ServerPreviews`). The selected tile's glow
/// rests at full strength here (the server bar's breathes; a still preview needs no frames).
pub(crate) fn server_previews(state: &CropState, name: &SharedString, members: &Option<SharedString>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let chrome = &theme.chrome;
    let tile = |active: bool| {
        let radius = theme.radius_md;
        div()
            .relative()
            .flex_none()
            .w(px(TILE_WIDTH))
            .h(px(TILE_HEIGHT))
            .when(active, |this| {
                // `ActiveTileGlow`: a 1.5 px accent ring at 60 % and a 22 px / 4 px bloom at 22 %.
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(radius)
                        .shadow(vec![
                            gpui::BoxShadow {
                                color: chrome.color_accent.opacity(0.22),
                                offset: point(px(0.), px(0.)),
                                blur_radius: px(11.),
                                spread_radius: px(4.),
                                inset: false,
                            },
                            gpui::BoxShadow {
                                color: chrome.color_accent.opacity(0.6),
                                offset: point(px(0.), px(0.)),
                                blur_radius: px(0.),
                                spread_radius: px(1.5),
                                inset: false,
                            },
                        ]),
                )
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .rounded(radius)
                    .px_3()
                    .child(div().absolute().inset_0().child(Crop::new(state, TILE_WIDTH, TILE_HEIGHT).radius(radius)))
                    .when(!active, |this| {
                        // `bg-black/45` and the name in white over the picture (renderer literals).
                        this.child(div().absolute().inset_0().rounded(radius).bg(gpui::black().opacity(0.45))).child(
                            div().relative().max_w_full().child(
                                Label::new(name.clone())
                                    .font(FontRole::Tile)
                                    .size(13.)
                                    .weight(600.)
                                    .color(gpui::white())
                                    .truncate(),
                            ),
                        )
                    }),
            )
            .when(active, |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom(px(-9.))
                        .left(px((TILE_WIDTH - 20.) / 2.))
                        .w_5()
                        .h(px(3.))
                        .rounded_t(px(4.))
                        .bg(chrome.color_accent),
                )
            })
    };
    div()
        .flex()
        .items_start()
        .gap_6()
        .child(
            div().child(preview_label("Server bar", cx)).child(
                div()
                    .flex()
                    .gap_2()
                    .rounded(theme.radius_lg)
                    .bg(chrome.color_bg_darkest)
                    .px_3()
                    .pb(px(14.))
                    .pt_2p5()
                    .child(tile(false))
                    .child(tile(true)),
            ),
        )
        .child(
            div().min_w_0().flex_1().child(preview_label("Invite card", cx)).child(
                // 432 × 112 in chat, here at 60 %.
                div()
                    .w(px(260.))
                    .overflow_hidden()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.color_border)
                    .bg(theme.color_bg_dark)
                    .child(Crop::new(state, 258., 67.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .child(
                                div().min_w_0().child(
                                    Label::new(name.clone()).size(13.).weight(600.).color(theme.color_text_bright).truncate(),
                                ),
                            )
                            .when_some(members.clone(), |this, members| {
                                this.child(
                                    Label::new(members)
                                        .font(FontRole::Meta)
                                        .size(11.)
                                        .color(theme.color_text_muted)
                                        .nowrap(),
                                )
                            }),
                    ),
            ),
        )
        .into_any_element()
}
