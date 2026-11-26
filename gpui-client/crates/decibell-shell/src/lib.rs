//! The app's shell (`gpui-client/CONVENTIONS.md`, "Shell"): the sign-in pages, the main layout
//! and its chrome — server bar, channel sidebar with the floating user panel, member list, the
//! Home / Friends frame — and the registry feature crates fill the panes through.
//!
//! - [`Shell`]: the content under the title bar (sign-in page or main layout).
//! - [`register_pane`], [`register_overlay`], [`register_channel_menu`]: how a feature plugs in.
//! - [`ShellRequests`]: dialogs the shell asks for and a feature draws (create channel, open a
//!   conversation).
//! - [`titlebar_title`], [`window_title`]: what the title bar and the OS window show.

mod auth;
mod channels;
mod conversations;
mod dm_rows;
#[cfg(feature = "fixtures")]
pub mod fixture;
mod friends;
mod glow;
mod ink;
mod main_view;
mod members;
mod overlays;
pub mod probes;
mod requests;
#[cfg(feature = "fixtures")]
mod screens;
mod selftests;
mod server_bar;
mod server_pane;
mod shell;
mod sidebar;
mod slots;
mod user_panel;

pub use auth::{AuthField, AuthMode, AuthView};
pub use channels::{ChannelSidebar, MemberDrag};
pub use conversations::ConversationSidebar;
pub use dm_rows::{DmRows, UnreadDmTiles};
pub use friends::FriendsPage;
pub use glow::{BREATHE_FPS, BREATHE_PERIOD};
pub use main_view::MainView;
pub use members::{MEMBERS_WIDTH, MembersList, member_row_height};
pub use overlays::{CertMismatchOverlay, ToastMirror};
pub use requests::{CreateChannelRequest, ShellRequests};
pub use server_bar::{BAR_HEIGHT, ServerBar, TILE_HEIGHT, TILE_WIDTH};
pub use server_pane::ServerPane;
pub use shell::{Shell, titlebar_title, window_title};
pub use slots::{
    ChannelMenuTarget, PaneBuilder, PanelAction, PanelActionHandler, Placeholder, Slot, build_pane, build_slot, is_registered,
    register_channel_menu, register_overlay, register_pane, register_user_panel_action,
};
pub use user_panel::{PanelHeight, UserPanel};

/// Once, after `Model::init`, before the first window: the model's toasts show in the kit's stack.
pub fn init(cx: &mut gpui::App) {
    let mirror = ToastMirror::install(cx);
    // Lives as long as the app.
    std::mem::forget(mirror);
}

/// The shell's self-tests (each starts the app in a child process: see `selftests.rs`).
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    selftests::selftests()
}

/// The checks `decibell --check <name>` runs inside the signed-in app.
pub fn checks() -> Vec<decibell_harness::SelfTest> {
    selftests::checks()
}

/// The shell's screens (with the `fixtures` feature).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}
