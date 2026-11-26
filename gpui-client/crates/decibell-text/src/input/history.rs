//! Undo and redo, grouped the way Chromium groups them.
//!
//! Blink keeps one "typing command" open while the user types: every character typed and
//! every Backspace / Delete joins it, so one Ctrl+Z takes the whole burst back. The command
//! closes when the selection changes by anything else (an arrow key, a click, Ctrl+A), when
//! another kind of edit happens (paste, cut, a programmatic insert, an IME composition, which
//! is one step of its own from its first preedit to its commit) and
//! when the field loses focus. Chromium has no pause timer (measured: see
//! `tests/data/chromium-input.json`, "undo"), and neither has this.
//!
//! A step stores only what it changed (the removed and inserted text of each edit), so the
//! history of a long draft is as large as the typing, not the typing times the length.

use std::ops::Range;

/// Blink's `kMaximumUndoStackDepth`.
const MAX_STEPS: usize = 1000;
/// And a byte bound on top (a few pasted novels must not stay in memory).
const MAX_BYTES: usize = 1 << 20;

/// A selection in byte offsets: `anchor` stays, `head` moves (the caret).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn caret(at: usize) -> Self {
        Self { anchor: at, head: at }
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// What kind of edit this is, for grouping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// A typed character, Backspace, Delete (and their word variants): joins the open
    /// typing step.
    Typing,
    /// The updates of one IME composition and its commit: one step together.
    Composition,
    /// Paste, cut, a programmatic insert: always a step of its own.
    Other,
}

/// One replacement: at `at`, `removed` became `inserted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Edit {
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

impl Edit {
    fn bytes(&self) -> usize {
        self.removed.len() + self.inserted.len() + size_of::<Edit>()
    }
}

#[derive(Debug)]
struct Step {
    /// In the order they were made.
    edits: Vec<Edit>,
    before: Selection,
    after: Selection,
    /// Edits of this kind may still join.
    open: Option<EditKind>,
}

impl Step {
    fn bytes(&self) -> usize {
        self.edits.iter().map(Edit::bytes).sum::<usize>() + size_of::<Step>()
    }
}

#[derive(Debug, Default)]
pub(crate) struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    bytes: usize,
}

impl History {
    /// Records an edit already applied to the text. `before` / `after` are the selections
    /// around it.
    pub fn record(&mut self, edit: Edit, kind: EditKind, before: Selection, after: Selection) {
        self.redo.clear();
        if kind != EditKind::Other
            && let Some(step) = self.undo.last_mut().filter(|step| step.open == Some(kind))
        {
            self.bytes += edit.bytes();
            step.after = after;
            match step.edits.last_mut() {
                // Typing forward: grow the last insertion instead of adding an edit.
                Some(last)
                    if kind == EditKind::Typing
                        && edit.removed.is_empty()
                        && last.at + last.inserted.len() == edit.at =>
                {
                    self.bytes -= size_of::<Edit>();
                    last.inserted.push_str(&edit.inserted);
                }
                _ => step.edits.push(edit),
            }
        } else {
            self.close();
            let step = Step {
                edits: vec![edit],
                before,
                after,
                open: (kind != EditKind::Other).then_some(kind),
            };
            self.bytes += step.bytes();
            self.undo.push(step);
        }
        while self.undo.len() > MAX_STEPS || (self.bytes > MAX_BYTES && self.undo.len() > 1) {
            let dropped = self.undo.remove(0);
            self.bytes -= dropped.bytes();
        }
    }

    /// Adds an edit to the step just recorded (the composer turning a typed `:shortcode:`
    /// into its emoji belongs to the keystroke that completed it).
    pub fn amend(&mut self, edit: Edit, after: Selection) {
        if let Some(step) = self.undo.last_mut() {
            self.bytes += edit.bytes();
            step.edits.push(edit);
            step.after = after;
        }
    }

    /// Ends the open typing step: the next typed character starts a new one.
    pub fn close(&mut self) {
        if let Some(step) = self.undo.last_mut() {
            step.open = None;
        }
    }

    /// Takes the last step back on `text`. Returns the selection to restore.
    pub fn undo(&mut self, text: &mut String) -> Option<Selection> {
        let mut step = self.undo.pop()?;
        self.bytes -= step.bytes();
        step.open = None;
        for edit in step.edits.iter().rev() {
            text.replace_range(edit.at..edit.at + edit.inserted.len(), &edit.removed);
        }
        let selection = step.before;
        self.redo.push(step);
        Some(selection)
    }

