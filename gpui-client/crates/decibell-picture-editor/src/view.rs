//! The editor itself (`PictureEditor.tsx`): a modal with the stage, the toolbar, the previews and
//! the footer.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use decibell_ui::{ActiveTheme as _, Button, ButtonVariant, CloseRequest, FontRole, Icon, IconName, Label, Modal, hsla};
use gpui::{
    Animation, AnimationExt as _, App, Bounds, ClickEvent, Context, Corners, Edges, EventEmitter, ExternalPaths, Hsla,
    FocusHandle, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels,
    Render, ScrollDelta, ScrollEvent, SharedString, Task, Window, canvas, div, fill, point, prelude::*, px, quad,
    size, BorderStyle,
};

use crate::geometry::{
    At, Range, Size, View, clamp_view, cover_scale, ease, frame_in, from_slider, initial_view, leaves_gap, range,
    rubber_view, same_view, to_slider, zoom_at,
};
use crate::picture::{self, Fill, Picture, PictureError};
use crate::previews::{CropState, avatar_previews, rgb, server_previews};
use crate::{EditorOptions, PictureShape, Previews};

/// Where the picture comes from.
pub enum PictureSource {
    /// A file picked in the dialog or dropped.
    Path(PathBuf),
    /// Bytes already read, with the file's name (for its error messages).
    Bytes { bytes: Vec<u8>, name: String },
}

/// What the editor tells its owner.
#[derive(Debug, Clone)]
pub enum PictureEditorEvent {
    /// The crop as a JPEG of the output size, ≤ 1 MB: upload it, then call
    /// [`PictureEditor::save_done`]. The editor reads "Saving…" until then.
    Save(Arc<Vec<u8>>),
    /// It closed (Cancel, Esc or a click outside with nothing worked on, or a save that
    /// succeeded, whose JPEG is `saved`): drop the entity, which frees everything it held.
    Closed { saved: Option<Arc<Vec<u8>>> },
}

enum Phase {
    Loading,
    Ready(Picture),
    Failed(SharedString),
}

struct Glide {
    from: View,
    to: View,
    start: Instant,
    ms: f32,
}

struct Drag {
    from: View,
    start: gpui::Point<Pixels>,
}

/// The four colours of the `.picture-editor` scope, for the active theme.
#[derive(Clone, Copy)]
struct Ink {
    veil: Hsla,
    corner: Hsla,
    edge: Hsla,
    grid: Hsla,
}

pub struct PictureEditor {
    options: EditorOptions,
    phase: Phase,
    /// Bumped by every file read, so a slow decode of a replaced file is dropped.
    generation: u64,
    view: View,
    glide: Option<Glide>,
    /// The crop this picture was last placed with (a new picture settles in from closer).
    placed: bool,
    frame: Option<Size>,
    fill: Fill,
    adjusted: bool,
    interacting: bool,
    drag: Option<Drag>,
    slider_drag: bool,
    open: bool,
    saving: bool,
    save_error: Option<SharedString>,
    nudge: u32,
    pending: Option<Arc<Vec<u8>>>,
    saved: Option<Arc<Vec<u8>>>,
    stage_bounds: Rc<Cell<Bounds<Pixels>>>,
    slider_bounds: Rc<Cell<Bounds<Pixels>>>,
    ink: Option<(decibell_theme::ThemeId, Ink)>,
    /// The card's: the keys (Enter, arrows, + / −, 0) go to it, inside the modal's focus trap.
    focus: FocusHandle,
    /// The thirds grid hides 700 ms after the last touch: exists while it shows.
    _grid: Option<Task<()>>,
    _work: Option<Task<()>>,
    _closing: Option<Task<()>>,
}

impl EventEmitter<PictureEditorEvent> for PictureEditor {}

const STAGE_MARGIN_AVATAR: (f32, f32) = (48., 48.);
const STAGE_MARGIN_BANNER: (f32, f32) = (40., 44.);

impl PictureEditor {
    pub fn new(source: PictureSource, options: EditorOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        // Inside the modal, so its trap keeps it (it takes the focus only when none is inside).
        focus.focus(window, cx);
        let mut this = PictureEditor {
            options,
            phase: Phase::Loading,
            generation: 0,
            view: View { scale: 1., x: 0., y: 0. },
            glide: None,
            placed: false,
            frame: None,
            fill: Fill::Blur,
            adjusted: false,
            interacting: false,
            drag: None,
            slider_drag: false,
            open: true,
            saving: false,
            save_error: None,
            nudge: 0,
            pending: None,
            saved: None,
            stage_bounds: Rc::default(),
            slider_bounds: Rc::default(),
            ink: None,
            focus,
            _grid: None,
            _work: None,
            _closing: None,
        };
        this.load(source, cx);
        this
    }

    /// The output this editor renders.
    pub fn output(&self) -> (u32, u32) {
        self.options.output
    }

