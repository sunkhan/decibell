//! The picture editor's geometry (`components/pictureEditor/geometry.ts`). Everything is in
//! frame pixels: the crop frame's top-left is the origin, `scale` is frame pixels per natural
//! pixel, and (x, y) is where the picture's top-left sits. The exported picture is the frame, so
//! these numbers are the crop, scaled by output / frame.

/// A width and a height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const fn new(w: f32, h: f32) -> Size {
        Size { w, h }
    }
}

/// Where the picture sits in the frame and how large.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub scale: f32,
    pub x: f32,
    pub y: f32,
}

/// How far the zoom goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub min: f32,
    pub max: f32,
}

/// A point in frame pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct At {
    pub x: f32,
    pub y: f32,
}

/// The smallest scale that leaves no gap in the frame.
pub fn cover_scale(nat: Size, frame: Size) -> f32 {
    (frame.w / nat.w).max(frame.h / nat.h)
}

/// The scale that shows the whole picture inside the frame.
pub fn contain_scale(nat: Size, frame: Size) -> f32 {
    (frame.w / nat.w).min(frame.h / nat.h)
}

/// How far in the slider and the wheel go: 8× past cover, or 1:1 with the export if that is
/// further (a small picture still zooms to its pixels).
pub fn max_scale(nat: Size, frame: Size, output: Size) -> f32 {
    (cover_scale(nat, frame) * 8.).max(frame.w / output.w)
}

pub fn min_scale(nat: Size, frame: Size, fit: bool) -> f32 {
    if fit { contain_scale(nat, frame).min(cover_scale(nat, frame)) } else { cover_scale(nat, frame) }
}

pub fn range(nat: Size, frame: Size, output: Size, fit: bool) -> Range {
    Range { min: min_scale(nat, frame, fit), max: max_scale(nat, frame, output) }
}

/// One axis' allowed positions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub lo: f32,
    pub hi: f32,
}

/// The range each axis may take: a picture wider than the frame keeps both edges outside it; a
/// narrower one stays inside it (a fitted logo can sit left of centre, never half out of the
/// frame).
pub fn bounds(nat: Size, frame: Size, scale: f32) -> (Span, Span) {
    let axis = |len: f32, frame_len: f32| {
        let d = len * scale;
        if d >= frame_len { Span { lo: frame_len - d, hi: 0. } } else { Span { lo: 0., hi: frame_len - d } }
    };
    (axis(nat.w, frame.w), axis(nat.h, frame.h))
}

pub fn clamp_view(v: View, nat: Size, frame: Size, range: Range) -> View {
    let scale = range.min.max(range.max.min(v.scale));
    let (bx, by) = bounds(nat, frame, scale);
    View { scale, x: bx.lo.max(bx.hi.min(v.x)), y: by.lo.max(by.hi.min(v.y)) }
}

/// A drag past an edge moves the picture less and less the further it goes (the overshoot
/// approaches a third of the frame), then springs back on release.
pub fn rubber(value: f32, lo: f32, hi: f32, frame_len: f32) -> f32 {
    let give = |over: f32| (over * frame_len * 0.33) / (over + frame_len * 0.33);
    if value < lo {
        lo - give(lo - value)
    } else if value > hi {
        hi + give(value - hi)
    } else {
        value
    }
}

pub fn rubber_view(v: View, nat: Size, frame: Size) -> View {
    let (bx, by) = bounds(nat, frame, v.scale);
    View { scale: v.scale, x: rubber(v.x, bx.lo, bx.hi, frame.w), y: rubber(v.y, by.lo, by.hi, frame.h) }
}

/// Zoom to `scale`, keeping the picture point under `at` (frame pixels) where it is.
pub fn zoom_at(v: View, scale: f32, at: At) -> View {
    let k = scale / v.scale;
    View { scale, x: at.x - (at.x - v.x) * k, y: at.y - (at.y - v.y) * k }
}

/// The starting crop: cover, centred.
pub fn initial_view(nat: Size, frame: Size) -> View {
    let scale = cover_scale(nat, frame);
    View { scale, x: (frame.w - nat.w * scale) / 2., y: (frame.h - nat.h * scale) / 2. }
}

/// The slider is logarithmic, so every step of it zooms by the same factor.
pub fn to_slider(scale: f32, range: Range) -> f32 {
    if range.max <= range.min {
        return 0.;
    }
    (scale / range.min).ln() / (range.max / range.min).ln()
}

