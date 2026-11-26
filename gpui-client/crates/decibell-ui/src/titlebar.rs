//! The 32 px title bar every window carries (the renderer's `layouts/Titlebar.tsx`): the title
//! centred, and at the right end a slot for window-level actions (the Transfers button), a
//! divider, and minimise / maximise / close.

use gpui::{
    AnyElement, App, ClickEvent, Decorations, Div, Hsla, IntoElement, MouseButton, Pixels,
    RenderOnce, SharedString, Stateful, Window, WindowControlArea, div, prelude::*, px,
};

use crate::window_frame::decorations;
use crate::{ActiveTheme as _, FontRole, Icon, IconName, Label, window_corner_radius};

/// `h-8`.
pub const TITLEBAR_HEIGHT: Pixels = px(32.);

/// What the close button does when the app installed something (the quit guard: uploads in flight
/// ask first, `window.ts`' `close` handler); without it the button closes the window.
pub struct CloseHandler(pub std::rc::Rc<CloseFn>);

/// What a [`CloseHandler`] runs.
pub type CloseFn = dyn Fn(&mut Window, &mut App);

impl gpui::Global for CloseHandler {}

/// Close the window as its close button does: through the app's [`CloseHandler`] if one is set.
pub fn request_window_close(window: &mut Window, cx: &mut App) {
    match cx.try_global::<CloseHandler>().map(|handler| handler.0.clone()) {
        Some(handler) => handler(window, cx),
        None => window.remove_window(),
    }
}

#[derive(IntoElement)]
pub struct TitleBar {
    title: SharedString,
    actions: Option<AnyElement>,
}

impl TitleBar {
    /// `title` is what the renderer shows: the open server's or conversation's name, else
    /// "Decibell".
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            actions: None,
        }
    }

    /// What sits left of the divider: full-height buttons, laid out in a row.
    pub fn actions(mut self, actions: impl IntoElement) -> Self {
        self.actions = Some(actions.into_any_element());
        self
    }
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let chrome = &cx.theme().chrome;
        // Windows always has our title bar (and reports no client decorations); on Linux the
        // buttons exist only when no server-side title bar already has them.
        let own_controls = cfg!(target_os = "windows")
            || matches!(decorations(window, cx), Decorations::Client { .. });
        let controls = window.window_controls();
        let maximized = window.is_maximized();
        let corner_radius = window_corner_radius(window, cx);

        div()
            .id("titlebar")
            .relative()
            .flex()
            .flex_none()
            .h(TITLEBAR_HEIGHT)
            .items_center()
            .justify_center()
            .rounded_t(corner_radius)
            .bg(chrome.color_bg_titlebar)
            .border_b_1()
            .border_color(chrome.color_border)
            .window_control_area(WindowControlArea::Drag)
            // On Windows the drag area is the system's caption (`HTCAPTION`): moving, the
            // double-click that maximises and restores, snapping and the right-click window
            // menu are Windows' own, and a second `zoom_window` here would undo a restore.
            .when(own_controls && !cfg!(target_os = "windows"), |bar| {
                bar.on_mouse_down(MouseButton::Left, |event, window, _| {
                    if event.click_count == 2 {
                        window.zoom_window();
                    } else {
                        window.start_window_move();
                    }
                })
                .on_mouse_down(MouseButton::Right, |event, window, _| {
                    window.show_window_menu(event.position);
                })
            })
            .child(
                Label::new(self.title)
                    .font(FontRole::Channel)
                    .size(12.)
                    .weight(500.)
                    .color(chrome.color_text_secondary),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .flex()
                    .children(self.actions)
                    .when(own_controls, |group| {
                        group
                            .child(div().my_2().w(px(1.)).bg(chrome.color_border))
                            .when(controls.minimize, |group| {
                                group.child(title_button(
                                    "minimize",
                                    IconName::WindowMinimize,
                                    WindowControlArea::Min,
                                    chrome.color_surface_active,
                                    chrome.color_text_primary,
                                    cx,
                                    |_, window, _| window.minimize_window(),
                                ))
                            })
                            .when(controls.maximize, |group| {
                                group.child(title_button(
                                    "maximize",
                                    if maximized {
                                        IconName::WindowRestore
                                    } else {
                                        IconName::WindowMaximize
                                    },
                                    WindowControlArea::Max,
                                    chrome.color_surface_active,
                                    chrome.color_text_primary,
                                    cx,
                                    |_, window, _| window.zoom_window(),
                                ))
                            })
                            .child(
                                title_button(
                                    "close",
                                    IconName::WindowClose,
                                    WindowControlArea::Close,
                                    chrome.color_error,
                                    gpui::white(),
                                    cx,
                                    |_, window, cx| request_window_close(window, cx),
                                )
                                .rounded_tr(corner_radius),
                            )
                    }),
            )
    }
}

/// `w-11`, full height; the icon is 14 px in `text-secondary` and takes `hover_icon` over
/// `hover_bg` under the pointer.
fn title_button(
    id: &'static str,
    icon: IconName,
    area: WindowControlArea,
    hover_bg: Hsla,
    hover_icon: Hsla,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .group(id)
        .flex()
        .h_full()
        .w(px(44.))
        .items_center()
        .justify_center()
        .hover(|style| style.bg(hover_bg))
        .window_control_area(area)
        .map(|button| {
            if cfg!(target_os = "windows") {
                // Windows asks the window what is under the pointer (`WM_NCHITTEST`) and GPUI
                // answers with the topmost control area: the button has to hide the bar's
                // drag area beneath it to be the minimise / maximise / close button (which is
                // also what makes Windows 11 show its snap layouts over the maximise button).
                // The press is then the system's: GPUI's Windows backend minimises, closes,
                // and maximises *or restores* on the button's mouse-up unless the app handled
                // the event, so nothing is handled here (`Window::zoom_window` only ever
                // maximises on Windows).
                button.occlude()
            } else {
                button
                    // The bar under the button starts a window move on mouse-down.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(on_click)
            }
        })
        .child(
            Icon::new(icon)
                .size(px(14.))
                .color(cx.theme().chrome.color_text_secondary)
                .group_hover(id, hover_icon),
        )
}
