//! Physical key identity. Bindings name keys by DOM `KeyboardEvent.code`
//! (layout-independent, recorded in the renderer); the listeners see
//! evdev codes (X11 keycode - 8) or Windows set-1 scancodes. `KEYS` is
//! generated from Chromium's `ui/events/keycodes/dom/dom_code_data.inc`
//! (columns: code, evdev, win; `0xe0xx` = E0-extended, 0 = none) so the
//! three spellings agree with what Chromium itself reports.

/// (DOM code, evdev code, Windows scancode)
const KEYS: &[(&str, u16, u16)] = &[
    ("KeyA", 30, 0x001e),
    ("KeyB", 48, 0x0030),
    ("KeyC", 46, 0x002e),
    ("KeyD", 32, 0x0020),
    ("KeyE", 18, 0x0012),
    ("KeyF", 33, 0x0021),
    ("KeyG", 34, 0x0022),
    ("KeyH", 35, 0x0023),
    ("KeyI", 23, 0x0017),
    ("KeyJ", 36, 0x0024),
    ("KeyK", 37, 0x0025),
    ("KeyL", 38, 0x0026),
    ("KeyM", 50, 0x0032),
    ("KeyN", 49, 0x0031),
    ("KeyO", 24, 0x0018),
    ("KeyP", 25, 0x0019),
    ("KeyQ", 16, 0x0010),
    ("KeyR", 19, 0x0013),
    ("KeyS", 31, 0x001f),
    ("KeyT", 20, 0x0014),
    ("KeyU", 22, 0x0016),
    ("KeyV", 47, 0x002f),
    ("KeyW", 17, 0x0011),
    ("KeyX", 45, 0x002d),
    ("KeyY", 21, 0x0015),
    ("KeyZ", 44, 0x002c),
    ("Digit1", 2, 0x0002),
    ("Digit2", 3, 0x0003),
    ("Digit3", 4, 0x0004),
    ("Digit4", 5, 0x0005),
    ("Digit5", 6, 0x0006),
    ("Digit6", 7, 0x0007),
    ("Digit7", 8, 0x0008),
    ("Digit8", 9, 0x0009),
    ("Digit9", 10, 0x000a),
    ("Digit0", 11, 0x000b),
    ("Enter", 28, 0x001c),
    ("Escape", 1, 0x0001),
    ("Backspace", 14, 0x000e),
    ("Tab", 15, 0x000f),
    ("Space", 57, 0x0039),
    ("Minus", 12, 0x000c),
    ("Equal", 13, 0x000d),
    ("BracketLeft", 26, 0x001a),
    ("BracketRight", 27, 0x001b),
    ("Backslash", 43, 0x002b),
    ("Semicolon", 39, 0x0027),
    ("Quote", 40, 0x0028),
    ("Backquote", 41, 0x0029),
    ("Comma", 51, 0x0033),
    ("Period", 52, 0x0034),
    ("Slash", 53, 0x0035),
    ("CapsLock", 58, 0x003a),
    ("F1", 59, 0x003b),
    ("F2", 60, 0x003c),
    ("F3", 61, 0x003d),
    ("F4", 62, 0x003e),
    ("F5", 63, 0x003f),
    ("F6", 64, 0x0040),
    ("F7", 65, 0x0041),
    ("F8", 66, 0x0042),
    ("F9", 67, 0x0043),
    ("F10", 68, 0x0044),
    ("F11", 87, 0x0057),
    ("F12", 88, 0x0058),
    ("PrintScreen", 99, 0xe037),
    ("ScrollLock", 70, 0x0046),
    ("Pause", 119, 0x0045),
    ("Insert", 110, 0xe052),
    ("Home", 102, 0xe047),
    ("PageUp", 104, 0xe049),
    ("Delete", 111, 0xe053),
    ("End", 107, 0xe04f),
    ("PageDown", 109, 0xe051),
    ("ArrowRight", 106, 0xe04d),
    ("ArrowLeft", 105, 0xe04b),
    ("ArrowDown", 108, 0xe050),
    ("ArrowUp", 103, 0xe048),
    ("NumLock", 69, 0xe045),
    ("NumpadDivide", 98, 0xe035),
    ("NumpadMultiply", 55, 0x0037),
    ("NumpadSubtract", 74, 0x004a),
    ("NumpadAdd", 78, 0x004e),
    ("NumpadEnter", 96, 0xe01c),
    ("Numpad1", 79, 0x004f),
    ("Numpad2", 80, 0x0050),
    ("Numpad3", 81, 0x0051),
    ("Numpad4", 75, 0x004b),
    ("Numpad5", 76, 0x004c),
    ("Numpad6", 77, 0x004d),
    ("Numpad7", 71, 0x0047),
    ("Numpad8", 72, 0x0048),
    ("Numpad9", 73, 0x0049),
    ("Numpad0", 82, 0x0052),
    ("NumpadDecimal", 83, 0x0053),
    ("IntlBackslash", 86, 0x0056),
    ("ContextMenu", 127, 0xe05d),
    ("Power", 116, 0xe05e),
    ("NumpadEqual", 117, 0x0059),
    ("F13", 183, 0x0064),
    ("F14", 184, 0x0065),
    ("F15", 185, 0x0066),
    ("F16", 186, 0x0067),
    ("F17", 187, 0x0068),
    ("F18", 188, 0x0069),
    ("F19", 189, 0x006a),
    ("F20", 190, 0x006b),
    ("F21", 191, 0x006c),
    ("F22", 192, 0x006d),
    ("F23", 193, 0x006e),
    ("F24", 194, 0x0076),
    ("Open", 134, 0x0000),
    ("Help", 138, 0xe03b),
    ("Select", 132, 0x0000),
    ("Again", 129, 0x0000),
    ("Undo", 131, 0xe008),
    ("Cut", 137, 0xe017),
    ("Copy", 133, 0xe018),
    ("Paste", 135, 0xe00a),
    ("Find", 136, 0x0000),
    ("AudioVolumeMute", 113, 0xe020),
    ("AudioVolumeUp", 115, 0xe030),
    ("AudioVolumeDown", 114, 0xe02e),
    ("NumpadComma", 121, 0x007e),
    ("IntlRo", 89, 0x0073),
    ("KanaMode", 93, 0x0070),
    ("IntlYen", 124, 0x007d),
    ("Convert", 92, 0x0079),
    ("NonConvert", 94, 0x007b),
    ("Lang1", 122, 0x0072),
    ("Lang2", 123, 0x0071),
    ("Lang3", 90, 0x0078),
    ("Lang4", 91, 0x0077),
    ("Lang5", 85, 0x0000),
    ("NumpadParenLeft", 179, 0x0000),
    ("NumpadParenRight", 180, 0x0000),
    ("ControlLeft", 29, 0x001d),
    ("ShiftLeft", 42, 0x002a),
    ("AltLeft", 56, 0x0038),
    ("MetaLeft", 125, 0xe05b),
    ("ControlRight", 97, 0xe01d),
    ("ShiftRight", 54, 0x0036),
    ("AltRight", 100, 0xe038),
    ("MetaRight", 126, 0xe05c),
    ("BrightnessUp", 225, 0x0000),
    ("BrightnessDown", 224, 0x0000),
    ("MediaPlay", 207, 0x0000),
    ("MediaPause", 201, 0x0000),
    ("MediaRecord", 167, 0x0000),
    ("MediaFastForward", 208, 0x0000),
    ("MediaRewind", 168, 0x0000),
    ("MediaTrackNext", 163, 0xe019),
    ("MediaTrackPrevious", 165, 0xe010),
    ("MediaStop", 166, 0xe024),
    ("Eject", 161, 0xe02c),
    ("MediaPlayPause", 164, 0xe022),
    ("MediaSelect", 171, 0xe06d),
    ("LaunchMail", 155, 0xe06c),
    ("LaunchApp2", 140, 0xe021),
    ("LaunchApp1", 144, 0xe06b),
    ("LaunchControlPanel", 579, 0x0000),
    ("SelectTask", 580, 0x0000),
    ("LaunchScreenSaver", 581, 0x0000),
    ("LaunchAssistant", 583, 0x0000),
    ("BrowserSearch", 217, 0xe065),
    ("BrowserHome", 172, 0xe032),
    ("BrowserBack", 158, 0xe06a),
    ("BrowserForward", 159, 0xe069),
    ("BrowserStop", 128, 0xe068),
    ("BrowserRefresh", 173, 0xe067),
    ("BrowserFavorites", 156, 0xe066),
    ("ZoomToggle", 372, 0x0000),
    ("MailReply", 232, 0x0000),
    ("MailForward", 233, 0x0000),
    ("MailSend", 231, 0x0000),
    ("KeyboardLayoutSelect", 584, 0x0000),
    ("ShowAllWindows", 120, 0x0000),
];

