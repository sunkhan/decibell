//! The custom-colour end of a role's colour row (`tabs/ColorPicker.tsx`): a colour-wheel tile
//! that opens a picker (saturation / brightness field, hue strip, hex and R / G / B entry), and
//! shows the custom colour itself once one is set. Colours are 0xRRGGBB, where 0 means "no
//! colour" on the wire.

use std::cell::OnceCell;
use std::sync::Arc;

use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label};
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, EventEmitter, Focusable as _, Hsla, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Render, RenderImage, SharedString, Subscription, Window, canvas,
    div, linear_color_stop, linear_gradient, point, prelude::*, px,
};
use smallvec::SmallVec;

use crate::ui::InputBox;

/// Hue 0–360, saturation and value 0–1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

/// Pure black would read as "no colour", so it's stored one step up.
pub const NEAR_BLACK: u32 = 0x010101;
/// Where the picker starts when the role has no colour yet.
pub const START: Hsv = Hsv { h: 210., s: 0.62, v: 0.9 };
/// The hue thumb's radius: it travels inside the strip, not past its ends.
const THUMB: f32 = 8.;
/// The popover's width (`w-64`): its right edge lines up with the tile's.
pub const PICKER_W: f32 = 256.;

pub fn channels(c: u32) -> [u32; 3] {
    [(c >> 16) & 255, (c >> 8) & 255, c & 255]
}

/// "3AAFFF".
pub fn hex_of(c: u32) -> String {
    format!("{c:06X}")
}

/// JavaScript's `Math.round`.
fn js_round(v: f32) -> i64 {
    (v + 0.5).floor() as i64
}

pub fn hsv_to_rgb(Hsv { h, s, v }: Hsv) -> u32 {
    let f = |n: f32| {
        let k = (n + h / 60.) % 6.;
        js_round(255. * (v - v * s * 0f32.max(k.min(4. - k).min(1.)))).clamp(0, 255) as u32
    };
    (f(5.) << 16) | (f(3.) << 8) | f(1.)
}

/// Greys have no hue (and black no saturation): keep the previous ones so the field doesn't jump
/// while dragging through them.
pub fn rgb_to_hsv(c: u32, prev: Hsv) -> Hsv {
    let [r, g, b] = channels(c).map(|x| x as f32 / 255.);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let mut h = prev.h;
    if d != 0. {
        h = if max == r {
            ((g - b) / d) % 6.
        } else if max == g {
            (b - r) / d + 2.
        } else {
            (r - g) / d + 4.
        };
        h = (h * 60. + 360.) % 360.;
    }
    Hsv { h, s: if max != 0. { d / max } else { prev.s }, v: max }
}

/// "#3af", "3AF", "#33aaff" → 0x33aaff; anything else → none.
pub fn parse_hex(text: &str) -> Option<u32> {
    let t = text.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    if !(t.len() == 3 || t.len() == 6) || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let full: String = if t.len() == 3 { t.chars().flat_map(|c| [c, c]).collect() } else { t.to_string() };
    u32::from_str_radix(&full, 16).ok()
}

/// Readable mark on a filled swatch: dark on light colours, white on the rest.
pub fn on_swatch(c: u32) -> Hsla {
    let [r, g, b] = channels(c).map(|x| x as f32);
    // Renderer literals: `rgb(0 0 0 / 0.75)` and `#fff`.
    if 0.2126 * r + 0.7152 * g + 0.0722 * b > 160. { gpui::black().opacity(0.75) } else { gpui::white() }
}

pub fn rgb(c: u32) -> Hsla {
    gpui::rgb(c).into()
}

/// `hsl(h 100% 50%)`.
fn hue(h: f32) -> Hsla {
    gpui::hsla((h % 360.) / 360., 1., 0.5, 1.)
}

thread_local! {
    /// The tile's colour wheel (`conic-gradient(from 90deg, #ff5b5b, #ffd34d, #57e389, #4dd8ff,
    /// #6a7dff, #e05bff, #ff5b5b)`): GPUI has no conic gradient, so it is a 64 px picture made
    /// once.
    static WHEEL: OnceCell<Arc<RenderImage>> = const { OnceCell::new() };
}

