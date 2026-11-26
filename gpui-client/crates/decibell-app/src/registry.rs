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
    tests
}

/// Each feature crate's `init(cx)`: what it plugs into the shell (`decibell_shell::register_pane`,
/// `register_overlay`, `register_channel_menu`). Called once at start-up, after the model, before
/// the window opens (app and screens alike).
pub fn init_features(cx: &mut gpui::App) {
    decibell_chat::init(cx);
}

/// The checks `--check <name>` runs inside the signed-in app (the shell's self-tests start the
/// app that way in a child process, with its own profile).
pub fn checks() -> Vec<SelfTest> {
    decibell_shell::checks()
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
        let mut names: Vec<_> = screens().iter().map(|screen| screen.name).collect();
        assert!(names.contains(&DEFAULT_SCREEN));
        names.extend(selftests().iter().map(|test| test.name));
        for name in &names {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{name}"
            );
            assert_ne!(*name, "all");
            assert_ne!(*name, "list");
        }
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate screen or self-test name");
    }
}