pub const MOUSE_MIDDLE: &str = "Mouse3";
pub const MOUSE_BACK: &str = "Mouse4";
pub const MOUSE_FORWARD: &str = "Mouse5";

pub fn is_mouse(key: &str) -> bool {
    matches!(key, MOUSE_MIDDLE | MOUSE_BACK | MOUSE_FORWARD)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    Control,
    Shift,
    Alt,
    Meta,
}

impl Modifier {
    pub const ALL: [Modifier; 4] = [Modifier::Control, Modifier::Shift, Modifier::Alt, Modifier::Meta];

    fn bit(self) -> u8 {
        match self {
            Modifier::Control => 1,
            Modifier::Shift => 2,
            Modifier::Alt => 4,
            Modifier::Meta => 8,
        }
    }

    /// The portal's (XDG shortcuts spec) modifier spelling.
    fn portal_name(self) -> &'static str {
        match self {
            Modifier::Control => "CTRL",
            Modifier::Shift => "SHIFT",
            Modifier::Alt => "ALT",
            Modifier::Meta => "LOGO",
        }
    }
}

/// Generic modifier names a binding may use (either side matches).
pub fn generic_modifier(key: &str) -> Option<Modifier> {
    match key {
        "Control" => Some(Modifier::Control),
        "Shift" => Some(Modifier::Shift),
        "Alt" => Some(Modifier::Alt),
        "Meta" => Some(Modifier::Meta),
        _ => None,
    }
}

