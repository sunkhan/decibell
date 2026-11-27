//! Drawing a field: the element an [`InputState`] renders, and the two builders a view
//! puts in its tree ([`TextInput`], [`TextArea`]).

use gpui::{
    App, AvailableSpace, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, KeyDownEvent, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, RenderOnce, Style, Window, fill, point, px,
    relative, size,
};

use super::display::InputStyle;
use super::state::InputState;
use crate::{Engine, PaintOptions, Wrapping};

/// A single-line field: the text scrolls sideways to keep the caret in view. As wide as
/// its container, one line high. Put it inside your own box (border, padding, focus ring):
/// `decibell_ui::TextField` is that box.
///
/// ```ignore
/// TextInput::new(&self.name, input_style)
/// ```
#[derive(IntoElement)]
pub struct TextInput {
    state: Entity<InputState>,
    style: InputStyle,
}

impl TextInput {
    pub fn new(state: &Entity<InputState>, style: InputStyle) -> Self {
        Self {
            state: state.clone(),
            style,
        }
    }
}

impl RenderOnce for TextInput {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        self.state.update(cx, |state, cx| state.set_style(self.style, cx));
        self.state
    }
}

/// A multi-line field: wraps at its container's width and is as high as its text (one
/// line when empty). It does not scroll by itself: for a maximum height, put it in a
/// scroll container and let [`InputState::on_reveal_caret`] keep the caret in view
/// (`decibell_ui::TextAreaField` does both).
#[derive(IntoElement)]
pub struct TextArea {
    state: Entity<InputState>,
    style: InputStyle,
}

impl TextArea {
    pub fn new(state: &Entity<InputState>, style: InputStyle) -> Self {
        Self {
            state: state.clone(),
            style,
        }
    }
}

impl RenderOnce for TextArea {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        self.state.update(cx, |state, cx| state.set_style(self.style, cx));
        self.state
    }
}

/// What [`InputState`] renders.
pub(crate) struct InputElement {
    state: Entity<InputState>,
}

impl InputElement {
    pub fn new(state: Entity<InputState>) -> Self {
        Self { state }
    }
}

/// The placeholder's wrapping: a single line is cut by the field's edge, a multi-line
/// field wraps it like its text.
fn placeholder_wrapping(multiline: bool, style: &InputStyle) -> Wrapping {
    if multiline { style.wrapping } else { Wrapping::NOWRAP }
}

/// The field's height at `width`: its text's, or its placeholder's when that is higher.
fn measure(state: &mut InputState, width: f32, scale: f32, cx: &mut App) -> f32 {
    let Some(style) = state.style else { return 0. };
    let mut height = state.measure(width, scale, cx);
    if state.buffer.is_empty() && state.buffer.rules().multiline && !state.placeholder.is_empty() {
        let engine = Engine::global(cx);
        let wrapping = placeholder_wrapping(true, &style);
        height = height.max(engine.label_layout(&state.placeholder, &style.text, wrapping, width, scale).height());
    }
    height
}

