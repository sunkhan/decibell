//! The scroll container every scrollable pane uses: Chromium's wheel and key scrolling
//! ([`ScrollAnimator`]) and Chromium's scrollbar as the renderer's CSS gets it.
//!
//! The scrollbar, as measured in the Electron client (Chromium 130, Linux; `tools/electron-captures`
//! `kit-scroll`): `* { scrollbar-width: thin; scrollbar-color: var(--t-scrollbar) transparent }`
//! wins over the `::-webkit-scrollbar` rules, so it is Chromium's thin Fluent bar: 10 px wide
//! (`floor(10 × scale)` device px) **taking layout space** while the content overflows (always with
//! `scrollbar-gutter: stable`), a transparent track, a 6 px square-ended thumb (2 device px clear
//! each side) in `--t-scrollbar`, 10 × 10 arrow zones at both ends holding small triangles in the
//! thumb colour, thumb length `max(round(L × visible / content), 20)` over the track
//! `L = height − 20`, and no hover or pressed look.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Display, Div, Element, ElementId, EntityId,
    FlexDirection, GlobalElementId, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point,
    RenderOnce, ScrollDelta, ScrollEvent, Stateful, Style, StyleRefinement, Styled, Task, Window,
    canvas, div, fill, point, prelude::*, px, size,
};

use super::{LINE_STEP_PX, ScrollAnimator, ScrollKind};
use crate::ActiveTheme as _;
use crate::interaction::focus_handle;

/// The bar's thickness in CSS px (`scrollbar-width: thin`).
const BAR: f32 = 10.;
/// The thumb's shortest length in CSS px.
const MIN_THUMB: f32 = 20.;
/// Holding an arrow or the track: the first repeat after this, then one every [`REPEAT_EVERY`].
const REPEAT_DELAY: Duration = Duration::from_millis(250);
const REPEAT_EVERY: Duration = Duration::from_millis(50);

/// Where to put a range of the content when making it visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAlign {
    /// Move as little as possible (`scrollIntoView({ block: "nearest" })`).
    Nearest,
    /// Centre it (`block: "center"`).
    Center,
    /// Its top at the viewport's top.
    Top,
}

#[derive(Debug, Clone, Copy)]
enum Request {
    To(f64),
    Bottom,
    Visible {
        top: f64,
        height: f64,
        align: ScrollAlign,
    },
}

#[derive(Default)]
struct ScrollState {
    animator: ScrollAnimator,
    /// The viewport's bounds in the last frame (window coordinates).
    viewport: Bounds<Pixels>,
    content_height: Pixels,
    overflowing: bool,
    /// The position the last frame drew (on whole device pixels), the animator's value before
    /// that rounding, and the clock reading it was computed for.
    drawn: f64,
    drawn_exact: f64,
    drawn_at: Option<std::time::Instant>,
    pending: Option<Request>,
    /// The view to notify when the position changes outside a frame.
    view: Option<EntityId>,
    /// Where the thumb was grabbed, from its top, while it is dragged.
    drag: Option<f32>,
    /// The auto-repeat of a held arrow or track press.
    repeat: Option<Task<()>>,
}

impl ScrollState {
    fn max(&self) -> f64 {
        self.animator.max_offset()
    }

    fn apply(&mut self, request: Request) {
        let visible = f64::from(f32::from(self.viewport.size.height));
        let current = self.animator.target();
        let to = match request {
            Request::To(offset) => offset,
            Request::Bottom => self.max(),
            Request::Visible { top, height, align } => match align {
                ScrollAlign::Top => top,
                ScrollAlign::Center => top + height / 2. - visible / 2.,
                ScrollAlign::Nearest if top < current => top,
                ScrollAlign::Nearest if top + height > current + visible => top + height - visible,
                ScrollAlign::Nearest => current,
            },
        };
        self.animator.jump_to(to);
    }
}

