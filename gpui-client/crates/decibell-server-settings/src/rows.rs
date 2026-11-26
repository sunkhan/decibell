//! Long lists inside the settings pane, and the pane's sticky parts.
//!
//! The settings window has one scroll area (the pane: header, tab, save bar). A list in a tab
//! (members, bans, the audit log, roles) scrolls with it, so the kit's `ScrollArea::windowed`
//! (which windows its own scroll) doesn't apply: [`PaneRows`] takes the pane's [`ScrollHandle`]
//! and builds only the rows that meet the pane's viewport, laying each out at its place. Its
//! height is the whole list's, so the scrollbar and everything under it are where they would be
//! with every row built. Heights are uniform, or measured once per width ([`Measured`]).
//!
//! [`StickyTop`] is `position: sticky; top: 0` inside the pane: the Members and Audit Log
//! toolbars stay put while their lists scroll under them.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use decibell_ui::ScrollHandle;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, ParentElement as _, Pixels, Style, Styled as _, Window, div, point, px, size,
};

type RenderRows = Box<dyn Fn(Range<usize>, &mut Window, &mut App) -> Vec<AnyElement> + 'static>;

/// Heights measured at one width: the rows of a list whose rows wrap (the audit log).
#[derive(Default)]
pub struct Measured {
    width: Pixels,
    /// Per row; `None` until measured at `width`.
    heights: Vec<Option<Pixels>>,
    estimate: Pixels,
}

impl Measured {
    pub fn new(estimate: Pixels) -> Rc<RefCell<Measured>> {
        Rc::new(RefCell::new(Measured { width: px(0.), heights: Vec::new(), estimate }))
    }

    /// The list changed from row `from` on (rows were added or replaced): those are measured
    /// again.
    pub fn invalidate_from(&mut self, from: usize) {
        self.heights.truncate(from);
    }

    fn offsets(&self, count: usize) -> Vec<Pixels> {
        let mut offsets = Vec::with_capacity(count + 1);
        let mut top = px(0.);
        offsets.push(top);
        for i in 0..count {
            top += self.heights.get(i).copied().flatten().unwrap_or(self.estimate);
            offsets.push(top);
        }
        offsets
    }
}

enum Heights {
    Uniform(Pixels),
    Measured(Rc<RefCell<Measured>>),
}

/// The rows of a list that meet the pane's viewport (see the module doc).
pub struct PaneRows {
    scroll: ScrollHandle,
    count: usize,
    heights: Heights,
    render: RenderRows,
    built: Vec<AnyElement>,
}

impl PaneRows {
    /// Every row `height` tall.
    pub fn uniform(
        scroll: &ScrollHandle,
        count: usize,
        height: Pixels,
        render: impl Fn(Range<usize>, &mut Window, &mut App) -> Vec<AnyElement> + 'static,
    ) -> Self {
        PaneRows { scroll: scroll.clone(), count, heights: Heights::Uniform(height), render: Box::new(render), built: Vec::new() }
    }

    /// Rows as tall as they lay out at the list's width (measured once per width, kept in
    /// `cache`).
    pub fn measured(
        scroll: &ScrollHandle,
        count: usize,
        cache: &Rc<RefCell<Measured>>,
        render: impl Fn(Range<usize>, &mut Window, &mut App) -> Vec<AnyElement> + 'static,
    ) -> Self {
        PaneRows { scroll: scroll.clone(), count, heights: Heights::Measured(cache.clone()), render: Box::new(render), built: Vec::new() }
    }

    fn offsets(&self) -> Vec<Pixels> {
        match &self.heights {
            Heights::Uniform(h) => (0..=self.count).map(|i| *h * i as f32).collect(),
            Heights::Measured(cache) => cache.borrow().offsets(self.count),
        }
    }
}

