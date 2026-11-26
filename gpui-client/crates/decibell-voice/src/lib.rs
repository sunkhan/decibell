//! The voice view of the Decibell GPUI client (`electron-client/src/features/voice/`,
//! `features/call/`): what a person sees and does in a voice channel or a DM call.
//!
//! - [`pane::VoicePane`] (`Slot::Voice`): the stage of 16:9 tiles (alone, grid with "+N more" and
//!   the hidden-speaker swap, streams over a people row), the focused stream with its filmstrip
//!   and window fullscreen ([`focus`]), the floating dock.
//! - [`mini::MiniPlayer`] (an overlay): the watched stream floating over every other view.
//! - [`go_live::GoLiveDialog`] (an overlay): presets, Advanced, the stream-audio picker.
//! - [`menus`]: the user menu's voice sections (local mute, volume, voice moderation: joined into
//!   the people crate's menu by the app), the device menus, the connection-stats popover, the
//!   stream-audio popover (the user panel's controls).
//! - [`calls`]: the incoming-call card (an overlay) and the call stage (`Slot::DmCallStage`, hosted
//!   by the DM pane).
//!
//! Streams are drawn by `decibell-video`: one decoder per stream, a [`decibell_video::VideoView`]
//! per place it shows ([`players`]). Layout maths: [`layout`]. Lean rules (CONVENTIONS.md "Voice"):
//! a speaking ring redraws its tile only; no timer without a live reason; a watched stream costs
//! one present per picture.

pub mod calls;
pub mod codecs;
pub mod dock;
#[cfg(feature = "fixtures")]
pub mod fixture;
pub mod focus;
pub mod go_live;
pub mod layout;
pub mod menus;
pub mod mini;
pub mod pane;
pub mod people;
pub mod players;
pub mod scrim;
#[cfg(feature = "fixtures")]
mod screens;
mod selftests;
pub mod state;
pub mod tiles;
pub mod video_events;
pub mod volume;

use gpui::{App, AppContext as _, Global};

pub use video_events::video_event;

/// `DECIBELL_VOICE_TRACE=1`: log each render of the voice views (which view redrew a frame).
pub(crate) fn trace(view: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("DECIBELL_VOICE_TRACE").is_some_and(|v| v == "1")) {
        log::info!("[voice-trace] render {view}");
    }
}

struct Initialized;
impl Global for Initialized {}

/// Once, from the app's `init_features` (and the screens, which a capture builds without it): the
/// voice pane, the overlays, the call stage, the user panel's voice controls, and the stores the
/// views share. A second call does nothing.
pub fn init(cx: &mut App) {
    if cx.has_global::<Initialized>() {
        return;
    }
    cx.set_global(Initialized);
    state::VoiceUi::global(cx);
    video_events::install(cx);
    players::Players::install(cx);
    decibell_shell::register_pane(cx, decibell_shell::Slot::Voice, |_, cx| cx.new(pane::VoicePane::new).into());
    decibell_shell::register_pane(cx, decibell_shell::Slot::DmCallStage, |_, cx| cx.new(calls::CallStage::new).into());
    decibell_shell::register_overlay(cx, |_, cx| cx.new(mini::MiniPlayer::new).into());
    decibell_shell::register_overlay(cx, |_, cx| cx.new(go_live::GoLiveDialog::new).into());
    decibell_shell::register_overlay(cx, |_, cx| cx.new(menus::PanelPopovers::new).into());
    decibell_shell::register_overlay(cx, |_, cx| cx.new(calls::IncomingCall::new).into());
    decibell_shell::register_user_panel_action(cx, |action, anchor, _, cx| state::panel_action(action, anchor, cx));
    // The first voice session needs the codec caps (decode, and the encoder probe for Go Live).
    if let Some(model) = cx.try_global::<decibell_model::Model>().cloned() {
        cx.observe(&model.live.voice.session, |session, cx| {
            if session.read(cx).media_session_active() {
                codecs::ensure_caps(cx);
            }
        })
        .detach();
    }
}

/// The voice feature's screens (with the `fixtures` feature).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The voice feature's self-tests (each starts the app in a child process against the local
/// stack; see `selftests.rs`).
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    selftests::selftests()
}

/// The checks `decibell --check <name>` runs inside the signed-in app.
pub fn checks() -> Vec<decibell_harness::SelfTest> {
    selftests::checks()
}