/// The scroll position of a [`ScrollArea`], shared with the view that owns it (cheap to clone).
/// Requests made outside a frame take effect when the area is next drawn: notify the view.
///
/// ```ignore
/// struct SettingsPane { scroll: ScrollHandle, … }
/// ScrollArea::new("pane").track_scroll(&self.scroll).child(…)
/// self.scroll.scroll_to_top(); cx.notify();                       // on a tab switch
/// ```
#[derive(Clone, Default)]
pub struct ScrollHandle(Rc<RefCell<ScrollState>>);

impl ScrollHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// `scrollTop` as last drawn.
    pub fn offset(&self) -> Pixels {
        px(self.0.borrow().drawn as f32)
    }

    /// How far it can scroll (content height − viewport height), as last laid out.
    pub fn max_offset(&self) -> Pixels {
        px(self.0.borrow().max() as f32)
    }

    /// The viewport's height as last laid out.
    pub fn viewport_height(&self) -> Pixels {
        self.0.borrow().viewport.size.height
    }

    /// The viewport in window coordinates, as last laid out (during a frame, after the area's
    /// own prepaint: this frame's).
    pub fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.0.borrow().viewport
    }

    /// Whether the last frame showed the end of the content (within 1 px).
    pub fn is_at_bottom(&self) -> bool {
        let state = self.0.borrow();
        state.drawn >= state.max() - 1.
    }

    pub fn is_animating(&self) -> bool {
        self.0.borrow().animator.is_animating()
    }

    /// The last frame's position as the animator gave it (before rounding to device pixels) and
    /// the clock reading it was computed for.
    pub fn last_frame(&self) -> (f64, Option<std::time::Instant>) {
        let state = self.0.borrow();
        (state.drawn_exact, state.drawn_at)
    }

    /// Jumps to `offset` (no animation, as `scrollTop =` does).
    pub fn scroll_to(&self, offset: Pixels) {
        self.0.borrow_mut().pending = Some(Request::To(f32::from(offset) as f64));
    }

    pub fn scroll_to_top(&self) {
        self.scroll_to(px(0.));
    }

    pub fn scroll_to_bottom(&self) {
        self.0.borrow_mut().pending = Some(Request::Bottom);
    }

    /// Jumps so that the content from `top` (px from the content's top) for `height` is visible.
    pub fn scroll_into_view(&self, top: Pixels, height: Pixels, align: ScrollAlign) {
        self.0.borrow_mut().pending = Some(Request::Visible {
            top: f32::from(top) as f64,
            height: f32::from(height) as f64,
            align,
        });
    }

    /// Scrolls by `delta` px (+ = down) as a notch or a key would, animated.
    pub fn scroll_by(&self, delta: Pixels, kind: ScrollKind, cx: &App) {
        let now = cx.background_executor().now();
        self.0
            .borrow_mut()
            .animator
            .input(now, f32::from(delta) as f64, kind);
    }

    /// Content was inserted (+) or removed (−) above the viewport: keep showing the same thing.
    pub fn shift(&self, by: Pixels) {
        self.0.borrow_mut().animator.shift(f32::from(by) as f64);
    }
}

/// Row geometry of a [`ScrollArea::windowed`] list.
#[derive(Clone)]
pub enum RowHeights {
    /// Every row this tall.
    Uniform(Pixels),
    /// Row `i` spans `offsets[i]..offsets[i + 1]` (`count + 1` cumulative offsets from 0).
    Offsets(Rc<[Pixels]>),
}

impl RowHeights {
    /// The top of row `index` (or the total height, for `index == count`).
    pub fn offset(&self, index: usize) -> Pixels {
        match self {
            RowHeights::Uniform(height) => *height * index as f32,
            RowHeights::Offsets(offsets) => offsets[index.min(offsets.len() - 1)],
        }
    }

