//! Layers: deferred elements (modals, popovers, select menus, context menus, tooltips) drawn in the
//! right order however they nest.
//!
//! GPUI paints every deferred draw of a frame sorted by priority alone, whatever it was opened
//! inside: a select menu (priority 3) opened in a modal (10) was painted *under* the modal, and
//! only callers that remembered to raise it got it on top. [`layer`] derives it instead: a
//! deferred element built while another layer is being laid out or painted gets that layer's
//! priority added to its own, so a popover opened from inside a modal sits above the modal, a
//! tooltip in a popover in a modal above the popover, and a modal opened from a modal above both.
//!
//! The scope is a thread-local stack that [`Scope`] (the child of every layer) pushes while its
//! content is laid out, prepainted and painted: the content is where the nested layers are built
//! (a component's `render` runs during its parent's layout; a cached view inside renders during
//! its parent's prepaint), so they read the right enclosing priority with no context passed
//! around. A cached view replayed from its cache keeps the priorities it was drawn with.
//!
//! Base priorities (top level): composer panels 1–2, popovers 2, select menus 3, context menus 4,
//! tooltips 5, the mini-player 5, the incoming call 7, modals 10, the channel drag layer 20, toasts
//! [`TOAST_PRIORITY`] (above any nesting).

use std::cell::RefCell;

use gpui::{
    AnyElement, App, Bounds, Deferred, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Window, deferred,
};

thread_local! {
    static STACK: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Toasts: above every layer, however deep.
pub const TOAST_PRIORITY: usize = 1000;

/// The priority of the layer being laid out or painted now (0: none, the window's own content).
pub fn current() -> usize {
    STACK.with(|s| s.borrow().last().copied().unwrap_or(0))
}

/// The priority a layer whose own (top-level) priority is `own` gets here: `own` at the top
/// level, the enclosing layer's plus `own` inside one (so its relative order among siblings in
/// that layer stays as at the top level).
pub fn priority(own: usize) -> usize {
    current() + own
}

/// `deferred(child).with_priority(own)`, drawn above the layer it is built in (see the module
/// doc). Use it for every overlay element instead of GPUI's `deferred`.
pub fn layer(own: usize, child: impl IntoElement) -> Deferred {
    let priority = priority(own);
    let chrome = crate::theme::in_chrome();
    deferred(Scope { priority, chrome, child: Some(child.into_any_element()) }).with_priority(priority)
}

/// A layer's content: while it is laid out, prepainted and painted, layers built inside it stack
/// on its priority, and a layer built inside [`crate::chrome_scope`] keeps the chrome's palette
/// for its deferred prepaint and paint.
pub struct Scope {
    priority: usize,
    chrome: bool,
    child: Option<AnyElement>,
}

struct Pushed;

impl Pushed {
    fn new(priority: usize) -> Self {
        STACK.with(|s| s.borrow_mut().push(priority));
        Pushed
    }
}

impl Drop for Pushed {
    fn drop(&mut self) {
        STACK.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

impl Element for Scope {
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
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let _scope = Pushed::new(self.priority);
        let _chrome = self.chrome.then(crate::theme::InChrome::enter);
        let layout_id = self.child.as_mut().expect("a layer's content").request_layout(window, cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let _scope = Pushed::new(self.priority);
        let _chrome = self.chrome.then(crate::theme::InChrome::enter);
        if let Some(child) = &mut self.child {
            child.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let _scope = Pushed::new(self.priority);
        let _chrome = self.chrome.then(crate::theme::InChrome::enter);
        if let Some(child) = &mut self.child {
            child.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_layers_stack_on_their_parent() {
        assert_eq!(priority(3), 3);
        let _modal = Pushed::new(10);
        assert_eq!(priority(3), 13);
        {
            let _popover = Pushed::new(priority(2));
            assert_eq!(priority(5), 17, "a tooltip in a popover in a modal");
        }
        assert_eq!(priority(10), 20, "a modal opened from a modal");
        drop(_modal);
        assert_eq!(current(), 0);
    }
}
