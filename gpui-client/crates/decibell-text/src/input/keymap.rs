//! Which key does what in a field: Chromium's editing keys on Linux and Windows (the two
//! agree; on macOS the word and line modifiers differ and the command key takes Ctrl's
//! place).
//!
//! The field matches keystrokes itself instead of registering actions in GPUI's keymap: a
//! key that is not an editing key propagates untouched, so the platform (or the IME) inserts
//! its character, and an application binding on the same key in an outer context still
//! works when the field does not use it.

use gpui::Keystroke;

/// A caret move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    WordLeft,
    WordRight,
    /// Home / End: the ends of the visual line.
    LineStart,
    LineEnd,
    Up,
    Down,
    PageUp,
    PageDown,
    DocStart,
    DocEnd,
}

/// What an editing key asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Move the caret; `extend` keeps the anchor (Shift).
    Move { to: Move, extend: bool },
    /// Delete the selection, or from the caret to where the move would go.
    Delete(Move),
    SelectAll,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    Enter { shift: bool },
    Escape,
}

/// The command a keystroke is, if it is an editing key.
pub fn command(keystroke: &Keystroke) -> Option<Command> {
    let modifiers = keystroke.modifiers;
    let mac = cfg!(target_os = "macos");
    // The "do something" modifier (Ctrl; Cmd on macOS) and the "by word" one (Ctrl; Option).
    let (primary, word) = if mac {
        (modifiers.platform && !modifiers.control, modifiers.alt)
    } else {
        (modifiers.control && !modifiers.platform, modifiers.control)
    };
    if !mac && (modifiers.alt || modifiers.platform) {
        return None;
    }
    let shift = modifiers.shift;
    let moved = |to: Move| Some(Command::Move { to, extend: shift });
    let key = keystroke.key.as_str();
    match key {
        "left" if mac && primary => moved(Move::LineStart),
        "right" if mac && primary => moved(Move::LineEnd),
        "up" if mac && primary => moved(Move::DocStart),
        "down" if mac && primary => moved(Move::DocEnd),
        "left" => moved(if word { Move::WordLeft } else { Move::Left }),
        "right" => moved(if word { Move::WordRight } else { Move::Right }),
        "up" if !word => moved(Move::Up),
        "down" if !word => moved(Move::Down),
        "home" => moved(if primary { Move::DocStart } else { Move::LineStart }),
        "end" => moved(if primary { Move::DocEnd } else { Move::LineEnd }),
        "pageup" if !primary => moved(Move::PageUp),
        "pagedown" if !primary => moved(Move::PageDown),
        "backspace" if mac && primary => Some(Command::Delete(Move::LineStart)),
        "backspace" => Some(Command::Delete(if word { Move::WordLeft } else { Move::Left })),
        "delete" if shift && !primary => Some(Command::Cut),
        "delete" => Some(Command::Delete(if word { Move::WordRight } else { Move::Right })),
        "insert" if shift && !primary => Some(Command::Paste),
        "insert" if primary && !shift => Some(Command::Copy),
        "enter" if !primary => Some(Command::Enter { shift }),
        "escape" if !primary && !shift => Some(Command::Escape),
        "a" if primary && !shift => Some(Command::SelectAll),
        "c" if primary && !shift => Some(Command::Copy),
        "x" if primary && !shift => Some(Command::Cut),
        "v" if primary => Some(Command::Paste),
        "z" if primary && shift => Some(Command::Redo),
        "z" if primary => Some(Command::Undo),
        "y" if primary && !shift && !mac => Some(Command::Redo),
        _ => None,
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    fn of(keystroke: &'static str) -> Option<Command> {
        command(&Keystroke::parse(keystroke).unwrap())
    }

    #[test]
    fn the_editing_keys_of_linux_and_windows() {
        let moved = |to, extend| Some(Command::Move { to, extend });
        assert_eq!(of("left"), moved(Move::Left, false));
        assert_eq!(of("shift-right"), moved(Move::Right, true));
        assert_eq!(of("ctrl-left"), moved(Move::WordLeft, false));
        assert_eq!(of("ctrl-shift-right"), moved(Move::WordRight, true));
        assert_eq!(of("home"), moved(Move::LineStart, false));
        assert_eq!(of("shift-end"), moved(Move::LineEnd, true));
        assert_eq!(of("ctrl-home"), moved(Move::DocStart, false));
        assert_eq!(of("ctrl-shift-end"), moved(Move::DocEnd, true));
        assert_eq!(of("up"), moved(Move::Up, false));
        assert_eq!(of("shift-pagedown"), moved(Move::PageDown, true));
        assert_eq!(of("backspace"), Some(Command::Delete(Move::Left)));
        assert_eq!(of("shift-backspace"), Some(Command::Delete(Move::Left)));
        assert_eq!(of("ctrl-backspace"), Some(Command::Delete(Move::WordLeft)));
        assert_eq!(of("delete"), Some(Command::Delete(Move::Right)));
        assert_eq!(of("ctrl-delete"), Some(Command::Delete(Move::WordRight)));
        assert_eq!(of("ctrl-a"), Some(Command::SelectAll));
        assert_eq!(of("ctrl-c"), Some(Command::Copy));
        assert_eq!(of("ctrl-insert"), Some(Command::Copy));
        assert_eq!(of("ctrl-x"), Some(Command::Cut));
        assert_eq!(of("shift-delete"), Some(Command::Cut));
        assert_eq!(of("ctrl-v"), Some(Command::Paste));
        assert_eq!(of("ctrl-shift-v"), Some(Command::Paste));
        assert_eq!(of("shift-insert"), Some(Command::Paste));
        assert_eq!(of("ctrl-z"), Some(Command::Undo));
        assert_eq!(of("ctrl-shift-z"), Some(Command::Redo));
        assert_eq!(of("ctrl-y"), Some(Command::Redo));
        assert_eq!(of("enter"), Some(Command::Enter { shift: false }));
        assert_eq!(of("shift-enter"), Some(Command::Enter { shift: true }));
        assert_eq!(of("escape"), Some(Command::Escape));
    }

    #[test]
    fn everything_else_is_left_to_the_platform_and_the_app() {
        for keystroke in ["a", "shift-a", "space", "tab", "ctrl-k", "alt-left", "ctrl-enter", "f5", "alt-a", "ctrl-alt-v", "super-a"] {
            assert_eq!(of(keystroke), None, "{keystroke}");
        }
    }
}
