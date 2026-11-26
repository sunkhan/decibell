use anyhow::Result;
use decibell_harness::perf;
use decibell_ui::outer_window_size;
use gpui::{
    AnyView, App, AppContext as _, Bounds, Window, TitlebarOptions, WindowBounds, WindowDecorations, WindowHandle,
    WindowOptions, px, size,
};

use crate::root::Root;

/// Opens the main window showing what `build` makes: 1280×800, at least 800×600, our own title
/// bar. (The renderer's `electron/main/window.ts`.)
pub fn open_main(
    build: impl FnOnce(&mut Window, &mut App) -> AnyView + 'static,
    cx: &mut App,
) -> Result<WindowHandle<Root>> {
    cx.open_window(main_window_options(cx), move |window, cx| {
        perf::watch_first_frame(window);
        let content = build(window, cx);
        cx.new(|cx| Root::new(content, window, cx))
    })
}

fn main_window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, outer_window_size(size(px(1280.), px(800.))), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(800.), px(600.))),
        // The title is what the taskbar and window switcher show. `appears_transparent` is
        // what gives Windows our title bar instead of the system caption.
        titlebar: Some(TitlebarOptions {
            title: Some("Decibell".into()),
            appears_transparent: true,
            traffic_light_position: None,
        }),
        // Linux: no server-side frame; `decibell_ui::window_frame` draws ours.
        window_decorations: Some(WindowDecorations::Client),
        // Wayland: the app id ties the window to its .desktop file (icon, pinning) and is
        // required for `Window::activate_window`. X11: WM_CLASS.
        app_id: Some("decibell".into()),
        // GPUI caps an unfocused window's frames at 30 fps by default. People watch streams
        // in unfocused windows, so no cap. (Nothing is drawn at idle either way. The
        // power-saver and thermal caps stay at GPUI's defaults.)
        inactive_frame_interval: None,
        ..Default::default()
    }
}
