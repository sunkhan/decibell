//! `decibell_text::input` against Chromium: caret stops, word stops, deletions and undo
//! steps recorded from Electron's Chromium with real key events
//! (`tools/chromium/input-keys.js` → `tests/data/chromium-input.json`; Chromium 130 on
//! Linux). Every difference that is known is listed here with its reason; anything else
//! fails.
//!
//! **Windows.** The recording is Linux's, and Chromium moves forward by word differently on
//! Windows: `EditingBehavior::ShouldSkipSpaceWhenMovingRight()` is true only for
//! `kEditingWindowsBehavior` ("ctrl+right arrow moves caret to `abc |def |hij |opq` on Windows
//! and `abc| def| hij| opq|` on Mac and Linux", `core/editing/editing_behavior.h`), and
//! `NextWordPositionInternal` (`core/editing/position_units_word.cc`) then returns every stop
//! through `SkipWhitespaceIfNeeded`: the Linux stop, moved past the whitespace after it but
//! not past a line break. `SelectionModifier` uses the same position to move and to extend
//! (`NextWordPositionForPlatform` in both `ModifyMovingForward` and
//! `ModifyExtendingForwardInternal`), so Ctrl+Delete and Shift+Ctrl+Right follow. Backward
//! movement is the same on every platform. There is no Windows recording here: the Windows
//! expectations below are the recorded Linux ones put through that one rule
//! ([`skip_spaces`]), and `the_windows_rule_is_the_codes` holds the code to it on any platform.

use decibell_text::input::boundaries::{byte_to_utf16, next_word, prev_word, utf16_to_byte};
use decibell_text::input::{Buffer, EditKind, Motion, Rules, Selection};
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("data/chromium-input.json")).unwrap()
}

fn offsets(value: &Value) -> Vec<usize> {
    value.as_array().unwrap().iter().map(|offset| offset.as_u64().unwrap() as usize).collect()
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|text| text.as_str().unwrap().to_string()).collect()
}

/// Presses a motion key until the caret stops; the stops in UTF-16 units, as the page reads
/// `selectionStart`.
fn walk(text: &str, from_end: bool, motion: Motion) -> Vec<usize> {
    let mut buffer = Buffer::default();
    buffer.set_text(text);
    buffer.move_to(if from_end { text.len() } else { 0 }, false);
    let mut stops = vec![byte_to_utf16(text, buffer.selection().head)];
    while buffer.move_by(motion, false) {
        stops.push(byte_to_utf16(text, buffer.selection().head));
    }
    stops
}

/// Presses a deleting key until nothing changes; the value after each press.
fn erase(text: &str, from_end: bool, motion: Motion) -> Vec<String> {
    let mut buffer = Buffer::default();
    buffer.set_text(text);
    buffer.move_to(if from_end { text.len() } else { 0 }, false);
    let mut values = Vec::new();
    while buffer.delete(motion) {
        values.push(buffer.text().to_string());
    }
    values
}

/// `offset` (UTF-16 units) moved past the spaces that follow it, never past a line break:
/// Blink's `SkipWhitespaceIfNeeded`.
fn skip_spaces(text: &str, offset: usize) -> usize {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut at = offset;
    while let Some(c) = units.get(at).and_then(|&unit| char::from_u32(unit as u32)) {
        if !c.is_whitespace() || c == '\n' || c == '\r' {
            break;
        }
        at += 1;
    }
    at
}

/// The Ctrl+Right stops of Windows from the recorded Linux ones.
fn windows_right(text: &str, linux: &[usize]) -> Vec<usize> {
    let mut stops: Vec<usize> = Vec::new();
    for (index, &stop) in linux.iter().enumerate() {
        // The first entry is where the walk starts, not a stop.
        let stop = if index == 0 { stop } else { skip_spaces(text, stop) };
        if stops.last() != Some(&stop) {
            stops.push(stop);
        }
    }
    stops
}

/// The values after each Ctrl+Delete on Windows from the recorded Linux ones: the spaces left
/// at the front go with the word.
fn windows_delete(linux: &[String]) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    for value in linux {
        let value = value.trim_start_matches(|c: char| c.is_whitespace() && c != '\n' && c != '\r').to_string();
        if values.last() != Some(&value) {
            values.push(value);
        }
    }
    values
}

const WINDOWS: bool = cfg!(target_os = "windows");

/// Chromium finds the words of Chinese and Japanese text with ICU's dictionaries
/// (日本語 | の | テキスト); here a run of such letters is one word.
const DICTIONARY_WORDS: &[&str] = &["日本語のテキスト"];