    /// The rows that intersect `top..bottom`.
    fn range(&self, count: usize, top: Pixels, bottom: Pixels) -> Range<usize> {
        match self {
            RowHeights::Uniform(height) if *height > px(0.) => {
                let first = (f32::from(top) / f32::from(*height)).floor().max(0.) as usize;
                let last = (f32::from(bottom) / f32::from(*height)).ceil().max(0.) as usize;
                first.min(count)..last.min(count)
            }
            RowHeights::Uniform(_) => 0..0,
            RowHeights::Offsets(offsets) => {
                let first = offsets.partition_point(|offset| *offset <= top).saturating_sub(1);
                let last = offsets.partition_point(|offset| *offset < bottom);
                first.min(count)..last.min(count)
            }
        }
    }
}

type RenderRows = Box<dyn Fn(Range<usize>, &mut Window, &mut App) -> Vec<AnyElement> + 'static>;

struct Windowed {
    count: usize,
    heights: RowHeights,
    render: RenderRows,
}

/// A vertical scroll container: wheel notches and scroll keys animate as in Chromium, touchpad
/// (precise) deltas scroll directly, the ends clamp, and Chromium's scrollbar is drawn (and
/// dragged, and its arrows and track clicked). Frames are requested only while an animation
/// runs; at rest it costs nothing. Nested areas: the innermost one that can still move in the
/// wheel's direction takes it.
///
/// Style it like a div (its size comes from the parent: `flex_1`, `size_full`, `max_h`…); put
/// padding on the content, not on the area. Keys scroll it while focus is inside it (a click on
/// its content focuses it): ↓ / ↑ 40 px, Page Down / Up and Space / Shift-Space ⌊87.5 %⌋ of the
/// height, Home / End.
///
/// ```ignore
/// ScrollArea::new("settings-pane").track_scroll(&self.scroll).flex_1().gutter_stable()
///     .child(div().px_8().pb_7().child(…))
/// // a long uniform list: only the rows near the viewport are built
/// ScrollArea::new("members").size_full()
///     .windowed(rows.len(), RowHeights::Uniform(px(44.)), move |range, window, cx| …)
/// ```
#[derive(IntoElement)]
pub struct ScrollArea {
    id: ElementId,
    handle: Option<ScrollHandle>,
    viewport: Stateful<Div>,
    children: Vec<AnyElement>,
    gutter_stable: bool,
    keyboard: bool,
    windowed: Option<Windowed>,
}

impl ScrollArea {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            viewport: div().id(id.clone()),
            id,
            handle: None,
            children: Vec::new(),
            gutter_stable: false,
            keyboard: true,
            windowed: None,
        }
    }

    /// Share the position with the owner (else it is kept in the area's element state).
    pub fn track_scroll(mut self, handle: &ScrollHandle) -> Self {
        self.handle = Some(handle.clone());
        self
    }

    /// `scrollbar-gutter: stable`: the bar's 10 px are kept whether or not the content overflows.
    pub fn gutter_stable(mut self) -> Self {
        self.gutter_stable = true;
        self
    }

    /// Leave the scroll keys to someone else.
    pub fn without_keys(mut self) -> Self {
        self.keyboard = false;
        self
    }

    /// Only the rows near the viewport are built, by `render(range)`; the content is
    /// `heights.offset(count)` tall. Overrides the children.
    pub fn windowed(
        mut self,
        count: usize,
        heights: RowHeights,
        render: impl Fn(Range<usize>, &mut Window, &mut App) -> Vec<AnyElement> + 'static,
    ) -> Self {
        self.windowed = Some(Windowed {
            count,
            heights,
            render: Box::new(render),
        });
        self
    }
}

impl Styled for ScrollArea {
    fn style(&mut self) -> &mut StyleRefinement {
        self.viewport.style()
    }
}

impl ParentElement for ScrollArea {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

/// The bar's thickness in px at `scale`: `floor(10 × scale)` device px.
fn bar_width(scale: f32) -> f32 {
    (BAR * scale).floor() / scale
}

impl RenderOnce for ScrollArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let area = self;
        let handle = match area.handle {
            Some(handle) => handle,
            None => window
                .use_keyed_state((area.id.clone(), "scroll"), cx, |_, _| ScrollHandle::new())
                .read(cx)
                .clone(),
        };
        let focus = focus_handle(window, cx, (area.id.clone(), "focus"), false);
        let scale = window.scale_factor();
        let bar = bar_width(scale);
        let thumb_color = cx.theme().t_scrollbar;
        let state = handle.0.clone();

