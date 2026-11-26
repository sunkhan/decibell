//! The viewer's geometry (`imageViewer/geometry.ts`), ported function for function. Everything is
//! in stage pixels; `scale` is rendered pixels per natural pixel, so 1 is the picture's actual
//! size and the zoom label reads true.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub left: f32,
    pub top: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub scale: f32,
    /// Offset of the picture's centre from the fit area's centre.
    pub x: f32,
    pub y: f32,
    /// Clockwise quarter turns in degrees (unbounded so a turn always goes the short way).
    pub rot: i32,
}

/// Room kept free for the header and the filmstrip at fit.
pub fn fit_area(stage: Size, strip: bool) -> Area {
    let side = if stage.w < 720. { 16. } else { 80. };
    let top = 72.;
    let bottom = if strip { 104. } else { 40. };
    Area { left: side, top, w: (stage.w - side * 2.).max(1.), h: (stage.h - top - bottom).max(1.) }
}

pub fn turned(rot: i32) -> bool {
    rot.rem_euclid(180) == 90
}

/// Natural size as displayed (a quarter turn swaps the sides).
pub fn shown(nat: Size, rot: i32) -> Size {
    if turned(rot) { Size { w: nat.h, h: nat.w } } else { nat }
}

/// The whole picture inside the area, never above actual size.
pub fn fit_scale(nat: Size, rot: i32, area: Area) -> f32 {
    let s = shown(nat, rot);
    1f32.min(area.w / s.w).min(area.h / s.h)
}

pub const MAX_SCALE: f32 = 8.;

pub fn clamp_scale(scale: f32, nat: Size, rot: i32, area: Area) -> f32 {
    fit_scale(nat, rot, area).max(MAX_SCALE.min(scale))
}

const LEVELS: [f32; 12] = [0.1, 0.25, 0.33, 0.5, 0.67, 1., 1.5, 2., 3., 4., 6., 8.];

pub fn step_scale(current: f32, fit: f32, dir: i32) -> f32 {
    if dir > 0 {
        return LEVELS.iter().copied().find(|l| *l > current * 1.01 && *l > fit * 1.01).unwrap_or(MAX_SCALE);
    }
    match LEVELS.iter().rev().copied().find(|l| *l < current * 0.99) {
        Some(prev) if prev >= fit => prev,
        _ => fit,
    }
}

/// The picture covers the stage on every side it overflows and sits centred in the fit area on
/// every side it doesn't.
pub fn clamp_view(v: View, nat: Size, stage: Size, area: Area) -> View {
    let s = shown(nat, v.rot);
    let axis = |pan: f32, d: f32, centre: f32, stage_len: f32, area_len: f32| -> f32 {
        if d <= area_len + 0.5 {
            return 0.;
        }
        let a = stage_len - centre - d / 2.;
        let b = d / 2. - centre;
        a.min(b).max(a.max(b).min(pan))
    };
    let x = axis(v.x, s.w * v.scale, area.left + area.w / 2., stage.w, area.w);
    let y = axis(v.y, s.h * v.scale, area.top + area.h / 2., stage.h, area.h);
    View { x, y, ..v }
}

/// Zoom to `scale` keeping the stage point `p` (relative to the fit area's centre) over the same
/// spot of the picture.
pub fn zoom_at(v: View, scale: f32, p: (f32, f32)) -> View {
    let k = scale / v.scale;
    View { scale, x: p.0 - (p.0 - v.x) * k, y: p.1 - (p.1 - v.y) * k, rot: v.rot }
}

pub fn overflows(v: View, nat: Size, stage: Size) -> bool {
    let s = shown(nat, v.rot);
    s.w * v.scale > stage.w + 1. || s.h * v.scale > stage.h + 1.
}

/// `cubic-bezier(0.2, 0.8, 0.2, 1)`: the viewer's motion.
pub fn ease(t: f32) -> f32 {
    decibell_ui::motion::CubicBezier::new(0.2, 0.8, 0.2, 1.).y(t.clamp(0., 1.) as f64) as f32
}

/// A view part of the way from `a` to `b`.
pub fn mix(a: View, b: View, t: f32) -> View {
    View {
        scale: a.scale + (b.scale - a.scale) * t,
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
        rot: if t >= 1. { b.rot } else { a.rot },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_and_steps_follow_the_renderer() {
        let stage = Size { w: 1280., h: 768. };
        let area = fit_area(stage, false);
        assert_eq!(area, Area { left: 80., top: 72., w: 1120., h: 656. });
        let nat = Size { w: 4000., h: 3000. };
        let fit = fit_scale(nat, 0, area);
        assert!((fit - 656. / 3000.).abs() < 1e-6);
        // A quarter turn fits the other way round.
        assert!((fit_scale(nat, 90, area) - 656. / 4000.).abs() < 1e-6);
        assert_eq!(step_scale(fit, fit, 1), 0.25);
        assert_eq!(step_scale(0.25, fit, -1), fit);
        assert_eq!(step_scale(1., fit, 1), 1.5);
        assert_eq!(step_scale(8., fit, 1), MAX_SCALE);
        // A small picture never grows past its pixels at fit.
        assert_eq!(fit_scale(Size { w: 200., h: 100. }, 0, area), 1.);
        assert_eq!(clamp_scale(20., nat, 0, area), MAX_SCALE);
    }

    #[test]
    fn panning_stops_at_the_edges() {
        let stage = Size { w: 1000., h: 800. };
        let area = fit_area(stage, false);
        let nat = Size { w: 2000., h: 1000. };
        // At 1: 2000 px wide on a 1000 px stage, the centre may move ±500.
        let v = clamp_view(View { scale: 1., x: 5000., y: 300., rot: 0 }, nat, stage, area);
        assert_eq!(v.x, 500.);
        // 1000 tall fits 800? No: it overflows vertically too, clamped by the stage around the
        // area's centre (72 + 344 = 416): y within [800 − 416 − 500, 500 − 416] = [−116, 84].
        assert_eq!(v.y, 84.);
        let z = zoom_at(View { scale: 1., x: 0., y: 0., rot: 0 }, 2., (100., 0.));
        assert_eq!((z.x, z.scale), (-100., 2.));
        assert!(overflows(View { scale: 1., x: 0., y: 0., rot: 0 }, nat, stage));
    }
}
