//! Turns raw key / mouse-button transitions (X11, Windows) into binding
//! presses and releases. The portal does its own matching; the renderer's
//! focused fallback mirrors this logic in `features/hotkeys/matcher.ts`.
//!
//! - Hold bindings are down while every key is held; extra keys are fine
//!   (Shift+Mouse4 still talks when push-to-talk is Mouse4).
//! - Press bindings fire on the key-down that completes them, and only
//!   when the held modifiers are exactly the binding's (Ctrl+Shift+M does
//!   not fire Ctrl+M).
//! - A generic modifier (`Control`) matches either side; a sided one
//!   (`ControlRight`, only in modifier-only bindings) matches that side.
//! - Auto-repeat (a down for a key already held) never fires anything.

use std::collections::HashSet;

use super::keys::{self, Modifier, Mods};
use super::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Key(&'static str),
    Modifier(Modifier),
}

struct Compiled {
    id: String,
    action: Action,
    parts: Vec<Part>,
    mods: Mods,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fired {
    pub id: String,
    pub action: Action,
    pub pressed: bool,
}

#[derive(Default)]
pub struct Matcher {
    bindings: Vec<Compiled>,
    held: HashSet<&'static str>,
    /// Indices into `bindings` of hold bindings currently down.
    active: HashSet<usize>,
}

impl Matcher {
    /// Replace the bindings. Hold bindings that were down are released.
    pub fn set_bindings(&mut self, bindings: &[super::Binding]) -> Vec<Fired> {
        let released = self.release_all();
        self.bindings = bindings
            .iter()
            .filter_map(|b| {
                let mut parts = Vec::with_capacity(b.keys.len());
                let mut mods = Mods::default();
                for k in &b.keys {
                    let k = keys::intern(k)?;
                    if let Some(m) = keys::generic_modifier(k) {
                        mods.insert(m);
                        parts.push(Part::Modifier(m));
                    } else {
                        if let Some(m) = keys::sided_modifier(k) {
                            mods.insert(m);
                        }
                        parts.push(Part::Key(k));
                    }
                }
                (!parts.is_empty()).then(|| Compiled {
                    id: b.id.clone(),
                    action: b.action,
                    parts,
                    mods,
                })
            })
            .collect();
        released
    }

    pub fn uses_mouse(&self) -> bool {
        self.bindings
            .iter()
            .any(|b| b.parts.iter().any(|p| matches!(p, Part::Key(k) if keys::is_mouse(k))))
    }

    /// Release every held key and binding (focus loss, pause, rebinding).
    pub fn release_all(&mut self) -> Vec<Fired> {
        self.held.clear();
        let mut out = Vec::new();
        for i in std::mem::take(&mut self.active) {
            let b = &self.bindings[i];
            out.push(Fired { id: b.id.clone(), action: b.action, pressed: false });
        }
        out
    }

    pub fn key(&mut self, key: &'static str, down: bool) -> Vec<Fired> {
        let mut out = Vec::new();
        if down {
            if !self.held.insert(key) {
                return out; // auto-repeat
            }
            let held_mods = self.held_mods();
            for (i, b) in self.bindings.iter().enumerate() {
                if !self.all_held(b) {
                    continue;
                }
                if b.action.is_hold() {
                    if self.active.insert(i) {
                        out.push(Fired { id: b.id.clone(), action: b.action, pressed: true });
                    }
                } else if part_matches_any(b, key) && held_mods == b.mods {
                    out.push(Fired { id: b.id.clone(), action: b.action, pressed: true });
                }
            }
        } else {
            if !self.held.remove(key) {
                return out;
            }
            let mut released: Vec<usize> =
                self.active.iter().copied().filter(|&i| !self.all_held(&self.bindings[i])).collect();
            released.sort_unstable();
            for i in released {
                self.active.remove(&i);
                let b = &self.bindings[i];
                out.push(Fired { id: b.id.clone(), action: b.action, pressed: false });
            }
        }
        out
    }

    fn held_mods(&self) -> Mods {
        let mut m = Mods::default();
        for k in &self.held {
            if let Some(md) = keys::sided_modifier(k) {
                m.insert(md);
            }
        }
        m
    }

    fn all_held(&self, b: &Compiled) -> bool {
        b.parts.iter().all(|p| match p {
            Part::Key(k) => self.held.contains(k),
            Part::Modifier(m) => self.held.iter().any(|h| keys::sided_modifier(h) == Some(*m)),
        })
    }
}

fn part_matches_any(b: &Compiled, key: &str) -> bool {
    b.parts.iter().any(|p| match p {
        Part::Key(k) => *k == key,
        Part::Modifier(m) => keys::sided_modifier(key) == Some(*m),
    })
}

#[cfg(test)]
mod tests {
    use super::super::Binding;
    use super::*;