    /// The picture, once read.
    pub fn picture(&self) -> Option<&Picture> {
        match &self.phase {
            Phase::Ready(p) => Some(p),
            _ => None,
        }
    }

    /// The reason the file couldn't be opened.
    pub fn error(&self) -> Option<&SharedString> {
        match &self.phase {
            Phase::Failed(e) => Some(e),
            _ => None,
        }
    }

    /// The crop as it will be saved (clamped), and the frame it is in.
    pub fn crop(&mut self) -> Option<(View, Size)> {
        let p = self.picture()?.clone();
        let frame = self.frame?;
        let r = self.range(&p, frame);
        Some((clamp_view(self.current_view(), p.nat, frame, r), frame))
    }

    pub fn is_saving(&self) -> bool {
        self.saving
    }

    /// The owner's upload finished: success closes the editor, a refusal shows its message and
    /// keeps the crop.
    pub fn save_done(&mut self, result: Result<(), SharedString>, cx: &mut Context<Self>) {
        match result {
            Ok(()) => {
                self.saved = self.pending.take();
                self.close(cx);
            }
            Err(message) => {
                self.saving = false;
                self.pending = None;
                self.save_error = Some(if message.is_empty() { "Couldn't save the picture.".into() } else { message });
                cx.notify();
            }
        }
    }

    // ── reading ────────────────────────────────────────────────────────────────────────

