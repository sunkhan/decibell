//! The profile popup's placement (`UserProfilePopup.tsx`): unlike a menu it never flips. Its left
//! edge is the anchor's x, pulled in so the 300 px card stays 16 px inside the window's right
//! edge; its top is the anchor's y once measured, clamped to 16 px inside the window
//! (`Math.max(16, Math.min(anchor.y, innerHeight − height − 16))`). The card is measured in the
//! frame it opens, so there is no first frame at the unclamped place.

use decibell_ui::visible_window_bounds;
use gpui::{
    AnyElement, App, Bounds, Display, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId,
    ParentElement, Pixels, Point, Position, Style, Window, point, px,
};

/// The popup's margin from the window's edges.
const EDGE: f32 = 16.;

pub struct Clamped {
    anchor: Point<Pixels>,
    children: Vec<AnyElement>,
}

impl Clamped {
    pub fn new(anchor: Point<Pixels>) -> Self {
        Self { anchor, children: Vec::new() }
    }
}

/// Where a card of `size` opened at `anchor` goes inside `viewport` (window coordinates).
pub fn clamp_popup(anchor: Point<Pixels>, size: gpui::Size<Pixels>, viewport: Bounds<Pixels>) -> Point<Pixels> {
    let (x, y) = (f32::from(anchor.x - viewport.origin.x), f32::from(anchor.y - viewport.origin.y));
    let (vw, vh) = (f32::from(viewport.size.width), f32::from(viewport.size.height));
    let (w, h) = (f32::from(size.width), f32::from(size.height));
    let left = x.min(vw - w - EDGE);
    let top = EDGE.max(y.min(vh - h - EDGE));
    point(viewport.origin.x + px(left.round()), viewport.origin.y + px(top.round()))
}

impl ParentElement for Clamped {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

impl Element for Clamped {
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
        let children: Vec<LayoutId> = self.children.iter_mut().map(|child| child.request_layout(window, cx)).collect();
        let style = Style { position: Position::Absolute, display: Display::Flex, ..Style::default() };
        (window.request_layout(style, children.iter().copied(), cx), children)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        children: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(child) = children.iter().map(|id| window.layout_bounds(*id)).reduce(|a, b| a.union(&b)) else {
            return;
        };
        let origin = clamp_popup(self.anchor, child.size, visible_window_bounds(window, cx));
        let offset = origin - child.origin;
        window.with_element_offset(point(offset.x.round(), offset.y.round()), |window| {
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

#[cfg(test)]
mod tests {
    use gpui::size;

    use super::*;

    #[test]
    fn the_popup_stays_inside_without_flipping() {
        let viewport = Bounds { origin: point(px(10.), px(10.)), size: size(px(1280.), px(768.)) };
        let card = size(px(300.), px(400.));
        // Room everywhere: at the anchor.
        assert_eq!(clamp_popup(point(px(110.), px(110.)), card, viewport), point(px(110.), px(110.)));
        // Near the right edge: pulled left to 16 px inside, not flipped to the anchor's left.
        assert_eq!(clamp_popup(point(px(1200.), px(110.)), card, viewport).x, px(10. + 1280. - 300. - 16.));
        // Near the bottom: pulled up so the whole card shows, 16 px inside.
        assert_eq!(clamp_popup(point(px(110.), px(700.)), card, viewport).y, px(10. + 768. - 400. - 16.));
        // Taller than the window: pinned 16 px from the top.
        assert_eq!(clamp_popup(point(px(110.), px(700.)), size(px(300.), px(900.)), viewport).y, px(26.));
    }
}
