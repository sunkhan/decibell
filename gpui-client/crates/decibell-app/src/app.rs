//! The signed-in app's start-up and glue (docs/model.md §2, §7, M3.2): the state model, the
//! core, the appearance, and the app-level hooks the model expects.
//!
//! Wired now: the model and the core (keychain-backed at-rest key), the appearance from
//! `ui.json` and then `config.json`, the shell's registry and toast mirror, the idle source,
//! the interface sounds and the microphone test ([`crate::audio`]: the model's cues played by
//! the core), and in development builds (`dev-local`) `--login` and the perf control pipe.
//!
//! The video crate's glue (frames, keyframes, frame sinks, decode caps) is [`crate::video`]; the
//! native encoder probe runs when Go Live first opens or the first voice session starts
//! (`decibell_voice::codecs::ensure_caps`), never at start.
//!
//! The desktop around the window (single instance, `decibell://` links, `--hotkey`, the flash on
//! `Cue::Flash`, taskbar progress, the quit guard, window state, sleep / lock) is
//! `decibell_platform`, installed by `boot` once the main window is open.
//!
//! Named TODOs for the features that own them:
//! - TODO(hotkeys): window key events → `hotkeys.key`, blur → `window_blurred` (M3.2).

use std::cell::Cell;
use std::rc::Rc;

use decibell_model::local_key::KeychainKey;
use decibell_model::stores::settings::SettingsEvent;
use decibell_model::{CoreOptions, Model};
use decibell_theme::ThemeId;
use decibell_ui::{ActiveTheme as _, Theme};
use gpui::App;

/// Build the model (reading `ui.json` from the user data directory), apply the appearance it
/// mirrors unless `--theme` chose one, and install the shell's glue. Synchronous: the window
/// opens right after with the right theme.
/// `profile`: the app's (the core's user data directory); `None` for screens, which have no core.
pub fn init_model(theme_override: Option<ThemeId>, profile: Option<std::path::PathBuf>, cx: &mut App) -> Model {
    let model = Model::init(cx, profile);
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
    model
}

/// Where the at-rest key lives: the OS keychain in a shipped build. A development build
/// (`dev-local`, or any debug build) keeps it in a file of the profile instead, because its runs
/// use throwaway profiles and each would leave an item in the desktop's keychain;
/// `DECIBELL_DEV_KEYCHAIN=real` opts one run back in (the test of a machine without a keychain
/// does, on a private bus).
fn local_key_provider() -> Box<dyn decibell_model::local_key::LocalKeyProvider> {
    if cfg!(any(feature = "dev-local", debug_assertions)) && std::env::var("DECIBELL_DEV_KEYCHAIN").as_deref() != Ok("real") {
        return Box::new(decibell_model::local_key::FileKey);
    }
    Box::new(KeychainKey)
}

/// Start the core (once per process) the way the model expects, then what follows it.
/// `cues`: where the interface sounds may play (a self-test's check never reaches the real
/// output device: [`crate::audio::CueOutput`]).
pub fn start_core(model: &Model, cues: crate::audio::CueOutput, cx: &mut App) {
    let user_data_dir = decibell_model::core::default_user_data_dir().ok();
    // Auto-away: the platform's idle source (Wayland / GNOME: the core's ext-idle-notify /
    // IdleMonitor; X11: the server's IDLETIME through SYNC alarms; Windows: GetLastInputInfo).
    let source = decibell_platform::idle::source(model, cx);
    model.live.presence.auto_away.update(cx, |a, cx| a.set_source(source, cx));
    crate::video::install(model, cx);
    // The interface sounds and Settings → Audio's microphone test, both the core's.
    crate::audio::install(model, cues, cx);
    Model::start_core(
        cx,
        CoreOptions {
            user_data_dir,
            app_version: crate::VERSION.into(),
            local_key: local_key_provider(),
        },
    )
    .detach();
    // The Transfers button in the title bar (`layouts/Titlebar.tsx` shows it on every page, the
    // sign-in page too). The downloads manager is wired by the content crate once the core runs.
    // Taskbar progress and the quit guard read `model.live.transfers`' `progress()` and
    // `uploads_in_flight()` (`decibell_platform::{progress, quit_guard}`).
    decibell_chat_content::transfers::enable_title_bar(cx);
}
