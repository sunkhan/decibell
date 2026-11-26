//! The active server tile's breathing glow (`ActiveTileGlow`): a static two-layer glow — a
//! 1.5 px ring of the accent at 60 % and a 22 px / 4 px bloom at 22 % — whose opacity breathes
//! between 1 and 0.4 over 2.4 s (`breathe`, ease-in-out).
//!
//! In Electron it breathes forever and costs ~15 % of a core on Linux (the compositor redraws the
//! window at the display's rate). Here it is one SVG sprite drawn once; while the window is
//! focused a timed alpha transformation re-presents the retained scene at most
//! [`BREATHE_FPS`] times a second, without rebuilding anything (`Svg::with_timed_transformation`,
//! gpui-notes §3.3). Unfocused (and under reduced motion, and in captures) it rests at full
//! opacity and the window draws nothing.

use std::time::Duration;

use gpui::{
    AnyElement, Easing, Hsla, IntoElement, Pixels, TimedTransformation, Transformation, div, prelude::*, px,
    svg,
};

/// How often the breathing glow may present while it is the only thing moving. The opacity moves
/// 0.6 in 1.2 s: at 30 fps a step is at most ~0.03 of opacity, below what the eye resolves on a
/// soft glow.
pub const BREATHE_FPS: f32 = 30.;
/// One full breath (1 → 0.4 → 1).
pub const BREATHE_PERIOD: Duration = Duration::from_millis(2400);

/// The ring's spread (`0 0 0 1.5px`).
const RING: f32 = 1.5;
/// The bloom's spread and CSS blur radius (`0 0 22px 4px`); GPUI's / SVG's σ is half the blur.
const BLOOM_SPREAD: f32 = 4.;
const BLOOM_BLUR: f32 = 22.;
/// How far the glow reaches past the tile: the spread plus three σ of the blur, whole px.
pub const GLOW_MARGIN: f32 = 38.;

/// The glow's SVG at a tile of `w` × `h` with corner radius `radius` (the theme's `rounded-md`):
/// alpha only, tinted with the accent when drawn.
pub fn glow_svg(w: f32, h: f32, radius: f32) -> Vec<u8> {
    let m = GLOW_MARGIN;
    let (vw, vh) = (w + 2. * m, h + 2. * m);
    let rect = |spread: f32, opacity: f32, filter: &str| {
        format!(
            r##"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="#fff" fill-opacity="{opacity}"{filter}/>"##,
            x = m - spread,
            y = m - spread,
            w = w + 2. * spread,
            h = h + 2. * spread,
            r = radius + spread,
        )
    };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{vw}" height="{vh}" viewBox="0 0 {vw} {vh}"><defs><filter id="b" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="{sigma}"/></filter></defs>{bloom}{ring}</svg>"##,
        sigma = BLOOM_BLUR / 2.,
        bloom = rect(BLOOM_SPREAD, 0.22, r##" filter="url(#b)""##),
        ring = rect(RING, 0.6, ""),
    )
    .into_bytes()
}

/// The glow behind a tile of `w` × `h` (its parent must be `relative()`), breathing while
/// `breathing`. Painted before the tile, so over the title bar above it but under the workspace
/// below (the renderer's bar is `z-10`, over both: its bloom's lower edge shows there).
pub fn active_tile_glow(data: &[u8], w: f32, h: f32, accent: Hsla, breathing: bool) -> AnyElement {
    let m = px(GLOW_MARGIN);
    let size = |v: f32| -> Pixels { px(v) + m * 2. };
    let mut sprite = svg().data(data).size_full().text_color(accent);
    if breathing {
        sprite = sprite.with_timed_transformation(
            TimedTransformation::new(BREATHE_PERIOD)
                .from(Transformation::alpha(1.))
                .to(Transformation::alpha(0.4))
                .with_easing(Easing::EaseInOut)
                .alternate()
                .with_max_fps(BREATHE_FPS),
        );
    }
    div()
        .absolute()
        .top(-m)
        .left(-m)
        .w(size(w))
        .h(size(h))
        .child(sprite)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glow_svg_is_the_tile_plus_its_margin() {
        let svg = String::from_utf8(glow_svg(128., 44., 6.)).unwrap();
        assert!(svg.contains(r#"width="204" height="120""#), "{svg}");
        assert!(svg.contains(r#"stdDeviation="11""#));
        // The ring: 1.5 px out, radius grown by the spread.
        assert!(svg.contains(r##"x="36.5" y="36.5" width="131" height="47" rx="7.5" fill="#fff" fill-opacity="0.6""##));
    }
}
