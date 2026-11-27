//! The voice feature's own view state, shared by its panes and overlays (what the renderer kept in
//! component state and module variables: the Go Live dialog's `showPicker`, the user panel's
//! popovers, `streamPipHost`'s last rects). One entity, a global; views observe it.

use decibell_shell::PanelAction;
use gpui::{App, AppContext as _, Bounds, Context, Entity, Global, Pixels};

/// A popover the user panel's voice controls opened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PanelPopover {
    /// The connection-stats popover over the latency readout.
    Stats(Bounds<Pixels>),
    /// The stream-audio app picker over its button.
    StreamAudio(Bounds<Pixels>),
    /// The device menu on a right click of mute (input) or deafen (output).
    Devices { input: bool, anchor: Bounds<Pixels> },
}

#[derive(Default)]
pub struct VoiceUi {
    /// The Go Live dialog (`CaptureSourcePicker`) is open.
    pub go_live: bool,
    pub popover: Option<PanelPopover>,
    /// Where the focused stream's video last sat (window coordinates): the mini-player shrinks out
    /// of it (`recordFullViewRect`).
    pub full_rect: Option<Bounds<Pixels>>,
    /// Where the mini-player last sat: the focused view grows back out of it (`recordMiniRect`).
    pub mini_rect: Option<Bounds<Pixels>>,
    /// When the mini-player last stopped showing (the voice view opened over it): a focused view
    /// made right after grows out of [`Self::mini_rect`].
    pub mini_left: Option<std::time::Instant>,
    /// Open Advanced with the dialog regardless of the preset (screens and self-tests).
    pub advanced_on_open: Option<bool>,
}

struct Handle(Entity<VoiceUi>);
impl Global for Handle {}

impl VoiceUi {
    pub fn global(cx: &mut App) -> Entity<VoiceUi> {
        if let Some(h) = cx.try_global::<Handle>() {
            return h.0.clone();
        }
        let entity = cx.new(|_| VoiceUi::default());
        cx.set_global(Handle(entity.clone()));
        entity
    }

    pub fn try_global(cx: &App) -> Option<Entity<VoiceUi>> {
        cx.try_global::<Handle>().map(|h| h.0.clone())
    }

    pub fn open_go_live(&mut self, cx: &mut Context<Self>) {
        if !self.go_live {
            self.go_live = true;
            self.popover = None;
            cx.notify();
        }
    }

    pub fn close_go_live(&mut self, cx: &mut Context<Self>) {
        if self.go_live {
            self.go_live = false;
            cx.notify();
        }
    }

    /// Toggle a popover (a second click on the same control closes it, as the renderer's
    /// `setShowStats((v) => !v)`).
    pub fn toggle_popover(&mut self, popover: PanelPopover, cx: &mut Context<Self>) {
        let same = matches!(
            (self.popover, popover),
            (Some(PanelPopover::Stats(_)), PanelPopover::Stats(_))
                | (Some(PanelPopover::StreamAudio(_)), PanelPopover::StreamAudio(_))
        ) || self.popover == Some(popover);
        self.popover = if same { None } else { Some(popover) };
        cx.notify();
    }

    /// The voice session ended: the popovers of the session's controls (connection stats, stream
    /// audio) and the Go Live dialog close, as the renderer unmounts them with its in-session
    /// block. A device menu stays (mute and deafen exist outside voice too).
    pub fn session_ended(&mut self, cx: &mut Context<Self>) {
        let session_popover = matches!(self.popover, Some(PanelPopover::Stats(_) | PanelPopover::StreamAudio(_)));
        if session_popover {
            self.popover = None;
        }
        if session_popover || self.go_live {
            self.go_live = false;
            cx.notify();
        }
    }

    pub fn close_popover(&mut self, cx: &mut Context<Self>) {
        if self.popover.take().is_some() {
            cx.notify();
        }
    }

    /// Recorded without a notify: nothing draws from them until the other surface mounts.
    pub fn record_full_rect(&mut self, rect: Bounds<Pixels>) {
        self.full_rect = Some(rect);
    }

    pub fn record_mini_rect(&mut self, rect: Bounds<Pixels>) {
        self.mini_rect = Some(rect);
    }

    /// The mini-player stopped showing.
    pub fn mini_left(&mut self) {
        self.mini_left = Some(std::time::Instant::now());
    }

    /// For a focused view being made now: the mini-player's last rect when the mini-player went
    /// away just before (returning to the voice view out of it), once. The renderer's panel
    /// grows out of `getMiniRect()` on mount.
    pub fn take_grow_from(&mut self) -> Option<Bounds<Pixels>> {
        let left = self.mini_left.take()?;
        (left.elapsed() < std::time::Duration::from_millis(500)).then_some(self.mini_rect).flatten()
    }
}

