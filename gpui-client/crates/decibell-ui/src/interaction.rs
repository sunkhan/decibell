//! What the controls share: a focus handle that lives as long as the control is drawn, the
//! hover state behind a `transition-colors`, and the focus ring.

use gpui::{App, BoxShadow, ElementId, Entity, FocusHandle, Window};

use crate::motion::{self, Transition};

/// A focus handle kept in the element state of `id` (one per control, made the first time it
/// is drawn), in the tab order unless `tab_stop` is false.
pub(crate) fn focus_handle(
    window: &mut Window,
    cx: &mut App,
    id: impl Into<ElementId>,
    tab_stop: bool,
) -> FocusHandle {
    window
        .use_keyed_state(id, cx, |_, cx| cx.focus_handle().tab_stop(tab_stop))
        .read(cx)
        .clone()
}

/// Whether the pointer is over a control, for controls whose hover colours transition
/// (`transition-colors`): GPUI's `.hover()` styles switch at once, so the state is kept here and
/// fed to a [`motion::transition`].
pub(crate) struct Hover {
    pub state: Entity<bool>,
    /// 0 → 1 as the pointer enters, back as it leaves.
    pub progress: f32,
}

impl Hover {
    /// `transition`: `None` switches at once (a control without `transition-colors`).
    pub fn new(
        window: &mut Window,
        cx: &mut App,
        id: impl Into<ElementId> + Clone,
        transition: Option<Transition>,
    ) -> Self {
        let id: ElementId = id.into();
        let state = window.use_keyed_state((id.clone(), "hover"), cx, |_, _| false);
        let hovered = *state.read(cx);
        let progress = match transition {
            Some(spec) => motion::transition_flag(window, cx, (id, "hover-t"), hovered, spec),
            None => f32::from(u8::from(hovered)),
        };
        Self { state, progress }
    }

    /// The `on_hover` listener that keeps the state.
    pub fn listener(&self) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        let state = self.state.clone();
        move |hovered, _, cx| {
            state.update(cx, |state, cx| {
                if *state != *hovered {
                    *state = *hovered;
                    cx.notify();
                }
            })
        }
    }
}

/// `focus-visible:shadow-ring`: the theme's focus ring.
pub(crate) fn ring(cx: &App) -> Vec<BoxShadow> {
    crate::ActiveTheme::theme(cx).shadow_ring.clone()
}
