//! `decibell_text::input` against Chromium: caret stops, word stops, deletions and undo
//! steps recorded from Electron's Chromium with real key events
//! (`tools/chromium/input-keys.js` → `tests/data/chromium-input.json`; Chromium 130 on
//! Linux). Every difference that is known is listed here with its reason; anything else
//! fails.

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
        assert_eq!(walk(text, false, Motion::WordRight), offsets(&case["right"]), "Ctrl+Right in {text:?}");
        assert_eq!(walk(text, true, Motion::WordLeft), offsets(&case["left"]), "Ctrl+Left in {text:?}");
        assert_eq!(erase(text, false, Motion::WordRight), strings(&case["delete"]), "Ctrl+Delete in {text:?}");
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
    // Left and Right collapse a selection to its edge, whichever way it was made.
    assert_eq!(after("abcdef", 2, 4, Motion::Left, false), expect("left_collapses"));
    assert_eq!(after("abcdef", 2, 4, Motion::Right, false), expect("right_collapses"));
    assert_eq!(after("abcdef", 4, 2, Motion::Left, false), expect("left_collapses_backward"));
    // A word move starts from the caret (the selection's moving end).
    assert_eq!(after("one two three", 4, 10, Motion::WordLeft, false), expect("word_left_from_selection"));
    assert_eq!(after("one two three", 4, 10, Motion::WordRight, false), expect("word_right_from_selection"));
    assert_eq!(after("one two three", 10, 4, Motion::WordRight, false), expect("word_right_from_backward_selection"));
    assert_eq!(after("one two three", 0, 0, Motion::WordRight, true), expect("shift_word_right"));
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