pub fn wheel() -> Arc<RenderImage> {
    WHEEL.with(|w| {
        w.get_or_init(|| {
            let stops: [u32; 7] = [0xff5b5b, 0xffd34d, 0x57e389, 0x4dd8ff, 0x6a7dff, 0xe05bff, 0xff5b5b];
            let n = 64u32;
            let mut img = image::RgbaImage::new(n, n);
            for (x, y, p) in img.enumerate_pixels_mut() {
                let (dx, dy) = (x as f32 + 0.5 - n as f32 / 2., y as f32 + 0.5 - n as f32 / 2.);
                // CSS conic angles run clockwise from the top; `from 90deg` starts at the right.
                let mut a = dx.atan2(-dy).to_degrees() - 90.;
                if a < 0. {
                    a += 360.;
                }
                let t = a / 360. * 6.;
                let i = (t.floor() as usize).min(5);
                let f = t - i as f32;
                let [r0, g0, b0] = channels(stops[i]);
                let [r1, g1, b1] = channels(stops[i + 1]);
                let mix = |a: u32, b: u32| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
                // BGRA for GPUI.
                *p = image::Rgba([mix(b0, b1), mix(g0, g1), mix(r0, r1), 255]);
            }
            Arc::new(RenderImage::new(SmallVec::from_elem(image::Frame::new(img), 1)))
        })
        .clone()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPickerEvent {
    Change(u32),
    /// Enter in a field: close.
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    Field,
    Hue,
}

pub struct ColorPicker {
    hsv: Hsv,
    hex: Entity<InputState>,
    rgb: [Entity<InputState>; 3],
    /// A field being typed into shows the typed text until it blurs; the others follow the
    /// colour.
    syncing: bool,
    drag: Option<Drag>,
    focus_field: gpui::FocusHandle,
    focus_hue: gpui::FocusHandle,
    field_bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    hue_bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ColorPickerEvent> for ColorPicker {}

impl ColorPicker {
    pub fn new(initial: u32, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let hsv = if initial != 0 { rgb_to_hsv(initial, START) } else { START };
        let hex = cx.new(|cx| InputState::single_line(window, cx).max_length(7));
        let rgb = [0, 1, 2].map(|_| cx.new(|cx| InputState::single_line(window, cx).max_length(3).accept(|c| c.is_ascii_digit())));
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&hex, |this: &mut Self, hex, e: &InputEvent, cx| match e {
            InputEvent::Change if !this.syncing => {
                // Six digits apply as they're typed; three only on Enter or blur, or typing
                // "33aaff" would flash #3333AA on the way.
                let text = hex.read(cx).value().to_string();
                if let Some(c) = parse_hex(&text)
                    && text.replace('#', "").len() == 6
                {
                    this.apply_rgb(c, Some(0), cx);
                }
            }
            InputEvent::Blur => {
                let text = hex.read(cx).value().to_string();
                if let Some(c) = parse_hex(&text)
                    && c != hsv_to_rgb(this.hsv)
                {
                    this.apply_rgb(c, None, cx);
                }
                this.sync_fields(None, cx);
            }
            InputEvent::Submit => {
                let text = hex.read(cx).value().to_string();
                if let Some(c) = parse_hex(&text)
                    && c != hsv_to_rgb(this.hsv)
                {
                    this.apply_rgb(c, None, cx);
                }
                cx.emit(ColorPickerEvent::Done);
            }
            _ => {}
        }));
        for (i, field) in rgb.iter().enumerate() {
            subs.push(cx.subscribe(field, move |this: &mut Self, field, e: &InputEvent, cx| match e {
                InputEvent::Change if !this.syncing => {
                    let text = field.read(cx).value().to_string();
                    if let Ok(v) = text.parse::<u32>() {
                        this.set_channel(i, v as i64, Some(i + 1), cx);
                    }
                }
                InputEvent::Blur => this.sync_fields(None, cx),
                InputEvent::Submit => cx.emit(ColorPickerEvent::Done),
                _ => {}
            }));
        }
        let mut this = ColorPicker {
            hsv,
            hex,
            rgb,
            syncing: false,
            drag: None,
            focus_field: cx.focus_handle().tab_stop(true),
            focus_hue: cx.focus_handle().tab_stop(true),
            field_bounds: Default::default(),
            hue_bounds: Default::default(),
            _subs: subs,
        };
        this.sync_fields(None, cx);
        // Open on the hex code, selected, so a typed or pasted one replaces it.
        this.hex.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(cx);
        });
        this
    }

    pub fn hsv(&self) -> Hsv {
        self.hsv
    }

    /// Show the colour in every field but `keep` (0 = hex, 1–3 = R, G, B: the one being typed).
    fn sync_fields(&mut self, keep: Option<usize>, cx: &mut Context<Self>) {
        let c = hsv_to_rgb(self.hsv);
        self.syncing = true;
        if keep != Some(0) {
            let hex = hex_of(c);
            self.hex.update(cx, |s, cx| {
                if s.value() != hex {
                    s.set_value(&hex, cx)
                }
            });
        }
        for (i, v) in channels(c).iter().enumerate() {
            if keep != Some(i + 1) {
                let text = v.to_string();
                self.rgb[i].update(cx, |s, cx| {
                    if s.value() != text {
                        s.set_value(&text, cx)
                    }
                });
            }
        }
        self.syncing = false;
    }

    fn apply(&mut self, next: Hsv, exact: Option<u32>, keep: Option<usize>, cx: &mut Context<Self>) {
        self.hsv = next;
        let c = exact.unwrap_or_else(|| hsv_to_rgb(next));
        cx.emit(ColorPickerEvent::Change(if c == 0 { NEAR_BLACK } else { c }));
        self.sync_fields(keep, cx);
        cx.notify();
    }

    fn apply_rgb(&mut self, c: u32, keep: Option<usize>, cx: &mut Context<Self>) {
        let next = rgb_to_hsv(c, self.hsv);
        self.apply(next, Some(c), keep, cx);
    }

    fn set_channel(&mut self, i: usize, v: i64, keep: Option<usize>, cx: &mut Context<Self>) {
        let mut values = channels(hsv_to_rgb(self.hsv));
        values[i] = v.clamp(0, 255) as u32;
        self.apply_rgb((values[0] << 16) | (values[1] << 8) | values[2], keep, cx);
    }

    fn drag_to(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        match self.drag {
            Some(Drag::Field) => {
                let b = self.field_bounds.get();
                let x = (f32::from(position.x - b.origin.x) / f32::from(b.size.width)).clamp(0., 1.);
                let y = (f32::from(position.y - b.origin.y) / f32::from(b.size.height)).clamp(0., 1.);
                self.apply(Hsv { s: x, v: 1. - y, ..self.hsv }, None, None, cx);
            }
            Some(Drag::Hue) => {
                let b = self.hue_bounds.get();
                let x = ((f32::from(position.x - b.origin.x) - THUMB) / (f32::from(b.size.width) - 2. * THUMB)).clamp(0., 1.);
                self.apply(Hsv { h: x * 360., ..self.hsv }, None, None, cx);
            }
            None => {}
        }
    }

    /// Arrow keys step 1 unit, 10 with Shift.
    fn step(event: &KeyDownEvent, keys: &[(&str, f32)]) -> Option<f32> {
        let dir = keys.iter().find(|(k, _)| *k == event.keystroke.key)?.1;
        Some(dir * if event.keystroke.modifiers.shift { 10. } else { 1. })
    }
}

