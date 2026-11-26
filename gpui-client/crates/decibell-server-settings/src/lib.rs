//! The community's administration windows (`features/servers/ServerSettingsModal.tsx`,
//! `ChannelSettingsModal.tsx` and their tabs), on the kit's shared settings frame:
//!
//! - **Server Settings**: Overview (identity, public listing, picture, ownership transfer),
//!   Members (server-side search, roles in place, the row menu: nickname, roles, time out, kick,
//!   ban), Roles (the hierarchy with drag-reorder, the editor's Display / Permissions / Members),
//!   Bans, Audit Log (server-side search and action filter, day groups), Storage.
//! - **Channel Settings**: Overview (name, topic, slowmode / bitrate, encryption), Permissions
//!   (role and member overwrites, the category's sync switch), Retention (keep-for, wipe), delete.
//!
//! Both are overlays the shell draws ([`init`]): closed, they hold nothing; open, only the open
//! tab exists. Every edit is a draft with a save bar that settles on the server's echo
//! (`decibell_model::save_echo`), and unsaved changes hold the window (Esc, ✕, the backdrop, a
//! tab or role switch shake the bar). `gpui-client/CONVENTIONS.md` "Server and channel settings"
//! has the pattern.

pub mod channel;
pub mod host;
pub mod member_search;
pub mod requests;
pub mod rows;
pub mod sentence;
pub mod server;
pub mod ui;

#[cfg(feature = "fixtures")]
pub mod fixture;
#[cfg(feature = "fixtures")]
mod screens;
#[cfg(feature = "fixtures")]
mod selftests;

use std::cell::Cell;

use gpui::{App, AppContext as _};

thread_local! {
    /// The screens pin "now" (the role preview's "Today at", time-out countdowns), so a capture
    /// is the same picture every time.
    static NOW: Cell<Option<i64>> = const { Cell::new(None) };
}

/// Unix seconds now (or what the screens pinned).
pub fn now_secs() -> i64 {
    NOW.with(|n| n.get()).unwrap_or_else(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
    })
}

/// Pin "now" (screens; `None` lets it run again).
pub fn pin_now(secs: Option<i64>) {
    NOW.with(|n| n.set(secs));
}

/// Once at start-up (before the window opens): the two windows as overlays of the main layout.
/// The shell's channel menu already has "Channel Settings" / "Category Settings" (as the
/// renderer's), which open the channel window.
pub fn init(cx: &mut App) {
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| host::ServerSettingsHost::new(window, cx)).into());
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| host::ChannelSettingsHost::new(window, cx)).into());
}

/// The screens (feature `fixtures`).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The self-tests (feature `fixtures`: the in-process ones draw the fixture): against the local
/// stack in a child app each, and in this process.
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    #[cfg(feature = "fixtures")]
    return selftests::selftests();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The checks `decibell --check <name>` runs inside the signed-in app.
pub fn checks() -> Vec<decibell_harness::SelfTest> {
    #[cfg(feature = "fixtures")]
    return selftests::checks();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}
