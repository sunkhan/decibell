//! The Keybinds recorder (`KeybindsTab.tsx` `recordCombo`) is the model's
//! (`decibell_model::live::hotkeys::Recorder`), fed the window's key, modifier and mouse events
//! while a row records (`tabs/keybinds.rs`); it answers when the combo is complete:
//!
//! - A non-modifier key completes the combo with the modifiers held (made generic:
//!   `normalize_combo`).
//! - Modifiers alone complete when the last one is released, keeping their sides
//!   (`ModifiersChangedEvent::key` says which key and side, GPUI patch 0012).
//! - A middle / back / forward click completes with the modifiers held; left / right clicks are
//!   not bindable.
//! - Escape with nothing held cancels; so does the window losing focus. Auto-repeats are ignored.
//!
//! Keys are `KeyboardEvent.code` names of the **physical** key (`KeyDownEvent::code`, named by
//! the core's table): a German user pressing the key labelled Z records `KeyY`, as the
//! listeners match it. The tests here are the tab's flows on the events GPUI's Linux layer
//! builds (the model's own tests compile real xkb layouts).

pub use decibell_model::live::hotkeys::{Recorded, Recorder};

#[cfg(test)]
mod tests {
    use gpui::{KeyCode, KeyDownEvent, KeyUpEvent, Keystroke, ModifierKey, Modifiers, ModifiersChangedEvent, MouseButton};

    use super::*;

    fn v(keys: &[&str]) -> Option<Recorded> {
        Some(Recorded::Combo(keys.iter().map(|k| k.to_string()).collect()))
    }

    /// What the layout types with the key (`typed`), and which key it is (`evdev`).
    fn down(typed: &str, evdev: u16) -> KeyDownEvent {
        KeyDownEvent { keystroke: keystroke(typed), is_held: false, prefer_character_input: false, code: KeyCode::Evdev(evdev) }
    }

    fn up(typed: &str, evdev: u16) -> KeyUpEvent {
        KeyUpEvent { keystroke: keystroke(typed), code: KeyCode::Evdev(evdev) }
    }

    fn keystroke(typed: &str) -> Keystroke {
        Keystroke { modifiers: Modifiers::default(), key: typed.into(), key_char: Some(typed.into()) }
    }

    fn modifier(evdev: u16, pressed: bool) -> ModifiersChangedEvent {
        ModifiersChangedEvent { key: Some(ModifierKey { code: KeyCode::Evdev(evdev), pressed }), ..Default::default() }
    }

    const CONTROL_L: u16 = 29;
    const SHIFT_L: u16 = 42;
    const SHIFT_R: u16 = 54;
    const ALT_L: u16 = 56;

    #[test]
    fn a_key_with_modifiers_completes_at_the_key() {
        let mut r = Recorder::new();
        assert_eq!(r.on_modifiers_changed(&modifier(CONTROL_L, true)), None);
        assert_eq!(r.on_modifiers_changed(&modifier(SHIFT_L, true)), None);
        assert_eq!(r.on_key_down(&down("m", 50)), v(&["Control", "Shift", "KeyM"]));
    }

    #[test]
    fn modifiers_alone_complete_on_release_with_their_sides() {
        let mut r = Recorder::new();
        r.on_modifiers_changed(&modifier(CONTROL_L, true));
        r.on_modifiers_changed(&modifier(SHIFT_R, true));
        assert_eq!(r.on_modifiers_changed(&modifier(CONTROL_L, false)), None);
        assert_eq!(r.on_modifiers_changed(&modifier(SHIFT_R, false)), v(&["ControlLeft", "ShiftRight"]));
    }

    #[test]
    fn escape_cancels_only_alone_and_repeats_are_ignored() {
        let mut r = Recorder::new();
        assert_eq!(r.on_key_down(&down("\u{1b}", 1)), Some(Recorded::Cancelled));
        let mut r = Recorder::new();
        r.on_modifiers_changed(&modifier(CONTROL_L, true));
        assert_eq!(r.on_key_down(&down("\u{1b}", 1)), v(&["Control", "Escape"]));
        let mut r = Recorder::new();
        let mut repeat = down("f9", 67);
        repeat.is_held = true;
        assert_eq!(r.on_key_down(&repeat), None);
        assert_eq!(r.on_key_down(&down("f9", 67)), v(&["F9"]));
        // A key released alone after a modifier went down and up completes nothing extra.
        let mut r = Recorder::new();
        assert_eq!(r.on_key_up(&up("a", 30)), None);
    }

    /// On a German QWERTZ layout the key right of T types "z": physically it is `KeyY`, and a
    /// binding must name the physical key (the listeners match physical keys). On a Turkish Q
    /// layout the key right of L types "ş": `Semicolon`; "ı" is `KeyI`, "i" is `Quote`.
    #[test]
    fn a_non_us_layout_records_the_physical_key() {
        let mut r = Recorder::new();
        assert_eq!(r.on_key_down(&down("z", 21)), v(&["KeyY"]));
        let mut r = Recorder::new();
        r.on_modifiers_changed(&modifier(CONTROL_L, true));
        assert_eq!(r.on_key_down(&down("y", 44)), v(&["Control", "KeyZ"]));
        for (typed, evdev, code) in [("ş", 39, "Semicolon"), ("ı", 23, "KeyI"), ("i", 40, "Quote"), ("ö", 51, "Comma")] {
            let mut r = Recorder::new();
            assert_eq!(r.on_key_down(&down(typed, evdev)), v(&[code]), "the key that types {typed:?}");
        }
    }

    #[test]
    fn mouse_buttons_take_the_held_modifiers() {
        let mut r = Recorder::new();
        assert_eq!(r.on_mouse_down(MouseButton::Left), None);
        r.on_modifiers_changed(&modifier(SHIFT_L, true));
        assert_eq!(r.on_mouse_down(MouseButton::Navigate(gpui::NavigationDirection::Back)), v(&["Shift", "Mouse4"]));
        assert_eq!(Recorder::new().on_mouse_down(MouseButton::Middle), v(&["Mouse3"]));
        let mut r = Recorder::new();
        r.on_modifiers_changed(&modifier(ALT_L, true));
        assert_eq!(r.on_mouse_down(MouseButton::Navigate(gpui::NavigationDirection::Forward)), v(&["Alt", "Mouse5"]));
    }
}