#[test]
fn word_stops_are_chromiums() {
    let vectors = vectors();
    let mut checked = 0;
    for case in vectors["words"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        if DICTIONARY_WORDS.contains(&text) {
            // Both ends agree; the stops in between are the dictionary's.
            assert_eq!(walk(text, false, Motion::WordRight), [0, 8], "{text:?}");
            continue;
        }
        let (mut right, mut delete) = (offsets(&case["right"]), strings(&case["delete"]));
        if WINDOWS {
            right = windows_right(text, &right);
            delete = windows_delete(&delete);
        }
        assert_eq!(walk(text, false, Motion::WordRight), right, "Ctrl+Right in {text:?}");
        assert_eq!(walk(text, true, Motion::WordLeft), offsets(&case["left"]), "Ctrl+Left in {text:?}");
        assert_eq!(erase(text, false, Motion::WordRight), delete, "Ctrl+Delete in {text:?}");
        assert_eq!(erase(text, true, Motion::WordLeft), strings(&case["backspace"]), "Ctrl+Backspace in {text:?}");
        checked += 1;
    }
    assert_eq!(checked, 15);
    // The free functions agree with the buffer (which adds atoms and the password rule).
    let text = "hello, wörld_1  foo.bar";
    assert_eq!(byte_to_utf16(text, next_word(text, 0, false)), 5);
    assert_eq!(byte_to_utf16(text, prev_word(text, text.len())), 20);
    assert_eq!(utf16_to_byte(text, 14), 15);
}

/// Runs everywhere: the code's Windows branch (`next_word(.., skip_space = true)`) gives, for
/// every recorded text, the Linux recording put through Blink's one Windows rule.
#[test]
fn the_windows_rule_is_the_codes() {
    let vectors = vectors();
    for case in vectors["words"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        if DICTIONARY_WORDS.contains(&text) {
            continue;
        }
        let mut stops = vec![0];
        let mut at = 0;
        while at < text.len() {
            let next = next_word(text, at, true);
            if next == at {
                break;
            }
            at = next;
            stops.push(byte_to_utf16(text, at));
        }
        assert_eq!(stops, windows_right(text, &offsets(&case["right"])), "Ctrl+Right on Windows in {text:?}");
    }
    // The example of `editing_behavior.h`.
    let text = "abc def hij opq";
    let stops: Vec<usize> = std::iter::successors(Some(0), |&at| (at < text.len()).then(|| next_word(text, at, true))).collect();
    assert_eq!(stops, [0, 4, 8, 12, 15]);
    assert_eq!(skip_spaces("a  \nb", 1), 3);
}

#[test]
fn caret_stops_and_deletions_are_chromiums() {
    let vectors = vectors();
    for case in vectors["graphemes"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        assert_eq!(walk(text, false, Motion::Right), offsets(&case["right"]), "Right in {text:?}");
        assert_eq!(walk(text, true, Motion::Left), offsets(&case["left"]), "Left in {text:?}");
        assert_eq!(erase(text, true, Motion::Left), strings(&case["backspace"]), "Backspace in {text:?}");
        assert_eq!(erase(text, false, Motion::Right), strings(&case["delete"]), "Delete in {text:?}");
    }
    assert_eq!(vectors["graphemes"].as_array().unwrap().len(), 8);
}