        // The children: all of them, or the rows near the viewport.
        let (children,) = match area.windowed {
            None => (area.children,),
            Some(windowed) => {
                let (top, visible) = {
                    let mut state = state.borrow_mut();
                    let now = cx.background_executor().now();
                    let top = state.animator.position(now) as f32;
                    let mut visible = f32::from(state.viewport.size.height);
                    if visible <= 0. {
                        visible = f32::from(window.viewport_size().height);
                    }
                    (top, visible)
                };
                // A frame's worth of travel either way: the position moves between render and
                // prepaint while an animation runs.
                let overscan = 0.5 * visible;
                let range = windowed.heights.range(
                    windowed.count,
                    px((top - overscan).max(0.)),
                    px(top + visible + overscan),
                );
                let rows = (windowed.render)(range.clone(), window, cx);
                let heights = windowed.heights.clone();
                let placed: Vec<AnyElement> = range
                    .zip(rows)
                    .map(|(index, row)| {
                        let top = heights.offset(index);
                        let height = heights.offset(index + 1) - top;
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(top)
                            .h(height)
                            .child(row)
                            .into_any_element()
                    })
                    .collect();
                let total = windowed.heights.offset(windowed.count);
                (
                    vec![div().relative().w_full().h(total).children(placed).into_any_element()],
                )
            }
        };

        let gutter = area.gutter_stable || state.borrow().overflowing;
        let keyboard = area.keyboard;

        area.viewport
            .relative()
            .overflow_hidden()
            .track_focus(&focus)
            .when(keyboard, |this| {
                let state = state.clone();
                let key_focus = focus.clone();
                this.on_key_down(move |event, window, cx| {
                    let keystroke = &event.keystroke;
                    let modifiers = keystroke.modifiers;
                    if modifiers.control || modifiers.alt || modifiers.platform {
                        return;
                    }
                    let mut state = state.borrow_mut();
                    let page = ScrollAnimator::page_step(f32::from(state.viewport.size.height) as f64);
                    let target = state.animator.target();
                    let delta = match (keystroke.key.as_str(), modifiers.shift) {
                        ("down", false) => LINE_STEP_PX,
                        ("up", false) => -LINE_STEP_PX,
                        ("pagedown", _) => page,
                        ("pageup", _) => -page,
                        // Space scrolls only when nothing inside has focus (a button takes it).
                        ("space", shift) if key_focus.is_focused(window) => {
                            if shift { -page } else { page }
                        }
                        ("home", false) => -target,
                        ("end", false) => state.max() - target,
                        _ => return,
                    };
                    let now = cx.background_executor().now();
                    state.animator.input(now, delta, ScrollKind::Key);
                    let view = state.view;
                    drop(state);
                    cx.stop_propagation();
                    if let Some(view) = view {
                        cx.notify(view);
                    }
                })
                .on_mouse_down(MouseButton::Left, {
                    let focus = focus.clone();
                    move |_, window, cx| {
                        if !focus.contains_focused(window, cx) {
                            focus.focus(window, cx);
                        }
                    }
                })
            })
            // First child: records the viewport, and listens for the wheel before anything
            // inside it registers (so a nested area hears it first).
            .child({
                let state = state.clone();
                canvas(
                    {
                        let state = state.clone();
                        move |bounds, window, _| {
                            state.borrow_mut().viewport = bounds;
                            window.insert_hitbox(bounds, HitboxBehavior::Normal)
                        }
                    },
                    move |_, hitbox, window, _| {
                        state.borrow_mut().view = Some(window.current_view());
                        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window)
                            {
                                return;
                            }
                            let mut state = state.borrow_mut();
                            let (delta, precise) = match event.delta {
                                ScrollDelta::Lines(lines) => (ScrollAnimator::lines_to_px(lines.y), false),
                                ScrollDelta::Pixels(pixels) => (-(f32::from(pixels.y) as f64), true),
                            };
                            let target = state.animator.target();
                            let can_move = (delta > 0. && target < state.max() - 0.5)
                                || (delta < 0. && target > 0.5);
                            if !can_move {
                                return;
                            }
                            let now = cx.background_executor().now();
                            if precise {
                                state.animator.precise(now, delta);
                            } else {
                                state.animator.input(now, delta, ScrollKind::Wheel);
                            }
                            let view = state.view;
                            drop(state);
                            cx.stop_propagation();
                            if let Some(view) = view {
                                cx.notify(view);
                            }
                        });
                    },
                )
                .absolute()
                .inset_0()
            })
            .child(ScrollContent {
                children,
                state: state.clone(),
                gutter: if gutter { px(bar) } else { px(0.) },
                scale,
                gutter_stable: area.gutter_stable,
            })
            .child(scrollbar(state, bar, scale, thumb_color))
    }
}

