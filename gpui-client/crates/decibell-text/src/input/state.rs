//! [`InputState`]: a field's state as a GPUI entity. It owns the [`Buffer`], the layout of
//! what is shown, the focus handle and the caret's blink, speaks GPUI's IME protocol
//! (`EntityInputHandler`) and is the view the field is drawn by, so an edit re-renders this
//! view and its ancestors and nothing else.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, IntoElement, KeyDownEvent, Keystroke, Pixels, Point, Render, SharedString, Subscription, Task,
    UTF16Selection, WeakFocusHandle, Window, point, px, size,
};

use super::boundaries::{byte_to_utf16, utf16_len, utf16_to_byte};
use super::buffer::{Buffer, Motion, Rules};
use super::display::{Display, DisplayStats, InputStyle};
use super::element::InputElement;
use super::history::{EditKind, Selection};
use super::keymap::{self, Command, Move};
use crate::{Affinity, Engine};

thread_local! {
    /// Every live field's focus handle (GPUI runs on one thread): [`text_field_focused`] asks the
    /// window's focus itself, not the focus events, which a window in the background doesn't send
    /// (a field focused there would read as not focused, and its keys would go to the composer).
    static FIELDS: RefCell<Vec<WeakFocusHandle>> = const { RefCell::new(Vec::new()) };
}

/// Whether a text field has the keyboard focus in `window`, so that a key that types a
/// character goes into it. The app's focused hotkey fallback asks: bare typing keys are the
/// field's, not a keybind's.
pub fn text_field_focused(window: &Window) -> bool {
    FIELDS.with(|fields| {
        let mut focused = false;
        fields.borrow_mut().retain(|field| {
            field.upgrade().inspect(|handle| focused |= handle.is_focused(window)).is_some()
        });
        focused
    })
}

/// Blink's caret blink interval (`LayoutTheme::CaretBlinkInterval`): half a second shown,
/// half a second hidden.
pub const BLINK_INTERVAL: Duration = Duration::from_millis(500);
/// How often a drag outside the field scrolls it one more step.
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// What a field tells its owner (`cx.subscribe(&input, |this, input, event, cx| …)`).
/// Events arrive after the edit is done, so the handler may read and change the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// The text changed (typing, paste, undo, `set_value`…).
    Change,
    /// Enter, where the field's [`Enter`] policy says it submits.
    Submit,
    /// Escape was pressed in the field (see also [`InputState::on_escape`]).
    Escape,
    /// ArrowUp in a multi-line field that holds nothing but white space (the composer:
    /// edit the last message).
    ArrowUpEmpty,
    Focus,
    Blur,
}

/// What Enter does.
#[derive(Clone, Copy, Debug)]
pub enum Enter {
    /// Always [`InputEvent::Submit`] (a single-line field).
    Submit,
    /// Always a line break (a plain text area).
    Newline,
    /// The composer: Enter submits, Shift+Enter breaks the line, and so does Enter inside
    /// an unclosed ``` fence (`decibell_richtext::composer::has_open_code_fence`).
    Chat,
    /// Your own rule: `(text, shift)` → `true` to submit, `false` for a line break.
    Custom(fn(&str, bool) -> bool),
}

/// A hook that may swallow a key: return `true` when you handled it. Hooks run outside
/// the input's own update, so they may read the input but should change it from an
/// [`InputEvent`] handler or later.
pub type KeyHook = Rc<dyn Fn(&mut Window, &mut App) -> bool>;
type PasteHook = Rc<dyn Fn(&ClipboardItem, &mut Window, &mut App) -> bool>;
type PassKeys = Rc<dyn Fn(&Keystroke) -> bool>;
type RevealHook = Rc<dyn Fn(Bounds<Pixels>, &mut App)>;
/// Styles for ranges of the text (byte ranges, in order, not overlapping): a code field's live
/// highlighting. Called once per edit.
pub type Colorizer = Rc<dyn Fn(&str) -> Vec<(std::ops::Range<usize>, crate::TextStyle)>>;

/// How a drag extends the selection: by what the click that started it selected.
#[derive(Clone, Copy, Debug)]
enum Drag {
    Caret,
    Word(usize, usize),
    Line(usize, usize),
}

