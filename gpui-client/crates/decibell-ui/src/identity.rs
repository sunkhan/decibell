//! Identity bits that need no model: the letter avatar and its colour hash (`utils/colors.ts`),
//! an avatar slot for a decoded picture, the presence dot and pill (`components/StatusDot.tsx`),
//! and the Decibell mark (`components/BellMark.tsx`).

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, ElementId, Hsla, ImageSource, IntoElement, Pixels,
    RenderImage, RenderOnce, SharedString, Window, div, img, linear_color_stop, linear_gradient,
    prelude::*, px, svg,
};

use crate::icon::icon_path;
use crate::{ActiveTheme as _, IconName, Label, Theme, motion};

/// The renderer's `rampIndex`: `hash = charCode + ((hash << 5) - hash)` over the string's UTF-16
/// units, as JavaScript evaluates it (the shift on the int32 of `hash`, the rest in doubles),
/// then `abs(hash) % 8 + 1`. A slot of the 8 `--color-av-*` / `--color-name-*` ramps.
pub fn ramp_index(text: &str) -> usize {
    let mut hash: f64 = 0.;
    for unit in text.encode_utf16() {
        let int32 = to_int32(hash);
        let shifted = int32.wrapping_shl(5) as f64;
        hash = unit as f64 + (shifted - hash);
    }
    (hash.abs() % 8.) as usize + 1
}

/// ECMAScript ToInt32.
fn to_int32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    let modulo = value.trunc().rem_euclid(4_294_967_296.);
    if modulo >= 2_147_483_648. {
        (modulo - 4_294_967_296.) as i32
    } else {
        modulo as i32
    }
}

/// The string a person's colour hashes (`personKey`): `uid:<uid>` with an account, else the name
/// they were shown under.
pub fn person_key(uid: i64, name: &str) -> String {
    if uid > 0 {
        format!("uid:{uid}")
    } else {
        name.to_owned()
    }
}

/// A person's ramp slot, 1–8 (`userColor` / `userGradient`).
pub fn person_slot(uid: i64, name: &str) -> usize {
    ramp_index(&person_key(uid, name))
}

/// The avatar fill of slot 1–8 (`--color-av-N`).
pub fn avatar_color(theme: &Theme, slot: usize) -> Hsla {
    [
        theme.color_av_1,
        theme.color_av_2,
        theme.color_av_3,
        theme.color_av_4,
        theme.color_av_5,
        theme.color_av_6,
        theme.color_av_7,
        theme.color_av_8,
    ][slot.clamp(1, 8) - 1]
}

/// A sender name's colour of slot 1–8 (`--color-name-N`).
pub fn name_color(theme: &Theme, slot: usize) -> Hsla {
    [
        theme.color_name_1,
        theme.color_name_2,
        theme.color_name_3,
        theme.color_name_4,
        theme.color_name_5,
        theme.color_name_6,
        theme.color_name_7,
        theme.color_name_8,
    ][slot.clamp(1, 8) - 1]
}

/// `color-mix(in srgb, c 72%, #000)`: the gradient's darker stop.
fn darker(color: Hsla) -> Hsla {
    let mut rgba: gpui::Rgba = color.into();
    rgba.r *= 0.72;
    rgba.g *= 0.72;
    rgba.b *= 0.72;
    rgba.into()
}

/// The letter avatar (`components/LetterAvatar.tsx`): `linear-gradient(135deg, av-N,
/// color-mix(av-N 72%, #000))` behind the name's first letter (upper-cased, `font-semibold`,
/// `--color-av-fg`, `max(10, floor(size × 0.42))` px), `rounded-avatar` (26 % of the box in most
/// themes, 4 px in console). The colour hashes the uid (0 or less: the name), so a rename keeps it.
///
/// ```ignore
/// LetterAvatar::new(member.uid, &member.name, 32.)
/// ```
#[derive(IntoElement)]
pub struct LetterAvatar {
    uid: i64,
    name: SharedString,
    size: f32,
}

impl LetterAvatar {
    pub fn new(uid: i64, name: impl Into<SharedString>, size: f32) -> Self {
        Self {
            uid,
            name: name.into(),
            size,
        }
    }
}

impl RenderOnce for LetterAvatar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let base = avatar_color(theme, person_slot(self.uid, &self.name));
        let letter: SharedString = self
            .name
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect::<String>())
            .unwrap_or_else(|| "?".into())
            .into();
        let font_size = (self.size * 0.42).floor().max(10.);
        let size = px(self.size);
        div()
            .flex()
            .flex_none()
            .size(size)
            .items_center()
            .justify_center()
            .rounded(theme.avatar_radius(size))
            .bg(linear_gradient(
                135.,
                linear_color_stop(base, 0.),
                linear_color_stop(darker(base), 1.),
            ))
            .child(
                Label::new(letter)
                    .size(font_size)
                    .line_height(font_size * 1.5)
                    .weight(600.)
                    .color(theme.color_av_fg),
            )
    }
}

