//! Floating layers: where a menu goes ([`place_menu`], the renderer's `useMenuPosition`), an
//! element that puts its child there once it knows the child's size ([`Placed`]), and
//! [`Popover`], a dismissable panel at a point.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, Decorations, Display, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, ParentElement, Pixels, Point, Position,
    RenderOnce, Size, Style, Window, deferred, div, point, prelude::*, px, size,
};

use crate::window_frame::decorations;
use crate::{ActiveTheme as _, motion};

/// Which side of the anchor a menu opens on when both fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    /// The menu's top-left corner at the anchor (a right-click menu at the cursor).
    #[default]
    Below,
    /// The menu's bottom-left corner at the anchor (a menu over a control at the window's foot).
    Above,
}

/// The gap a menu keeps from every edge of the window (`useMenuPosition`'s default).
pub const MENU_MARGIN: Pixels = px(8.);

/// The renderer's `placeMenu`: the top-left corner for a menu of `menu` size at `anchor`, inside
/// `viewport` (window coordinates): on the preferred side when it fits there, flipped to the other
/// side of the anchor when only that fits, else slid along the edge; the same left / right. Whole px.
pub fn place_menu(
    anchor: Point<Pixels>,
    menu: Size<Pixels>,
    prefer: Side,
    margin: Pixels,
    viewport: Bounds<Pixels>,
) -> Point<Pixels> {
    let (x, y) = (
        f32::from(anchor.x - viewport.origin.x),
        f32::from(anchor.y - viewport.origin.y),
    );
    let (vw, vh) = (
        f32::from(viewport.size.width),
        f32::from(viewport.size.height),
    );
    let (width, height, margin) = (
        f32::from(menu.width),
        f32::from(menu.height),
        f32::from(margin),
    );

    let fits_below = y + height <= vh - margin;
    let fits_above = y - height >= margin;
    let clamped_top = margin.max(y.min(vh - height - margin));
    let top = match prefer {
        Side::Below if fits_below => y,
        Side::Below if fits_above => y - height,
        Side::Above if fits_above => y - height,
        Side::Above if fits_below => y,
        _ => clamped_top,
    };
    let fits_right = x + width <= vw - margin;
    let fits_left = x - width >= margin;
    let clamped_left = margin.max(x.min(vw - width - margin));
    let left = if fits_right {
        x
    } else if fits_left {
        x - width
    } else {
        clamped_left
    };
    // JavaScript's Math.round: halves go up.
    let round = |value: f32| (value + 0.5).floor();
    point(
        viewport.origin.x + px(round(left)),
        viewport.origin.y + px(round(top)),
    )
}

/// The visible part of the window in window coordinates: the surface minus the transparent
/// margin a floating client-decorated window draws its shadow in (CONVENTIONS gotcha 7).
pub fn visible_window_bounds(window: &Window, cx: &App) -> Bounds<Pixels> {
    let viewport = window.viewport_size();
    let inset = window.client_inset().unwrap_or(px(0.));
    let (top, bottom, left, right) = match decorations(window, cx) {
        Decorations::Client { tiling } => (
            if tiling.top { px(0.) } else { inset },
            if tiling.bottom { px(0.) } else { inset },
            if tiling.left { px(0.) } else { inset },
            if tiling.right { px(0.) } else { inset },
        ),
        Decorations::Server => (px(0.), px(0.), px(0.), px(0.)),
    };
    Bounds {
        origin: point(left, top),
        size: size(
            (viewport.width - left - right).max(px(0.)),
            (viewport.height - top - bottom).max(px(0.)),
        ),
    }
}

/// Where a [`Placed`] child goes.
#[derive(Debug, Clone, Copy)]
pub enum Placement {
    /// [`place_menu`] at a point.
    Menu { anchor: Point<Pixels>, prefer: Side },
    /// A fixed top-left corner in window coordinates.
    At(Point<Pixels>),
    /// Under (or over) the box `Placed` is laid out in (it fills its parent: the trigger),
    /// `gap` away, left-aligned and kept `MENU_MARGIN` inside the window (the Select's menu).
    Attached { above: bool, gap: Pixels },
}

/// Lays its children out at their own size, then moves them to `placement` in window
/// coordinates (an absolutely positioned layer: put it in `deferred` to draw it over everything).
/// The child's size is known before it is placed, so a menu is clamped in the frame it opens.
///
/// ```ignore
/// deferred(Placed::new(Placement::Menu { anchor, prefer: Side::Below }).child(menu)).with_priority(1)
/// ```
pub struct Placed {
    placement: Placement,
    children: Vec<AnyElement>,
}

impl Placed {
    pub fn new(placement: Placement) -> Self {
        Self {
            placement,
            children: Vec::new(),
        }
    }
}

