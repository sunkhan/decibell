//! The signed-in app's start-up and glue (docs/model.md §2, §7, M3.2): the state model, the
//! core, the appearance, and the app-level hooks the model expects.
//!
//! Wired now: the model and the core (keychain-backed at-rest key), the appearance from
//! `ui.json` and then `config.json`, the shell's registry and toast mirror, the idle source,
//! a subscriber for the sound / attention cues (silent until the audio layer exists), and in
//! development builds (`dev-local`) `--login` and the perf control pipe.
//!
//! The video crate's glue (frames, keyframes, frame sinks, decode caps) is [`crate::video`]; the
//! native encoder probe runs when Go Live first opens or the first voice session starts
//! (`decibell_voice::codecs::ensure_caps`), never at start.
//!
//! Named TODOs for the features that own them:
//! - TODO(audio): play `Cue::Play` / ring on `Cue::StartRing` (assets/sounds), flash on `Cue::Flash`.
//! - TODO(hotkeys): window key events → `hotkeys.key`, blur → `window_blurred` (M3.2).
//! - TODO(devices): audio hot-plug → `devices.hotplug`.
//! - TODO(single-instance): the `decibell://` handler and the single-instance socket, forwarding
//!   links to `actions::servers::open_invite_link` and `--hotkey=` to `actions::hotkeys::inject_cli`.

use std::cell::Cell;
use std::rc::Rc;

use decibell_model::live::Cue;
use decibell_model::local_key::KeychainKey;
use decibell_model::stores::settings::SettingsEvent;
use decibell_model::{CoreOptions, Model};
use decibell_theme::ThemeId;
use decibell_ui::{ActiveTheme as _, Theme};
use gpui::App;

/// Build the model (reading `ui.json` from the user data directory), apply the appearance it
/// mirrors unless `--theme` chose one, and install the shell's glue. Synchronous: the window
/// opens right after with the right theme.
pub fn init_model(theme_override: Option<ThemeId>, with_core: bool, cx: &mut App) -> Model {
    let dir = if with_core { decibell_model::core::default_user_data_dir().ok() } else { None };
    let model = Model::init(cx, dir);
    decibell_shell::init(cx);
    crate::registry::init_features(cx);

    // The window opens with the appearance of the last run (`ui.json` mirrors `config.json`).
    let applied = Rc::new(Cell::new((0f32, 0f32)));
    let apply = {
        let applied = applied.clone();
        move |theme: Option<ThemeId>, text: f32, row: f32, cx: &mut App| {
            if let Some(id) = theme_override.or(theme)
                && cx.theme().id != id
            {
                Theme::set(cx, id);
            }
            // `set_scale` repaints every window: only when the scale moved.
            if applied.get() != (text, row) {
                applied.set((text, row));
                Theme::set_scale(cx, text, row);
            }
        }
    };
    let prefs = model.layout.read(cx).prefs().clone();
    apply(prefs.theme.parse().ok(), prefs.text_size_px as f32, prefs.row_scale as f32, cx);
    // …and follows the configuration once the core has read it, and every change in Settings.
    cx.subscribe(&model.settings, move |settings, _: &SettingsEvent, cx| {
        let s = settings.read(cx);
        let (id, text, row) = (s.theme(), s.text_size_px() as f32, s.row_scale() as f32);
        apply(Some(id), text, row, cx);
    })
    .detach();

    // Sounds and attention: the audio layer is a later feature's (TODO(audio) above).
    cx.subscribe(&model.live.cues, |_, cue: &Cue, _| log::debug!("[cues] {cue:?} (no audio layer yet)")).detach();
    model
}

/// Start the core (once per process) the way the model expects, then what follows it.
pub fn start_core(model: &Model, cx: &mut App) {
    // Auto-away: the core's idle source (Wayland's ext-idle-notify, GNOME's IdleMonitor; it
    // answers "unsupported" on X11, where the app is to supply its own).
    let source = Rc::new(decibell_model::actions::presence::CoreIdleSource);
    model.live.presence.auto_away.update(cx, |a, cx| a.set_source(source, cx));
    crate::video::install(model, cx);
    Model::start_core(
        cx,
        CoreOptions {
            user_data_dir: None,
            app_version: env!("CARGO_PKG_VERSION").into(),
            local_key: Box::new(KeychainKey),
        },
    )
    .detach();
}
