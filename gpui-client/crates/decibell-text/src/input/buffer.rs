//! The text of a field: a `String`, a selection, the rules for what may be typed, and the
//! undo history. No layout and no GPUI: everything here is unit-tested as plain data.
//!
//! Offsets are bytes into the text, always on a caret stop: a grapheme boundary that is
//! not inside an **atom**. An atom is a piece of the text that behaves as one object (an
//! emoji in the composer): one caret step, one deletion, drawn as a box by the element and
//! copied as its characters.

use std::borrow::Cow;
use std::ops::Range;

use decibell_richtext::emoji::EmojiAsset;

use super::boundaries::{self, utf16_len};
use super::history::{Edit, EditKind, History, Selection};

/// What a field accepts. The defaults are a multi-line field without limits.
#[derive(Clone, Copy, Debug)]
pub struct Rules {
    /// `false`: one line. A pasted line break becomes a space, as in Chromium's `<input>`.
    pub multiline: bool,
    /// HTML `maxlength`: at most this many UTF-16 code units. Text that would exceed it is
    /// cut where it is inserted (Chromium inserts what still fits).
    pub max_utf16: Option<usize>,
    /// The renderer's `clampLine(value, n)` (`lib/utf8.ts`), for fields the community
    /// server stores under a byte cap: runs of line breaks and tabs become one space,
    /// other control characters go, and the **value's tail** is cut to at most this many
    /// UTF-8 bytes on a character boundary.
    pub max_utf8: Option<usize>,
    /// Only these characters can be inserted ([`numeric`] for a number field).
    pub accept: Option<fn(char) -> bool>,
    /// The composer: emoji become atoms, and a known `:shortcode:` becomes its emoji as
    /// soon as it is complete (`RichInput.tsx` `tokenize`).
    pub emoji_atoms: bool,
    /// A password: drawn as bullets, never copied or cut, one word for Ctrl+arrows.
    pub masked: bool,
    /// Selectable and copyable, not editable.
    pub read_only: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            multiline: true,
            max_utf16: None,
            max_utf8: None,
            accept: None,
            emoji_atoms: false,
            masked: false,
            read_only: false,
        }
    }
}

/// What Chromium lets into `<input type="number">`: digits, a sign, a decimal point, an
/// exponent.
pub fn numeric(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E')
}

/// An atom: bytes `start..end` of the text are one object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtomSpan {
    pub start: usize,
    pub end: usize,
    /// The Twemoji drawing, when there is one (otherwise the characters are drawn).
    pub asset: Option<EmojiAsset>,
}

/// A horizontal or document move (the ones that need no layout).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    DocStart,
    DocEnd,
}

/// See the module docs.
#[derive(Debug, Default)]
pub struct Buffer {
    text: String,
    selection: Selection,
    /// The IME's preedit text, part of `text` until it is committed or cancelled.
    marked: Option<Range<usize>>,
    atoms: Vec<AtomSpan>,
    history: History,
    rules: Rules,
    /// Bumped by every change of the text.
    revision: u64,
}

