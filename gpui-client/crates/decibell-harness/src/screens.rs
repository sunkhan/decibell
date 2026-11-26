use gpui::{AnyView, App, Window};

/// A view that can be shown on its own, from fixture state it builds itself: the unit of
/// capture (`decibell-capture --screen <name>`) and of `decibell --screen <name>`.
///
/// `build` must not depend on anything outside the binary (no network, no config, no clock):
/// a capture of the same screen, theme, size and scale is the same picture every time.
pub struct Screen {
    /// Lower-case, dash-separated, unique across the workspace (`gallery`, `chat-busy`).
    pub name: &'static str,
    /// One line for `--list`.
    pub about: &'static str,
    /// Builds the view that fills the window under the title bar.
    pub build: fn(&mut Window, &mut App) -> AnyView,
}
