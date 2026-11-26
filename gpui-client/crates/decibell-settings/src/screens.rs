//! The Settings screens, drawn from [`crate::fixture`] (no network, no clock): `decibell-capture
//! --screen <name>` and `decibell --screen <name>`. Each is the signed-in shell with the window
//! open on a tab or a dialog over it. `settings-narrow` is meant to be captured 900 px wide
//! (`--size 900x800`): the sidebar folds into the icon rail.

use decibell_harness::Screen;
use decibell_ui::AssumeClientDecorations;
use gpui::{AnyView, App, AppContext as _, Window};

use crate::fixture::{self, Stage};
use crate::tabs::TabId;

/// Captures render one frame on a clock that doesn't run: entrances show their end.
fn settle_motion_for_capture(cx: &mut App) {
    if cx.has_global::<AssumeClientDecorations>() {
        cx.set_reduce_motion(true);
    }
}

fn build(stage: Stage, cx: &mut App) -> AnyView {
    settle_motion_for_capture(cx);
    crate::init(cx);
    fixture::apply(stage, cx);
    cx.new(decibell_shell::Shell::new).into()
}

macro_rules! screen {
    ($name:literal, $about:literal, $stage:expr) => {
        Screen { name: $name, about: $about, build: |_: &mut Window, cx: &mut App| build($stage, cx) }
    };
}

pub fn screens() -> Vec<Screen> {
    vec![
        screen!("settings-account", "Settings → Account: profile card, picture, username / password links, Log Out", Stage::Tab(TabId::Account)),
        screen!("settings-devices", "Settings → Devices: this device and three others, relative times on a fixed clock", Stage::Tab(TabId::Devices)),
        screen!("settings-appearance", "Settings → Appearance: the seven theme cards, text size, density, time format", Stage::Tab(TabId::Appearance)),
        screen!("settings-privacy", "Settings → Privacy: encryption on (fingerprint), the toggles, Show me as Away", Stage::Tab(TabId::Privacy)),
        screen!("settings-privacy-locked", "Settings → Privacy with the keys backed up but locked on this device", Stage::PrivacyLocked),
        screen!("settings-audio", "Settings → Audio: devices, voice activity with the meter, processing, stream audio", Stage::Tab(TabId::Audio)),
        screen!("settings-keybinds", "Settings → Keybinds: the portal's summary and four keybinds (an assignment, a mouse problem)", Stage::Tab(TabId::Keybinds)),
        screen!("settings-keybinds-recording", "Settings → Keybinds with the first row recording", Stage::KeybindsRecording),
        screen!("settings-network", "Settings → Network: downloads folder, 5 MB/s up, a custom 7.5 MB/s down", Stage::Tab(TabId::Network)),
        screen!("settings-codecs", "Settings → Codecs: the switches and the detected encoders / decoders", Stage::Tab(TabId::Codecs)),
        screen!("settings-about", "Settings → About: version and the updater's status", Stage::Tab(TabId::About)),
        screen!("settings-narrow", "Settings → Privacy below 1024 px: the icon rail (capture at 900 wide)", Stage::Tab(TabId::Privacy)),
        screen!("passphrase-setup", "The passphrase dialog setting encryption up, over Settings → Privacy", Stage::PassphraseSetup),
        screen!("passphrase-unlock", "The passphrase dialog unlocking a backup, over Settings → Privacy", Stage::PassphraseUnlock),
        screen!("rename-modal", "Change username, over Settings → Account", Stage::Rename),
        screen!("password-modal", "Change password, over Settings → Account", Stage::Password),
    ]
}
