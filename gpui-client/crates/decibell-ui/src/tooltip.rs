//! Tooltips. The renderer has one custom tooltip, the settings rail's labels (`SettingsNavItem`:
//! beside the icon on hover or keyboard focus, fading over 150 ms); everything else is a native
//! `title=` tooltip, which [`title_tooltip`] stands in for through GPUI's own delayed tooltip.

use gpui::{
    AnyView, App, AppContext as _, Bounds, Context, ElementId, IntoElement, Pixels, Render,
    RenderOnce, SharedString, Window, div, point, prelude::*, px,
};

use crate::motion::{self, Transition};
use crate::popover::{Placed, Placement};
use crate::{ActiveTheme as _, Label};

/// A label beside an element: `whitespace-nowrap rounded-md border border-border bg-bg-darkest
/// px-2.5 py-1.5 text-[13px] font-medium text-text-primary shadow-float`, 16 px to the right
/// of `anchor` (the element's bounds) and centred on it, fading in and out with `transition-opacity`.
/// Render it every frame with `shown` (it draws nothing once faded out).
///
/// ```ignore
/// Tooltip::new(("rail-label", index), label, item_bounds).shown(hovered || focus_visible)
/// ```
#[derive(IntoElement)]
pub struct Tooltip {
    id: ElementId,
    label: SharedString,
    anchor: Bounds<Pixels>,
    shown: bool,
}

/// The label's height: `py-1.5` around a 13 px line.
const HEIGHT: f32 = 12. + 19.5 + 2.;

impl Tooltip {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, anchor: Bounds<Pixels>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            anchor,
            shown: false,
        }
    }

    pub fn shown(mut self, shown: bool) -> Self {
        self.shown = shown;
        self
    }
}

impl RenderOnce for Tooltip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = motion::transition_flag(window, cx, (self.id.clone(), "shown"), self.shown, Transition::TAILWIND);
        if t <= 0. {
            return div().into_any_element();
        }
        let theme = cx.theme();
        let origin = point(
            self.anchor.right() + px(16.),
            (self.anchor.center().y - px(HEIGHT / 2.)).round(),
        );
        crate::layer::layer(
            5,
            Placed::new(Placement::At(origin)).child(
                div()
                    .flex_none()
                    .rounded(theme.radius_md)
                    .border_1()
                    .border_color(theme.color_border)
                    .bg(theme.color_bg_darkest)
                    .px_2p5()
                    .py_1p5()
                    .shadow(theme.shadow_float.clone())
                    .opacity(t)
                    .child(
                        Label::new(self.label)
                            .size(13.)
                            .weight(500.)
                            .color(theme.color_text_primary),
                    ),
            ),
        )
        .into_any_element()
    }
}

/// The view behind [`title_tooltip`].
pub struct TitleTooltip {
    text: SharedString,
}

impl Render for TitleTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .rounded(theme.radius_sm)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_darkest)
            .px_2()
            .py_1()
            .shadow(theme.shadow_float.clone())
            .child(Label::new(self.text.clone()).size(12.).color(theme.color_text_primary))
    }
}

/// A `title=` tooltip for GPUI's `.tooltip(…)` on an element with an id (shown after 500 ms at
/// the pointer, one at a time). Chromium draws `title` tooltips natively; this is themed instead.
///
/// ```ignore
/// div().id("mute").tooltip(title_tooltip("Mute"))
/// ```
pub fn title_tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| {
        cx.new(|_| TitleTooltip { text: text.clone() }).into()
    }
}