/// The modifier a physical (sided) key belongs to.
pub fn sided_modifier(key: &str) -> Option<Modifier> {
    match key {
        "ControlLeft" | "ControlRight" => Some(Modifier::Control),
        "ShiftLeft" | "ShiftRight" => Some(Modifier::Shift),
        "AltLeft" | "AltRight" => Some(Modifier::Alt),
        "MetaLeft" | "MetaRight" => Some(Modifier::Meta),
        _ => None,
    }
}

/// Bitset of modifiers, for the exact-modifier check on press actions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods(u8);

impl Mods {
    pub fn insert(&mut self, m: Modifier) {
        self.0 |= m.bit();
    }
}

pub fn from_evdev(code: u16) -> Option<&'static str> {
    if code == 0 {
        return None;
    }
    KEYS.iter().find(|k| k.1 == code).map(|k| k.0)
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn from_win_scancode(scancode: u16) -> Option<&'static str> {
    if scancode == 0 {
        return None;
    }
    KEYS.iter().find(|k| k.2 == scancode).map(|k| k.0)
}

/// Canonical `&'static str` for a key a binding names, so the listener
/// side can compare by value without allocating. None for unknown keys.
pub fn intern(key: &str) -> Option<&'static str> {
    if let Some(k) = KEYS.iter().find(|k| k.0 == key) {
        return Some(k.0);
    }
    [MOUSE_MIDDLE, MOUSE_BACK, MOUSE_FORWARD, "Control", "Shift", "Alt", "Meta"]
        .into_iter()
        .find(|k| *k == key)
}

/// XKB keysym name (US layout) for the portal's `preferred_trigger`.
fn keysym_name(code: &str) -> Option<&'static str> {
    const LETTERS: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    const KP_DIGITS: [&str; 10] = [
        "KP_0", "KP_1", "KP_2", "KP_3", "KP_4", "KP_5", "KP_6", "KP_7", "KP_8", "KP_9",
    ];
    const F_KEYS: [&str; 24] = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14",
        "F15", "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
    ];
    if let Some(c) = code.strip_prefix("Key") {
        let b = c.as_bytes();
        if b.len() == 1 && b[0].is_ascii_uppercase() {
            return Some(LETTERS[(b[0] - b'A') as usize]);
        }
    }
    if let Some(d) = code.strip_prefix("Digit").and_then(|d| d.parse::<usize>().ok()) {
        return DIGITS.get(d).copied();
    }
    if let Some(d) = code.strip_prefix("Numpad").and_then(|d| d.parse::<usize>().ok()) {
        return KP_DIGITS.get(d).copied();
    }
    if let Some(n) = code.strip_prefix('F').and_then(|n| n.parse::<usize>().ok()) {
        return n.checked_sub(1).and_then(|i| F_KEYS.get(i)).copied();
    }
    Some(match code {
        "Space" => "space",
        "Enter" => "Return",
        "Escape" => "Escape",
        "Backspace" => "BackSpace",
        "Tab" => "Tab",
        "Minus" => "minus",
        "Equal" => "equal",
        "BracketLeft" => "bracketleft",
        "BracketRight" => "bracketright",
        "Backslash" => "backslash",
        "Semicolon" => "semicolon",
        "Quote" => "apostrophe",
        "Backquote" => "grave",
        "Comma" => "comma",
        "Period" => "period",
        "Slash" => "slash",
        "IntlBackslash" => "less",
        "CapsLock" => "Caps_Lock",
        "PrintScreen" => "Print",
        "ScrollLock" => "Scroll_Lock",
        "Pause" => "Pause",
        "Insert" => "Insert",
        "Home" => "Home",
        "PageUp" => "Prior",
        "Delete" => "Delete",
        "End" => "End",
        "PageDown" => "Next",
        "ArrowRight" => "Right",
        "ArrowLeft" => "Left",
        "ArrowDown" => "Down",
        "ArrowUp" => "Up",
        "NumLock" => "Num_Lock",
        "NumpadDivide" => "KP_Divide",
        "NumpadMultiply" => "KP_Multiply",
        "NumpadSubtract" => "KP_Subtract",
        "NumpadAdd" => "KP_Add",
        "NumpadEnter" => "KP_Enter",
        "NumpadDecimal" => "KP_Decimal",
        "ContextMenu" => "Menu",
        "MediaPlayPause" => "XF86AudioPlay",
        "MediaStop" => "XF86AudioStop",
        "MediaTrackNext" => "XF86AudioNext",
        "MediaTrackPrevious" => "XF86AudioPrev",
        "AudioVolumeMute" => "XF86AudioMute",
        "AudioVolumeUp" => "XF86AudioRaiseVolume",
        "AudioVolumeDown" => "XF86AudioLowerVolume",
        _ => return None,
    })
}