/// The content box: laid out at the viewport's width (less the bar while there is one) and its
/// own height, then moved up by the scroll position before it is prepainted.
struct ScrollContent {
    children: Vec<AnyElement>,
    state: Rc<RefCell<ScrollState>>,
    gutter: Pixels,
    scale: f32,
    gutter_stable: bool,
}

impl Element for ScrollContent {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let children: Vec<LayoutId> = self
            .children
            .iter_mut()
            .map(|child| child.request_layout(window, cx))
            .collect();
        let mut style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            ..Style::default()
        };
        style.padding.right = self.gutter.into();
        (window.request_layout(style, children, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let position = {
            let mut state = self.state.borrow_mut();
            let visible = f32::from(state.viewport.size.height);
            let content = f32::from(bounds.size.height);
            state.animator.set_max_offset((content - visible).max(0.) as f64);
            if let Some(request) = state.pending.take() {
                state.apply(request);
            }
            let now = cx.background_executor().now();
            let exact = state.animator.position(now);
            // Drawn on whole device pixels, as the compositor does.
            let position = (exact * self.scale as f64).round() / self.scale as f64;
            state.drawn = position;
            state.drawn_exact = exact;
            state.drawn_at = Some(now);
            state.content_height = bounds.size.height;
            let overflowing = content > visible + 0.5;
            if overflowing != state.overflowing {
                state.overflowing = overflowing;
                if !self.gutter_stable {
                    // The bar takes (or gives back) its width: lay the content out again.
                    window.request_animation_frame();
                }
            }
            if state.animator.is_animating() {
                window.request_animation_frame();
            }
            position
        };
        window.with_element_offset(point(px(0.), px(-position as f32)), |window| {
            for child in &mut self.children {
                child.prepaint(window, cx);
            }
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        for child in &mut self.children {
            child.paint(window, cx);
        }
    }
}

/// The geometry of the bar in a viewport, all in px: the track (between the arrow zones), the
/// thumb's top and length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BarGeometry {
    pub track_top: f32,
    pub track_len: f32,
    pub thumb_top: f32,
    pub thumb_len: f32,
}

impl BarGeometry {
    /// `height`: the viewport's; `content`: the content's; `position`: scrollTop; `bar`: the bar's
    /// thickness (the arrow zones are square).
    pub fn new(height: f32, content: f32, position: f32, bar: f32) -> Self {
        let track_len = (height - 2. * bar).max(0.);
        let thumb_len = (track_len * height / content).round().max(MIN_THUMB).min(track_len);
        let max = (content - height).max(0.);
        let travel = track_len - thumb_len;
        let thumb_offset = if max > 0. {
            (travel * position / max).round()
        } else {
            0.
        };
        Self {
            track_top: bar,
            track_len,
            thumb_top: bar + thumb_offset,
            thumb_len,
        }
    }

    /// The scroll position that puts the thumb's top at `thumb_top`.
    pub fn position_for(&self, thumb_top: f32, max: f32) -> f32 {
        let travel = self.track_len - self.thumb_len;
        if travel <= 0. {
            return 0.;
        }
        ((thumb_top - self.track_top) / travel).clamp(0., 1.) * max
    }
}

/// What a press on the bar hit.
#[derive(Debug, Clone, Copy, PartialEq)]
enum BarPart {
    Up,
    Down,
    Thumb,
    TrackAbove,
    TrackBelow,
}

fn scrollbar(
    state: Rc<RefCell<ScrollState>>,
    bar: f32,
    scale: f32,
    color: gpui::Hsla,
) -> impl IntoElement {
    canvas(
        {
            let state = state.clone();
            move |bounds, window, _| {
                state
                    .borrow()
                    .overflowing
                    .then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal))
            }
        },
        move |bounds, hitbox, window, _| {
            let Some(hitbox) = hitbox else {
                return;
            };
            let (geometry, max) = {
                let state = state.borrow();
                let height = f32::from(bounds.size.height);
                (
                    BarGeometry::new(
                        height,
                        f32::from(state.content_height),
                        state.drawn as f32,
                        bar,
                    ),
                    state.max() as f32,
                )
            };
            let device = |value: f32| px(value / scale);
            let thickness = (bar * scale).round();
            // The thumb: the bar less 2 device px a side, square ends.
            window.paint_quad(fill(
                Bounds {
                    origin: point(bounds.left() + device(2.), bounds.top() + px(geometry.thumb_top)),
                    size: size(device(thickness - 4.), px(geometry.thumb_len)),
                },
                color,
            ));
            // The arrows: rows of 2, 4, 6 … device px (1, 3, 5 … for an odd bar), centred.
            let rows = ((thickness + 1.) / 4.).floor();
            let first_row = ((thickness - rows + 1.) / 2.).ceil();
            let first_width = if thickness as i32 % 2 == 0 { 2. } else { 1. };
            for row in 0..rows as i32 {
                let width = first_width + 2. * row as f32;
                let x = bounds.left() + device((thickness - width) / 2.);
                let up_y = bounds.top() + device(first_row + row as f32);
                let down_y = bounds.bottom() - device(first_row + row as f32 + 1.);
                for y in [up_y, down_y] {
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(x, y),
                            size: size(device(width), device(1.)),
                        },
                        color,
                    ));
                }
            }

            let part_at = move |position: Point<Pixels>| {
                let y = f32::from(position.y - bounds.top());
                if y < geometry.track_top {
                    BarPart::Up
                } else if y >= geometry.track_top + geometry.track_len {
                    BarPart::Down
                } else if y < geometry.thumb_top {
                    BarPart::TrackAbove
                } else if y < geometry.thumb_top + geometry.thumb_len {
                    BarPart::Thumb
                } else {
                    BarPart::TrackBelow
                }
            };

            {
                let state = state.clone();
                let hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.button != MouseButton::Left
                        || !hitbox.is_hovered(window)
                    {
                        return;
                    }
                    cx.stop_propagation();
                    window.capture_pointer(hitbox.id);
                    let part = part_at(event.position);
                    if part == BarPart::Thumb {
                        let grab = f32::from(event.position.y - bounds.top()) - geometry.thumb_top;
                        state.borrow_mut().drag = Some(grab);
                        return;
                    }
                    step(&state, part, f32::from(bounds.size.height), cx);
                    let task = window.spawn(cx, {
                        let state = state.clone();
                        let pointer_y = f32::from(event.position.y - bounds.top());
                        async move |cx| {
                            cx.background_executor().timer(REPEAT_DELAY).await;
                            loop {
                                let keep_going = cx
                                    .update(|_, cx| {
                                        // The track repeats until the thumb reaches the pointer.
                                        let (thumb_top, thumb_len) = {
                                            let state = state.borrow();
                                            let target = BarGeometry::new(
                                                f32::from(bounds.size.height),
                                                f32::from(state.content_height),
                                                state.animator.target() as f32,
                                                bar,
                                            );
                                            (target.thumb_top, target.thumb_len)
                                        };
                                        let reached = match part {
                                            BarPart::TrackAbove => pointer_y >= thumb_top,
                                            BarPart::TrackBelow => pointer_y < thumb_top + thumb_len,
                                            _ => false,
                                        };
                                        if !reached {
                                            step(&state, part, f32::from(bounds.size.height), cx);
                                        }
                                        !reached
                                    })
                                    .unwrap_or(false);
                                if !keep_going {
                                    break;
                                }
                                cx.background_executor().timer(REPEAT_EVERY).await;
                            }
                        }
                    });
                    state.borrow_mut().repeat = Some(task);
                });
            }
            {
                let state = state.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let mut state = state.borrow_mut();
                    let Some(grab) = state.drag else {
                        return;
                    };
                    let thumb_top = f32::from(event.position.y - bounds.top()) - grab;
                    let position = geometry.position_for(thumb_top, max);
                    if (position as f64 - state.drawn).abs() >= 0.5 / scale as f64 {
                        state.animator.jump_to(position as f64);
                        if let Some(view) = state.view {
                            cx.notify(view);
                        }
                    }
                });
            }
            {
                let state = state.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, window, _| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let mut state = state.borrow_mut();
                    if state.drag.take().is_some() || state.repeat.take().is_some() {
                        window.release_pointer();
                    }
                });
            }
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .right_0()
    .w(px(bar))
}