impl Buffer {
    pub fn new(rules: Rules) -> Self {
        Self {
            rules,
            ..Self::default()
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn rules(&self) -> &Rules {
        &self.rules
    }

    /// Changes the rules. The text is not re-checked against them: set them before the
    /// value.
    pub fn set_rules(&mut self, rules: Rules) {
        self.rules = rules;
        self.retokenize(false);
    }

    pub fn selection(&self) -> Selection {
        self.selection
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn atoms(&self) -> &[AtomSpan] {
        &self.atoms
    }

    /// A number that changes whenever the text does.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// (undo steps, redo steps, bytes of history).
    pub fn history_usage(&self) -> (usize, usize, usize) {
        self.history.usage()
    }

    /// The selected text as the clipboard gets it: the characters, atoms included. `None`
    /// for an empty selection and for a password.
    pub fn selected_text(&self) -> Option<&str> {
        if self.rules.masked || self.selection.is_empty() {
            return None;
        }
        Some(&self.text[self.selection.range()])
    }

    // ---- caret stops ----

    fn atom_inside(&self, offset: usize) -> Option<&AtomSpan> {
        let at = self.atoms.partition_point(|atom| atom.end <= offset);
        self.atoms.get(at).filter(|atom| atom.start < offset)
    }

    /// The caret stop after `offset`.
    pub fn next_stop(&self, offset: usize) -> usize {
        let at = self.atoms.partition_point(|atom| atom.end <= offset);
        if let Some(atom) = self.atoms.get(at).filter(|atom| atom.start <= offset) {
            return atom.end;
        }
        let next = boundaries::next_grapheme(&self.text, offset);
        self.atom_inside(next).map_or(next, |atom| atom.end)
    }

    /// The caret stop before `offset`.
    pub fn prev_stop(&self, offset: usize) -> usize {
        let at = self.atoms.partition_point(|atom| atom.end < offset);
        if let Some(atom) = self.atoms.get(at).filter(|atom| atom.start < offset) {
            return atom.start;
        }
        let prev = boundaries::prev_grapheme(&self.text, offset);
        self.atom_inside(prev).map_or(prev, |atom| atom.start)
    }

    /// `offset` moved to the nearest caret stop at or before it.
    pub fn snap(&self, offset: usize) -> usize {
        let offset = boundaries::floor_grapheme(&self.text, offset);
        self.atom_inside(offset).map_or(offset, |atom| atom.start)
    }

    /// Where `motion` takes the caret from the current selection. Left / Right without
    /// extending collapse a selection to its edge, as in Chromium.
    pub fn target(&self, motion: Motion, extend: bool) -> usize {
        let head = self.selection.head;
        let collapse = !extend && !self.selection.is_empty();
        match motion {
            Motion::Left if collapse => self.selection.start(),
            Motion::Right if collapse => self.selection.end(),
            Motion::Left => self.prev_stop(head),
            Motion::Right => self.next_stop(head),
            Motion::WordLeft | Motion::DocStart if self.rules.masked => 0,
            Motion::WordRight | Motion::DocEnd if self.rules.masked => self.text.len(),
            Motion::WordLeft => self.snap(boundaries::prev_word(&self.text, head)),
            Motion::WordRight => {
                let next = boundaries::next_word(&self.text, head, cfg!(target_os = "windows"));
                self.atom_inside(next).map_or(next, |atom| atom.end)
            }
            Motion::DocStart => 0,
            Motion::DocEnd => self.text.len(),
        }
    }

    // ---- selection ----

    /// Moves the caret to `offset`, keeping the anchor when `extend`. Ends the open
    /// typing step. Returns whether the selection changed.
    pub fn move_to(&mut self, offset: usize, extend: bool) -> bool {
        let head = self.snap(offset);
        let next = Selection {
            anchor: if extend { self.selection.anchor } else { head },
            head,
        };
        self.select(next)
    }

    /// Sets the selection (offsets are snapped to caret stops).
    pub fn select(&mut self, selection: Selection) -> bool {
        let next = Selection {
            anchor: self.snap(selection.anchor),
            head: self.snap(selection.head),
        };
        self.history.close();
        if next == self.selection {
            return false;
        }
        self.selection = next;
        true
    }

    pub fn move_by(&mut self, motion: Motion, extend: bool) -> bool {
        self.move_to(self.target(motion, extend), extend)
    }

    pub fn select_all(&mut self) -> bool {
        self.select(Selection {
            anchor: 0,
            head: self.text.len(),
        })
    }

    /// The word a double click at `offset` selects (an atom is its own word).
    pub fn word_at(&self, offset: usize) -> (usize, usize) {
        if self.rules.masked {
            return (0, self.text.len());
        }
        let at = self.atoms.partition_point(|atom| atom.end <= offset);
        if let Some(atom) = self.atoms.get(at).filter(|atom| atom.start <= offset) {
            return (atom.start, atom.end);
        }
        let (start, end) = boundaries::word_at(&self.text, offset);
        (self.snap(start), self.atom_inside(end).map_or(end, |atom| atom.end))
    }

    /// The hard line around `offset`, without its line break (a triple click).
    pub fn line_at(&self, offset: usize) -> (usize, usize) {
        boundaries::line_at(&self.text, offset)
    }

    // ---- edits ----

    /// The one place the text changes. Records the edit, moves the caret behind what was
    /// inserted, drops atoms (the caller re-tokenizes).
    fn splice(&mut self, range: Range<usize>, insert: &str, kind: EditKind) {
        let before = self.selection;
        let removed = self.text[range.clone()].to_string();
        self.text.replace_range(range.clone(), insert);
        self.shift_atoms(&range, insert.len());
        self.selection = Selection::caret(range.start + insert.len());
        self.revision += 1;
        self.history.record(
            Edit {
                at: range.start,
                removed,
                inserted: insert.to_string(),
            },
            kind,
            before,
            self.selection,
        );
    }

    /// The same as [`splice`](Self::splice) for a follow-up change that belongs to the
    /// step just recorded (a clamp, a shortcode becoming its emoji).
    fn amend(&mut self, range: Range<usize>, insert: &str) {
        let removed = self.text[range.clone()].to_string();
        self.text.replace_range(range.clone(), insert);
        self.shift_atoms(&range, insert.len());
        let shift = |offset: usize| {
            if offset >= range.end {
                offset - range.len() + insert.len()
            } else if offset > range.start {
                range.start + insert.len()
            } else {
                offset
            }
        };
        self.selection = Selection {
            anchor: shift(self.selection.anchor),
            head: shift(self.selection.head),
        };
        self.revision += 1;
        self.history.amend(
            Edit {
                at: range.start,
                removed,
                inserted: insert.to_string(),
            },
            self.selection,
        );
    }

    /// Keeps the atoms in step with a replacement of `range` by `inserted` bytes: the ones
    /// it touched go (the caller scans the edited lines again), the ones after it move.
    fn shift_atoms(&mut self, range: &Range<usize>, inserted: usize) {
        if self.atoms.is_empty() {
            return;
        }
        self.atoms.retain(|atom| atom.end <= range.start || atom.start >= range.end);
        for atom in self.atoms.iter_mut().filter(|atom| atom.start >= range.end) {
            atom.start = atom.start - range.len() + inserted;
            atom.end = atom.end - range.len() + inserted;
        }
    }

    /// What of `input` may replace `replaced`: line breaks, the character filter and the
    /// UTF-16 limit applied.
    fn admit<'a>(&self, input: &'a str, replaced: Range<usize>) -> Cow<'a, str> {
        let rules = &self.rules;
        let mut text = Cow::Borrowed(input);
        if text.contains('\r') {
            text = Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"));
        }
        if rules.max_utf8.is_some() && text.contains(|c: char| c.is_control()) {
            // clampLine: `[\r\n\t]+` → one space, then the other control characters go.
            let mut line = String::with_capacity(text.len());
            let mut in_break = false;
            for c in text.chars() {
                if matches!(c, '\n' | '\t') {
                    if !in_break {
                        line.push(' ');
                    }
                    in_break = true;
                } else {
                    in_break = false;
                    if !c.is_control() {
                        line.push(c);
                    }
                }
            }
            text = Cow::Owned(line);
        } else if !rules.multiline && text.contains('\n') {
            text = Cow::Owned(text.replace('\n', " "));
        }
        if let Some(accept) = rules.accept
            && !text.chars().all(accept)
        {
            text = Cow::Owned(text.chars().filter(|c| accept(*c)).collect());
        }
        if let Some(max) = rules.max_utf16 {
            let kept = utf16_len(&self.text) - utf16_len(&self.text[replaced]);
            let room = max.saturating_sub(kept);
            if utf16_len(&text) > room {
                let cut = boundaries::floor_grapheme(&text, boundaries::utf16_to_byte(&text, room));
                text = Cow::Owned(text[..cut].to_string());
            }
        }
        text
    }

    /// Replaces `range` with `input` after the rules have had their say. Returns whether
    /// the text changed.
    pub fn replace(&mut self, range: Range<usize>, input: &str, kind: EditKind) -> bool {
        if self.rules.read_only {
            return false;
        }
        let range = self.snap(range.start)..self.snap_up(range.end);
        let insert = self.admit(input, range.clone());
        if range.is_empty() && insert.is_empty() {
            return false;
        }
        self.marked = None;
        let edited = range.start..range.start + insert.len();
        self.splice(range, &insert, kind);
        self.settle(kind, edited);
        true
    }

    fn snap_up(&self, offset: usize) -> usize {
        let down = self.snap(offset);
        if down == offset.min(self.text.len()) { down } else { self.next_stop(down) }
    }

    /// After an edit that left `edited` (bytes of the new text) behind: the byte cap, then
    /// the atoms of the lines it touched.
    fn settle(&mut self, kind: EditKind, edited: Range<usize>) {
        if let Some(max) = self.rules.max_utf8
            && self.text.len() > max
        {
            let mut cut = max;
            while !self.text.is_char_boundary(cut) {
                cut -= 1;
            }
            self.amend(cut..self.text.len(), "");
        }
        // A composition's text is the IME's until it commits.
        let replace = kind != EditKind::Composition || self.marked.is_none();
        let edited = edited.start.min(self.text.len())..edited.end.min(self.text.len());
        self.retokenize_lines(edited, replace);
    }

    /// Finds the atoms of the whole text again; with `replace`, turns complete shortcodes
    /// into their emoji first.
    fn retokenize(&mut self, replace: bool) {
        self.atoms.clear();
        self.retokenize_lines(0..self.text.len(), replace);
    }

    /// The same for the lines `span` touches: an emoji never crosses a line break, so the
    /// other lines' atoms stand, and typing costs a scan of one line, not of the draft.
    fn retokenize_lines(&mut self, span: Range<usize>, replace: bool) {
        if !self.rules.emoji_atoms || self.rules.masked {
            self.atoms.clear();
            return;
        }
        let start = self.text[..span.start].rfind('\n').map_or(0, |at| at + 1);
        let line_end = |text: &str, from: usize| text[from..].find('\n').map_or(text.len(), |at| from + at);
        let mut end = line_end(&self.text, span.end);
        if replace && self.text[start..end].contains(':') {
            // Back to front, so the earlier ranges stay valid.
            let lines = &self.text[start..end];
            let shortcodes: Vec<(Range<usize>, &'static str)> = decibell_richtext::composer::emoji_atoms(lines)
                .into_iter()
                .filter(|atom| atom.native != &lines[atom.range()])
                .filter_map(|atom| {
                    let id = lines[atom.range()].trim_matches(':');
                    Some((start + atom.range.0..start + atom.range.1, decibell_richtext::emoji::shortcode(id)?))
                })
                .collect();
            // What follows these lines is untouched: the lines end where it starts.
            let after = self.text.len() - end;
            for (range, native) in shortcodes.into_iter().rev() {
                self.amend(range, native);
            }
            end = self.text.len() - after;
        }
        let first = self.atoms.partition_point(|atom| atom.end <= start);
        let last = self.atoms.partition_point(|atom| atom.start < end);
        let lines = &self.text[start..end];
        if lines.is_ascii() {
            self.atoms.drain(first..last);
            return;
        }
        let found = decibell_richtext::composer::emoji_atoms(lines);
        let fresh = found
            .iter()
            .filter(|atom| atom.native == &lines[atom.range()])
            .map(|atom| AtomSpan {
                start: start + atom.range.0,
                end: start + atom.range.1,
                asset: atom.asset,
            });
        self.atoms.splice(first..last, fresh);
    }

    /// Types or pastes `input` over the selection.
    pub fn insert(&mut self, input: &str, kind: EditKind) -> bool {
        self.replace(self.selection.range(), input, kind)
    }

    /// Backspace / Delete: the selection when there is one, else from the caret to where
    /// `motion` would take it.
    pub fn delete(&mut self, motion: Motion) -> bool {
        let head = self.selection.head;
        let backward = matches!(motion, Motion::Left | Motion::WordLeft | Motion::DocStart);
        let range = if !self.selection.is_empty() {
            self.selection.range()
        } else if motion == Motion::Left && self.prev_stop(head) == boundaries::prev_grapheme(&self.text, head) {
            // Backspace inside plain text: one code point of a combining sequence, a whole
            // emoji (Chromium). An atom goes whole, through `prev_stop`.
            boundaries::backspace_start(&self.text, head)..head
        } else {
            let target = self.target(motion, true);
            target.min(head)..target.max(head)
        };
        if range.is_empty() || self.rules.read_only {
            return false;
        }
        let kind = if backward { EditKind::Backspace } else { EditKind::Delete };
        self.marked = None;
        // Not through `replace`: its snapping would widen a one-code-point Backspace.
        let at = range.start;
        self.splice(range, "", kind);
        self.settle(kind, at..at);
        true
    }

    /// Replaces the whole text from code (a draft restored, "edit last message"): no undo
    /// step survives, the caret goes to the end. The rules apply.
    pub fn set_text(&mut self, text: &str) -> bool {
        let read_only = std::mem::take(&mut self.rules.read_only);
        let before = std::mem::take(&mut self.text);
        self.selection = Selection::default();
        self.marked = None;
        self.atoms.clear();
        let admitted = self.admit(text, 0..0);
        self.splice(0..0, &admitted, EditKind::Other);
        self.settle(EditKind::Other, 0..self.text.len());
        self.history.clear();
        self.rules.read_only = read_only;
        self.text != before
    }

    // ---- IME ----

    /// The IME's preedit: replaces `range` (default: the marked text, else the selection)
    /// with `text` and marks it. `selected` is the caret inside the new text.
    pub fn set_marked(&mut self, range: Option<Range<usize>>, text: &str, selected: Option<Range<usize>>) -> bool {
        if self.rules.read_only {
            return false;
        }
        let range = range.or(self.marked.clone()).unwrap_or(self.selection.range());
        let range = self.snap(range.start)..self.snap_up(range.end);
        if range.is_empty() && text.is_empty() && self.marked.is_none() {
            return false;
        }
        self.splice(range.clone(), text, EditKind::Composition);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        if let Some(selected) = selected {
            let clamp = |offset: usize| boundaries::floor_grapheme(text, offset.min(text.len()));
            self.selection = Selection {
                anchor: range.start + clamp(selected.start),
                head: range.start + clamp(selected.end),
            };
        }
        self.settle(EditKind::Composition, range.start..range.start + text.len());
        true
    }

    /// The IME commits `text` over `range` (default: the marked text, else the selection).
    pub fn commit(&mut self, range: Option<Range<usize>>, text: &str) -> bool {
        let composing = self.marked.is_some();
        let range = range.or(self.marked.clone()).unwrap_or(self.selection.range());
        // A commit that ends a composition belongs to its step; plain typed text (the
        // platform inserts every printable key through this path) is typing.
        let kind = if composing { EditKind::Composition } else { EditKind::Typing };
        let changed = self.replace(range, text, kind);
        self.marked = None;
        if composing {
            self.history.close();
            self.retokenize(true);
        }
        changed
    }

    /// The composition ends where it stands (focus left, the IME gave up): the preedit
    /// text stays as text.
    pub fn unmark(&mut self) {
        if self.marked.take().is_some() {
            self.history.close();
            self.retokenize(true);
        }
    }

    // ---- undo ----

    pub fn undo(&mut self) -> bool {
        self.history_step(History::undo)
    }

    pub fn redo(&mut self) -> bool {
        self.history_step(History::redo)
    }

    fn history_step(&mut self, step: fn(&mut History, &mut String) -> Option<Selection>) -> bool {
        if self.rules.read_only {
            return false;
        }
        let Some(selection) = step(&mut self.history, &mut self.text) else {
            return false;
        };
        self.marked = None;
        self.revision += 1;
        self.selection = Selection {
            anchor: selection.anchor.min(self.text.len()),
            head: selection.head.min(self.text.len()),
        };
        self.retokenize(false);
        true
    }

    /// Ends the open typing step (the field lost focus).
    pub fn close_undo_step(&mut self) {
        self.history.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(buffer: &mut Buffer, text: &str) {
        for c in text.chars() {
            buffer.insert(c.encode_utf8(&mut [0; 4]), EditKind::Typing);
        }
    }

    fn composer() -> Buffer {
        Buffer::new(Rules {
            emoji_atoms: true,
            ..Rules::default()
        })
    }

    #[test]
    fn typing_selecting_deleting() {
        let mut buffer = Buffer::default();
        typed(&mut buffer, "hello world");
        assert_eq!(buffer.selection(), Selection::caret(11));
        buffer.move_by(Motion::WordLeft, true);
        assert_eq!(buffer.selected_text(), Some("world"));
        buffer.insert("there", EditKind::Typing);
        assert_eq!(buffer.text(), "hello there");
        buffer.move_by(Motion::DocStart, false);
        buffer.delete(Motion::WordRight);
        assert_eq!(buffer.text(), " there");
        buffer.delete(Motion::Right);
        buffer.move_by(Motion::DocEnd, false);
        buffer.delete(Motion::Left);
        buffer.delete(Motion::WordLeft);
        assert_eq!(buffer.text(), "");
        assert!(!buffer.delete(Motion::Left));
    }

    #[test]
    fn left_and_right_collapse_a_selection_to_its_edge() {
        let mut buffer = Buffer::default();
        typed(&mut buffer, "abcdef");
        buffer.select(Selection { anchor: 4, head: 2 });
        buffer.move_by(Motion::Left, false);
        assert_eq!(buffer.selection(), Selection::caret(2));
        buffer.select(Selection { anchor: 2, head: 4 });
        buffer.move_by(Motion::Right, false);
        assert_eq!(buffer.selection(), Selection::caret(4));
        buffer.move_by(Motion::Right, true);
        assert_eq!(buffer.selection(), Selection { anchor: 4, head: 5 });
    }

    #[test]
    fn undo_groups_a_burst_and_breaks_on_a_caret_move() {
        let mut buffer = Buffer::default();
        typed(&mut buffer, "one two");
        buffer.move_by(Motion::Left, false);
        buffer.move_by(Motion::Right, false);
        typed(&mut buffer, " three");
        buffer.insert(" PASTED", EditKind::Other);
        // A Backspace is a step of its own, with the typing that follows it.
        buffer.delete(Motion::Left);
        typed(&mut buffer, "!");
        assert_eq!(buffer.text(), "one two three PASTE!");
        let mut seen = Vec::new();
        while buffer.undo() {
            seen.push(buffer.text().to_string());
        }
        assert_eq!(seen, ["one two three PASTED", "one two three", "one two", ""]);
        assert!(buffer.redo() && buffer.redo());
        assert_eq!(buffer.text(), "one two three");
        assert_eq!(buffer.selection(), Selection::caret(13));
        typed(&mut buffer, "!");
        assert!(!buffer.redo());
    }

    #[test]
    fn a_single_line_takes_line_breaks_as_spaces() {
        let mut buffer = Buffer::new(Rules {
            multiline: false,
            ..Rules::default()
        });
        buffer.insert("one\r\ntwo\nthree\rfour", EditKind::Other);
        assert_eq!(buffer.text(), "one two three four");
        let mut area = Buffer::default();
        area.insert("one\r\ntwo\rthree", EditKind::Other);
        assert_eq!(area.text(), "one\ntwo\nthree");
    }

    #[test]
    fn maxlength_counts_utf16_units_and_cuts_the_insertion() {
        let mut buffer = Buffer::new(Rules {
            multiline: false,
            max_utf16: Some(6),
            ..Rules::default()
        });
        typed(&mut buffer, "abcd");
        // "😀" is two units: one fits with "ab", a second does not, and half an emoji never.
        buffer.move_to(2, false);
        buffer.insert("😀😀x", EditKind::Other);
        assert_eq!(buffer.text(), "ab😀cd");
        assert!(!buffer.insert("z", EditKind::Typing));
        // Replacing a selection frees its units.
        buffer.select(Selection { anchor: 0, head: 2 });
        buffer.insert("xyz", EditKind::Typing);
        assert_eq!(buffer.text(), "xy😀cd");
        let mut odd = Buffer::new(Rules {
            max_utf16: Some(3),
            ..Rules::default()
        });
        odd.insert("a😀😀", EditKind::Other);
        assert_eq!(odd.text(), "a😀");
    }

    /// `lib/utf8.ts` `clampLine`, with its own cases.
    #[test]
    fn the_byte_cap_clamps_as_clamp_line_does() {
        let rules = Rules {
            multiline: false,
            max_utf8: Some(10),
            ..Rules::default()
        };
        let clamp = |input: &str| {
            let mut buffer = Buffer::new(rules);
            buffer.set_text(input);
            buffer.text().to_string()
        };
        assert_eq!(clamp("hello"), "hello");
        assert_eq!(clamp("hello world!"), "hello worl");
        // ş is two bytes: the cut never splits it.
        assert_eq!(clamp("şşşşşş"), "şşşşş");
        assert_eq!(clamp("aşşşşş"), "aşşşş");
        // A 4-byte emoji that does not fit goes whole.
        assert_eq!(clamp("12345678😀"), "12345678");
        assert_eq!(clamp("a\r\n\tb\u{7}c\u{7f}"), "a bc");
        // Typing in the middle cuts the tail, as the controlled React input does.
        let mut buffer = Buffer::new(rules);
        buffer.set_text("0123456789");
        buffer.move_to(2, false);
        buffer.insert("ab", EditKind::Typing);
        assert_eq!(buffer.text(), "01ab234567");
        assert_eq!(buffer.selection(), Selection::caret(4));
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "0123456789");
        // A 1024-byte cap with multi-byte text.
        let mut big = Buffer::new(Rules {
            max_utf8: Some(1024),
            ..rules
        });
        big.set_text(&"ığüşöç日本".repeat(80));
        assert!(big.text().len() <= 1024 && big.text().len() > 1020);
        assert!(big.text().chars().all(|c| "ığüşöç日本".contains(c)));
    }

    #[test]
    fn a_number_field_takes_only_number_characters() {
        let mut buffer = Buffer::new(Rules {
            multiline: false,
            accept: Some(numeric),
            ..Rules::default()
        });
        buffer.insert("12 apples, -3.5e2!", EditKind::Other);
        assert_eq!(buffer.text(), "12e-3.5e2");
        assert!(!buffer.insert("x", EditKind::Typing));
    }

    #[test]
    fn emoji_are_atoms() {
        let mut buffer = composer();
        buffer.set_text("a👨‍👩‍👧b🇹🇷");
        let family = "👨‍👩‍👧".len();
        assert_eq!(buffer.atoms().len(), 2);
        assert_eq!((buffer.atoms()[0].start, buffer.atoms()[0].end), (1, 1 + family));
        // One step over each, both ways; never inside.
        buffer.move_to(0, false);
        let mut stops = vec![0];
        while buffer.move_by(Motion::Right, false) {
            stops.push(buffer.selection().head);
        }
        assert_eq!(stops, vec![0, 1, 1 + family, 2 + family, buffer.text().len()]);
        assert_eq!(buffer.snap(4), 1);
        // One Backspace takes the whole flag, one Delete the whole family.
        buffer.delete(Motion::Left);
        assert_eq!(buffer.text(), "a👨‍👩‍👧b");
        buffer.move_to(1, false);
        buffer.delete(Motion::Right);
        assert_eq!(buffer.text(), "ab");
        assert!(buffer.atoms().is_empty());
        // Copying gives the characters; a double click on an atom selects it alone.
        buffer.set_text("hi 😀 there");
        assert_eq!(buffer.word_at(4), (3, 7));
        buffer.select_all();
        assert_eq!(buffer.selected_text(), Some("hi 😀 there"));
    }

    #[test]
    fn a_completed_shortcode_becomes_its_emoji() {
        let mut buffer = composer();
        typed(&mut buffer, "ok :smile");
        assert!(buffer.atoms().is_empty());
        typed(&mut buffer, ":");
        assert_eq!(buffer.text(), "ok 😄");
        assert_eq!(buffer.selection(), Selection::caret(buffer.text().len()));
        assert_eq!(buffer.atoms().len(), 1);
        // Several at once (a paste, a restored draft), with other lines around them.
        let mut pasted = composer();
        pasted.set_text("ığü\n:fire: şş :tada: çç :fire:\nöö 😀");
        assert_eq!(pasted.text(), "ığü\n🔥 şş 🎉 çç 🔥\nöö 😀");
        assert_eq!(pasted.atoms().len(), 4);
        pasted.move_to("ığü\n🔥 şş".len(), false);
        typed(&mut pasted, ":+1:");
        assert_eq!(pasted.text(), "ığü\n🔥 şş👍 🎉 çç 🔥\nöö 😀");
        assert_eq!(pasted.atoms().len(), 5);
        assert!(pasted.atoms().windows(2).all(|pair| pair[0].end <= pair[1].start));
        for atom in pasted.atoms() {
            assert!(pasted.text().is_char_boundary(atom.start) && pasted.text().is_char_boundary(atom.end));
        }
        // An unknown one stays as typed.
        typed(&mut buffer, " :nosuchcode:");
        assert_eq!(buffer.text(), "ok 😄 :nosuchcode:");
        // Undo takes the burst back, emoji and all.
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "");
        // Outside the composer nothing is replaced.
        let mut plain = Buffer::default();
        typed(&mut plain, ":smile:");
        assert_eq!(plain.text(), ":smile:");
    }

    #[test]
    fn a_password_is_never_copied_and_is_one_word() {
        let mut buffer = Buffer::new(Rules {
            multiline: false,
            masked: true,
            ..Rules::default()
        });
        typed(&mut buffer, "hunter two");
        buffer.select_all();
        assert_eq!(buffer.selected_text(), None);
        buffer.move_to(10, false);
        buffer.move_by(Motion::WordLeft, false);
        assert_eq!(buffer.selection().head, 0);
        assert_eq!(buffer.word_at(3), (0, 10));
    }

    #[test]
    fn read_only_refuses_edits_but_not_code() {
        let mut buffer = Buffer::new(Rules {
            read_only: true,
            ..Rules::default()
        });
        assert!(buffer.set_text("fixed"));
        assert!(!buffer.insert("x", EditKind::Typing));
        assert!(!buffer.delete(Motion::Left));
        buffer.select_all();
        assert_eq!(buffer.selected_text(), Some("fixed"));
    }

    #[test]
    fn a_composition_is_marked_then_committed_as_one_step() {
        let mut buffer = Buffer::default();
        typed(&mut buffer, "ab");
        buffer.move_to(1, false);
        buffer.set_marked(None, "k", None);
        buffer.set_marked(None, "か", Some(3..3));
        assert_eq!(buffer.text(), "aかb");
        assert_eq!(buffer.marked(), Some(1..4));
        buffer.commit(None, "火");
        assert_eq!(buffer.text(), "a火b");
        assert_eq!(buffer.marked(), None);
        assert_eq!(buffer.selection(), Selection::caret(4));
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "ab");
        // An empty preedit cancels the composition.
        buffer.set_marked(None, "x", None);
        buffer.set_marked(None, "", None);
        assert_eq!((buffer.text(), buffer.marked()), ("ab", None));
        // Unmarking keeps the text.
        buffer.set_marked(None, "y", None);
        buffer.unmark();
        assert_eq!((buffer.text(), buffer.marked()), ("ayb", None));
    }
}
