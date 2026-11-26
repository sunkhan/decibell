use decibell_ui::{ActiveTheme as _, FontRole, TitleBar, window_frame};
use gpui::{AnyView, Context, IntoElement, Render, StyleRefinement, Window, div, prelude::*, px};

/// The root view of a window: the window frame, the title bar, and one content view under it
/// (a registered screen today, the app shell later).
///
/// The content is drawn as a cached view: a frame caused by the title bar (a hovered button)
/// replays the content's last paint instead of rendering it again.
pub struct Root {
    content: AnyView,
}

impl Root {
    pub fn new(content: AnyView) -> Self {
        Self { content }
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let content = div()
            .size_full()
            .flex()
            .flex_col()
            // What text inherits unless a Label says otherwise (globals.css `body`).
            .font_family(theme.font(FontRole::Sans))
            .text_size(px(theme.scaled.text_body))
            .text_color(theme.color_text_primary)
            .child(TitleBar::new("Decibell"))
            .child(
                div().flex_1().min_h_0().child(
                    self.content
                        .clone()
                        .cached(StyleRefinement::default().size_full()),
                ),
            );
        window_frame(content, window, cx)
    }
}