#[test]
fn undo_steps_are_chromiums() {
    let vectors = vectors();
    for case in vectors["undo"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut buffer = Buffer::default();
        for step in case["steps"].as_array().unwrap() {
            let kind = step[0].as_str().unwrap();
            let control = step.get(2).is_some_and(|modifiers| !modifiers.as_array().unwrap().is_empty());
            match (kind, &step[1]) {
                ("type", text) => {
                    for c in text.as_str().unwrap().chars() {
                        buffer.insert(c.encode_utf8(&mut [0; 4]), EditKind::Typing);
                    }
                }
                // Chromium has no timer: a pause changes nothing.
                ("wait", _) => {}
                ("key", key) => match (key.as_str().unwrap(), control) {
                    ("Backspace", false) => drop(buffer.delete(Motion::Left)),
                    ("Backspace", true) => drop(buffer.delete(Motion::WordLeft)),
                    ("Delete", false) => drop(buffer.delete(Motion::Right)),
                    ("Left", false) => drop(buffer.move_by(Motion::Left, false)),
                    ("Right", false) => drop(buffer.move_by(Motion::Right, false)),
                    ("Enter", false) => drop(buffer.insert("\n", EditKind::Typing)),
                    ("a", true) => drop(buffer.select_all()),
                    other => panic!("{name}: no such key {other:?}"),
                },
                // `document.execCommand("insertText")` is typing to Blink.
                ("insert", text) => drop(buffer.insert(text.as_str().unwrap(), EditKind::Typing)),
                other => panic!("{name}: no such step {other:?}"),
            }
        }
        // The page's <textarea> is read back without line breaks only where none were typed.
        let typed = case["typed"].as_str().unwrap();
        assert_eq!(buffer.text().replace('\n', ""), typed.replace('\n', ""), "{name}: typed");
        let mut undone = Vec::new();
        while buffer.undo() {
            undone.push(buffer.text().to_string());
        }
        assert_eq!(undone, strings(&case["undo"]), "{name}: undo");
        let mut redone = Vec::new();
        while buffer.redo() {
            redone.push(buffer.text().replace('\n', ""));
        }
        let expected: Vec<String> = strings(&case["redo"]).iter().map(|value| value.replace('\n', "")).collect();
        assert_eq!(redone, expected, "{name}: redo");
    }
    assert_eq!(vectors["undo"].as_array().unwrap().len(), 11);
}

#[test]
fn the_rest_is_chromiums() {
    let misc = &vectors()["misc"];
    let after = |text: &str, anchor: usize, head: usize, motion: Motion, extend: bool| {
        let mut buffer = Buffer::default();
        buffer.set_text(text);
        buffer.select(Selection { anchor, head });
        buffer.move_by(motion, extend);
        (buffer.selection().start(), buffer.selection().end())
    };
    let expect = |name: &str| {
        let value = &misc[name];
        (value[0].as_u64().unwrap() as usize, value[1].as_u64().unwrap() as usize)
    };
    // A forward word move ends past the spaces on Windows (the module comment): the recorded
    // end, moved. ASCII texts here, so bytes are UTF-16 units.
    let forward = |text: &str, name: &str, extend: bool| {
        let (start, end) = expect(name);
        match (WINDOWS, extend) {
            (false, _) => (start, end),
            (true, true) => (start, skip_spaces(text, end)),
            (true, false) => (skip_spaces(text, end), skip_spaces(text, end)),
        }
    };
    // Left and Right collapse a selection to its edge, whichever way it was made.
    assert_eq!(after("abcdef", 2, 4, Motion::Left, false), expect("left_collapses"));
    assert_eq!(after("abcdef", 2, 4, Motion::Right, false), expect("right_collapses"));
    assert_eq!(after("abcdef", 4, 2, Motion::Left, false), expect("left_collapses_backward"));
    // A word move starts from the caret (the selection's moving end).
    assert_eq!(after("one two three", 4, 10, Motion::WordLeft, false), expect("word_left_from_selection"));
    assert_eq!(after("one two three", 4, 10, Motion::WordRight, false), forward("one two three", "word_right_from_selection", false));
    assert_eq!(after("one two three", 10, 4, Motion::WordRight, false), forward("one two three", "word_right_from_backward_selection", false));
    assert_eq!(after("one two three", 0, 0, Motion::WordRight, true), forward("one two three", "shift_word_right", true));
    // A single line: line breaks become spaces; `maxlength` cuts the insertion on a character.
    let line = |max: Option<usize>, value: &str, at: usize, insert: &str| {
        let mut buffer = Buffer::new(Rules {
            multiline: false,
            max_utf16: max,
            ..Rules::default()
        });
        buffer.set_text(value);
        buffer.move_to(at, false);
        buffer.insert(insert, EditKind::Other);
        buffer.text().to_string()
    };
    assert_eq!(line(None, "", 0, "one\r\ntwo\nthree\rfour"), misc["input_newlines"].as_str().unwrap());
    assert_eq!(line(Some(6), "abcd", 2, "😀😀x"), misc["maxlength_insert"].as_str().unwrap());
    assert_eq!(line(Some(3), "", 0, "a😀😀"), misc["maxlength_odd"].as_str().unwrap());
    // What needs a layout (Up / Down, Home / End, the goal column) is in the same file and
    // checked by the `input-typing` self-test and `input::tests`:
    for name in ["input_up", "input_down", "area_up_first_line", "area_down_last_line", "home_end", "ctrl_home_end", "goal_column"] {
        assert!(!misc[name].is_null(), "{name} is missing from the vectors");
    }
}