    fn load(&mut self, source: PictureSource, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let aspect = self.options.shape.aspect();
        self._work = Some(cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move {
                    let (bytes, name) = match source {
                        PictureSource::Bytes { bytes, name } => (bytes, name),
                        PictureSource::Path(path) => {
                            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            match std::fs::read(&path) {
                                Ok(bytes) => (bytes, name),
                                Err(_) => return Err(PictureError::NotAPicture),
                            }
                        }
                    };
                    picture::read(&bytes, &name, aspect)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.release_textures(cx);
                match read {
                    Ok(p) => {
                        this.fill = if p.alpha { Fill::Color(p.palette[1]) } else { Fill::Blur };
                        this.adjusted = false;
                        this.placed = false;
                        this.phase = Phase::Ready(p);
                    }
                    Err(e) => this.phase = Phase::Failed(e.message().into()),
                }
                cx.notify();
            });
        }));
    }

    /// A picture fixed by the caller (the screens): no decode.
    pub fn with_picture(mut self, picture: Picture) -> Self {
        self.generation += 1;
        self._work = None;
        self.fill = if picture.alpha { Fill::Color(picture.palette[1]) } else { Fill::Blur };
        self.phase = Phase::Ready(picture);
        self
    }

    fn replace(&mut self, source: PictureSource, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.save_error = None;
        self.glide = None;
        self.load(source, cx);
    }

    fn choose_file(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
            filters: crate::picture_filters(),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| this.replace(PictureSource::Path(path), cx));
        })
        .detach();
    }

    fn release_textures(&mut self, cx: &mut Context<Self>) {
        if let Phase::Ready(p) = &self.phase {
            for texture in p.textures() {
                cx.drop_image(texture, None);
            }
        }
    }

    // ── geometry ───────────────────────────────────────────────────────────────────────

    fn range(&self, p: &Picture, frame: Size) -> Range {
        let out = self.options.output;
        range(p.nat, frame, Size::new(out.0 as f32, out.1 as f32), self.options.shape.fit())
    }

    /// The view now: where a glide has got to (ending it when done).
    fn current_view(&mut self) -> View {
        if let Some(glide) = &self.glide {
            let t = glide.start.elapsed().as_secs_f32() * 1000. / glide.ms;
            if t >= 1. {
                self.view = glide.to;
                self.glide = None;
            } else {
                self.view = ease(glide.from, glide.to, t);
            }
        }
        self.view
    }

    /// Where the picture is headed: wheel notches add up during a glide.
    fn aim(&mut self) -> View {
        match &self.glide {
            Some(glide) => glide.to,
            None => self.current_view(),
        }
    }

    fn animate_to(&mut self, to: View, ms: f32, cx: &mut Context<Self>) {
        let from = self.current_view();
        if ms <= 0. || same_view(from, to) || cx.reduce_motion() {
            self.glide = None;
            self.view = to;
        } else {
            self.glide = Some(Glide { from, to, start: Instant::now(), ms });
        }
        cx.notify();
    }

    fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.glide = None;
        self.view = view;
        cx.notify();
    }

    /// The thirds grid shows while you work and fades shortly after.
    fn touch(&mut self, cx: &mut Context<Self>) {
        self.adjusted = true;
        if !self.interacting {
            self.interacting = true;
            cx.notify();
        }
        self._grid = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(700)).await;
            let _ = this.update(cx, |this, cx| {
                this.interacting = false;
                this._grid = None;
                cx.notify();
            });
        }));
    }

    fn ready(&self) -> Option<(Picture, Size)> {
        Some((self.picture()?.clone(), self.frame?))
    }

    fn zoom_by(&mut self, factor: f32, at: Option<At>, ms: f32, cx: &mut Context<Self>) {
        let Some((p, f)) = self.ready() else { return };
        let r = self.range(&p, f);
        let base = self.aim();
        let scale = r.min.max(r.max.min(base.scale * factor));
        let at = at.unwrap_or(At { x: f.w / 2., y: f.h / 2. });
        let to = clamp_view(zoom_at(base, scale, at), p.nat, f, r);
        self.animate_to(to, ms, cx);
        self.touch(cx);
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        let Some((p, f)) = self.ready() else { return };
        self.animate_to(initial_view(p.nat, f), 300., cx);
        self.fill = if p.alpha { Fill::Color(p.palette[1]) } else { Fill::Blur };
        self.adjusted = false;
    }

    fn pan(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        let Some((p, f)) = self.ready() else { return };
        let r = self.range(&p, f);
        let v = self.aim();
        self.animate_to(clamp_view(View { x: v.x + dx, y: v.y + dy, ..v }, p.nat, f, r), 90., cx);
        self.touch(cx);
    }

    /// The stage size for the window (the renderer's `size-88` / `h-68`, smaller below 680 px of
    /// height), the frame inside it, and the frame's origin in the stage.
    fn layout(&self, window: &Window) -> (Size, Size, (f32, f32)) {
        let viewport = window.viewport_size();
        let short = f32::from(viewport.height) <= 680.;
        let stage = match self.options.shape {
            PictureShape::Avatar => {
                let s = if short { 288. } else { 352. };
                Size::new(s, s)
            }
            PictureShape::ServerTile => {
                let card = 688f32.min(f32::from(viewport.width) - 32.);
                Size::new((card - 48.).max(80.), if short { 224. } else { 272. })
            }
        };
        let margin = match self.options.shape {
            PictureShape::Avatar => STAGE_MARGIN_AVATAR,
            PictureShape::ServerTile => STAGE_MARGIN_BANNER,
        };
        let frame = frame_in(stage, margin, self.options.shape.aspect());
        let round = |v: f32| (v + 0.5).floor();
        (stage, frame, (round((stage.w - frame.w) / 2.), round((stage.h - frame.h) / 2.)))
    }

    /// A resize keeps the crop (the view scales with the frame); a new picture settles into the
    /// frame from slightly closer.
    fn fit_frame(&mut self, frame: Size, cx: &mut Context<Self>) {
        if let Some(prev) = self.frame
            && prev != frame
        {
            let k = frame.w / prev.w;
            let v = self.current_view();
            self.glide = None;
            self.view = View { scale: v.scale * k, x: v.x * k, y: v.y * k };
        }
        self.frame = Some(frame);
        if !self.placed
            && let Phase::Ready(p) = &self.phase
        {
            self.placed = true;
            let start = initial_view(p.nat, frame);
            self.view = zoom_at(start, start.scale * 1.06, At { x: frame.w / 2., y: frame.h / 2. });
            self.animate_to(start, 420., cx);
        }
    }

    // ── open / close / save ────────────────────────────────────────────────────────────

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        cx.notify();
        let delay = if cx.reduce_motion() { Duration::ZERO } else { Duration::from_millis(300) };
        self._closing = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.release_textures(cx);
                this.phase = Phase::Loading;
                let saved = this.saved.take();
                cx.emit(PictureEditorEvent::Closed { saved });
            });
        }));
    }

    /// Esc or a click outside: closes, unless you've worked on the crop; then the footer shakes
    /// (as an unsaved settings tab does). Cancel always closes.
    fn request_close(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        if self.adjusted && self.picture().is_some() {
            self.nudge += 1;
            cx.notify();
        } else {
            self.close(cx);
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some((p, f)) = self.ready() else { return };
        let r = self.range(&p, f);
        let view = clamp_view(self.current_view(), p.nat, f, r);
        self.set_view(view, cx);
        self.saving = true;
        self.save_error = None;
        let (out, fill) = (self.options.output, self.fill);
        self._work = Some(cx.spawn(async move |this, cx| {
            let jpeg = cx.background_executor().spawn(async move { picture::render_crop(&p, view, f, out, fill) }).await;
            let _ = this.update(cx, |this, cx| match jpeg {
                Ok(bytes) => {
                    let bytes = Arc::new(bytes);
                    this.pending = Some(bytes.clone());
                    cx.emit(PictureEditorEvent::Save(bytes));
                }
                Err(e) => {
                    this.saving = false;
                    this.save_error = Some(if e.is_empty() { "Couldn't save the picture.".into() } else { e.into() });
                    cx.notify();
                }
            });
        }));
    }

    // ── input ──────────────────────────────────────────────────────────────────────────

    fn frame_point(&self, position: gpui::Point<Pixels>, origin: (f32, f32)) -> At {
        let stage = self.stage_bounds.get();
        At {
            x: f32::from(position.x - stage.origin.x) - origin.0,
            y: f32::from(position.y - stage.origin.y) - origin.1,
        }
    }

    fn on_wheel(&mut self, event: &ScrollEvent, origin: (f32, f32), cx: &mut Context<Self>) {
        let Some((p, f)) = self.ready() else { return };
        if self.drag.is_some() || self.saving {
            return;
        }
        let ctrl = event.modifiers.control;
        let (dy, lines) = match event.delta {
            ScrollDelta::Lines(l) => (-l.y * 33., true),
            ScrollDelta::Pixels(p) => (-f32::from(p.y), false),
        };
        let factor = (-dy * if ctrl { 0.012 } else { 0.0025 }).exp();
        let at = self.frame_point(event.position, origin);
        let r = self.range(&p, f);
        let base = self.aim();
        let scale = r.min.max(r.max.min(base.scale * factor));
        let next = clamp_view(zoom_at(base, scale, at), p.nat, f, r);
        // A mouse notch glides; a trackpad's stream of small deltas (and a pinch) tracks.
        if !ctrl && (lines || dy.abs() >= 40.) {
            self.animate_to(next, 160., cx);
        } else {
            self.set_view(next, cx);
        }
        self.touch(cx);
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.saving || self.picture().is_none() {
            return;
        }
        let key = event.keystroke.key.as_str();
        let typed = event.keystroke.key_char.as_deref().unwrap_or("");
        let step = if event.keystroke.modifiers.shift { 40. } else { 8. };
        let handled = match key {
            "enter" => {
                self.save(cx);
                true
            }
            "left" => {
                self.pan(-step, 0., cx);
                true
            }
            "right" => {
                self.pan(step, 0., cx);
                true
            }
            "up" => {
                self.pan(0., -step, cx);
                true
            }
            "down" => {
                self.pan(0., step, cx);
                true
            }
            _ if typed == "+" || typed == "=" || key == "+" || key == "=" => {
                self.zoom_by(1.25, None, 180., cx);
                true
            }
            _ if typed == "-" || typed == "_" || key == "-" => {
                self.zoom_by(1. / 1.25, None, 180., cx);
                true
            }
            "0" => {
                self.reset(cx);
                true
            }
            _ => false,
        };
        if handled {
            window.prevent_default();
            cx.stop_propagation();
        }
    }

    fn slide_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some((p, f)) = self.ready() else { return };
        let bounds = self.slider_bounds.get();
        let width = f32::from(bounds.size.width) - 14.;
        if width <= 0. {
            return;
        }
        let t = ((f32::from(x - bounds.origin.x) - 7.) / width).clamp(0., 1.);
        let r = self.range(&p, f);
        let mut s = from_slider(t, r);
        // A detent at "fills the frame".
        let c = cover_scale(p.nat, f);
        if (s / c).ln().abs() < 0.035 {
            s = c;
        }
        let v = self.current_view();
        self.set_view(clamp_view(zoom_at(v, s, At { x: f.w / 2., y: f.h / 2. }), p.nat, f, r), cx);
        self.touch(cx);
    }

    fn ink(&mut self, cx: &App) -> Ink {
        let id = cx.theme().id;
        if let Some((cached, ink)) = self.ink
            && cached == id
        {
            return ink;
        }
        let e = &id.scopes().picture_editor;
        let ink = Ink { veil: hsla(e.crop_veil), corner: hsla(e.crop_corner), edge: hsla(e.crop_edge), grid: hsla(e.crop_grid) };
        self.ink = Some((id, ink));
        ink
    }

    fn crop_state(&self) -> CropState {
        CropState { picture: self.picture().cloned(), view: self.view, frame: self.frame, fill: self.fill }
    }

    /// The frame's corner: the theme's avatar shape, or the tile's corner scaled with the frame.
    fn frame_radius(&self, frame: Size, cx: &App) -> Pixels {
        let theme = cx.theme();
        match self.options.shape {
            PictureShape::Avatar => theme.avatar_radius(px(frame.w)),
            PictureShape::ServerTile => theme.radius_md * (frame.w / crate::TILE_WIDTH),
        }
    }
}

