//! The inline editor (`InlineEditor` in MessageBubble.tsx): what the message list shows in place of
//! a message's body while it is edited (the list's `editor` slot).
//!
//! A plain text area over the raw wire string (no emoji atoms, no shortcode expansion, no
//! preview), `rounded-md border bg-bg-lighter px-3 py-2 text-body leading-body`, focused with the
//! caret at the end, and the hint under it: "escape to **cancel** • enter to **save**". Enter saves
//! the trimmed text (nothing when it is empty: no delete prompt), Shift+Enter breaks the line,
//! Escape cancels. The list asks for the editor's height before it lays the row out
//! ([`InlineEditorSlot::height`]): it is computed from the text exactly as the field lays it out
//! (one paragraph per hard line, an empty line as a space), so the row never jumps; when an edit
//! changes the number of lines the list is asked to measure again (`invalidate_slots`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use decibell_chat::{CancelEdit, EditorSlot, SubmitEdit};
use decibell_model::chat::Message;
use decibell_text::input::{Enter, InputEvent, InputState, InputStyle, TextArea};
use decibell_text::{Engine, FontSpec, Layout, LineHeight, ParagraphBuilder, TextStyle, Wrapping};
use decibell_ui::{ActiveTheme as _, FontRole, Label};
use gpui::Focusable as _;
use gpui::{AnyElement, App, AppContext as _, Context, Entity, IntoElement, Render, Subscription, Window, div, prelude::*, px};

use crate::ink::Ink;

/// `border` (2) + `py-2` (16).
const FIELD_CHROME: f32 = 2. + 16.;
/// `px-3` + the border: the text's width is the column's less this.
const FIELD_INSET_X: f32 = 2. * 12. + 2.;
/// The hint's `mt-1`.
const HINT_GAP: f32 = 4.;

/// The editor's text style: the message body's (`text-body leading-body`, the body face and
/// tracking), built from the theme and the engine without `&mut App` (the list measures with `&App`).
fn body_style(engine: &Engine, cx: &App) -> TextStyle {
    let theme = cx.theme();
    let face = engine.face(&FontSpec { family: theme.font(FontRole::Sans), weight: 400, italic: false });
    let mut style = TextStyle::new(face, theme.scaled.text_body, theme.color_text_primary);
    style.line_height = LineHeight::Factor(theme.palette.t_lh_body);
    // The body's letter-spacing, px of the 16 px root size (as `Label` and the rows take it).
    style.letter_spacing = theme.palette.t_ls_body * 16.;
    style
}

/// The hint line's style: `text-meta` in the body face, line-height 1.5 (inherited).
fn hint_height(cx: &App) -> f32 {
    decibell_chat::style::layout_unit(cx.theme().scaled.text_meta * 1.5)
}

/// The text's height at `width` as the field lays it out: a paragraph per hard line (an empty one
/// as a space), wrapped `pre-wrap`; whole device pixels.
pub fn text_height(text: &str, style: &TextStyle, width: f32, scale: f32, engine: &Engine) -> f32 {
    let mut height = 0.;
    for line in text.split('\n') {
        let mut builder = ParagraphBuilder::new(engine, style, Wrapping::PRE_WRAP, scale);
        builder.text(if line.is_empty() { " " } else { line }, style, 0);
        let layout = Layout::new(Rc::new(builder.build()), width, engine);
        height += layout.height();
    }
    (height * scale).ceil() / scale
}

/// The editor's whole height (field, gap, hint) for `text` at the content column's `width`.
pub fn editor_height(text: &str, width: f32, scale: f32, engine: &Engine, cx: &App) -> f32 {
    let style = body_style(engine, cx);
    FIELD_CHROME + text_height(text, &style, (width - FIELD_INSET_X).max(1.), scale, engine) + HINT_GAP + hint_height(cx)
}

type Current = Rc<RefCell<Option<(i64, Entity<InlineEditor>)>>>;

/// The registered slot (the self-tests read the editor on screen through it).
pub struct EditorRegistry(pub Rc<InlineEditorSlot>);

impl gpui::Global for EditorRegistry {}

/// The inline editor on screen, if a message is being edited.
pub fn current_editor(cx: &App) -> Option<Entity<InlineEditor>> {
    cx.try_global::<EditorRegistry>().and_then(|registry| registry.0.current())
}

/// The list's `editor` slot: one [`InlineEditor`] for the message being edited.
pub struct InlineEditorSlot {
    engine: Option<Rc<Engine>>,
    current: Current,
}

impl Default for InlineEditorSlot {
    fn default() -> Self {
        Self { engine: None, current: Rc::default() }
    }
}

impl InlineEditorSlot {
    pub fn new(engine: Rc<Engine>) -> Self {
        Self { engine: Some(engine), current: Rc::default() }
    }

    /// The editor on screen, if a message is being edited.
    pub fn current(&self) -> Option<Entity<InlineEditor>> {
        self.current.borrow().as_ref().map(|(_, editor)| editor.clone())
    }
}

