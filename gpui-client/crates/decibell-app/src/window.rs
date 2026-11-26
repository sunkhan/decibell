use anyhow::Result;
use decibell_harness::perf;
use decibell_ui::outer_window_size;
use gpui::{
    AnyView, App, AppContext as _, Pixels, Size, Window, TitlebarOptions, WindowDecorations, WindowHandle, WindowOptions,
    px, size,
};

use crate::root::Root;

/// The visible size a first start opens with (`window.ts`).
pub const DEFAULT_SIZE: Size<Pixels> = size(px(1280.), px(800.));
/// The smallest visible size (`window.ts`' `minWidth` / `minHeight`; owner decision in CLAUDE.md).
pub const MIN_SIZE: Size<Pixels> = size(px(800.), px(600.));

/// Opens the main window showing what `build` makes: 1280×800 centred, at least 800×600, our own
/// title bar (the renderer's `electron/main/window.ts`). `profile`: the app's, whose saved window
/// state (`decibell_platform::window_state`) it then opens with; screens have none.
pub fn open_main(
    build: impl FnOnce(&mut Window, &mut App) -> AnyView + 'static,
    profile: Option<&std::path::Path>,
    cx: &mut App,
) -> Result<WindowHandle<Root>> {
    cx.open_window(main_window_options(profile, cx), move |window, cx| {
        perf::watch_first_frame(window);
        let content = build(window, cx);
        cx.new(|cx| Root::new(content, window, cx))
    })
}

fn main_window_options(profile: Option<&std::path::Path>, cx: &App) -> WindowOptions {
    let (bounds, display_id) = decibell_platform::window_state::initial_bounds(
        profile,
        outer_window_size(DEFAULT_SIZE),
        outer_window_size(MIN_SIZE),
        cx,
    );
    WindowOptions {
        window_bounds: Some(bounds),
        // The screen the saved place is on (Windows converts the bounds with its DPI scale).
        display_id,
        window_min_size: Some(MIN_SIZE),
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