/// Everything on the stage, painted in one canvas: the fill, the picture, the veil around the
/// frame, the half-lit corners between the square and the shape, the frame's edge and the thirds
/// grid.
#[allow(clippy::too_many_arguments)]
fn paint_stage(
    bounds: Bounds<Pixels>,
    stage_radius: Pixels,
    state: &CropState,
    origin: (f32, f32),
    frame_radius: Pixels,
    ink: Ink,
    grid: bool,
    window: &mut Window,
) {
    let (Some(p), Some(frame)) = (&state.picture, state.frame) else { return };
    let f = Bounds { origin: bounds.origin + point(px(origin.0), px(origin.1)), size: size(px(frame.w), px(frame.h)) };
    // What shows in the frame where the picture doesn't (always there, so a gap opening
    // mid-zoom is already filled).
    let bg = match state.fill {
        Fill::Color(c) => rgb(c),
        Fill::Blur => rgb(p.palette[1]),
    };
    window.paint_quad(fill(f, bg));
    if state.fill == Fill::Blur {
        let s = (frame.w / p.nat.w).max(frame.h / p.nat.h) * 1.1;
        let rect = Bounds {
            origin: f.origin + point(px((frame.w - p.nat.w * s) / 2.), px((frame.h - p.nat.h * s) / 2.)),
            size: size(px(p.nat.w * s), px(p.nat.h * s)),
        };
        let _ = window.paint_image(f, rect, Corners::default(), p.blur.clone(), 0, false);
    }
    // The picture, across the whole stage.
    let v = state.view;
    let rect = Bounds {
        origin: f.origin + point(px(v.x), px(v.y)),
        size: size(px(p.nat.w * v.scale), px(p.nat.h * v.scale)),
    };
    let _ = window.paint_image(bounds, rect, Corners::all(stage_radius), p.stage.clone(), 0, false);
    // The veil outside the frame: a border as wide as the gap on each side, inside the stage's
    // rounded corners (`box-shadow: 0 0 0 100vmax var(--crop-veil)`).
    let edges = Edges {
        top: f.origin.y - bounds.origin.y,
        right: bounds.right() - f.right(),
        bottom: bounds.bottom() - f.bottom(),
        left: f.origin.x - bounds.origin.x,
    };
    window.paint_quad(quad(bounds, Corners::all(stage_radius), gpui::transparent_black(), edges, ink.veil, BorderStyle::Solid));
    // The square's corners outside the shape, half lit: a border around the shape, clipped to
    // the frame.
    if frame_radius > px(0.5) {
        let w = px(frame.w.max(frame.h));
        let outer = f.dilate(w);
        window.with_content_mask(Some(gpui::ContentMask { bounds: f }), |window| {
            window.paint_quad(quad(outer, Corners::all(frame_radius + w), gpui::transparent_black(), Edges::all(w), ink.corner, BorderStyle::Solid));
        });
    }
    // The frame's edge (`inset 0 0 0 1.5px var(--crop-edge)`).
    window.paint_quad(quad(f, Corners::all(frame_radius), gpui::transparent_black(), Edges::all(px(1.5)), ink.edge, BorderStyle::Solid));
    if grid {
        for i in [1., 2.] {
            let x = f.origin.x + px((i * frame.w / 3.).round());
            window.paint_quad(fill(Bounds { origin: point(x, f.origin.y), size: size(px(1.), f.size.height) }, ink.grid));
            let y = f.origin.y + px((i * frame.h / 3.).round());
            window.paint_quad(fill(Bounds { origin: point(f.origin.x, y), size: size(f.size.width, px(1.)) }, ink.grid));
        }
    }
}

