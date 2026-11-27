//! Writes the Settings screens' fixture as the Electron capture pages read it:
//! `settings-fixture <dir>` → `<dir>/settings-<stage>.json` (see `decibell_settings::fixture::json`).

use decibell_settings::TabId;
use decibell_settings::fixture::{Stage, UpdateCase, json};

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let stages = [
        ("tab-account", Stage::Tab(TabId::Account)),
        ("tab-devices", Stage::Tab(TabId::Devices)),
        ("tab-appearance", Stage::Tab(TabId::Appearance)),
        ("tab-privacy", Stage::Tab(TabId::Privacy)),
        ("tab-audio", Stage::Tab(TabId::Audio)),
        ("tab-keybinds", Stage::Tab(TabId::Keybinds)),
        ("tab-network", Stage::Tab(TabId::Network)),
        ("tab-codecs", Stage::Tab(TabId::Codecs)),
        ("tab-about", Stage::Tab(TabId::About)),
        ("privacy-locked", Stage::PrivacyLocked),
        ("keybinds-recording", Stage::KeybindsRecording),
        ("passphrase-setup", Stage::PassphraseSetup),
        ("passphrase-unlock", Stage::PassphraseUnlock),
        ("rename", Stage::Rename),
        ("password", Stage::Password),
    ];
    let about = UpdateCase::ALL.map(|case| (format!("about-{}", case.name()), Stage::About(case)));
    let stages = stages.into_iter().map(|(name, stage)| (name.to_string(), stage)).chain(about);
    for (name, stage) in stages {
        let path = std::path::Path::new(&dir).join(format!("settings-{name}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&json(stage)).expect("json")).expect("write");
        println!("{}", path.display());
    }
}
