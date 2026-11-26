//! Decibell's design system on GPUI: everything a view needs to look like the app, and no
//! feature logic.
//!
//! - [`Theme`] / [`ActiveTheme`]: the active theme's tokens as GPUI values (`cx.theme()`).
//! - [`fonts`]: the bundled faces; [`FontRole`] picks one through the theme.
//! - [`Label`]: text. [`Icon`]: the bundled SVGs.
//! - [`window_frame`] and [`TitleBar`]: the client-side window frame.
//! - [`Assets`]: the embedded assets, for `Application::with_assets`.
//! - [`gallery`]: the design-token sheet, the first registered screen.
//!
//! [`init`] installs the theme and the fonts; call it before opening a window.
//! How view code uses these: `gpui-client/CONVENTIONS.md`.

mod assets;
pub mod fonts;
pub mod gallery;
mod icon;
mod text;
mod theme;
mod titlebar;
mod window_frame;

use decibell_harness::Screen;
use decibell_theme::ThemeId;
use gpui::{App, AppContext as _};

pub use assets::Assets;
pub use icon::Icon;
pub use text::Label;
pub use theme::{ActiveTheme, FontRole, Theme, Tokens, box_shadows, hsla};
pub use titlebar::{TITLEBAR_HEIGHT, TitleBar};
pub use window_frame::{
    AssumeClientDecorations, outer_window_size, window_corner_radius, window_frame,
};

/// Installs the theme global and registers the bundled fonts.
pub fn init(cx: &mut App, theme: ThemeId) -> anyhow::Result<()> {
    Theme::init(cx, theme);
    fonts::register(cx)
}

/// The screens this crate registers (see `decibell_harness::Screen`).
pub fn screens() -> Vec<Screen> {
    vec![Screen {
        name: "gallery",
        about: "The design-token sheet: surfaces, colours, type, radii, elevation, icons",
        build: |_, cx| cx.new(|_| gallery::Gallery).into(),
    }]
}