fn labelled(label: &'static str, flex: f32, child: impl IntoElement, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    let mut d = div();
    d.style().flex_grow = Some(flex);
    d.style().flex_shrink = Some(1.);
    d.flex_basis(px(0.))
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .child(div().w_full().child(child))
        .child(Label::new(label).font(FontRole::Meta).size(10.5).weight(600.).tracking(0.025).color(theme.color_text_muted))
}

impl Render for ColorPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let hsv = self.hsv;
        let current = hsv_to_rgb(hsv);
        let field_bounds = self.field_bounds.clone();
        let hue_bounds = self.hue_bounds.clone();
        let focus_field = self.focus_field.clone();
        let focus_hue = self.focus_hue.clone();
        let ring = theme.shadow_ring.clone();
        let ring2 = theme.shadow_ring.clone();
        let white_clear = gpui::white().opacity(0.);
        let black_clear = gpui::black().opacity(0.);
        let thumb = |bg: Hsla| {
            div()
                .absolute()
                .size_4()
                .rounded_full()
                .border_2()
                .border_color(gpui::white())
                .bg(bg)
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::black().opacity(0.4),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(1.),
                    inset: false,
                }])
        };
        let field = div()
            .id("color-field")
            .track_focus(&focus_field)
            .relative()
            .h(px(144.))
            .rounded(theme.radius_md)
            .bg(hue(hsv.h))
            .cursor_crosshair()
            .focus_visible(move |s| s.shadow(ring.clone()))
            .child(canvas(move |b, _, _| field_bounds.set(b), |_, _, _, _| {}).absolute().inset_0())
            .child(div().absolute().inset_0().rounded(theme.radius_md).bg(linear_gradient(90., linear_color_stop(gpui::white(), 0.), linear_color_stop(white_clear, 1.))))
            .child(div().absolute().inset_0().rounded(theme.radius_md).bg(linear_gradient(0., linear_color_stop(gpui::black(), 0.), linear_color_stop(black_clear, 1.))))
            .child(div().absolute().inset_0().rounded(theme.radius_md).border_1().border_color(theme.color_border))
            .child(thumb(rgb(current)).left(gpui::relative(hsv.s)).top(gpui::relative(1. - hsv.v)).ml(px(-8.)).mt(px(-8.)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.drag = Some(Drag::Field);
                    this.drag_to(e.position, cx);
                }),
            )
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                let ds = Self::step(e, &[("left", -1.), ("right", 1.)]);
                let dv = Self::step(e, &[("down", -1.), ("up", 1.)]);
                if let Some(ds) = ds {
                    this.apply(Hsv { s: (this.hsv.s + ds / 100.).clamp(0., 1.), ..this.hsv }, None, None, cx);
                    cx.stop_propagation();
                }
                if let Some(dv) = dv {
                    this.apply(Hsv { v: (this.hsv.v + dv / 100.).clamp(0., 1.), ..this.hsv }, None, None, cx);
                    cx.stop_propagation();
                }
            }));
        // GPUI's gradients have two stops: the hue track is six segments.
        let hue_stops = [0xff0000u32, 0xffff00, 0x00ff00, 0x00ffff, 0x0000ff, 0xff00ff, 0xff0000];
        let mut track = div().absolute().inset_0().flex().rounded_full().overflow_hidden();
        for i in 0..6 {
            track = track.child(
                div()
                    .flex_1()
                    .h_full()
                    .when(i == 0, |d| d.rounded_l_full())
                    .when(i == 5, |d| d.rounded_r_full())
                    .bg(linear_gradient(90., linear_color_stop(rgb(hue_stops[i]), 0.), linear_color_stop(rgb(hue_stops[i + 1]), 1.))),
            );
        }
        let hue_strip = div()
            .id("color-hue")
            .track_focus(&focus_hue)
            .relative()
            .h_3()
            .rounded_full()
            .cursor_pointer()
            .focus_visible(move |s| s.shadow(ring2.clone()))
            .child(canvas(move |b, _, _| hue_bounds.set(b), |_, _, _, _| {}).absolute().inset_0())
            .child(track)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(THUMB))
                    .right(px(THUMB))
                    .child(thumb(hue(hsv.h)).top(px(-2.)).left(gpui::relative(hsv.h / 360.)).ml(px(-8.))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.drag = Some(Drag::Hue);
                    this.drag_to(e.position, cx);
                }),
            )
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if let Some(d) = Self::step(e, &[("left", -1.), ("down", -1.), ("right", 1.), ("up", 1.)]) {
                    this.apply(Hsv { h: (this.hsv.h + d).clamp(0., 360.), ..this.hsv }, None, None, cx);
                    cx.stop_propagation();
                }
            }));
        let muted = theme.color_text_muted;
        let hex_box = div()
            .flex()
            .h_8()
            .items_center()
            .rounded(theme.radius_sm)
            .border_1()
            .border_color(if self.hex.read(cx).focus_handle(cx).is_focused(window) { theme.color_accent } else { theme.color_border })
            .bg(theme.color_bg_lighter)
            .child(div().pl_2().child(Label::new("#").font(FontRole::Mono).size(12.).color(muted)))
            .child(
                div().min_w_0().flex_1().child(
                    InputBox::new("color-hex", &self.hex).font(FontRole::Mono).text(12., 400).pad(2., 0.).bg(gpui::transparent_black()).no_ring().focus_border(gpui::transparent_black()).color(theme.color_text_primary),
                ),
            );
        let mut fields = div().flex().gap_1p5().child(labelled("HEX", 1.75, hex_box, cx));
        for (i, name) in ["R", "G", "B"].into_iter().enumerate() {
            let field = &self.rgb[i];
            fields = fields.child(labelled(
                name,
                1.,
                div()
                    .id(SharedString::from(format!("color-{name}-box")))
                    .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                        if let Some(d) = Self::step(e, &[("down", -1.), ("up", 1.)]) {
                            let v = channels(hsv_to_rgb(this.hsv))[i] as i64 + d as i64;
                            this.set_channel(i, v, None, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(InputBox::new(SharedString::from(format!("color-{name}")), field).font(FontRole::Mono).text(12., 400).pad(4., 0.).height(32.).small_radius()),
                cx,
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(field)
            .child(hue_strip)
            .child(fields)
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.drag.is_some() {
                    if e.pressed_button == Some(MouseButton::Left) {
                        this.drag_to(e.position, cx);
                    } else {
                        this.drag = None;
                    }
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.drag = None))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| this.drag = None))
    }
}