/// See the module docs.
pub struct InputState {
    pub(crate) buffer: Buffer,
    pub(crate) focus: FocusHandle,
    pub(crate) style: Option<InputStyle>,
    pub(crate) placeholder: SharedString,
    pub(crate) display: Display,
    /// Which side of a soft wrap the caret is on.
    pub(crate) affinity: Affinity,
    /// The x a run of Up / Down keeps returning to.
    goal_x: Option<f32>,
    /// A single line's horizontal scroll, px.
    pub(crate) scroll_x: f32,
    /// The caret must be brought into view by the next paint.
    pub(crate) reveal: bool,
    /// The last paint: the element's bounds, what of it was visible, the scale.
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) visible: Bounds<Pixels>,
    pub(crate) width: f32,
    pub(crate) scale: f32,
    /// The caret rectangle last reported to the IME.
    pub(crate) reported_caret: Option<Bounds<Pixels>>,
    pub(crate) caret_on: bool,
    last_activity: Instant,
    blink: Option<Task<()>>,
    drag: Option<Drag>,
    pub(crate) drag_point: Point<Pixels>,
    autoscroll: Option<Task<()>>,
    enter: Enter,
    disabled: bool,
    pub(crate) on_escape: Option<KeyHook>,
    pub(crate) on_paste: Option<PasteHook>,
    pub(crate) pass_key: Option<PassKeys>,
    pub(crate) on_reveal: Option<RevealHook>,
    /// Live styling of the text (a code field), and its result for the buffer's revision.
    colorizer: Option<Colorizer>,
    colors: Vec<(std::ops::Range<usize>, crate::TextStyle)>,
    colors_for: Option<u64>,
    /// Bumped when the colorizer changes: the display builds every line again.
    colors_epoch: u64,
    stats: DisplayStats,
    _subscriptions: [Subscription; 3],
}

impl EventEmitter<InputEvent> for InputState {}

impl Focusable for InputState {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl InputState {
    /// A field with these rules. Enter submits in a single line and breaks the line in a
    /// multi-line field until [`set_enter`](Self::set_enter) says otherwise.
    pub fn new(rules: Rules, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle().tab_stop(true);
        FIELDS.with(|fields| {
            let mut fields = fields.borrow_mut();
            fields.retain(|field| field.upgrade().is_some());
            fields.push(focus.downgrade());
        });
        let subscriptions = [
            cx.on_focus(&focus, window, Self::focused),
            cx.on_blur(&focus, window, Self::blurred),
            cx.observe_window_activation(window, |this, window, cx| this.sync_blink(window, cx)),
        ];
        Self {
            enter: if rules.multiline { Enter::Newline } else { Enter::Submit },
            buffer: Buffer::new(rules),
            focus,
            style: None,
            placeholder: SharedString::default(),
            display: Display::default(),
            affinity: Affinity::Downstream,
            goal_x: None,
            scroll_x: 0.,
            reveal: false,
            bounds: Bounds::default(),
            visible: Bounds::default(),
            width: f32::INFINITY,
            scale: window.scale_factor(),
            reported_caret: None,
            caret_on: false,
            last_activity: Instant::now(),
            blink: None,
            drag: None,
            drag_point: Point::default(),
            autoscroll: None,
            disabled: false,
            on_escape: None,
            on_paste: None,
            pass_key: None,
            on_reveal: None,
            colorizer: None,
            colors: Vec::new(),
            colors_for: None,
            colors_epoch: 0,
            stats: DisplayStats::default(),
            _subscriptions: subscriptions,
        }
    }

    /// One line: sign-in, search, settings, dialogs.
    pub fn single_line(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new(
            Rules {
                multiline: false,
                ..Rules::default()
            },
            window,
            cx,
        )
    }

    /// One line drawn as bullets, never copied.
    pub fn password(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new(
            Rules {
                multiline: false,
                masked: true,
                ..Rules::default()
            },
            window,
            cx,
        )
    }

    /// Several lines that wrap; grows with its text.
    pub fn multi_line(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new(Rules::default(), window, cx)
    }

    /// The chat composer's input: multi-line, emoji as atoms, Enter sends.
    pub fn composer(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::new(
            Rules {
                emoji_atoms: true,
                ..Rules::default()
            },
            window,
            cx,
        );
        this.enter = Enter::Chat;
        this
    }

    // ---- configuration (builder style, for `cx.new(|cx| InputState::single_line(window, cx).placeholder("…"))`) ----

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// The initial value (the rules apply; no undo step).
    pub fn with_value(mut self, value: &str) -> Self {
        self.buffer.set_text(value);
        self
    }