/// `CTRL+SHIFT+m`-style trigger for the GlobalShortcuts portal. None when
/// the combo can't be one (mouse buttons, modifier-only, unmapped key);
/// the desktop then asks the user to pick the key.
pub fn portal_trigger(keys: &[String]) -> Option<String> {
    let mut mods = Mods::default();
    let mut main: Option<&'static str> = None;
    for k in keys {
        if let Some(m) = generic_modifier(k).or_else(|| sided_modifier(k)) {
            mods.insert(m);
        } else if main.is_some() {
            return None;
        } else {
            main = Some(keysym_name(k)?);
        }
    }
    let main = main?;
    let mut out = String::new();
    for m in Modifier::ALL {
        if mods.0 & m.bit() != 0 {
            out.push_str(m.portal_name());
            out.push('+');
        }
    }
    out.push_str(main);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn evdev_and_scancode_spellings_agree() {
        assert_eq!(from_evdev(50), Some("KeyM"));
        assert_eq!(from_win_scancode(0x32), Some("KeyM"));
        assert_eq!(from_evdev(97), Some("ControlRight"));
        assert_eq!(from_win_scancode(0xe01d), Some("ControlRight"));
        assert_eq!(from_win_scancode(0xe045), Some("NumLock"));
        assert_eq!(from_win_scancode(0x0045), Some("Pause"));
        assert_eq!(from_evdev(183), Some("F13"));
        assert_eq!(from_evdev(0), None);
    }

    #[test]
    fn no_duplicate_codes() {
        for (i, a) in KEYS.iter().enumerate() {
            for b in &KEYS[i + 1..] {
                assert_ne!(a.0, b.0, "dup code");
                assert!(a.1 == 0 || a.1 != b.1, "dup evdev {} {} {}", a.1, a.0, b.0);
                assert!(a.2 == 0 || a.2 != b.2, "dup scancode {:x} {} {}", a.2, a.0, b.0);
            }
        }
    }

    #[test]
    fn portal_triggers() {
        assert_eq!(portal_trigger(&keys(&["Control", "Shift", "KeyM"])).as_deref(), Some("CTRL+SHIFT+m"));
        assert_eq!(portal_trigger(&keys(&["Meta", "F13"])).as_deref(), Some("LOGO+F13"));
        assert_eq!(portal_trigger(&keys(&["Backquote"])).as_deref(), Some("grave"));
        assert_eq!(portal_trigger(&keys(&["Numpad5"])).as_deref(), Some("KP_5"));
        assert_eq!(portal_trigger(&keys(&["Mouse4"])), None);
        assert_eq!(portal_trigger(&keys(&["ControlRight"])), None);
        assert_eq!(portal_trigger(&keys(&["KeyA", "KeyB"])), None);
    }

    #[test]
    fn intern_knows_generic_and_mouse_names() {
        assert_eq!(intern("Control"), Some("Control"));
        assert_eq!(intern("Mouse4"), Some("Mouse4"));
        assert_eq!(intern("KeyQ"), Some("KeyQ"));
        assert_eq!(intern("Nope"), None);
    }
}