/// An avatar that shows a decoded picture when it has one and the letter avatar until then (or
/// when the picture failed). Fetching and caching the pictures is the avatar store's job; this
/// only draws what it is given, cropped to `rounded-avatar`.
///
/// ```ignore
/// Avatar::new(uid, name, 40.).image(avatar_store.get(uid))
/// ```
#[derive(IntoElement)]
pub struct Avatar {
    uid: i64,
    name: SharedString,
    size: f32,
    image: Option<Arc<RenderImage>>,
}

impl Avatar {
    pub fn new(uid: i64, name: impl Into<SharedString>, size: f32) -> Self {
        Self {
            uid,
            name: name.into(),
            size,
            image: None,
        }
    }

    pub fn image(mut self, image: Option<Arc<RenderImage>>) -> Self {
        self.image = image;
        self
    }
}

impl RenderOnce for Avatar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        match self.image {
            Some(image) => {
                let size = px(self.size);
                img(ImageSource::Render(image))
                    .size(size)
                    .flex_none()
                    .rounded(cx.theme().avatar_radius(size))
                    .into_any_element()
            }
            None => LetterAvatar::new(self.uid, self.name, self.size).into_any_element(),
        }
    }
}

/// Presence as the dot shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Online,
    Away,
    Dnd,
    /// Offline, and invisible (an invisible user is shown offline).
    Offline,
}

impl Presence {
    fn color(self, theme: &Theme) -> Hsla {
        match self {
            Presence::Online => theme.color_success,
            Presence::Away => theme.color_warning,
            Presence::Dnd => theme.color_error,
            Presence::Offline => theme.color_text_muted,
        }
    }

    fn glyph(self) -> IconName {
        match self {
            Presence::Online => IconName::StatusOnline,
            Presence::Away => IconName::StatusAway,
            Presence::Dnd => IconName::StatusDnd,
            Presence::Offline => IconName::StatusOffline,
        }
    }

    /// `STATUS_LABEL`.
    pub fn label(self) -> &'static str {
        match self {
            Presence::Online => "Online",
            Presence::Away => "Away",
            Presence::Dnd => "Do Not Disturb",
            Presence::Offline => "Offline",
        }
    }
}

/// The presence dot: shape and colour carry the status (online a disc, away a crescent, do not
/// disturb a disc with a bar, offline a ring), drawn as one SVG glyph so its cut-outs stay centred
/// at any position and scale (`StatusDot.tsx`'s comment). It sits on a disc of `ring`, the surface
/// behind it, `ring_width` wide (default 2.5; the cut-outs show that same colour). `size` is the
/// whole dot (`avatar-dot`: 13 px × the list density).
///
/// ```ignore
/// div().relative().child(avatar).child(div().absolute().bottom(px(-1.)).right(px(-1.))
///     .child(StatusDot::new(Presence::Online, theme.color_bg_dark).size(13.)))
/// ```
#[derive(IntoElement)]
pub struct StatusDot {
    presence: Presence,
    ring: Hsla,
    ring_width: f32,
    size: f32,
}

impl StatusDot {
    pub fn new(presence: Presence, ring: Hsla) -> Self {
        Self {
            presence,
            ring,
            ring_width: 2.5,
            size: 13.,
        }
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn ring_width(mut self, width: f32) -> Self {
        self.ring_width = width;
        self
    }
}

impl RenderOnce for StatusDot {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex_none()
            .size(px(self.size))
            .rounded_full()
            .bg(self.ring)
            .p(px(self.ring_width))
            .child(
                svg()
                    .path(icon_path(self.presence.glyph(), None))
                    .size_full()
                    .text_color(self.presence.color(theme)),
            )
    }
}

/// "● Online" (`StatusPill`): `inline-flex items-center gap-1.5 rounded-sm px-2 py-0.5
/// text-[11px] font-medium`, tinted by the status (`bg-success/15 text-success` …).
#[derive(IntoElement)]
pub struct StatusPill {
    presence: Presence,
}

impl StatusPill {
    pub fn new(presence: Presence) -> Self {
        Self { presence }
    }
}

impl RenderOnce for StatusPill {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = self.presence.color(theme);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1p5()
            .rounded(theme.radius_sm)
            .bg(color.opacity(0.15))
            .px_2()
            .py_0p5()
            .child(div().size_1p5().rounded_full().bg(color))
            .child(Label::new(self.presence.label()).size(11.).weight(500.).color(color))
    }
}