    /// HTML `maxlength`, in UTF-16 code units.
    pub fn max_length(mut self, units: usize) -> Self {
        self.update_rules(|rules| rules.max_utf16 = Some(units));
        self
    }

    /// The renderer's `clampLine(value, bytes)`: see [`Rules::max_utf8`].
    pub fn max_bytes(mut self, bytes: usize) -> Self {
        self.update_rules(|rules| rules.max_utf8 = Some(bytes));
        self
    }

    /// Only characters `accept` allows ([`super::numeric`]).
    pub fn accept(mut self, accept: fn(char) -> bool) -> Self {
        self.update_rules(|rules| rules.accept = Some(accept));
        self
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.update_rules(|rules| rules.read_only = read_only);
        self
    }

    pub fn enter(mut self, enter: Enter) -> Self {
        self.enter = enter;
        self
    }

    fn update_rules(&mut self, change: impl FnOnce(&mut Rules)) {
        let mut rules = *self.buffer.rules();
        change(&mut rules);
        self.buffer.set_rules(rules);
    }

    // ---- configuration (later) ----

    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) {
        let placeholder = placeholder.into();
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    pub fn set_enter(&mut self, enter: Enter) {
        self.enter = enter;
    }

    /// Changes the rules (the text is not re-checked against them).
    pub fn set_rules(&mut self, rules: Rules, cx: &mut Context<Self>) {
        self.buffer.set_rules(rules);
        self.display.clear();
        cx.notify();
    }

    pub fn rules(&self) -> &Rules {
        self.buffer.rules()
    }

    /// A disabled field takes no focus, no clicks and no keys.
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            self.focus = self.focus.clone().tab_stop(!disabled);
            cx.notify();
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// The style the field is drawn in. The elements ([`super::TextInput`],
    /// [`super::TextArea`]) call this every time their parent renders; it does nothing
    /// when the style is the same.
    pub fn set_style(&mut self, style: InputStyle, cx: &mut Context<Self>) {
        if self.style != Some(style) {
            self.style = Some(style);
            cx.notify();
        }
    }

    /// Escape: return `true` from the hook to swallow the key (a pending reply was
    /// dropped); otherwise it travels on (a dialog closes). [`InputEvent::Escape`] is
    /// emitted either way.
    pub fn on_escape(&mut self, hook: impl Fn(&mut Window, &mut App) -> bool + 'static) {
        self.on_escape = Some(Rc::new(hook));
    }

    /// Called with the clipboard's content when a paste holds something that is not text
    /// (an image, files). Return `true` when you took it (the composer makes attachments);
    /// `false` pastes the item's text, if it has any.
    pub fn on_paste_non_text(&mut self, hook: impl Fn(&ClipboardItem, &mut Window, &mut App) -> bool + 'static) {
        self.on_paste = Some(Rc::new(hook));
    }

    /// Editing keys the field should leave alone (return `true`): a list's filter box
    /// lets Up / Down / Enter through to the list.
    pub fn pass_keys(&mut self, pass: impl Fn(&Keystroke) -> bool + 'static) {
        self.pass_key = Some(Rc::new(pass));
    }

    /// Called when the caret moved (an edit, a key, a click, a drag), with its rectangle
    /// relative to the field's top-left and the text already laid out: a scroll container
    /// around a multi-line field scrolls it into view, before the frame that shows the
    /// change is drawn. (A single line scrolls sideways by itself.)
    pub fn on_reveal_caret(&mut self, hook: impl Fn(Bounds<Pixels>, &mut App) + 'static) {
        self.on_reveal = Some(Rc::new(hook));
    }

    /// Styles the text live (a code field: `RichComposer`'s highlighted overlay): `colorizer`
    /// is asked once per edit for styled byte ranges of the whole text; `None` draws it in the
    /// field's own style.
    pub fn set_colorizer(&mut self, colorizer: Option<Colorizer>, cx: &mut Context<Self>) {
        self.colorizer = colorizer;
        self.colors.clear();
        self.colors_for = None;
        self.colors_epoch += 1;
        cx.notify();
    }

    /// The styled ranges the colorizer gave for the text as last laid out (tests).
    pub fn styled_ranges(&self) -> &[(std::ops::Range<usize>, crate::TextStyle)] {
        &self.colors
    }

    pub fn has_reveal_hook(&self) -> bool {
        self.on_reveal.is_some()
    }

    // ---- the handle: what an owner reads and does ----

    /// The text. Atoms are their characters; a password is its real text.
    pub fn value(&self) -> &str {
        self.buffer.text()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// `RichInput.isEmpty`: nothing but white space.
    pub fn is_blank(&self) -> bool {
        self.buffer.text().trim().is_empty()
    }

    /// The selection in bytes of [`value`](Self::value): `anchor` is where it started,
    /// `head` is the caret.
    pub fn selection(&self) -> Selection {
        self.buffer.selection()
    }

    /// Whether the IME is composing (preedit text is in the value).
    pub fn is_composing(&self) -> bool {
        self.buffer.marked().is_some()
    }

    /// The caret's rectangle in window coordinates, as last painted.
    pub fn caret_bounds(&self) -> Bounds<Pixels> {
        let (x, top, height) = self.display.caret(self.buffer.selection().head, self.affinity);
        Bounds::new(
            point(
                self.bounds.origin.x + px(x - self.scroll_x),
                self.bounds.origin.y + px(top),
            ),
            size(px(self.style.map_or(1., |style| style.caret_width)), px(height)),
        )
    }

    /// Replaces the text (a restored draft, "edit last message", a controlled value). The
    /// undo history starts over and the caret goes to the end. Emits `Change` when the
    /// text is different.
    pub fn set_value(&mut self, value: &str, cx: &mut Context<Self>) {
        if self.buffer.text() == value {
            return;
        }
        if self.buffer.set_text(value) {
            cx.emit(InputEvent::Change);
        }
        self.after_change(cx);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_value("", cx);
    }

    /// Inserts text over the selection, as a paste would (the emoji picker, a mention).
    /// One undo step.
    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.buffer.insert(text, EditKind::Other) {
            cx.emit(InputEvent::Change);
            self.after_change(cx);
        }
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        if self.buffer.select_all() {
            self.after_move(cx);
        }
    }

    /// Sets the selection, in bytes (snapped to caret stops).
    pub fn select(&mut self, selection: Selection, cx: &mut Context<Self>) {
        if self.buffer.select(selection) {
            self.after_move(cx);
        }
    }

    /// Focuses the field with the caret at the end (`RichInput.focus`: a "type anywhere"
    /// keystroke appends).
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if !self.focus.is_focused(window) {
            let end = self.buffer.text().len();
            self.buffer.move_to(end, false);
            self.after_move(cx);
        }
        self.focus.focus(window, cx);
    }