impl ParentElement for Placed {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

impl Element for Placed {
    type RequestLayoutState = Vec<LayoutId>;
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
    ) -> (LayoutId, Self::RequestLayoutState) {
        let children: Vec<LayoutId> = self
            .children
            .iter_mut()
            .map(|child| child.request_layout(window, cx))
            .collect();
        let mut style = Style {
            position: Position::Absolute,
            display: Display::Flex,
            ..Style::default()
        };
        if matches!(self.placement, Placement::Attached { .. }) {
            // Fill the parent, so the element's bounds are the anchor's.
            style.inset = gpui::Edges::all(px(0.).into());
        }
        (
            window.request_layout(style, children.iter().copied(), cx),
            children,
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        children: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(child_bounds) = children
            .iter()
            .map(|id| window.layout_bounds(*id))
            .reduce(|a, b| a.union(&b))
        else {
            return;
        };
        let origin = match self.placement {
            Placement::Menu { anchor, prefer } => place_menu(
                anchor,
                child_bounds.size,
                prefer,
                MENU_MARGIN,
                visible_window_bounds(window, cx),
            ),
            Placement::At(origin) => origin,
            Placement::Attached { above, gap } => {
                let window_bounds = visible_window_bounds(window, cx);
                let left = bounds
                    .left()
                    .max(window_bounds.left() + MENU_MARGIN)
                    .min(window_bounds.right() - child_bounds.size.width - MENU_MARGIN);
                let top = if above {
                    bounds.top() - gap - child_bounds.size.height
                } else {
                    bounds.bottom() + gap
                };
                point(left, top)
            }
        };
        let offset = origin - child_bounds.origin;
        let offset = point(offset.x.round(), offset.y.round());
        window.with_element_offset(offset, |window| {
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
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for child in &mut self.children {
            child.paint(window, cx);
        }
    }
}

pub(crate) type DismissHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

/// What a menu or popover's `on_dismiss` receives: it went away (a press outside, Escape, the
/// window losing focus, or a row's action).
#[derive(Debug, Clone, Copy, Default)]
pub struct DismissEvent;

/// What every floating layer does to go away (the renderer's menus and popovers): a press
/// outside it, Escape (kept from whatever is underneath, such as a modal), and the window losing
/// focus. Focus moves into the layer as it opens and back to where it was as it closes.
pub(crate) struct Dismissal {
    focus: gpui::FocusHandle,
    dismiss: DismissHandler,
}

impl Dismissal {
    /// `id`: the layer's element id. Call while the layer is drawn (open).
    pub fn new(
        window: &mut Window,
        cx: &mut App,
        id: &ElementId,
        on_dismiss: DismissHandler,
    ) -> Self {
        let focus = crate::interaction::focus_handle(window, cx, (id.clone(), "layer-focus"), false);
        // Where focus was when the layer opened, and whether the window was active last frame.
        // Active as a self-test may hold it (`decibell_harness::focus`): other windows share the
        // desktop, and a layer would close whenever one took the focus.
        let opened = window.use_keyed_state((id.clone(), "layer-opened"), cx, |window, cx| {
            (window.focused(cx), decibell_harness::focus::window_active(window, cx), false)
        });
        let (restore, was_active, focused_once) = opened.read(cx).clone();
        let active = decibell_harness::focus::window_active(window, cx);
        let dismiss: DismissHandler = {
            let on_dismiss = on_dismiss.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                if let Some(restore) = &restore {
                    restore.focus(window, cx);
                }
                on_dismiss(window, cx)
            })
        };
        if !focused_once {
            let focus = focus.clone();
            opened.update(cx, |state, _| state.2 = true);
            window.defer(cx, move |window, cx| {
                if !focus.contains_focused(window, cx) {
                    focus.focus(window, cx)
                }
            });
        }
        if was_active && !active {
            let dismiss = dismiss.clone();
            window.defer(cx, move |window, cx| dismiss(window, cx));
        } else if was_active != active {
            opened.update(cx, |state, _| state.1 = active);
        }
        Self { focus, dismiss }
    }

    pub fn handler(&self) -> DismissHandler {
        self.dismiss.clone()
    }

    /// Wires the outside press and Escape onto the layer's root.
    pub fn attach<E: InteractiveElement>(&self, element: E) -> E {
        let outside = self.dismiss.clone();
        let escape = self.dismiss.clone();
        element
            .track_focus(&self.focus)
            .on_mouse_down_out(move |_, window, cx| outside(window, cx))
            .on_key_down(move |event, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    escape(window, cx);
                }
            })
    }
}

/// A floating panel at a point: `rounded-lg border border-border bg-bg-light shadow-float`,
/// entering with `dropIn` (0.15 s `ease`: from 4 px up and transparent), kept inside the window by
/// [`place_menu`], closed by a press outside, Escape or the window losing focus (`on_dismiss`).
/// Render it while it is open; the owner keeps the anchor (`anchorBelow(trigger)` is the
/// trigger's bottom-left + 4 px).
///
/// ```ignore
/// Popover::new("role-menu", anchor)
///     .on_dismiss(cx.listener(|this, _: &DismissEvent, _, cx| { this.menu = None; cx.notify() }))
///     .child(…)
/// ```
#[derive(IntoElement)]
pub struct Popover {
    id: ElementId,
    anchor: Point<Pixels>,
    prefer: Side,
    on_dismiss: Option<DismissHandler>,
    content: Vec<AnyElement>,
    priority: usize,
}

