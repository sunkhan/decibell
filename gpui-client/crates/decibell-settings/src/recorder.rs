//! The Keybinds recorder (`KeybindsTab.tsx` `recordCombo`): fed the window's key, modifier and
//! mouse events while a row records, it answers when the combo is complete.
//!
//! - A non-modifier key completes the combo with the modifiers held (made generic:
//!   `normalize_combo`).
//! - Modifiers alone complete when the last one is released, keeping their sides.
//! - A middle / back / forward click completes with the modifiers held; left / right clicks are
//!   not bindable.
//! - Escape with nothing held cancels; so does the window losing focus. Auto-repeats are ignored.
//!
//! Key names are `KeyboardEvent.code`s, the model's mapping (`live::hotkeys`): from GPUI's key
//! **assuming a US layout** (`code_from_gpui_key`) until GPUI's key events carry the physical key
//! (patch 0012; the model then exposes the code and a recorder of its own, which replaces this
//! one). GPUI reports modifier keys only as a modifier state, without their side: a modifier is
//! recorded as its left key.

use decibell_model::live::hotkeys::{code_from_gpui_key, modifier_of, mouse_key, normalize_combo};
use gpui::{Modifiers, MouseButton};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    Combo(Vec<String>),
    Cancelled,
}

#[derive(Debug, Default)]
pub struct Recorder {
    held: Vec<String>,
    peak: Vec<String>,
    modifiers: Modifiers,
}

type Held = fn(&Modifiers) -> bool;

/// The modifier keys GPUI's state names, as the left key of each.
const MODIFIER_KEYS: [(&str, Held); 4] = [
    ("ControlLeft", |m| m.control),
    ("AltLeft", |m| m.alt),
    ("ShiftLeft", |m| m.shift),
    ("MetaLeft", |m| m.platform),
];

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// A key went down, by name.
    pub fn key_down(&mut self, code: &str, repeat: bool) -> Option<Recorded> {
        if repeat {
            return None;
        }
        if code == "Escape" && self.held.is_empty() {
            return Some(Recorded::Cancelled);
        }
        if !self.held.iter().any(|k| k == code) {
            self.held.push(code.to_string());
        }
        if !self.peak.iter().any(|k| k == code) {
            self.peak.push(code.to_string());
        }
        if modifier_of(code).is_none() {
            return Some(Recorded::Combo(normalize_combo(&self.held)));
        }
        None
    }

    pub fn key_up(&mut self, code: &str) -> Option<Recorded> {
        self.held.retain(|k| k != code);
        if self.held.is_empty() && !self.peak.is_empty() {
            return Some(Recorded::Combo(normalize_combo(&self.peak)));
        }
        None
    }

    /// GPUI's key event: `key` is `Keystroke::key`.
    pub fn gpui_key_down(&mut self, key: &str, repeat: bool) -> Option<Recorded> {
        let code = code_from_gpui_key(key)?;
        self.key_down(&code, repeat)
    }

    pub fn gpui_key_up(&mut self, key: &str) -> Option<Recorded> {
        let code = code_from_gpui_key(key)?;
        self.key_up(&code)
    }

    /// GPUI's modifier state changed: the modifiers that went down or up, as keys.
    pub fn modifiers_changed(&mut self, now: Modifiers) -> Option<Recorded> {
        let before = std::mem::replace(&mut self.modifiers, now);
        let mut done = None;
        for (key, on) in MODIFIER_KEYS {
            match (on(&before), on(&now)) {
                (false, true) => done = done.or(self.key_down(key, false)),
                (true, false) => done = done.or(self.key_up(key)),
                _ => {}
            }
        }
        done
    }

    pub fn mouse_down(&mut self, button: MouseButton) -> Option<Recorded> {
        let key = mouse_key(button)?;
        let mut keys: Vec<String> = self.held.iter().filter(|k| modifier_of(k).is_some()).cloned().collect();
        keys.push(key.to_string());
        Some(Recorded::Combo(normalize_combo(&keys)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn a_key_with_modifiers_completes_at_the_key() {
        let mut r = Recorder::new();
        assert_eq!(r.modifiers_changed(Modifiers { control: true, ..Default::default() }), None);
        assert_eq!(r.modifiers_changed(Modifiers { control: true, shift: true, ..Default::default() }), None);
        assert_eq!(r.gpui_key_down("m", false), Some(Recorded::Combo(v(&["Control", "Shift", "KeyM"]))));
    }

    #[test]
    fn modifiers_alone_complete_on_release_with_their_sides() {
        let mut r = Recorder::new();
        r.modifiers_changed(Modifiers { control: true, ..Default::default() });
        r.modifiers_changed(Modifiers { control: true, alt: true, ..Default::default() });
        assert_eq!(r.modifiers_changed(Modifiers { alt: true, ..Default::default() }), None);
        assert_eq!(r.modifiers_changed(Modifiers::default()), Some(Recorded::Combo(v(&["ControlLeft", "AltLeft"]))));
    }

    #[test]
    fn escape_cancels_only_alone_and_repeats_are_ignored() {
        let mut r = Recorder::new();
        assert_eq!(r.gpui_key_down("escape", false), Some(Recorded::Cancelled));
        let mut r = Recorder::new();
        r.modifiers_changed(Modifiers { control: true, ..Default::default() });
        assert_eq!(r.gpui_key_down("escape", false), Some(Recorded::Combo(v(&["Control", "Escape"]))));
        let mut r = Recorder::new();
        assert_eq!(r.gpui_key_down("f9", true), None);
        assert_eq!(r.gpui_key_down("f9", false), Some(Recorded::Combo(v(&["F9"]))));
    }

    /// On a German QWERTZ layout the key right of T types "z": physically it is `KeyY`, and a
    /// binding must name the physical key (the listeners match physical keys). GPUI's key events at
    /// this pin carry only the character, so this records `KeyZ`; with patch 0012 the event's
    /// physical code (`gpui::KeyCode::Evdev(21)` on Linux) reaches the model's `event_code`, and
    /// this test feeds that instead of the character.
    #[test]
    #[ignore = "needs GPUI patch 0012 (physical key codes on key events, wip/land): until it lands the recorder maps the layout's character as a US layout would"]
    fn a_non_us_layout_records_the_physical_key() {
        let mut r = Recorder::new();
        assert_eq!(r.gpui_key_down("z", false), Some(Recorded::Combo(v(&["KeyY"]))));
    }

    #[test]
    fn mouse_buttons_take_the_held_modifiers() {
        let mut r = Recorder::new();
        assert_eq!(r.mouse_down(MouseButton::Left), None);
        r.modifiers_changed(Modifiers { shift: true, ..Default::default() });
        assert_eq!(
            r.mouse_down(MouseButton::Navigate(gpui::NavigationDirection::Back)),
            Some(Recorded::Combo(v(&["Shift", "Mouse4"])))
        );
        assert_eq!(Recorder::new().mouse_down(MouseButton::Middle), Some(Recorded::Combo(v(&["Mouse3"]))));
    }
}