    /// "Type anywhere to compose" (`useTypeToFocusComposer`): call from a key-down
    /// listener of the surrounding view when no field has focus. A printable key without
    /// Ctrl / Alt / Super focuses the field and types the character at its end; returns
    /// whether it did (then stop the event).
    pub fn type_to_focus(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let modifiers = event.keystroke.modifiers;
        if self.disabled || modifiers.control || modifiers.alt || modifiers.platform || self.focus.is_focused(window) {
            return false;
        }
        let Some(typed) = event.keystroke.key_char.as_deref().filter(|typed| !typed.chars().any(char::is_control))
        else {
            return false;
        };
        self.focus(window, cx);
        if self.buffer.insert(typed, EditKind::Typing) {
            cx.emit(InputEvent::Change);
            self.after_change(cx);
        }
        true
    }

    /// What a copy would put on the clipboard: the selected characters (atoms as their
    /// characters), `None` without a selection and always for a password.
    pub fn selected_text(&self) -> Option<&str> {
        self.buffer.selected_text()
    }

    /// The field's rectangle in window coordinates, as last painted.
    pub fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// The text as it is drawn, line by line: bullets for a password, U+FFFC for an emoji
    /// atom. For tests.
    pub fn displayed_text(&self) -> String {
        self.display.shown_text()
    }