pub fn from_slider(t: f32, range: Range) -> f32 {
    range.min * (range.max / range.min).powf(t.clamp(0., 1.))
}

/// The frame no longer filled edge to edge (a fill shows behind it).
pub fn leaves_gap(v: View, nat: Size, frame: Size) -> bool {
    let e = 0.5;
    v.x > e || v.y > e || v.x + nat.w * v.scale < frame.w - e || v.y + nat.h * v.scale < frame.h - e
}

pub fn same_view(a: View, b: View) -> bool {
    (a.scale - b.scale).abs() < a.scale * 1e-4 && (a.x - b.x).abs() < 0.25 && (a.y - b.y).abs() < 0.25
}

/// The frame inside a stage of `stage` px with `margin` (x, y) kept clear around it, at the
/// output's `aspect` (`PictureEditor.tsx`' measure): as large as fits, at least 40 px wide,
/// whole px.
pub fn frame_in(stage: Size, margin: (f32, f32), aspect: f32) -> Size {
    let w = 40f32.max((stage.w - margin.0 * 2.).min((stage.h - margin.1 * 2.) * aspect));
    // JavaScript's Math.round: halves go up.
    let round = |v: f32| (v + 0.5).floor();
    Size { w: round(w), h: round(w / aspect) }
}

/// Where a "zoom 1.25×" step or a slider step lands, eased: the renderer's glide (an ease-out
/// cubic in `t`, the scale interpolated geometrically).
pub fn ease(from: View, to: View, t: f32) -> View {
    let t = t.clamp(0., 1.);
    let e = 1. - (1. - t).powi(3);
    View {
        scale: from.scale * (to.scale / from.scale).powf(e),
        x: from.x + (to.x - from.x) * e,
        y: from.y + (to.y - from.y) * e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAT: Size = Size::new(4000., 3000.);
    const FRAME: Size = Size::new(256., 256.);
    const OUT: Size = Size::new(256., 256.);

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn cover_fills_contain_shows_all() {
        assert!(close(cover_scale(NAT, FRAME), 256. / 3000.));
        assert!(close(contain_scale(NAT, FRAME), 256. / 4000.));
        // Without fit, the smallest zoom fills the frame (profile pictures, owner decision);
        // with it, the whole picture fits (server pictures).
        assert!(close(min_scale(NAT, FRAME, false), cover_scale(NAT, FRAME)));
        assert!(close(min_scale(NAT, FRAME, true), contain_scale(NAT, FRAME)));
        // 8× past cover, or 1:1 with the export if further.
        assert!(close(max_scale(NAT, FRAME, Size::new(4096., 4096.)), cover_scale(NAT, FRAME) * 8.));
        assert!(close(max_scale(NAT, FRAME, OUT), 1.), "a 256 px export of a 256 px frame zooms to its pixels");
        let tiny = Size::new(20., 20.);
        assert!(close(max_scale(tiny, Size::new(256., 256.), Size::new(4096., 4096.)), (256. / 20.) * 8.));
        assert!(close(max_scale(Size::new(4000., 4000.), Size::new(256., 256.), Size::new(16., 16.)), 16.));
    }

    #[test]
    fn the_initial_view_is_centred_cover() {
        let v = initial_view(NAT, FRAME);
        assert!(close(v.scale, 256. / 3000.));
        assert!(close(v.y, 0.));
        assert!(close(v.x, (256. - 4000. * v.scale) / 2.));
        assert!(!leaves_gap(v, NAT, FRAME));
    }

    #[test]
    fn clamping_keeps_a_large_picture_over_the_frame_and_a_small_one_inside_it() {
        let range = range(NAT, FRAME, OUT, true);
        let cover = cover_scale(NAT, FRAME);
        // Dragged far right: the picture's left edge stops at the frame's.
        let v = clamp_view(View { scale: cover, x: 500., y: 0. }, NAT, FRAME, range);
        assert!(close(v.x, 0.));
        // Dragged far left: the right edge stops at the frame's.
        let v = clamp_view(View { scale: cover, x: -5000., y: 0. }, NAT, FRAME, range);
        assert!(close(v.x + NAT.w * cover, 256.));
        // Zoomed out past cover (fit): the picture stays inside the frame.
        let contain = contain_scale(NAT, FRAME);
        let v = clamp_view(View { scale: contain, x: -10., y: 300. }, NAT, FRAME, range);
        assert!(close(v.x, 0.));
        assert!(close(v.y + NAT.h * contain, 256.));
        assert!(leaves_gap(v, NAT, FRAME));
        // Scale clamps to the range.
        let v = clamp_view(View { scale: 1000., x: 0., y: 0. }, NAT, FRAME, range);
        assert!(close(v.scale, range.max));
        let v = clamp_view(View { scale: 0.0001, x: 0., y: 0. }, NAT, FRAME, range);
        assert!(close(v.scale, range.min));
        // Without fit the scale never drops below cover.
        let strict = super::range(NAT, FRAME, OUT, false);
        assert!(close(clamp_view(View { scale: contain, x: 0., y: 0. }, NAT, FRAME, strict).scale, cover));
    }

    #[test]
    fn the_rubber_band_gives_less_the_further_it_goes() {
        // Inside: untouched.
        assert_eq!(rubber(5., 0., 10., 100.), 5.);
        // Past the end: some give, never more than a third of the frame.
        let a = rubber(20., 0., 10., 100.);
        let b = rubber(110., 0., 10., 100.);
        let c = rubber(100_000., 0., 10., 100.);
        assert!(a > 10. && a < 20.);
        assert!(b > a && b - 10. < 33.);
        assert!(c < 10. + 33. + 1e-3);
        assert!(rubber(-20., 0., 10., 100.) < 0.);
        let v = rubber_view(View { scale: cover_scale(NAT, FRAME), x: 400., y: 0. }, NAT, FRAME);
        assert!(v.x > 0. && v.x < 256. * 0.33);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let v = View { scale: 0.1, x: -50., y: -20. };
        let at = At { x: 100., y: 80. };
        let z = zoom_at(v, 0.2, at);
        // The picture point under `at` before and after.
        let before = ((at.x - v.x) / v.scale, (at.y - v.y) / v.scale);
        let after = ((at.x - z.x) / z.scale, (at.y - z.y) / z.scale);
        assert!(close(before.0, after.0) && close(before.1, after.1));
    }

    #[test]
    fn the_slider_is_logarithmic_and_round_trips() {
        let r = Range { min: 0.05, max: 0.8 };
        assert!(close(to_slider(r.min, r), 0.));
        assert!(close(to_slider(r.max, r), 1.));
        // The middle of the slider is the geometric mean.
        assert!(close(from_slider(0.5, r), (0.05f32 * 0.8).sqrt()));
        for t in [0., 0.13, 0.5, 0.77, 1.] {
            assert!(close(to_slider(from_slider(t, r), r), t));
        }
        // Out of range clamps; an empty range is 0.
        assert!(close(from_slider(2., r), r.max));
        assert_eq!(to_slider(1., Range { min: 1., max: 1. }), 0.);
    }

    #[test]
    fn same_view_tolerates_sub_pixel_noise() {
        let a = View { scale: 1., x: 10., y: 10. };
        assert!(same_view(a, View { scale: 1.00001, x: 10.1, y: 9.9 }));
        assert!(!same_view(a, View { scale: 1., x: 10.5, y: 10. }));
    }

    #[test]
    fn frames_fit_the_stage_inside_its_margin() {
        // The avatar stage: size-88 with 48 px margins → 256 square.
        assert_eq!(frame_in(Size::new(352., 352.), (48., 48.), 1.), Size::new(256., 256.));
        // The banner stage: 640 × 272 with 40 / 44 px margins at the tile's 128:44.
        let aspect = 128. / 44.;
        let f = frame_in(Size::new(640., 272.), (40., 44.), aspect);
        assert_eq!(f, Size::new(535., 184.));
        // Never narrower than 40.
        assert_eq!(frame_in(Size::new(50., 50.), (48., 48.), 1.).w, 40.);
    }

    #[test]
    fn the_glide_ends_where_it_was_headed() {
        let from = View { scale: 0.1, x: 0., y: 0. };
        let to = View { scale: 0.4, x: -100., y: -50. };
        assert_eq!(ease(from, to, 0.), from);
        assert!(same_view(ease(from, to, 1.), to));
        let mid = ease(from, to, 0.5);
        assert!(mid.scale > from.scale && mid.scale < to.scale);
    }
}