impl Element for InputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        let state = self.state.clone();
        let scale = window.scale_factor();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let layout_id = window.request_measured_layout(style, move |known, available, _, cx| {
            let width = match (known.width, available.width) {
                (Some(width), _) => f32::from(width),
                (None, AvailableSpace::Definite(width)) => f32::from(width),
                (None, AvailableSpace::MinContent) => 0.,
                (None, AvailableSpace::MaxContent) => f32::INFINITY,
            };
            state.update(cx, |state, cx| {
                if width <= 0. {
                    // Flex layout asks for the narrowest the field could be before it asks
                    // for its real width, every frame: a field can be any width, and
                    // answering from the layout it has keeps that layout.
                    return size(px(0.), px((state.display.height() * scale).ceil() / scale));
                }
                let height = measure(state, width, scale, cx);
                // A multi-line field that doesn't wrap (`wrap="off"`: a code field) is as wide as
                // its longest line and the caret, at least its container: the container scrolls
                // sideways.
                let unwrapped = state.buffer.rules().multiline && state.style.is_some_and(|style| style.wrapping == Wrapping::PRE);
                let width = if !width.is_finite() {
                    state.display.width() + 1.
                } else if unwrapped {
                    width.max(state.display.width() + state.style.map_or(1., |style| style.caret_width))
                } else {
                    width
                };
                // Whole device pixels, so the box is never lower than its text.
                size(px(width), px((height * scale).ceil() / scale))
            })
        });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Hitbox> {
        let scale = window.scale_factor();
        let visible = bounds.intersect(&window.content_mask().bounds);
        let width = f32::from(bounds.size.width);
        self.state.update(cx, |state, cx| {
            measure(state, width, scale, cx);
            state.bounds = bounds;
            state.visible = visible;
            let Some(style) = state.style else { return };
            if state.buffer.rules().multiline {
                return;
            }
            // A single line scrolls sideways: keep the caret inside the box, and never
            // scroll past the text's end.
            let (x, _, _) = state.display.caret(state.buffer.selection().head, state.affinity);
            let room = (width - style.caret_width).max(0.);
            if std::mem::take(&mut state.reveal) {
                state.scroll_x = state.scroll_x.clamp((x - room).max(0.), x);
            }
            let content = state.display.width().max(x);
            state.scroll_x = state.scroll_x.min((content - room).max(0.)).max(0.);
        });
        let state = self.state.read(cx);
        window.set_focus_handle(&state.focus, cx);
        (!state.is_disabled()).then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.state.read(cx).focus.clone();
        let focused = focus.is_focused(window);
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.state.clone()), cx);
        if let Some(hitbox) = hitbox.as_ref() {
            window.set_cursor_style(CursorStyle::IBeam, hitbox);
            self.listen(hitbox, window);
        }

        let scale = window.scale_factor();
        let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
        let caret_moved = self.state.update(cx, |state, cx| {
            let Some(style) = state.style else { return false };
            let engine = Engine::global(cx);
            let origin = point(bounds.origin.x - px(state.scroll_x), bounds.origin.y);
            let selection = state.buffer.selection();
            let caret = state.caret_visible(window).then(|| state.caret_bounds());
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                if state.buffer.is_empty() && !state.placeholder.is_empty() {
                    let multiline = state.buffer.rules().multiline;
                    let layout = engine.label_layout(
                        &state.placeholder,
                        &style.text,
                        placeholder_wrapping(multiline, &style),
                        if multiline { f32::from(bounds.size.width) } else { f32::INFINITY },
                        scale,
                    );
                    let options = PaintOptions {
                        color: Some(style.placeholder_color),
                        ..PaintOptions::default()
                    };
                    layout.paint(bounds.origin, &options, window, cx);
                }
                // An unfocused field shows no selection, as in Chromium.
                let shown = if focused { selection.range() } else { 0..0 };
                state.display.paint(origin, state.visible, shown, &style, &engine, window, cx);
                if let Some(caret) = caret {
                    let width = px((style.caret_width * scale).round().max(1.) / scale);
                    let top = snap(caret.origin.y);
                    let bottom = snap(caret.origin.y + caret.size.height);
                    // The last caret position sits on the box's right edge: keep it inside.
                    let x = snap(caret.origin.x).min(bounds.right() - width).max(bounds.left());
                    window.paint_quad(fill(
                        Bounds::new(point(x, top), size(width, bottom - top)),
                        style.caret_color,
                    ));
                }
            });
            // The IME's candidate window follows the caret.
            let caret = focused.then(|| state.caret_bounds());
            let moved = caret.is_some() && caret != state.reported_caret;
            state.reported_caret = caret;
            moved
        });
        if caret_moved {
            window.invalidate_character_coordinates();
        }
    }
}

impl InputElement {
    /// Keys while focused, and the mouse: click, double and triple click, drag, the
    /// primary selection's middle click.
    fn listen(&self, hitbox: &Hitbox, window: &mut Window) {
        let state = self.state.clone();
        window.on_key_event(move |event: &KeyDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                InputState::key_down(&state, event, window, cx);
            }
        });

        let state = self.state.clone();
        let hitbox_id = hitbox.id;
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox_id.is_hovered(window) {
                return;
            }
            match event.button {
                MouseButton::Left => {
                    state.update(cx, |state, cx| {
                        state.focus.clone().focus(window, cx);
                        state.drag_point = event.position;
                        let at = state.text_point(event.position);
                        state.mouse_down(at, event.click_count, event.modifiers.shift, cx);
                    });
                    // The click is the field's: a parent must not take the focus back.
                    window.prevent_default();
                    cx.stop_propagation();
                }
                #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                MouseButton::Middle => {
                    state.update(cx, |state, cx| {
                        state.focus.clone().focus(window, cx);
                        let at = state.text_point(event.position);
                        state.paste_primary(at, cx);
                    });
                    window.prevent_default();
                    cx.stop_propagation();
                }
                _ => {}
            }
        });

        // Moves and the release are followed wherever the pointer is, once a drag started
        // in the field.
        let state = self.state.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble || !state.read(cx).is_dragging() {
                return;
            }
            state.update(cx, |state, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    // The button went up where no event reached us (outside the window).
                    state.mouse_up(cx);
                    return;
                }
                state.drag_point = event.position;
                let (at, outside) = state.drag_target();
                state.mouse_drag(at, outside, cx);
            });
        });
        let state = self.state.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && state.read(cx).is_dragging() {
                state.update(cx, |state, cx| state.mouse_up(cx));
            }
        });
    }
}
