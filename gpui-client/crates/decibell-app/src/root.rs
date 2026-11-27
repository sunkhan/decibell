use decibell_ui::{ActiveTheme as _, FontRole, TitleBar, ToastStack, focus_navigation, window_frame};
use gpui::{
    AnyView, Context, FocusHandle, IntoElement, Render, SharedString, StyleRefinement, Subscription, Window, div, prelude::*,
    px,
};

/// The root view of a window: the window frame, the title bar (with the Transfers button in its
/// actions slot once the app enabled it), one content view under it (the shell, or a registered
/// screen) and the toast stack.
///
/// The content is drawn as a cached view: a frame caused by the title bar (a hovered button)
/// replays the content's last paint instead of rendering it again. The root reads the
/// navigation and the community list for the titles, so a change there redraws it (cheap: the
/// panes below are cached).
///
/// It also feeds the window's keys to the focused hotkey fallback while the listener asks for it
/// ([`crate::hotkeys`], capture phase) and hands printable keys that no text field took to the
/// composer on screen ("type anywhere", bubble phase: `decibell_composer::type_to_focus`; a key
/// the fallback takes is the hotkey's and isn't typed). For that it holds the focus whenever
/// nothing else in the window does (the renderer's `document.body`): GPUI dispatches keys along
/// the focused element's path, and with nothing focused only to the window's root node, above
/// every listener.
pub struct Root {
    content: AnyView,
    /// The OS window title last set (`useWindowTitle`).
    os_title: SharedString,
    focus: FocusHandle,
    _subscriptions: [Subscription; 2],
    /// Drawn over everything, uncached (the platform's quit question: `decibell_platform`).
    overlay: Option<AnyView>,
}

impl Root {
    pub fn new(content: AnyView, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        // Unless the content focused something of its own while it was built (a test window's
        // field), the root starts with the focus.
        if window.focused(cx).is_none() {
            focus.focus(window, cx);
        }
        let subscriptions = [
            // A focused element went away (a pane, a closed dialog that had nothing to give the
            // focus back to): the root takes it again. Not from a handle focused meanwhile that
            // the frame didn't draw yet (the inline editor focuses itself as it is built): only if
            // it still isn't drawn after the next frame.
            cx.on_focus_lost(window, |this: &mut Root, window, cx| match window.focused(cx) {
                None => this.focus.focus(window, cx),
                Some(handle) => {
                    let root = this.focus.clone();
                    window.on_next_frame(move |window, cx| {
                        if window.focused(cx).is_none_or(|f| f == handle && !f.contains_focused(window, cx)) {
                            root.focus(window, cx);
                        }
                    });
                }
            }),
            crate::hotkeys::release_on_blur(window, cx),
        ];
        Self { content, os_title: SharedString::default(), focus, _subscriptions: subscriptions, overlay: None }
    }

    pub fn set_overlay(&mut self, overlay: AnyView, cx: &mut Context<Self>) {
        self.overlay = Some(overlay);
        cx.notify();
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
        // The title bar's actions: the Transfers button, in the app (and the transfers screens).
        let actions = decibell_chat_content::transfers::title_bar_button(cx);
        let theme = cx.theme();
        let content = crate::hotkeys::focused_fallback(focus_navigation(div()))
            // "Type anywhere to compose", in the bubble phase: after every element on the focused
            // path (the renderer's `keydown` on `window`), after the fallback's capture listener.
            .on_key_down(|event, window, cx| {
                if decibell_composer::type_to_focus(event, window, cx) {
                    cx.stop_propagation();
                }
            })
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            // What text inherits unless a Label says otherwise (globals.css `body`).
            .font_family(theme.font(FontRole::Sans))
            .text_size(px(theme.scaled.text_body))
            .text_color(theme.color_text_primary)
            .child(match actions {
                Some(actions) => TitleBar::new(title).actions(actions),
                None => TitleBar::new(title),
            })
            .child(
                div().flex_1().min_h_0().child(
                    self.content
                        .clone()
                        .cached(StyleRefinement::default().size_full()),
                ),
            )
            .child(ToastStack::new())
            .children(self.overlay.clone());
        window_frame(content, window, cx)
    }
}