/// A modal's layer: GPUI paints deferred layers by priority alone, so a popover or a select's
/// menu opened inside a modal must sit above this to be drawn over it (see `over_modal`).
pub const MODAL_PRIORITY: usize = 10;

impl Popover {
    pub fn new(id: impl Into<ElementId>, anchor: Point<Pixels>) -> Self {
        Self {
            id: id.into(),
            anchor,
            prefer: Side::Below,
            on_dismiss: None,
            content: Vec::new(),
            priority: 2,
        }
    }

    /// Opened from inside a modal (a settings window): drawn over the modal's layer, which GPUI
    /// would otherwise paint on top of it.
    pub fn over_modal(mut self) -> Self {
        self.priority = MODAL_PRIORITY + 2;
        self
    }

    pub fn prefer(mut self, side: Side) -> Self {
        self.prefer = side;
        self
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&DismissEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(move |window, cx| handler(&DismissEvent, window, cx)));
        self
    }
}

impl ParentElement for Popover {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.content.extend(elements)
    }
}

impl RenderOnce for Popover {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let on_dismiss = self.on_dismiss.unwrap_or_else(|| Rc::new(|_, _| {}));
        let dismissal = Dismissal::new(window, cx, &self.id, on_dismiss);
        let theme = cx.theme();
        let max_height = visible_window_bounds(window, cx).size.height - MENU_MARGIN * 2.;
        let panel = div()
            .id(self.id.clone())
            .occlude()
            .flex()
            .flex_col()
            .max_h(max_height)
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_light)
            .shadow(theme.shadow_float.clone())
            .children(self.content);
        deferred(
            Placed::new(Placement::Menu {
                anchor: self.anchor,
                prefer: self.prefer,
            })
            .child(drop_in(self.id, dismissal.attach(panel))),
        )
        .with_priority(self.priority)
    }
}

/// `animate-[dropIn_0.15s_ease]`: from 4 px up and transparent. (The keyframe's `scale(0.97)`
/// is left out: GPUI cannot scale a box.)
pub(crate) fn drop_in<E: IntoElement + Styled + 'static>(id: ElementId, element: E) -> impl IntoElement {
    element.with_animation(
        (id, "drop-in"),
        Animation::new(Duration::from_millis(150)).with_easing(motion::EASE.easing()),
        |element, t| element.opacity(t).top(px(-4. * (1. - t))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(10.), px(10.)),
            size: size(px(800.), px(600.)),
        }
    }

    fn at(x: f32, y: f32) -> Point<Pixels> {
        point(px(x), px(y))
    }

    #[test]
    fn opens_where_it_fits() {
        let menu = size(px(200.), px(300.));
        // Plenty of room: top-left at the anchor.
        assert_eq!(
            place_menu(at(110., 110.), menu, Side::Below, MENU_MARGIN, viewport()),
            at(110., 110.)
        );
        // Near the bottom: flips above the anchor.
        assert_eq!(
            place_menu(at(110., 510.), menu, Side::Below, MENU_MARGIN, viewport()),
            at(110., 210.)
        );
        // Preferring above with room above.
        assert_eq!(
            place_menu(at(110., 510.), menu, Side::Above, MENU_MARGIN, viewport()),
            at(110., 210.)
        );
        // Preferring above at the top: below instead.
        assert_eq!(
            place_menu(at(110., 60.), menu, Side::Above, MENU_MARGIN, viewport()),
            at(110., 60.)
        );
        // Near the right edge: flips to the left of the anchor.
        assert_eq!(
            place_menu(at(750., 110.), menu, Side::Below, MENU_MARGIN, viewport()),
            at(550., 110.)
        );
    }

    #[test]
    fn slides_along_the_edge_when_neither_side_fits() {
        let tall = size(px(200.), px(500.));
        // 500 px fits neither below (from y 300) nor above: clamped to the bottom margin.
        assert_eq!(
            place_menu(at(110., 310.), tall, Side::Below, MENU_MARGIN, viewport()),
            at(110., 10. + 600. - 500. - 8.)
        );
        // Taller than the window: pinned to the top margin.
        let huge = size(px(200.), px(700.));
        assert_eq!(
            place_menu(at(110., 310.), huge, Side::Below, MENU_MARGIN, viewport()).y,
            px(18.)
        );
        // Rounds like Math.round.
        assert_eq!(
            place_menu(at(110.5, 110.5), size(px(200.), px(300.)), Side::Below, px(0.), viewport()),
            at(111., 111.)
        );
    }
}