impl Element for PaneRows {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let total = *self.offsets().last().unwrap_or(&px(0.));
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = total.into();
        style.flex_shrink = 0.;
        (window.request_layout(style, [], cx), ())
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
        // Measure what isn't measured at this width, then lay out again if that moved anything.
        if let Heights::Measured(cache) = &self.heights {
            let stale = {
                let c = cache.borrow();
                c.width != bounds.size.width || c.heights.len() < self.count || c.heights.iter().take(self.count).any(Option::is_none)
            };
            if stale {
                let before = cache.borrow().offsets(self.count);
                let from = {
                    let mut c = cache.borrow_mut();
                    if c.width != bounds.size.width {
                        c.width = bounds.size.width;
                        c.heights.clear();
                    }
                    let from = c.heights.iter().position(Option::is_none).unwrap_or(c.heights.len());
                    c.heights.resize(self.count, None);
                    from
                };
                let rows = (self.render)(from..self.count, window, cx);
                let space = size(AvailableSpace::Definite(bounds.size.width), AvailableSpace::MinContent);
                let mut measured = Vec::with_capacity(rows.len());
                for row in rows {
                    // As a layout root an element with an auto width shrinks to its content: pin
                    // each row to the pane's width.
                    let mut row = div().w(bounds.size.width).child(row).into_any_element();
                    measured.push(row.layout_as_root(space, window, cx).height);
                }
                let mut c = cache.borrow_mut();
                for (i, h) in measured.into_iter().enumerate() {
                    if let Some(slot) = c.heights.get_mut(from + i) {
                        *slot = Some(h);
                    }
                }
                if c.offsets(self.count) != before {
                    // The list's height changed: lay the pane out again next frame (this one
                    // paints at the new offsets already).
                    window.request_animation_frame();
                }
            }
        }
        let offsets = self.offsets();
        let viewport = self.scroll.viewport_bounds();
        let (view_top, view_bottom) = if viewport.size.height > px(0.) {
            (viewport.top(), viewport.bottom())
        } else {
            (px(0.), window.viewport_size().height)
        };
        // Half a screen either way: the position moves between frames of a glide.
        let overscan = (view_bottom - view_top) * 0.5;
        let top = view_top - overscan - bounds.top();
        let bottom = view_bottom + overscan - bounds.top();
        let first = offsets.partition_point(|o| *o <= top).saturating_sub(1).min(self.count);
        let last = offsets.partition_point(|o| *o < bottom).min(self.count);
        if first >= last {
            self.built.clear();
            return;
        }
        let rows = (self.render)(first..last, window, cx);
        self.built = Vec::with_capacity(rows.len());
        for (index, row) in (first..last).zip(rows) {
            let height = offsets[index + 1] - offsets[index];
            let mut row = div().w(bounds.size.width).h(height).child(row).into_any_element();
            let space = size(AvailableSpace::Definite(bounds.size.width), AvailableSpace::Definite(height));
            row.layout_as_root(space, window, cx);
            row.prepaint_at(bounds.origin + point(px(0.), offsets[index]), window, cx);
            self.built.push(row);
        }
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
        for row in &mut self.built {
            row.paint(window, cx);
        }
    }
}

/// `position: sticky; top: 0` inside the pane for an absolutely placed child that comes after
/// what scrolls under it (so it paints on top): drawn no higher than the pane's top while the
/// tab is in view. Give its parent `relative()` and the content under it the child's height as
/// top padding.
pub struct StickyTop {
    scroll: ScrollHandle,
    child: AnyElement,
}

impl StickyTop {
    pub fn new(scroll: &ScrollHandle, child: impl IntoElement) -> Self {
        StickyTop { scroll: scroll.clone(), child: child.into_any_element() }
    }
}

impl Element for StickyTop {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let child = self.child.request_layout(window, cx);
        let mut style = Style { position: gpui::Position::Absolute, ..Default::default() };
        style.inset.top = px(0.).into();
        style.inset.left = px(0.).into();
        style.inset.right = px(0.).into();
        (window.request_layout(style, [child], cx), ())
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
        let viewport = self.scroll.viewport_bounds();
        let lift = if viewport.size.height > px(0.) && bounds.top() < viewport.top() { viewport.top() - bounds.top() } else { px(0.) };
        window.with_element_offset(point(px(0.), lift), |window| self.child.prepaint(window, cx));
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
        self.child.paint(window, cx);
    }
}