/// Open the Go Live dialog, as the renderer mounts `CaptureSourcePicker`: its error line and the
/// picked source are component state there, so each opening starts without the last attempt's
/// error or source (unless a start is still under way); the encoder caps load.
pub fn open_go_live(cx: &mut App) {
    crate::codecs::ensure_caps(cx);
    if let Some(model) = cx.try_global::<decibell_model::Model>().cloned() {
        model.live.voice.own_stream.update(cx, |o, cx| {
            if o.phase == decibell_model::live::voice::streams::OwnStreamPhase::Idle {
                o.set_error(None, cx);
                o.set_source(None, cx);
            }
        });
    }
    VoiceUi::global(cx).update(cx, |u, cx| u.open_go_live(cx));
}

/// The user panel's voice controls (registered with the shell in [`crate::init`]).
pub fn panel_action(action: PanelAction, anchor: Bounds<Pixels>, cx: &mut App) {
    let ui = VoiceUi::global(cx);
    match action {
        PanelAction::GoLive => open_go_live(cx),
        PanelAction::Latency => ui.update(cx, |u, cx| u.toggle_popover(PanelPopover::Stats(anchor), cx)),
        PanelAction::StreamAudio => ui.update(cx, |u, cx| u.toggle_popover(PanelPopover::StreamAudio(anchor), cx)),
        PanelAction::InputDevices | PanelAction::OutputDevices => {
            // The menu holds a watch on the device lists while it is open (`menus::PanelPopovers`):
            // they are read again as it opens and follow the devices meanwhile.
            let input = action == PanelAction::InputDevices;
            ui.update(cx, |u, cx| u.toggle_popover(PanelPopover::Devices { input, anchor }, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, point, px, size};

    use super::*;

    #[gpui::test]
    fn a_second_click_closes_the_popover(cx: &mut TestAppContext) {
        let ui = cx.new(|_| VoiceUi::default());
        let at = Bounds::new(point(px(10.), px(10.)), size(px(20.), px(20.)));
        let moved = Bounds::new(point(px(12.), px(10.)), size(px(20.), px(20.)));
        ui.update(cx, |u, cx| u.toggle_popover(PanelPopover::Stats(at), cx));
        ui.read_with(cx, |u, _| assert!(matches!(u.popover, Some(PanelPopover::Stats(_)))));
        // The same control (its bounds may have moved a pixel): closed.
        ui.update(cx, |u, cx| u.toggle_popover(PanelPopover::Stats(moved), cx));
        ui.read_with(cx, |u, _| assert!(u.popover.is_none()));
        // Opening Go Live closes any popover.
        ui.update(cx, |u, cx| {
            u.toggle_popover(PanelPopover::Devices { input: true, anchor: at }, cx);
            u.open_go_live(cx);
        });
        ui.read_with(cx, |u, _| assert!(u.go_live && u.popover.is_none()));
    }

    #[gpui::test]
    fn the_full_view_grows_out_of_the_mini_player_only_right_after_it_left(cx: &mut TestAppContext) {
        let ui = cx.new(|_| VoiceUi::default());
        let rect = Bounds::new(point(px(900.), px(600.)), size(px(320.), px(180.)));
        ui.update(cx, |u, _| u.record_mini_rect(rect));
        // The mini-player is still showing (or never left): nothing to grow from.
        ui.update(cx, |u, _| assert_eq!(u.take_grow_from(), None));
        // It just left (the voice view opened): once.
        ui.update(cx, |u, _| u.mini_left());
        ui.update(cx, |u, _| assert_eq!(u.take_grow_from(), Some(rect)));
        ui.update(cx, |u, _| assert_eq!(u.take_grow_from(), None));
        // Left long ago (a stream focused later from the grid): no grow.
        ui.update(cx, |u, _| u.mini_left = Some(std::time::Instant::now() - std::time::Duration::from_secs(2)));
        ui.update(cx, |u, _| assert_eq!(u.take_grow_from(), None));
    }

    #[gpui::test]
    fn the_session_ending_closes_its_popovers_and_go_live_but_not_a_device_menu(cx: &mut TestAppContext) {
        let ui = cx.new(|_| VoiceUi::default());
        let at = Bounds::new(point(px(10.), px(10.)), size(px(20.), px(20.)));
        for popover in [PanelPopover::Stats(at), PanelPopover::StreamAudio(at)] {
            ui.update(cx, |u, cx| u.toggle_popover(popover, cx));
            ui.update(cx, |u, cx| u.session_ended(cx));
            ui.read_with(cx, |u, _| assert!(u.popover.is_none(), "{popover:?} outlived the session"));
        }
        ui.update(cx, |u, cx| u.open_go_live(cx));
        ui.update(cx, |u, cx| u.session_ended(cx));
        ui.read_with(cx, |u, _| assert!(!u.go_live, "Go Live outlived the session"));
        let menu = PanelPopover::Devices { input: false, anchor: at };
        ui.update(cx, |u, cx| u.toggle_popover(menu, cx));
        ui.update(cx, |u, cx| u.session_ended(cx));
        ui.read_with(cx, |u, _| assert_eq!(u.popover, Some(menu)));
    }
}
