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
mod button;
mod field;
pub mod fonts;
pub mod gallery;
mod icon;
pub mod identity;
mod interaction;
pub mod kit;
pub mod layer;
mod kit_tests;
mod menu;
pub mod modal;
pub mod motion;
pub mod popover;
mod runs;
pub mod scroll;
mod select;
pub mod settings;
mod slider;
mod spinner;
mod structure;
mod text;
mod theme;
mod titlebar;
mod toast;
mod toggles;
mod tooltip;
mod window_frame;

use decibell_harness::Screen;
use decibell_theme::ThemeId;
use gpui::{App, AppContext as _, InteractiveElement, KeyBinding, actions};

pub use assets::Assets;
pub use button::{Button, ButtonSize, ButtonVariant, IconButton, LinkButton, button_row};
pub use field::{Field, TextField, TextFieldVariant};
pub use interaction::Hover;
pub use identity::{Avatar, BellMark, LetterAvatar, Presence, StatusDot, StatusPill};
pub use menu::{ContextMenu, MenuEntry, MenuItem};
pub use modal::{CloseRequest, ConfirmModal, ConfirmTone, Modal, delete_message_tip, dialog_card};
pub use settings::{NavItem, SaveBar, SettingsPane, SettingsWindow};
pub use structure::{
    Badge, BadgeTone, Banner, EmptyState, ErrorCard, Heading, Notice, SectionLabel, divider,
    vertical_divider,
};
pub use toast::{Toast, ToastAction, ToastId, ToastSeverity, ToastStack, Toasts};
pub use tooltip::{Tooltip, title_tooltip};
pub use layer::layer;
pub use popover::{
    DismissEvent, Placed, Placement, Popover, Side, place_menu, visible_window_bounds,
};
pub use scroll::{RowHeights, ScrollAlign, ScrollArea, ScrollFrame, ScrollHandle};
pub use select::{
    Select, SelectGroup, SelectHandle, SelectIcon, SelectOption, SelectSize, SelectVariant,
};
pub use slider::Slider;
pub use spinner::Spinner;
pub use toggles::{Checkbox, Segment, SegmentedControl, Switch};
pub use icon::{Icon, IconName};
pub use text::Label;
pub use theme::{ActiveTheme, ChromeScope, FontRole, Theme, Tokens, box_shadows, chrome_scope, hsla};
pub use runs::{Runs, run_style};
pub use titlebar::{CloseHandler, TITLEBAR_HEIGHT, TitleBar, request_window_close};
pub use window_frame::{
    AssumeClientDecorations, outer_window_size, window_corner_radius, window_frame,
};

actions!(decibell_ui, [FocusNext, FocusPrev]);

/// Installs the theme global, registers the bundled fonts, and binds Tab / Shift-Tab to
/// [`FocusNext`] / [`FocusPrev`] (GPUI binds no keys of its own: give the root element
/// [`focus_navigation`] so they move focus through the controls' tab stops).
pub fn init(cx: &mut App, theme: ThemeId) -> anyhow::Result<()> {
    Theme::init(cx, theme);
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrev, None),
    ]);
    fonts::register(cx)
}

/// Tab / Shift-Tab move focus through the window's tab stops: put this on a window's root
/// element. (A [`Modal`] keeps Tab inside itself.)
///
/// ```ignore
/// focus_navigation(div().size_full()).child(content)
/// ```
pub fn focus_navigation<E: InteractiveElement>(element: E) -> E {
    element
        .on_action(|_: &FocusNext, window, cx| window.focus_next(cx))
        .on_action(|_: &FocusPrev, window, cx| window.focus_prev(cx))
}

/// The self-tests this crate registers (see `decibell_harness::SelfTest`): the kit's.
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    kit_tests::selftests()
}

/// The screens this crate registers (see `decibell_harness::Screen`): the token sheet and the
/// component kit's gallery.
pub fn screens() -> Vec<Screen> {
    let mut screens = vec![Screen {
        name: "gallery",
        about: "The design-token sheet: surfaces, colours, type, radii, elevation, icons",
        build: |_, cx| cx.new(|_| gallery::Gallery).into(),
    }];
    screens.extend(kit::screens());
    screens
}