    pub fn redo(&mut self, text: &mut String) -> Option<Selection> {
        let step = self.redo.pop()?;
        for edit in &step.edits {
            text.replace_range(edit.at..edit.at + edit.removed.len(), &edit.inserted);
        }
        let selection = step.after;
        self.bytes += step.bytes();
        self.undo.push(step);
        Some(selection)
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// (undo steps, redo steps, bytes held by the undo steps).
    pub fn usage(&self) -> (usize, usize, usize) {
        (self.undo.len(), self.redo.len(), self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(history: &mut History, text: &mut String, at: usize, remove: usize, insert: &str, kind: EditKind) {
        let before = Selection::caret(at + remove);
        let removed = text[at..at + remove].to_string();
        text.replace_range(at..at + remove, insert);
        let after = Selection::caret(at + insert.len());
        history.record(Edit { at, removed, inserted: insert.into() }, kind, before, after);
    }

    #[test]
    fn a_typing_burst_is_one_step_with_its_backspaces() {
        let (mut history, mut text) = (History::default(), String::new());
        for (index, c) in "hello".char_indices() {
            apply(&mut history, &mut text, index, 0, &c.to_string(), EditKind::Typing);
        }
        apply(&mut history, &mut text, 4, 1, "", EditKind::Typing);
        apply(&mut history, &mut text, 4, 0, "p", EditKind::Typing);
        assert_eq!(text, "hellp");
        assert_eq!(history.usage().0, 1);
        assert_eq!(history.undo(&mut text), Some(Selection::caret(0)));
        assert_eq!(text, "");
        assert_eq!(history.redo(&mut text), Some(Selection::caret(5)));
        assert_eq!(text, "hellp");
    }

    #[test]
    fn a_closed_step_and_other_edits_start_new_steps() {
        let (mut history, mut text) = (History::default(), String::new());
        apply(&mut history, &mut text, 0, 0, "a", EditKind::Typing);
        apply(&mut history, &mut text, 1, 0, "b", EditKind::Typing);
        history.close(); // the caret moved
        apply(&mut history, &mut text, 2, 0, "c", EditKind::Typing);
        apply(&mut history, &mut text, 3, 0, "PASTE", EditKind::Other);
        apply(&mut history, &mut text, 8, 0, "d", EditKind::Typing);
        assert_eq!(history.usage().0, 4);
        let mut seen = Vec::new();
        while history.undo(&mut text).is_some() {
            seen.push(text.clone());
        }
        assert_eq!(seen, ["abcPASTE", "abc", "ab", ""]);
        // A new edit drops the redo stack.
        history.redo(&mut text);
        apply(&mut history, &mut text, 2, 0, "x", EditKind::Typing);
        assert!(!history.can_redo());
        assert_eq!(text, "abx");
    }

    #[test]
    fn an_amended_edit_is_undone_with_its_step() {
        let (mut history, mut text) = (History::default(), String::from(":smile"));
        apply(&mut history, &mut text, 6, 0, ":", EditKind::Typing);
        text.replace_range(0..7, "😄");
        history.amend(Edit { at: 0, removed: ":smile:".into(), inserted: "😄".into() }, Selection::caret(4));
        assert_eq!(history.undo(&mut text), Some(Selection::caret(6)));
        assert_eq!(text, ":smile");
        assert_eq!(history.redo(&mut text), Some(Selection::caret(4)));
        assert_eq!(text, "😄");
    }

    #[test]
    fn the_history_is_bounded() {
        let (mut history, mut text) = (History::default(), String::new());
        for _ in 0..MAX_STEPS + 50 {
            apply(&mut history, &mut text, 0, 0, "x", EditKind::Other);
        }
        assert_eq!(history.usage().0, MAX_STEPS);
        let big = "y".repeat(MAX_BYTES / 2);
        for _ in 0..4 {
            apply(&mut history, &mut text, 0, 0, &big, EditKind::Other);
        }
        assert!(history.usage().2 <= MAX_BYTES + big.len() + 1024);
        assert!(history.usage().0 < 10);
    }
}
