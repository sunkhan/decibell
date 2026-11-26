use decibell_ui::{ActiveTheme as _, FontRole, TitleBar, ToastStack, focus_navigation, window_frame};
use gpui::{AnyView, Context, IntoElement, Render, SharedString, StyleRefinement, Window, div, prelude::*, px};

/// The root view of a window: the window frame, the title bar, one content view under it (the
/// shell, or a registered screen) and the toast stack.
///
/// The content is drawn as a cached view: a frame caused by the title bar (a hovered button)
/// replays the content's last paint instead of rendering it again. The root reads the
/// navigation and the community list for the titles, so a change there redraws it (cheap: the
/// panes below are cached).
pub struct Root {
    content: AnyView,
    /// The OS window title last set (`useWindowTitle`).
    os_title: SharedString,
}

impl Root {
    pub fn new(content: AnyView) -> Self {
        Self { content, os_title: SharedString::default() }
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let os_title = decibell_shell::window_title(cx);
        if os_title != self.os_title {
            window.set_window_title(&os_title);
            self.os_title = os_title;
        }
        let title = decibell_shell::titlebar_title(cx);
        let theme = cx.theme();
        let content = focus_navigation(div())
            .size_full()
            .flex()
            .flex_col()
            // What text inherits unless a Label says otherwise (globals.css `body`).
            .font_family(theme.font(FontRole::Sans))
            .text_size(px(theme.scaled.text_body))
            .text_color(theme.color_text_primary)
            .child(TitleBar::new(title))
            .child(
                div().flex_1().min_h_0().child(
                    self.content
                        .clone()
                        .cached(StyleRefinement::default().size_full()),
                ),
            )
            .child(ToastStack::new());
        window_frame(content, window, cx)
    }
}