/// The Decibell mark (`components/BellMark.tsx`): level bars along a bell curve in the accent,
/// brighter toward the centre (`opacity 0.4 + 0.6 × level`), `rounded-full`, bottom-aligned.
/// `rise` plays the one-time entrance (`bellRise`, 0.75 s `cubic-bezier(0.2, 0.8, 0.2, 1)`: each
/// bar grows from 6 % of its height, from the centre outward, 55 ms apart); reduced motion skips it.
///
/// ```ignore
/// BellMark::new("login-mark").bars(11).height(28.).rise(true)
/// ```
#[derive(IntoElement)]
pub struct BellMark {
    id: ElementId,
    bars: usize,
    height: f32,
    bar_width: f32,
    gap: f32,
    rise: bool,
    color: Option<Hsla>,
}

impl BellMark {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            bars: 11,
            height: 28.,
            bar_width: 3.,
            gap: 2.,
            rise: false,
            color: None,
        }
    }

    pub fn bars(mut self, bars: usize) -> Self {
        self.bars = bars;
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub fn bar_width(mut self, width: f32) -> Self {
        self.bar_width = width;
        self
    }

    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    pub fn rise(mut self, rise: bool) -> Self {
        self.rise = rise;
        self
    }

    /// The bars' colour (default the accent).
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

/// Each bar's (height, opacity, entrance delay in ms) for a mark.
pub(crate) fn bell_bars(bars: usize, height: f32, bar_width: f32) -> Vec<(f32, f32, f32)> {
    let centre = (bars as f32 - 1.) / 2.;
    // σ scales with the bar count so every size keeps the icon's shape.
    let sigma = centre / 1.95;
    (0..bars)
        .map(|i| {
            let d = (i as f32 - centre).abs();
            let level = if sigma > 0. {
                (-(d * d) / (2. * sigma * sigma)).exp()
            } else {
                1.
            };
            (
                bar_width.max((height * level).round()),
                0.4 + 0.6 * level,
                (d * 55.).round(),
            )
        })
        .collect()
}

impl RenderOnce for BellMark {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or(cx.theme().color_accent);
        let bars = bell_bars(self.bars, self.height, self.bar_width);
        let longest_delay = bars.iter().map(|bar| bar.2).fold(0., f32::max);
        let rise = self.rise;
        let id = self.id;
        div()
            .flex()
            .items_end()
            .h(px(self.height))
            .gap(px(self.gap))
            .children(bars.into_iter().enumerate().map(move |(index, (height, opacity, delay))| {
                let bar = div()
                    .flex_none()
                    .w(px(self.bar_width))
                    .h(px(height))
                    .rounded_full()
                    .bg(color.opacity(color.a * opacity));
                if !rise {
                    return bar.into_any_element();
                }
                // `animation-delay` with `both`: held at the start until the bar's turn.
                let total = 750. + longest_delay;
                let start = delay / total;
                let span = 750. / total;
                bar.with_animation(
                    ElementId::NamedChild(Arc::new(id.clone()), SharedString::from(index.to_string())),
                    Animation::new(Duration::from_secs_f32(total / 1000.)).with_easing(move |t| {
                        let local = ((t - start) / span).clamp(0., 1.);
                        motion::POP.y(local as f64) as f32
                    }),
                    move |bar, t| bar.h(px(height * (0.06 + 0.94 * t))),
                )
                .into_any_element()
            }))
    }
}

/// Pixels of a bell mark's width, for layout.
pub fn bell_mark_width(bars: usize, bar_width: f32, gap: f32) -> Pixels {
    px(bars as f32 * bar_width + bars.saturating_sub(1) as f32 * gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected slots computed by the renderer's own `utils/colors.ts` under Node
    /// (`tests/gen_ramp_expected.mjs` regenerates them).
    #[test]
    fn ramp_matches_the_renderer() {
        for (input, slot) in include_str!("../tests/ramp_expected.txt")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                let (slot, input) = line.split_once('\t').unwrap();
                (input.replace("\\t", "\t"), slot.parse::<usize>().unwrap())
            })
        {
            assert_eq!(ramp_index(&input), slot, "{input:?}");
        }
    }

    #[test]
    fn person_keys() {
        assert_eq!(person_key(42, "ada"), "uid:42");
        assert_eq!(person_key(0, "ada"), "ada");
        assert_eq!(person_key(-3, "ada"), "ada");
    }

    #[test]
    fn bell_curve_matches_the_component() {
        let bars = bell_bars(11, 28., 3.);
        assert_eq!(bars[5], (28., 1., 0.));
        // The outermost bars land near 15 % of the peak, never under the bar width.
        assert!(bars[0].0 >= 3. && bars[0].0 <= 5.);
        assert_eq!(bars[0].2, 275.);
        assert_eq!(bars[0], bars[10]);
    }
}