/// A 32 px icon button of the toolbar.
fn tool_button(id: &'static str, icon: IconName, disabled: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    let (fg, hover_fg, hover_bg) = (theme.color_text_secondary, theme.color_text_primary, theme.color_surface_hover);
    div()
        .id(id)
        .flex()
        .flex_none()
        .size_8()
        .items_center()
        .justify_center()
        .rounded(theme.radius_sm)
        .text_color(fg)
        .when(disabled, |this| this.opacity(0.4))
        .when(!disabled, |this| this.cursor_pointer().hover(move |s| s.bg(hover_bg).text_color(hover_fg)))
        .child(Icon::new(icon).size(px(16.)).color(fg).group_hover(id, hover_fg))
        .group(id)
}

impl Render for PictureEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (stage, frame, origin) = self.layout(window);
        self.fit_frame(frame, cx);
        let view = self.current_view();
        if self.glide.is_some() {
            window.request_animation_frame();
        }
        // The fill's swatches, floating under the frame while the crop shows any of it: a gap
        // after zooming out, or a transparent picture. Judged on where the picture comes to
        // rest, so a drag past an edge doesn't flash them.
        let fill_picker = self.picture().cloned().and_then(|p| {
            let r = self.range(&p, frame);
            let shown = p.alpha || leaves_gap(clamp_view(view, p.nat, frame, r), p.nat, frame);
            shown.then(|| self.fill_picker(&p, cx).into_any_element())
        });
        let ink = self.ink(cx);
        let theme = cx.theme();
        let shape = self.options.shape;
        let avatar = shape == PictureShape::Avatar;
        let picture = self.picture().cloned();
        let ready = picture.is_some();
        let state = self.crop_state();
        let frame_radius = self.frame_radius(frame, cx);
        let stage_radius = theme.radius_lg;
        let range = picture.as_ref().map(|p| self.range(p, frame));
        let disabled = !ready || self.saving;

        // ── header ──
        let header = div()
            .flex()
            .items_start()
            .justify_between()
            .gap_4()
            .px_6()
            .pb_4()
            .pt_5()
            .child(
                div()
                    .min_w_0()
                    .child(Label::new(self.options.title.clone()).font(FontRole::Display).size(18.).weight(600.).color(theme.color_text_primary))
                    .child(
                        div().mt_0p5().child(
                            Label::new(if picture.as_ref().is_some_and(|p| p.animated) {
                                "GIFs keep their first frame. Drag to move, scroll to zoom."
                            } else {
                                "Drag to move, scroll to zoom."
                            })
                            .size(13.)
                            .color(theme.color_text_muted),
                        ),
                    ),
            )
            .when_some(picture.as_ref(), |this, p| {
                this.child(
                    div()
                        .mt_1()
                        .flex_none()
                        .max_w(px(224.))
                        .rounded(theme.radius_sm)
                        .bg(theme.color_bg_lighter)
                        .px_2()
                        .py_1()
                        .child(
                            Label::new(format!("{} × {}", p.nat.w as u32, p.nat.h as u32))
                                .font(FontRole::Meta)
                                .size(11.)
                                .color(theme.color_text_muted)
                                .truncate(),
                        ),
                )
            });

        // ── stage ──
        let stage_bounds = self.stage_bounds.clone();
        let grid = self.interacting;
        let paint_state = state.clone();
        let mut stage_el = div()
            .id("picture-stage")
            .relative()
            .flex_none()
            .w(px(stage.w))
            .h(px(stage.h))
            .overflow_hidden()
            .rounded(stage_radius)
            .bg(theme.color_bg_darkest)
            .when(ready && !self.saving, |this| this.cursor(if self.drag.is_some() { gpui::CursorStyle::ClosedHand } else { gpui::CursorStyle::OpenHand }))
            .child(
                canvas(
                    move |bounds, _, _| stage_bounds.set(bounds),
                    move |bounds, _, window, _| paint_stage(bounds, stage_radius, &paint_state, origin, frame_radius, ink, grid, window),
                )
                .absolute()
                .inset_0(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    if this.saving || this.picture().is_none() {
                        return;
                    }
                    if e.click_count == 2 {
                        this.reset(cx);
                        return;
                    }
                    let from = this.current_view();
                    this.glide = None;
                    this.drag = Some(Drag { from, start: e.position });
                    this.touch(cx);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
                let Some(drag) = &this.drag else { return };
                if e.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                let Some((p, f)) = this.ready() else { return };
                let raw = View {
                    scale: drag.from.scale,
                    x: drag.from.x + f32::from(e.position.x - drag.start.x),
                    y: drag.from.y + f32::from(e.position.y - drag.start.y),
                };
                this.set_view(rubber_view(raw, p.nat, f), cx);
                this.touch(cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_drag(cx)),
            )
            .on_scroll(cx.listener(move |this, e: &ScrollEvent, _, cx| {
                cx.stop_propagation();
                this.on_wheel(e, origin, cx);
            }));
        stage_el = stage_el.children(fill_picker);
        if !ready && self.error().is_none() {
            stage_el = stage_el.child(div().absolute().inset_0().bg(theme.color_bg_light.opacity(0.4)));
        }
        if let Some(error) = self.error().cloned() {
            stage_el = stage_el.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .px_8()
                    .child(Icon::new(IconName::PictureAlert).size(px(22.)).color(theme.color_text_muted))
                    .child(Label::new("Can't open this file").size(14.).weight(500.).color(theme.color_text_primary))
                    .child(div().text_center().child(Label::new(error).size(12.).line_height(18.).color(theme.color_text_muted)))
                    .child(
                        div().mt_2().child(
                            Button::new("picture-choose-error", "Choose another picture")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.choose_file(cx))),
                        ),
                    ),
            );
        }
        // Shown while a picture file is dragged over the card.
        stage_el = stage_el.child(
            div()
                .absolute()
                .inset_2()
                .invisible()
                .group_drag_over::<ExternalPaths>("picture-card", |s| s.visible())
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .border_2()
                .border_dashed()
                .border_color(theme.color_accent)
                .bg(theme.color_accent_soft)
                .child(Label::new("Drop to use this picture instead").size(13.).weight(600.).color(theme.color_text_primary)),
        );

        // ── toolbar ──
        let t = range.map_or(0., |r| to_slider(view.scale, r));
        let cover = match (picture.as_ref(), range) {
            (Some(p), Some(r)) => to_slider(cover_scale(p.nat, frame), r),
            _ => 0.,
        };
        let at_start = picture.as_ref().is_none_or(|p| same_view(view, initial_view(p.nat, frame)));
        let zoom_out_off = disabled || t <= 0.001;
        let zoom_in_off = disabled || t >= 0.999;
        let slider_bounds = self.slider_bounds.clone();
        let accent = theme.color_accent;
        let track_rest = theme.color_text_muted.opacity(0.32);
        let thumb_border = if disabled { theme.color_text_muted } else { accent };
        let slider = div()
            .id("picture-zoom")
            .relative()
            .mx_1()
            .flex()
            .h_8()
            .min_w_0()
            .flex_1()
            .items_center()
            .when(!disabled, |this| this.cursor_pointer())
            .child(canvas(move |bounds, _, _| slider_bounds.set(bounds), |_, _, _, _| {}).absolute().inset_0())
            // The track: accent up to `--fill` (t of the whole width), the rest muted
            // (`.picture-zoom`).
            .child(
                div()
                    .relative()
                    .w_full()
                    .h_1()
                    .rounded_full()
                    .bg(track_rest)
                    .child(div().absolute().left_0().top_0().bottom_0().w(gpui::relative(t)).rounded_full().bg(accent)),
            )
            // The thumb's centre travels 7 px inside each end.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(7.))
                    .right(px(7.))
                    .when(cover > 0.02, |this| {
                        // "Fills the frame" sits partway along when the picture can zoom out past it.
                        this.child(
                            div()
                                .absolute()
                                .top(px(11.))
                                .h(px(10.))
                                .w(px(2.))
                                .left(gpui::relative(cover))
                                .ml(px(-1.))
                                .rounded_full()
                                .bg(theme.color_text_muted.opacity(0.6)),
                        )
                    })
                    .child(
                        // 14 px, bg-light with a 2 px accent border and an accent-mid glow.
                        div()
                            .absolute()
                            .top(px(9.))
                            .left(gpui::relative(t))
                            .ml(px(-7.))
                            .size(px(14.))
                            .rounded_full()
                            .border_2()
                            .border_color(thumb_border)
                            .bg(theme.color_bg_light)
                            .when(!disabled, |this| {
                                this.shadow(vec![gpui::BoxShadow {
                                    color: theme.color_accent_mid,
                                    offset: point(px(0.), px(0.)),
                                    blur_radius: px(3.),
                                    spread_radius: px(0.),
                                    inset: false,
                                }])
                            }),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    if this.saving || this.picture().is_none() {
                        return;
                    }
                    this.slider_drag = true;
                    this.slide_to(e.position.x, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.slider_drag && e.pressed_button == Some(MouseButton::Left) {
                    this.slide_to(e.position.x, cx);
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.slider_drag = false))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.slider_drag = false));
        let toolbar = div()
            .flex()
            .items_center()
            .gap_1()
            .child(tool_button("picture-zoom-out", IconName::PictureZoomOut, zoom_out_off, cx).when(!zoom_out_off, |this| {
                this.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.zoom_by(1. / 1.25, None, 180., cx)))
            }))
            .child(slider)
            .child(tool_button("picture-zoom-in", IconName::PictureZoomIn, zoom_in_off, cx).when(!zoom_in_off, |this| {
                this.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.zoom_by(1.25, None, 180., cx)))
            }))
            .child({
                let off = disabled || at_start;
                let (fg, hover_fg, hover_bg) = (theme.color_text_secondary, theme.color_text_primary, theme.color_surface_hover);
                div()
                    .id("picture-reset")
                    .group("picture-reset")
                    .ml_1()
                    .flex()
                    .h_8()
                    .flex_none()
                    .items_center()
                    .gap_1p5()
                    .rounded(theme.radius_sm)
                    .px_2p5()
                    .text_color(fg)
                    .when(off, |this| this.opacity(0.4))
                    .when(!off, |this| {
                        this.cursor_pointer()
                            .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reset(cx)))
                    })
                    .child(Icon::new(IconName::PictureReset).size(px(14.)).color(fg).group_hover("picture-reset", hover_fg))
                    .child(Label::new("Reset").size(12.).weight(500.))
            });

        let previews = match &self.options.previews {
            Previews::Avatar { uid, name, status } => Some(avatar_previews(&state, *uid, name, *status, cx)),
            Previews::Server { name, members } => Some(server_previews(&state, name, members, cx)),
            Previews::Custom(build) => Some(build(&state, window, cx)),
            Previews::None => None,
        };
        let theme = cx.theme();
        let body = div()
            .flex()
            .min_h_0()
            .px_6()
            .map(|this| if avatar { this.flex_row().gap_6() } else { this.flex_col().gap_4() })
            .child(div().flex().min_w_0().flex_col().gap_3().child(stage_el).child(toolbar))
            .when_some(previews, |this, previews| {
                this.child(div().map(|d| if avatar { d.w(px(224.)).flex_none() } else { d.min_w_0() }).child(previews))
            });

        // ── footer ──
        let nudge = self.nudge;
        let footer = div()
            .mt_5()
            .flex()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(theme.color_border_divider)
            .px_6()
            .py_4()
            .child({
                let (fg, hover_fg, hover_bg) = (theme.color_text_secondary, theme.color_text_primary, theme.color_surface_hover);
                div()
                    .id("picture-choose")
                    .group("picture-choose")
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(theme.radius_sm)
                    .px_2()
                    .py_2()
                    .text_color(fg)
                    .when(self.saving, |this| this.opacity(0.5))
                    .when(!self.saving, |this| {
                        this.cursor_pointer()
                            .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.choose_file(cx)))
                    })
                    .child(Icon::new(IconName::PictureImage).size(px(16.)).color(fg).group_hover("picture-choose", hover_fg))
                    .child(Label::new("Choose another").size(13.).weight(500.))
            })
            .child(
                div().min_w_0().flex_1().child(
                    Label::new(if nudge > 0 { "Save the picture, or Cancel to drop it." } else { "" })
                        .size(12.)
                        .color(theme.color_text_muted)
                        .truncate(),
                ),
            )
            .child(
                Button::new("picture-cancel", "Cancel")
                    .variant(ButtonVariant::Ghost)
                    .disabled(self.saving)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx))),
            )
            .child(
                Button::new("picture-save", if self.saving { "Saving…" } else { "Save" })
                    .probe("picture-save")
                    .loading(self.saving)
                    .disabled(!ready || self.saving)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx))),
            );
        let footer = if nudge > 0 {
            footer
                .with_animation(
                    ("picture-footer-nudge", nudge as u64),
                    Animation::new(Duration::from_millis(360)),
                    |el, t| el.left(px(decibell_ui::motion::Keyframes::BAR_NUDGE.value(t))),
                )
                .into_any_element()
        } else {
            footer.into_any_element()
        };

        let card = div()
            .id("picture-card")
            .group("picture-card")
            .occlude()
            .relative()
            .flex()
            .flex_col()
            .max_h_full()
            .max_w_full()
            .w(px(if avatar { 648. } else { 688. }))
            .overflow_hidden()
            .rounded(theme.radius_xl)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_dark)
            .shadow(theme.shadow_modal.clone())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| this.on_key(e, window, cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                let Some(path) = paths.paths().iter().find(|p| p.to_str().is_some_and(picture::looks_like_picture)) else { return };
                this.replace(PictureSource::Path(path.clone()), cx);
            }))
            .child(header)
            .child(body)
            .when_some(self.save_error.clone(), |this, error| {
                this.child(
                    div()
                        .mx_6()
                        .mt_4()
                        .rounded(theme.radius_md)
                        .border_1()
                        .border_color(theme.color_error.opacity(0.25))
                        .bg(theme.color_error.opacity(0.1))
                        .px_3()
                        .py_2()
                        .child(Label::new(error).size(12.).line_height(18.).color(theme.color_error)),
                )
            })
            .child(footer);

        Modal::new("picture-editor", self.open)
            .padding(16., 16.)
            .on_close(cx.listener(|this, _: &CloseRequest, _, cx| this.request_close(cx)))
            .child(card)
    }
}