/// The custom tile: the colour itself (with a tick) once a custom one is set, else the wheel with
/// a + on it.
pub fn custom_tile(active: bool, open: bool, color: u32, disabled: bool, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    let ring_bg = theme.color_bg_dark;
    let ring_fg = theme.color_text_primary;
    let tile = div().relative().flex().size_8().items_center().justify_center().rounded(theme.radius_md);
    let tile = if active {
        tile.bg(rgb(color)).child(Icon::new(IconName::SettingsCheck).size(px(15.)).stroke_width(3.).color(on_swatch(color)))
    } else {
        let w = wheel();
        let radius = theme.radius_md;
        tile.child(
            canvas(|_, _, _| {}, move |b, _, window, _| {
                let _ = window.paint_image(b, b, gpui::Corners::all(radius), w.clone(), 0, false);
            })
            .absolute()
            .inset_0(),
        )
        .child(
            div()
                .relative()
                .flex()
                .size_5()
                .items_center()
                .justify_center()
                .rounded(theme.radius_sm)
                .bg(theme.color_bg_dark)
                .child(Icon::new(IconName::SettingsPlus).size(px(12.)).stroke_width(2.75).color(theme.color_text_primary)),
        )
    };
    // `ring-2 ring-text-primary ring-offset-2 ring-offset-bg-dark` while active; scaled up while open
    // (GPUI can't scale a box: left at its size).
    let _ = open;
    div()
        .flex_none()
        .rounded(theme.radius_md + px(4.))
        .p(px(if active { 2. } else { 0. }))
        .m(px(if active { -4. } else { 0. }))
        .when(active, |this| this.border_2().border_color(ring_fg).bg(ring_bg))
        .when(disabled, |this| this.opacity(0.6))
        .child(tile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips_through_rgb() {
        assert_eq!(hsv_to_rgb(Hsv { h: 0., s: 1., v: 1. }), 0xff0000);
        assert_eq!(hsv_to_rgb(Hsv { h: 120., s: 1., v: 1. }), 0x00ff00);
        assert_eq!(hsv_to_rgb(Hsv { h: 240., s: 1., v: 1. }), 0x0000ff);
        assert_eq!(hsv_to_rgb(Hsv { h: 0., s: 0., v: 1. }), 0xffffff);
        for c in [0x3498dbu32, 0xe74c3c, 0x2ecc71, 0x95a5a6, 0x010101] {
            assert_eq!(hsv_to_rgb(rgb_to_hsv(c, START)), c, "{c:06x}");
        }
        // A grey keeps the previous hue.
        assert_eq!(rgb_to_hsv(0x808080, Hsv { h: 123., s: 0.5, v: 0.5 }).h, 123.);
    }

    #[test]
    fn hex_codes_parse_as_the_renderer() {
        assert_eq!(parse_hex("#3af"), Some(0x33aaff));
        assert_eq!(parse_hex("3AF"), Some(0x33aaff));
        assert_eq!(parse_hex("#33aaff"), Some(0x33aaff));
        assert_eq!(parse_hex(" 33AAFF "), Some(0x33aaff));
        assert_eq!(parse_hex("#33aaf"), None);
        assert_eq!(parse_hex("zzz"), None);
        assert_eq!(hex_of(0x33aaff), "33AAFF");
    }

    #[test]
    fn swatch_marks_are_readable() {
        assert_eq!(on_swatch(0xf1c40f), gpui::black().opacity(0.75));
        assert_eq!(on_swatch(0x3498db), gpui::white());
    }
}
