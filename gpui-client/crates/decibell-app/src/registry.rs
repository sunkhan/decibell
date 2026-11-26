//! Every screen and self-test of the workspace. A crate that has some exposes
//! `pub fn screens() -> Vec<Screen>` / `pub fn selftests() -> Vec<SelfTest>` and is added to the
//! lists here; that is the whole mechanism.

use anyhow::{Result, anyhow};
use decibell_harness::{Screen, SelfTest};

/// What the self-tests' window shows without `--screen` (the app itself, without `--selftest`,
/// shows the shell: `decibell_shell::Shell`).
pub const DEFAULT_SCREEN: &str = "gallery";

pub fn screens() -> Vec<Screen> {
    let mut screens = Vec::new();
    screens.extend(decibell_ui::screens());
    screens.extend(decibell_text_gallery::screens());
    screens.extend(decibell_video::screens());
    screens.extend(decibell_shell::screens());
    screens.extend(decibell_chat::screens());
    screens.extend(decibell_voice::screens());
    screens.extend(decibell_settings::screens());
    screens.extend(decibell_composer::screens());
    screens.extend(decibell_home::screens());
    screens.extend(decibell_chat_content::screens());
    screens.extend(decibell_chat_content::transfers::screens());
    screens.extend(decibell_server_settings::screens());
    screens.extend(decibell_picture_editor::screens());
    screens.extend(decibell_search::screens());
    screens
}

pub fn selftests() -> Vec<SelfTest> {
    let mut tests = Vec::new();
    tests.extend(crate::selftests::selftests());
    tests.extend(decibell_ui::selftests());
    tests.extend(decibell_text_gallery::selftests());
    tests.extend(decibell_video::selftests());
    tests.extend(decibell_shell::selftests());
    tests.extend(decibell_chat::selftests());
    tests.extend(decibell_voice::selftests());
    tests.extend(decibell_settings::selftests());
    tests.extend(decibell_composer::selftests());
    tests.extend(decibell_home::selftests());
    tests.extend(decibell_chat_content::selftests());
    tests.extend(decibell_chat_content::transfers::selftests());
    tests.extend(decibell_server_settings::selftests());
    tests.extend(decibell_search::selftests());
    tests
}

/// Each feature crate's `init(cx)`: what it plugs into the shell (`decibell_shell::register_pane`,
/// `register_overlay`, `register_channel_menu`). Called once at start-up, after the model, before
/// the window opens (app and screens alike).
pub fn init_features(cx: &mut gpui::App) {
    decibell_chat::init(cx);
    decibell_voice::init(cx);
    decibell_settings::init(cx);
    decibell_settings::SettingsHooks::set_app_version(env!("CARGO_PKG_VERSION"), cx);
    // After the chat crate: the composer and editor slots of its pane and list.
    decibell_composer::init(cx);
    decibell_home::init(cx);
    decibell_chat_content::init(cx);
    decibell_server_settings::init(cx);
    decibell_search::init(cx);
    join_features(cx);
    // "Type anywhere to compose" with nothing focused (an app-level keystroke interceptor).
    decibell_composer::install_type_to_focus(cx);
}

/// What one feature crate hands another through the app, so the crates stay independent of each
/// other: the composer crate's composer under the DM conversation and its emoji picker in the
/// custom status dialog. Registrations only (no model
/// needed): the capture tool calls this too, so screens show the joined pieces.
pub fn join_features(cx: &mut gpui::App) {
    decibell_home::dm::set_dm_composer(cx, decibell_composer::dm_composer);
    // The custom status dialog's emoji picker: the composer's, emoji tab only.
    decibell_home::status::set_emoji_picker(cx, |request, window, cx| {
        decibell_composer::status_emoji_picker(request.on_pick, request.on_close, request.anchor, window, cx)
    });
    join_user_menu(cx);
}

/// The voice crate's sections of the people crate's user menu (`UserContextMenu.tsx`: local mute,
/// the volume, then "Moderation"), in the renderer's order, drawn with the menu's own rows.
fn join_user_menu(cx: &mut gpui::App) {
    use std::rc::Rc;

    use decibell_home::menu::{Above, MenuSection, UserMenuExtension, order, register_user_menu};
    use decibell_voice::menus::{MenuKit, user_menu_moderation, user_menu_mute, user_menu_volume, watch_user_menu};
    use gpui::IntoElement as _;

    const KIT: MenuKit = MenuKit {
        row: |id, label, cx| decibell_home::menu::menu_row(id, label, cx),
        danger: |id, label, cx| decibell_home::menu::menu_row_danger(id, label, cx),
        label: |text, cx| decibell_home::menu::menu_label(text, cx).into_any_element(),
        close: decibell_home::menu::close_user_menu,
    };
    register_user_menu(
        cx,
        UserMenuExtension {
            order: order::VOICE,
            watch: Some(Rc::new(watch_user_menu::<decibell_home::menu::UserMenu>)),
            build: Rc::new(|target, _, cx| {
                // Mute, the renderer's inset divider, the volume (none for ourselves).
                let mut sections = Vec::new();
                sections.extend(user_menu_mute(target.uid, target.own, cx).map(|mute| MenuSection::new(Above::Rule, mute)));
                sections.extend(user_menu_volume(target.uid, target.own, cx).map(|volume| MenuSection::new(Above::Inset, volume)));
                sections
            }),
        },
    );
    register_user_menu(
        cx,
        UserMenuExtension {
            order: order::VOICE_MODERATION,
            watch: None,
            build: Rc::new(|target, _, cx| {
                if target.own {
                    return Vec::new();
                }
                user_menu_moderation(target.uid, target.server, KIT, cx).map(|m| vec![MenuSection::new(Above::Rule, m)]).unwrap_or_default()
            }),
        },
    );
}

/// The checks `--check <name>` runs inside the signed-in app (the shell's self-tests start the
/// app that way in a child process, with its own profile).
pub fn checks() -> Vec<SelfTest> {
    let mut checks = decibell_shell::checks();
    checks.extend(decibell_voice::checks());
    checks.extend(decibell_settings::checks());
    checks.extend(decibell_composer::checks());
    checks.extend(decibell_home::checks());
    checks.extend(decibell_chat::checks());
    checks.extend(decibell_chat_content::checks());
    checks.extend(decibell_chat_content::transfers::checks());
    checks.extend(decibell_server_settings::checks());
    checks.extend(decibell_search::checks());
    checks
}

pub fn screen(name: &str) -> Result<Screen> {
    let mut screens = screens();
    match screens.iter().position(|screen| screen.name == name) {
        Some(index) => Ok(screens.swap_remove(index)),
        None => Err(anyhow!("no screen named {name:?} (try --screen list)")),
    }
}

/// The `list` output of `--screen` / `--selftest`.
pub fn listing<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    entries
        .into_iter()
        .map(|(name, about)| format!("{name:<16} {about}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_the_default_screen_exists() {
        // Screens (`--screen`) and self-tests (`--selftest`) are two namespaces: a screen and the
        // test that exercises it may share a name (`kit-scroll`), two screens or two tests may not.
        let screens: Vec<_> = screens().iter().map(|screen| screen.name).collect();
        let tests: Vec<_> = selftests().iter().map(|test| test.name).collect();
        assert!(screens.contains(&DEFAULT_SCREEN));
        for (kind, names) in [("screen", &screens), ("self-test", &tests)] {
            for name in names {
                assert!(
                    name.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                    "{name}"
                );
                assert_ne!(*name, "all");
                assert_ne!(*name, "list");
            }
            let mut sorted = names.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), names.len(), "duplicate {kind} name");
        }
    }
}