    /// The marked (preedit) range in bytes, while the IME composes.
    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.buffer.marked()
    }

    /// Whether the caret is in its shown half of the blink (it is drawn only while the
    /// field is focused in the active window).
    pub fn caret_shown(&self) -> bool {
        self.caret_on && self.blink.is_some()
    }

    /// Counters of the last layout update (tests, measurements).
    pub fn display_stats(&self) -> DisplayStats {
        self.stats
    }

    /// Heap bytes this field holds: text, history, layouts.
    pub fn heap_bytes(&self) -> usize {
        self.buffer.text().len() + self.buffer.history_usage().2 + self.display.heap_bytes()
    }

    pub fn can_undo(&self) -> bool {
        self.buffer.can_undo()
    }

    // ---- after something happened ----

    fn after_change(&mut self, cx: &mut Context<Self>) {
        self.affinity = Affinity::Downstream;
        self.goal_x = None;
        self.touch();
        self.relayout(cx);
        cx.notify();
    }

    fn after_move(&mut self, cx: &mut Context<Self>) {
        self.touch();
        self.relayout(cx);
        cx.notify();
    }

    /// The caret shows solid and must be visible.
    fn touch(&mut self) {
        self.caret_on = true;
        self.last_activity = Instant::now();
        self.reveal = true;
    }

    /// Lays the text out for the last known width. Painting does this anyway; edits do it
    /// at once so that the next key (Up, End) and the owner's measurement see the new text.
    pub(crate) fn relayout(&mut self, cx: &mut App) {
        let Some(style) = self.style else { return };
        let engine = Engine::global(cx);
        if let Some(colorizer) = &self.colorizer
            && self.colors_for != Some(self.buffer.revision())
        {
            self.colors = colorizer(self.buffer.text());
            self.colors_for = Some(self.buffer.revision());
        }
        let colors = (&self.colors[..], self.colors_epoch);
        let stats = self.display.update(&self.buffer, &style, colors, self.width, self.scale, &engine);
        if stats != DisplayStats::default() {
            self.stats = stats;
        }
        if self.reveal
            && let Some(hook) = self.on_reveal.clone()
        {
            self.reveal = false;
            let (x, top, height) = self.display.caret(self.buffer.selection().head, self.affinity);
            hook(Bounds::new(point(px(x), px(top)), size(px(style.caret_width), px(height))), cx);
        }
    }

    /// The text's height at `width` (a multi-line field's own height).
    pub(crate) fn measure(&mut self, width: f32, scale: f32, cx: &mut App) -> f32 {
        self.width = width;
        self.scale = scale;
        self.relayout(cx);
        self.display.height()
    }

    // ---- focus and the blinking caret ----

    fn focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.touch();
        self.sync_blink(window, cx);
        cx.emit(InputEvent::Focus);
        cx.notify();
    }

    /// Also when the window goes to the background with the field still focused (GPUI's blur
    /// then too): the typing step stays open over that, as in Chromium, where only a selection
    /// change ends it (a burst typed across a window switch is undone at once).
    fn blurred(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.unmark();
        if !self.focus.is_focused(window) {
            self.buffer.close_undo_step();
        }
        self.drag = None;
        self.autoscroll = None;
        self.sync_blink(window, cx);
        cx.emit(InputEvent::Blur);
        cx.notify();
    }

    /// The blink timer exists exactly while the field is focused in the active window.
    fn sync_blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live = self.focus.is_focused(window) && window.is_window_active() && !self.buffer.rules().read_only;
        if !live {
            if self.blink.take().is_some() || self.caret_on {
                self.caret_on = false;
                cx.notify();
            }
            return;
        }
        if self.blink.is_some() {
            return;
        }
        self.caret_on = true;
        self.last_activity = Instant::now();
        cx.notify();
        self.blink = Some(cx.spawn(async move |this, cx| {
            loop {
                // Sleep to the next half-second mark since the last key or click: typing
                // only moves the mark, it never restarts the task.
                let Ok(wait) = this.update(cx, |this, _| {
                    let since = this.last_activity.elapsed().as_nanos();
                    let interval = BLINK_INTERVAL.as_nanos();
                    Duration::from_nanos((interval - since % interval) as u64)
                }) else {
                    break;
                };
                cx.background_executor().timer(wait).await;
                let alive = this.update(cx, |this, cx| {
                    let phase = this.last_activity.elapsed().as_nanos() / BLINK_INTERVAL.as_nanos();
                    let on = phase.is_multiple_of(2);
                    if on != this.caret_on {
                        this.caret_on = on;
                        // A range selection shows no caret: nothing to redraw.
                        if this.buffer.selection().is_empty() {
                            cx.notify();
                        }
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    /// Whether the caret is drawn in this frame.
    pub(crate) fn caret_visible(&self, window: &Window) -> bool {
        self.caret_on && self.blink.is_some() && self.buffer.selection().is_empty() && self.focus.is_focused(window)
    }

    // ---- keys ----

    /// A key went down while the field has focus. Runs the hooks outside the input's
    /// update, then the command inside.
    pub(crate) fn key_down(state: &Entity<Self>, event: &KeyDownEvent, window: &mut Window, cx: &mut App) {
        let Some(command) = keymap::command(&event.keystroke) else {
            return;
        };
        let (disabled, pass, on_escape, on_paste) = {
            let this = state.read(cx);
            (
                this.disabled,
                this.pass_key.clone(),
                this.on_escape.clone(),
                this.on_paste.clone(),
            )
        };
        if disabled || pass.is_some_and(|pass| pass(&event.keystroke)) {
            return;
        }
        let handled = match command {
            Command::Escape => {
                if state.read(cx).is_composing() {
                    return;
                }
                state.update(cx, |_, cx| cx.emit(InputEvent::Escape));
                on_escape.is_some_and(|hook| hook(window, cx))
            }
            Command::Paste => {
                if let Some(item) = cx.read_from_clipboard() {
                    let non_text = item.entries().iter().any(|entry| !matches!(entry, ClipboardEntry::String(_)));
                    let taken = non_text && on_paste.is_some_and(|hook| hook(&item, window, cx));
                    if !taken && let Some(text) = item.text() {
                        state.update(cx, |this, cx| this.paste(&text, cx));
                    }
                }
                true
            }
            command => state.update(cx, |this, cx| this.run(command, window, cx)),
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn paste(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.buffer.insert(text, EditKind::Other) {
            cx.emit(InputEvent::Change);
            self.after_change(cx);
        }
    }

    /// Middle click on Linux: the primary selection at the click.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    pub(crate) fn paste_primary(&mut self, at: Point<f32>, cx: &mut Context<Self>) {
        let (offset, affinity) = self.display.hit(at.x, at.y);
        self.buffer.move_to(offset, false);
        self.affinity = affinity;
        if let Some(text) = cx.read_from_primary().and_then(|item| item.text()) {
            self.paste(&text, cx);
        } else {
            self.after_move(cx);
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(text) = self.buffer.selected_text() else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        true
    }

    /// The selection as the X11 / Wayland primary selection (what a middle click pastes).
    fn publish_primary(&self, cx: &mut Context<Self>) {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        if let Some(text) = self.buffer.selected_text() {
            cx.write_to_primary(ClipboardItem::new_string(text.to_string()));
        }
        let _ = cx;
    }

    /// Runs an editing command. Returns whether the key is used up.
    pub fn run(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match command {
            Command::Move { to, extend } => {
                if to == Move::Up && !extend && self.buffer.rules().multiline && self.is_blank() && !self.is_composing() {
                    cx.emit(InputEvent::ArrowUpEmpty);
                    return true;
                }
                self.move_caret(to, extend, cx);
                if extend {
                    self.publish_primary(cx);
                }
            }
            Command::Delete(to) => {
                let changed = match to {
                    Move::Left => self.buffer.delete(Motion::Left),
                    Move::Right => self.buffer.delete(Motion::Right),
                    Move::WordLeft => self.buffer.delete(Motion::WordLeft),
                    Move::WordRight => self.buffer.delete(Motion::WordRight),
                    _ => {
                        // To a line end (macOS Cmd+Backspace): select, then delete.
                        if self.buffer.selection().is_empty() {
                            self.move_caret(to, true, cx);
                        }
                        self.buffer.delete(Motion::Left)
                    }
                };
                if changed {
                    cx.emit(InputEvent::Change);
                    self.after_change(cx);
                }
            }
            Command::SelectAll => {
                self.select_all(cx);
                self.publish_primary(cx);
            }
            Command::Copy => {
                self.copy(cx);
            }
            Command::Cut => {
                if self.copy(cx) && self.buffer.insert("", EditKind::Other) {
                    cx.emit(InputEvent::Change);
                    self.after_change(cx);
                }
            }
            Command::Undo | Command::Redo => {
                let changed = if command == Command::Undo { self.buffer.undo() } else { self.buffer.redo() };
                if changed {
                    cx.emit(InputEvent::Change);
                    self.after_change(cx);
                }
            }
            Command::Enter { shift } => {
                if self.is_composing() {
                    return false;
                }
                let submit = match self.enter {
                    Enter::Submit => true,
                    Enter::Newline => false,
                    Enter::Chat => !shift && !decibell_richtext::composer::has_open_code_fence(self.buffer.text()),
                    Enter::Custom(rule) => rule(self.buffer.text(), shift),
                };
                if submit {
                    self.buffer.close_undo_step();
                    cx.emit(InputEvent::Submit);
                } else if self.buffer.rules().multiline && self.buffer.insert("\n", EditKind::Typing) {
                    cx.emit(InputEvent::Change);
                    self.after_change(cx);
                }
            }
            // Handled by `key_down`, outside the update.
            Command::Escape | Command::Paste => return false,
        }
        let _ = window;
        true
    }

    fn move_caret(&mut self, to: Move, extend: bool, cx: &mut Context<Self>) {
        self.relayout(cx);
        let head = self.buffer.selection().head;
        let multiline = self.buffer.rules().multiline;
        let vertical = matches!(to, Move::Up | Move::Down | Move::PageUp | Move::PageDown);
        let mut affinity = Affinity::Downstream;
        let target = match to {
            Move::Left => self.buffer.target(Motion::Left, extend),
            Move::Right => self.buffer.target(Motion::Right, extend),
            Move::WordLeft => self.buffer.target(Motion::WordLeft, extend),
            Move::WordRight => self.buffer.target(Motion::WordRight, extend),
            Move::DocStart => 0,
            Move::DocEnd => self.buffer.text().len(),
            Move::LineStart | Move::LineEnd => {
                let (start, end) = self.display.visual_line(head, self.affinity);
                let (offset, side) = if to == Move::LineStart { start } else { end };
                affinity = side;
                offset
            }
            // In a single line Up and Down go to the ends, as in Chromium's `<input>`.
            Move::Up | Move::PageUp if !multiline => 0,
            Move::Down | Move::PageDown if !multiline => self.buffer.text().len(),
            Move::Up | Move::Down | Move::PageUp | Move::PageDown => {
                let down = matches!(to, Move::Down | Move::PageDown);
                let (x, top, height) = self.display.caret(head, self.affinity);
                let goal = *self.goal_x.get_or_insert(x);
                let found = if matches!(to, Move::PageUp | Move::PageDown) {
                    // A page is what is visible, less one line of context.
                    let page = (f32::from(self.visible.size.height) - height).max(height);
                    let y = top + height / 2. + if down { page } else { -page };
                    (y >= 0. && y < self.display.height()).then(|| self.display.hit(goal, y))
                } else {
                    self.display.vertical(head, self.affinity, goal, down)
                };
                match found {
                    Some((offset, side)) => {
                        affinity = side;
                        offset
                    }
                    None if down => self.buffer.text().len(),
                    None => 0,
                }
            }
        };
        if !vertical {
            self.goal_x = None;
        }
        let moved = self.buffer.move_to(target, extend);
        if moved || affinity != self.affinity {
            self.affinity = affinity;
        }
        self.after_move(cx);
    }

    // ---- mouse (points are relative to the text's top-left) ----

    pub(crate) fn mouse_down(&mut self, at: Point<f32>, clicks: usize, shift: bool, cx: &mut Context<Self>) {
        self.relayout(cx);
        let (offset, affinity) = self.display.hit(at.x, at.y);
        self.goal_x = None;
        match clicks {
            0 | 1 => {
                self.buffer.move_to(offset, shift);
                self.affinity = affinity;
                self.drag = Some(Drag::Caret);
            }
            2 => {
                let (start, end) = self.buffer.word_at(offset);
                self.buffer.select(Selection { anchor: start, head: end });
                self.drag = Some(Drag::Word(start, end));
            }
            _ => {
                // Chromium selects the paragraph; a single line is one.
                let (start, end) = self.buffer.line_at(offset);
                self.buffer.select(Selection { anchor: start, head: end });
                self.drag = Some(Drag::Line(start, end));
            }
        }
        self.after_move(cx);
    }

    /// The pointer moved with the button down. `outside`: it left the visible part of the
    /// field, so the field keeps scrolling towards it until it comes back or the button
    /// is released.
    pub(crate) fn mouse_drag(&mut self, at: Point<f32>, outside: bool, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        let (offset, affinity) = self.display.hit(at.x, at.y);
        let changed = match drag {
            Drag::Caret => {
                self.affinity = affinity;
                self.buffer.move_to(offset, true)
            }
            Drag::Word(start, end) | Drag::Line(start, end) => {
                let (from, to) = if matches!(drag, Drag::Word(..)) {
                    self.buffer.word_at(offset)
                } else {
                    self.buffer.line_at(offset)
                };
                // The first word stays selected whichever way the drag goes.
                let selection = if offset < start {
                    Selection { anchor: end, head: from }
                } else {
                    Selection { anchor: start, head: to.max(end) }
                };
                self.buffer.select(selection)
            }
        };
        if changed {
            self.after_move(cx);
        }
        if outside && self.autoscroll.is_none() {
            self.autoscroll = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(AUTOSCROLL_INTERVAL).await;
                    let dragging = this.update(cx, |this, cx| {
                        if this.drag.is_none() {
                            return false;
                        }
                        let (at, outside) = this.drag_target();
                        if outside {
                            this.mouse_drag(at, true, cx);
                        }
                        outside
                    });
                    if !matches!(dragging, Ok(true)) {
                        break;
                    }
                }
                let _ = this.update(cx, |this, _| this.autoscroll = None);
            }));
        }
    }

    /// Where a drag at the last pointer position points in the text, and whether the
    /// pointer is outside the visible part. Outside, the point is held one step past the
    /// edge, so each autoscroll tick extends the selection by about a character or a line.
    pub(crate) fn drag_target(&self) -> (Point<f32>, bool) {
        let visible = self.visible;
        let step = self.display.line_height().max(1.);
        let pointer = self.drag_point;
        let outside = !visible.contains(&pointer);
        let x = pointer.x.clamp(visible.left() - px(step / 2.), visible.right() + px(step / 2.));
        let y = pointer.y.clamp(visible.top() - px(step), visible.bottom() + px(step));
        (self.text_point(point(x, y)), outside)
    }

    /// A window point → a point relative to the text's top-left.
    pub(crate) fn text_point(&self, at: Point<Pixels>) -> Point<f32> {
        point(
            f32::from(at.x - self.bounds.origin.x) + self.scroll_x,
            f32::from(at.y - self.bounds.origin.y),
        )
    }

    pub(crate) fn mouse_up(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            self.autoscroll = None;
            self.publish_primary(cx);
        }
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    fn utf16_range(&self, range: &Range<usize>) -> Range<usize> {
        let text = self.buffer.text();
        byte_to_utf16(text, range.start)..byte_to_utf16(text, range.end)
    }

    fn byte_range(&self, range_utf16: &Range<usize>) -> Range<usize> {
        let text = self.buffer.text();
        utf16_to_byte(text, range_utf16.start)..utf16_to_byte(text, range_utf16.end)
    }
}

/// GPUI's IME protocol. Offsets crossing it are UTF-16 code units.
impl EntityInputHandler for InputState {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.buffer.rules().masked {
            // An input method has no business reading a password.
            return None;
        }
        let range = self.byte_range(&range_utf16);
        *adjusted_range = Some(self.utf16_range(&range));
        Some(self.buffer.text()[range].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let selection = self.buffer.selection();
        Some(UTF16Selection {
            range: self.utf16_range(&selection.range()),
            reversed: selection.head < selection.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer.marked().map(|range| self.utf16_range(&range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_composing() {
            self.buffer.unmark();
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        // The platform sends every unhandled key's character here, Tab and Enter
        // included: a field takes line breaks only through its Enter policy.
        if text.chars().count() == 1 && text.chars().all(char::is_control) {
            return;
        }
        let range = range_utf16.map(|range| self.byte_range(&range));
        if self.buffer.commit(range, text) {
            cx.emit(InputEvent::Change);
        }
        self.after_change(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let range = range_utf16.map(|range| self.byte_range(&range));
        let selected =
            new_selected_range_utf16.map(|range| utf16_to_byte(new_text, range.start)..utf16_to_byte(new_text, range.end));
        if self.buffer.set_marked(range, new_text, selected) {
            cx.emit(InputEvent::Change);
        }
        self.after_change(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.relayout(cx);
        let range = self.byte_range(&range_utf16);
        let (x, top, height) = self.display.caret(range.start, Affinity::Downstream);
        let (end_x, end_top, _) = self.display.caret(range.end, Affinity::Upstream);
        // A range on one line is its span; one that wraps is its first caret.
        let width = if end_top == top { (end_x - x).max(0.) } else { 0. };
        Some(Bounds::new(
            point(
                element_bounds.origin.x + px(x - self.scroll_x),
                element_bounds.origin.y + px(top),
            ),
            size(px(width), px(height)),
        ))
    }

    fn character_index_for_point(&mut self, at: Point<Pixels>, _: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
        self.relayout(cx);
        let at = self.text_point(at);
        let (offset, _) = self.display.hit(at.x, at.y);
        Some(byte_to_utf16(self.buffer.text(), offset))
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(utf16_len(self.buffer.text()))
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.disabled && !self.buffer.rules().read_only
    }
}

impl Render for InputState {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        InputElement::new(cx.entity())
    }
}