impl PictureEditor {
    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_none() {
            return;
        }
        if let Some((p, f)) = self.ready() {
            let r = self.range(&p, f);
            let to = clamp_view(self.current_view(), p.nat, f, r);
            self.animate_to(to, 320., cx);
        }
        cx.notify();
    }

    fn fill_picker(&self, p: &Picture, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let options: [(Fill, &'static str); 5] = [
            (Fill::Blur, "Blurred picture"),
            (Fill::Color(p.palette[0]), "Picture colour"),
            (Fill::Color(p.palette[1]), "Deep shade"),
            (Fill::Color([255, 255, 255]), "White"),
            (Fill::Color([0x0b, 0x0a, 0x0d]), "Black"),
        ];
        let mut pill = div()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_full()
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_light)
            .py_1()
            .pl_3()
            .pr_1()
            .shadow(theme.shadow_float.clone())
            .child(div().mr_0p5().child(Label::new("Background").size(11.).weight(500.).color(theme.color_text_muted)));
        for (index, (option, label)) in options.into_iter().enumerate() {
            let on = option == self.fill;
            let blur = p.blur.clone();
            let swatch = div()
                .size(px(22.))
                .rounded_full()
                .border_1()
                .border_color(theme.color_border)
                .overflow_hidden()
                .map(|this| match option {
                    Fill::Color(c) => this.bg(rgb(c)),
                    Fill::Blur => this.child(
                        canvas(|_, _, _| {}, move |bounds, _, window, _| {
                            let grow = bounds.size.width * 0.25;
                            let rect = bounds.dilate(grow);
                            let _ = window.paint_image(bounds, rect, Corners::all(bounds.size.width / 2.), blur.clone(), 0, false);
                        })
                        .size_full(),
                    ),
                });
            pill = pill.child(
                div()
                    .id(("picture-fill", index))
                    .flex_none()
                    .rounded_full()
                    .p(px(if on { 2. } else { 0. }))
                    .m(px(if on { -4. } else { 0. }))
                    .when(on, |this| this.border_2().border_color(theme.color_accent).bg(theme.color_bg_light))
                    .cursor_pointer()
                    .tooltip(decibell_ui::title_tooltip(label))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.fill = option;
                        this.adjusted = true;
                        cx.notify();
                    }))
                    .child(swatch),
            );
        }
        div().absolute().bottom_2().left_0().right_0().flex().justify_center().child(pill)
    }
}