    fn binding(id: &str, action: Action, keys: &[&str]) -> Binding {
        Binding { id: id.into(), action, keys: keys.iter().map(|s| s.to_string()).collect() }
    }

    fn fired(id: &str, action: Action, pressed: bool) -> Fired {
        Fired { id: id.into(), action, pressed }
    }

    #[test]
    fn press_binding_fires_once_on_completion() {
        let mut m = Matcher::default();
        m.set_bindings(&[binding("a", Action::ToggleMute, &["Control", "Shift", "KeyM"])]);
        assert!(m.key("ControlLeft", true).is_empty());
        assert!(m.key("ShiftRight", true).is_empty());
        assert_eq!(m.key("KeyM", true), vec![fired("a", Action::ToggleMute, true)]);
        assert!(m.key("KeyM", true).is_empty(), "auto-repeat");
        assert!(m.key("KeyM", false).is_empty(), "press bindings have no release");
        assert_eq!(m.key("KeyM", true), vec![fired("a", Action::ToggleMute, true)]);
    }

    #[test]
    fn press_binding_needs_exact_modifiers() {
        let mut m = Matcher::default();
        m.set_bindings(&[binding("a", Action::ToggleMute, &["Control", "KeyM"])]);
        m.key("ControlLeft", true);
        m.key("ShiftLeft", true);
        assert!(m.key("KeyM", true).is_empty());
        m.key("KeyM", false);
        m.key("ShiftLeft", false);
        assert_eq!(m.key("KeyM", true), vec![fired("a", Action::ToggleMute, true)]);
    }

    #[test]
    fn bare_key_press_binding_ignores_modified_presses() {
        let mut m = Matcher::default();
        m.set_bindings(&[binding("a", Action::ToggleDeafen, &["F13"])]);
        m.key("AltLeft", true);
        assert!(m.key("F13", true).is_empty());
    }

    #[test]
    fn hold_binding_allows_extra_keys_and_releases_on_any_part() {
        let mut m = Matcher::default();
        m.set_bindings(&[binding("p", Action::PushToTalk, &["Mouse4"])]);
        m.key("ShiftLeft", true);
        assert_eq!(m.key("Mouse4", true), vec![fired("p", Action::PushToTalk, true)]);
        assert!(m.key("KeyW", true).is_empty());
        assert!(m.key("ShiftLeft", false).is_empty());
        assert_eq!(m.key("Mouse4", false), vec![fired("p", Action::PushToTalk, false)]);
    }

    #[test]
    fn hold_combo_releases_when_a_modifier_lifts() {
        let mut m = Matcher::default();
        m.set_bindings(&[binding("p", Action::PushToTalk, &["Control", "KeyV"])]);
        m.key("ControlRight", true);
        assert_eq!(m.key("KeyV", true), vec![fired("p", Action::PushToTalk, true)]);
        assert_eq!(m.key("ControlRight", false), vec![fired("p", Action::PushToTalk, false)]);
        assert!(m.key("KeyV", false).is_empty());
    }

    #[test]
    fn sided_modifier_only_binding() {
        let mut m = Matcher::default();
        m.set_bindings(&[binding("p", Action::PushToTalk, &["ControlRight"])]);
        assert!(m.key("ControlLeft", true).is_empty());
        assert_eq!(m.key("ControlRight", true), vec![fired("p", Action::PushToTalk, true)]);
        assert_eq!(m.key("ControlRight", false), vec![fired("p", Action::PushToTalk, false)]);
    }

    #[test]
    fn rebinding_releases_held_bindings() {
        let mut m = Matcher::default();
        m.set_bindings(&[binding("p", Action::PushToMute, &["KeyB"])]);
        m.key("KeyB", true);
        assert_eq!(m.set_bindings(&[]), vec![fired("p", Action::PushToMute, false)]);
        assert!(m.key("KeyB", false).is_empty());
    }

    #[test]
    fn unknown_keys_drop_the_binding() {
        let mut m = Matcher::default();
        m.set_bindings(&[binding("x", Action::ToggleMute, &["Control", "Bogus"])]);
        m.key("ControlLeft", true);
        assert!(m.key("KeyM", true).is_empty());
        assert!(!m.uses_mouse());
        m.set_bindings(&[binding("y", Action::PushToTalk, &["Mouse5"])]);
        assert!(m.uses_mouse());
    }
}