impl EditorSlot for InlineEditorSlot {
    fn height(&self, message: &Message, width: f32, scale: f32, cx: &App) -> f32 {
        let Some(engine) = &self.engine else { return 0. };
        let current = self.current.borrow();
        let text = match current.as_ref() {
            Some((id, editor)) if *id == message.id => editor.read(cx).input.read(cx).value().to_string(),
            _ => message.content.to_string(),
        };
        editor_height(&text, width, scale, engine, cx)
    }

    fn element(&self, message: &Arc<Message>, width: f32, submit: SubmitEdit, cancel: CancelEdit, window: &mut Window, cx: &mut App) -> AnyElement {
        let existing = self.current.borrow().as_ref().filter(|(id, _)| *id == message.id).map(|(_, editor)| editor.clone());
        let editor = match existing {
            Some(editor) => editor,
            None => {
                let current = self.current.clone();
                let editor = cx.new(|cx| InlineEditor::new(message, current, window, cx));
                *self.current.borrow_mut() = Some((message.id, editor.clone()));
                editor
            }
        };
        editor.update(cx, |editor, _| {
            editor.submit = Some(submit);
            editor.cancel = Some(cancel);
            editor.width.set(width);
        });
        editor.into_any_element()
    }
}

/// The edit box of one message.
pub struct InlineEditor {
    pub message_id: i64,
    input: Entity<InputState>,
    submit: Option<SubmitEdit>,
    cancel: Option<CancelEdit>,
    current: Current,
    /// The column width it was last laid out at, and the height the list was last told.
    width: Cell<f32>,
    told: Cell<f32>,
    _subscriptions: Vec<Subscription>,
}

impl InlineEditor {
    fn new(message: &Message, current: Current, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Enter saves, Shift+Enter breaks the line (no fence rule here).
        let input = cx.new(|cx| InputState::multi_line(window, cx).with_value(&message.content).enter(Enter::Custom(|_, shift| !shift)));
        let focus = input.clone();
        window.defer(cx, move |window, cx| focus.update(cx, |input, cx| input.focus(window, cx)));
        let subscriptions = vec![cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Submit => {
                let trimmed = input.read(cx).value().trim().to_string();
                if !trimmed.is_empty() {
                    this.finish(Some(trimmed), window, cx);
                }
            }
            InputEvent::Escape => this.finish(None, window, cx),
            InputEvent::Change => this.text_changed(window, cx),
            _ => {}
        })];
        Self { message_id: message.id, input, submit: None, cancel: None, current, width: Cell::new(0.), told: Cell::new(0.), _subscriptions: subscriptions }
    }

    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// A different number of lines: the list measures the row again.
    fn text_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width = self.width.get();
        if width <= 0. {
            return;
        }
        let engine = Engine::global(cx);
        let height = editor_height(self.input.read(cx).value(), width, window.scale_factor(), &engine, cx);
        if (height - self.told.get()).abs() > 0.01 {
            self.told.set(height);
            decibell_chat::invalidate_slots(cx);
        }
    }

    /// Save (`Some(text)`) or cancel; the focus goes back to the composer.
    pub fn finish(&mut self, text: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.current.borrow_mut().take();
        match text {
            Some(text) => {
                if let Some(submit) = self.submit.clone() {
                    submit(text, window, cx);
                }
            }
            None => {
                if let Some(cancel) = self.cancel.clone() {
                    cancel(window, cx);
                }
            }
        }
        crate::composer::focus_active_composer(window, cx);
    }
}

impl Render for InlineEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let engine = Engine::global(cx);
        let mut style = InputStyle::new(body_style(&engine, cx));
        style.placeholder_color = cx.theme().color_text_muted;
        let ink = Ink::of(cx);
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        if self.told.get() == 0. {
            self.told.set(editor_height(self.input.read(cx).value(), self.width.get().max(1.), window.scale_factor(), &engine, cx));
        }
        let hint = |text: &'static str, color| Label::new(text).size(ink.text_meta).color(color).nowrap();
        // The hint's words as the renderer spaces them (one space between the pieces).
        let space = {
            let theme = cx.theme();
            let face = engine.face(&FontSpec { family: theme.font(FontRole::Sans), weight: 400, italic: false });
            let s = TextStyle::new(face, ink.text_meta, ink.color_text_muted);
            let scale = window.scale_factor();
            engine.label_layout("a b", &s, Wrapping::NOWRAP, f32::INFINITY, scale).width() - engine.label_layout("ab", &s, Wrapping::NOWRAP, f32::INFINITY, scale).width()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .rounded(ink.radius_md)
                    .border_1()
                    .border_color(if focused { ink.color_accent } else { ink.color_border })
                    .bg(ink.color_bg_lighter)
                    .px_3()
                    .py_2()
                    .child(TextArea::new(&self.input, style)),
            )
            .child(
                div()
                    .mt_1()
                    .flex()
                    .gap(px(space))
                    .child(hint("escape to", ink.color_text_muted))
                    .child(
                        div()
                            .id("edit-cancel")
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| this.finish(None, window, cx)))
                            .child(Label::new("cancel").size(ink.text_meta).color(ink.color_accent).nowrap()),
                    )
                    .child(hint("•", ink.color_text_muted))
                    .child(hint("enter to", ink.color_text_muted))
                    .child(hint("save", ink.color_accent)),
            )
    }
}