/// One arrow (a line, 40 px) or track (a page) step, animated like a key.
fn step(state: &Rc<RefCell<ScrollState>>, part: BarPart, height: f32, cx: &mut App) {
    let delta = match part {
        BarPart::Up => -LINE_STEP_PX,
        BarPart::Down => LINE_STEP_PX,
        BarPart::TrackAbove => -ScrollAnimator::page_step(height as f64),
        BarPart::TrackBelow => ScrollAnimator::page_step(height as f64),
        BarPart::Thumb => return,
    };
    let now = cx.background_executor().now();
    let mut state = state.borrow_mut();
    state.animator.input(now, delta, ScrollKind::Key);
    if let Some(view) = state.view {
        cx.notify(view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_matches_chromium() {
        // tools/electron-captures kit-scroll, box A: 400 px tall, 40 rows of 32 px, scrollTop 200.
        let a = BarGeometry::new(400., 1280., 200., 10.);
        assert_eq!(a.track_len, 380.);
        assert_eq!(a.thumb_len, 119.);
        assert_eq!(a.thumb_top, 69.);
        // At the top and at the bottom (boxes D and C).
        assert_eq!(BarGeometry::new(400., 1280., 0., 10.).thumb_top, 10.);
        let bottom = BarGeometry::new(400., 1280., 880., 10.);
        assert_eq!(bottom.thumb_top + bottom.thumb_len, 390.);
        // Box F: 24 000 px of content keeps a 20 px thumb.
        assert_eq!(BarGeometry::new(400., 24000., 0., 10.).thumb_len, 20.);
        // Dragging back: the thumb's top maps to the position.
        assert!((a.position_for(69., 880.) - 200.).abs() < 2.);
        assert_eq!(bar_width(1.), 10.);
        assert_eq!(bar_width(1.25) * 1.25, 12.);
    }

    #[test]
    fn rows_in_a_range() {
        let uniform = RowHeights::Uniform(px(10.));
        assert_eq!(uniform.range(100, px(25.), px(55.)), 2..6);
        assert_eq!(uniform.range(3, px(25.), px(55.)), 2..3);
        let offsets: Rc<[Pixels]> = [0., 10., 30., 60., 100.].map(px).into();
        let varied = RowHeights::Offsets(offsets);
        assert_eq!(varied.range(4, px(15.), px(35.)), 1..3);
        assert_eq!(varied.range(4, px(0.), px(10.)), 0..1);
        assert_eq!(varied.offset(4), px(100.));
    }
}
