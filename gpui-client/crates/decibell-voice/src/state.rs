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
}

/// The user panel's voice controls (registered with the shell in [`crate::init`]).
pub fn panel_action(action: PanelAction, anchor: Bounds<Pixels>, cx: &mut App) {
    let ui = VoiceUi::global(cx);
    match action {
        PanelAction::GoLive => {
            crate::codecs::ensure_caps(cx);
            ui.update(cx, |u, cx| u.open_go_live(cx));
        }
        PanelAction::Latency => ui.update(cx, |u, cx| u.toggle_popover(PanelPopover::Stats(anchor), cx)),
        PanelAction::StreamAudio => ui.update(cx, |u, cx| u.toggle_popover(PanelPopover::StreamAudio(anchor), cx)),
        PanelAction::InputDevices | PanelAction::OutputDevices => {
            let input = action == PanelAction::InputDevices;
            if let Some(model) = cx.try_global::<decibell_model::Model>().cloned() {
                model.live.voice.devices.update(cx, |d, cx| d.ensure_loaded(cx));
            }
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
}
