//! [`Composer`]: the message box under a chat (ChatPanel.tsx's input bar, DmChatPanel.tsx's).
//!
//! What re-renders when: a key re-renders the input (its own view) and its ancestors, this view
//! among them (GPUI marks a notified view's ancestors dirty); the message list beside it is a
//! cached sibling and replays. This view keeps its render cheap: the preview strip's layout is
//! kept per draft, the chips and pill read small stores. It notifies only for what it draws: its
//! draft's derived flags, its channel's attachments, slowmode window and gate, the reply target.
//!
//! Timers, each with its live reason: the slowmode countdown (once a second, only while a wait
//! runs), the timeout notice's countdown (only while timed out), the drop pulse's breathing
//! (only while a file is dragged over the window), the caret blink (the input's, only while it is
//! focused in the active window). Nothing runs at rest.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use decibell_attachments::upload::UploadSource;
use decibell_chat::style::ChatStyle;
use decibell_chat::{ListEvent, MessageList};
use decibell_model::actions::chat::SendOutcome;
use decibell_model::chat::{ComposerEvent, DraftKey, DraftsEvent, Message, SlowmodeEvent};
use decibell_model::Model;
use decibell_text::input::{InputEvent, InputState, TextArea};
use decibell_ui::{ActiveTheme as _, FontRole, Label, ScrollAlign, ScrollArea, ScrollHandle, TextField};
use gpui::Focusable as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClipboardEntry, ClipboardItem, Context, Entity, ExternalPaths, FocusHandle, IntoElement,
    KeyDownEvent, Pixels, Render, SharedString, Subscription, Task, WeakEntity, Window, canvas, div, prelude::*, px,
};

use crate::chrome;
use crate::panel::{SnippetKind, SnippetPanel};
use crate::picker::Picker;
use crate::preview::{DraftBody, PreviewCache};
use crate::target::{ComposerTarget, Gate, Variant, reply_parent};

/// The input's `maxHeight` (RichInput): it grows to this, then scrolls.
pub const INPUT_MAX_HEIGHT: f32 = 160.;
/// The preview body's `max-h-44`.
const PREVIEW_MAX_HEIGHT: f32 = 176.;
/// The emoji picker's fixed size (`EMOJI_PICKER_SIZE`).
pub const PICKER_WIDTH: f32 = 352.;
pub const PICKER_HEIGHT: f32 = 420.;

/// Counters for the self-tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct ComposerStats {
    pub renders: u64,
}

/// What the snippet trigger shows: nothing, its little menu, or a panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Snippet {
    Closed,
    Menu,
    Panel,
}

pub struct Composer {
    target: Rc<dyn ComposerTarget>,
    variant: Variant,
    key: DraftKey,
    input: Entity<InputState>,
    input_scroll: ScrollHandle,
    card_focus: FocusHandle,
    list: Option<WeakEntity<MessageList>>,
    gate: Gate,
    send_error: Option<SharedString>,
    /// A send's call is in flight (DMs): the send button is disabled.
    sending: bool,
    /// The message being replied to, as it was picked (the bar follows its edits while loaded).
    reply: Option<Arc<Message>>,
    reply_watch: Option<Subscription>,
    /// The draft's derived facts: something to send; the preview's text when it shows.
    has_text: bool,
    preview: Option<SharedString>,
    preview_collapsed: bool,
    preview_cache: PreviewCache,
    preview_scroll: ScrollHandle,
    chat_style: Option<((usize, u32, u32), Rc<ChatStyle>)>,
    /// The slowmode countdown: re-renders once a second while a wait runs.
    slow_tick: Option<Task<()>>,
    nudges: u32,
    /// The timed-out notice's countdown.
    timeout_tick: Option<Task<()>>,
    picker: Option<Entity<Picker>>,
    snippet: Snippet,
    panel: Option<Entity<SnippetPanel>>,
    /// Last frame's boxes: the emoji button's wrapper (the picker opens above it, right edges
    /// aligned), the snippet trigger (its menu), the card (the panel spans it).
    emoji_anchor: Rc<Cell<Bounds<Pixels>>>,
    snippet_anchor: Rc<Cell<Bounds<Pixels>>>,
    card_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// A file drag over the window (`dragActive`), and whether it is over the card
    /// (`dragHoveredKey === "active-input"`).
    file_drag: Option<bool>,
    pub stats: ComposerStats,
    _subscriptions: Vec<Subscription>,
    _target_watch: Vec<Subscription>,
}

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

/// The composer last mounted (the one on screen: one chat pane shows at a time): where the focus
/// returns when the inline editor closes.
struct ActiveComposer(WeakEntity<Composer>);

impl gpui::Global for ActiveComposer {}

/// Focus the input of the composer on screen (after the inline editor closed).
pub(crate) fn focus_active_composer(window: &mut Window, cx: &mut App) {
    let Some(composer) = cx.try_global::<ActiveComposer>().and_then(|active| active.0.upgrade()) else { return };
    let input = composer.read(cx).input.clone();
    input.update(cx, |input, cx| input.focus(window, cx));
}

impl Composer {
    /// A composer sending through `target`, under `list` (its reply arrows and "edit last").
    pub fn new(target: Rc<dyn ComposerTarget>, list: Option<Entity<MessageList>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key = target.draft_key();
        let draft = model(cx).map(|m| m.chat.drafts.read(cx).get(&key)).unwrap_or_default();
        let gate = target.gate(cx);
        let placeholder = target.placeholder(&gate, cx);
        let input = cx.new(|cx| InputState::composer(window, cx).placeholder(placeholder).with_value(&draft));
        let input_scroll = ScrollHandle::new();
        let weak = cx.weak_entity();
        input.update(cx, |state, _| {
            let escape = weak.clone();
            state.on_escape(move |window, cx| escape.update(cx, |this, cx| this.escape(window, cx)).unwrap_or(false));
            let paste = weak.clone();
            state.on_paste_non_text(move |item, window, cx| paste.update(cx, |this, cx| this.paste(item, window, cx)).unwrap_or(false));
            let scroll = input_scroll.clone();
            state.on_reveal_caret(move |caret, _| scroll.scroll_into_view(caret.top(), caret.size.height, ScrollAlign::Nearest));
        });
        let mut subscriptions = vec![cx.subscribe_in(&input, window, Self::input_event)];
        if let Some(model) = model(cx) {
            subscriptions.push(cx.subscribe(&model.chat.drafts, |this, drafts, event: &DraftsEvent, cx| {
                // A refused or unconfirmed send put its text back: show it.
                if let DraftsEvent::Restored(key) = event
                    && *key == this.key
                {
                    let text = drafts.read(cx).get(key);
                    this.input.update(cx, |input, cx| input.set_value(&text, cx));
                }
            }));
            if let Some((server, channel)) = target.slowmode_key() {
                subscriptions.push(cx.subscribe(&model.chat.composer, move |_, _, event: &ComposerEvent, cx| {
                    let ComposerEvent::Changed(s, c) = event;
                    if *s == server && *c == channel {
                        cx.notify();
                    }
                }));
            }
            if let Some(slow_key) = target.slowmode_key() {
                subscriptions.push(cx.subscribe(&model.chat.slowmode, move |this, _, event: &SlowmodeEvent, cx| match event {
                    SlowmodeEvent::Changed(key) if *key == slow_key => {
                        this.restart_slow_tick(cx);
                        cx.notify();
                    }
                    SlowmodeEvent::Nudged(key) if *key == slow_key => {
                        this.nudges += 1;
                        cx.notify();
                    }
                    _ => {}
                }));
            }
        }
        if let Some(list) = &list {
            subscriptions.push(cx.subscribe_in(list, window, |this, _, event: &ListEvent, window, cx| {
                if let ListEvent::Reply(id) = event {
                    this.start_reply(*id, window, cx);
                }
            }));
        }
        cx.set_global(ActiveComposer(cx.weak_entity()));
        let target_watch = target.watch(cx);
        let send_error = target.send_error(cx);
        let sending = target.sending(cx);
        let mut this = Self {
            variant: target.variant(),
            key,
            target,
            input,
            input_scroll,
            card_focus: cx.focus_handle(),
            list: list.map(|list| list.downgrade()),
            gate,
            send_error,
            sending,
            reply: None,
            reply_watch: None,
            has_text: false,
            preview: None,
            preview_collapsed: false,
            preview_cache: PreviewCache::default(),
            preview_scroll: ScrollHandle::new(),
            chat_style: None,
            slow_tick: None,
            nudges: 0,
            timeout_tick: None,
            picker: None,
            snippet: Snippet::Closed,
            panel: None,
            emoji_anchor: Rc::default(),
            snippet_anchor: Rc::default(),
            card_bounds: Rc::default(),
            file_drag: None,
            stats: ComposerStats::default(),
            _subscriptions: subscriptions,
            _target_watch: target_watch,
        };
        this.draft_changed(cx);
        this.restart_slow_tick(cx);
        this.restart_timeout_tick(cx);
        this
    }

    /// The pane's "type anywhere" hook for this composer (`ComposerParts::type_to_focus`).
    pub fn type_to_focus_hook(composer: &Entity<Composer>) -> decibell_chat::TypeToFocus {
        let weak = composer.downgrade();
        Rc::new(move |event, window, cx| weak.update(cx, |this, cx| this.key_from_pane(event, window, cx)).unwrap_or(false))
    }

    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// The inline send error shown above the card (DMs).
    pub fn send_error(&self) -> Option<&SharedString> {
        self.send_error.as_ref()
    }

    /// A send's call is in flight: the send button is disabled (DMs).
    pub fn sending(&self) -> bool {
        self.sending
    }

    /// The composer on screen (the one mounted last), if it is still alive.
    pub fn active(cx: &App) -> Option<Entity<Composer>> {
        cx.try_global::<ActiveComposer>().and_then(|active| active.0.upgrade())
    }

    /// The list this composer sits under.
    pub fn list(&self) -> Option<Entity<MessageList>> {
        self.list.as_ref().and_then(WeakEntity::upgrade)
    }

    pub fn picker(&self) -> Option<&Entity<Picker>> {
        self.picker.as_ref()
    }

    pub fn panel(&self) -> Option<&Entity<SnippetPanel>> {
        self.panel.as_ref()
    }

    pub fn reply(&self) -> Option<i64> {
        self.reply.as_ref().map(|m| m.id)
    }

    /// The slowmode interval that applies here, the seconds left of its window, how many sends
    /// it held, whether the countdown's timer runs.
    pub fn slowmode_state(&self, cx: &App) -> (u32, u32, u32, bool) {
        (self.gate.slowmode, self.slow_remaining(cx), self.nudges, self.slow_tick.is_some())
    }

    pub fn preview_shown(&self) -> bool {
        self.preview.is_some()
    }

    pub fn target(&self) -> &Rc<dyn ComposerTarget> {
        &self.target
    }

    /// The card's box in the last frame (window coordinates).
    pub fn card_bounds(&self) -> Bounds<Pixels> {
        self.card_bounds.get()
    }

    /// The emoji button's box in the last frame.
    pub fn emoji_button_bounds(&self) -> Bounds<Pixels> {
        self.emoji_anchor.get()
    }

    // ── Input ───────────────────────────────────────────────────────────────────────────────

    fn input_event(&mut self, _: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                let value: SharedString = self.input.read(cx).value().to_string().into();
                self.target.set_draft(value, cx);
                self.draft_changed(cx);
            }
            InputEvent::Submit => self.send(window, cx),
            InputEvent::ArrowUpEmpty => self.edit_latest(window, cx),
            // The card's focus ring follows the input (GPUI redraws the window on a focus change).
            InputEvent::Focus | InputEvent::Blur | InputEvent::Escape => {}
        }
    }

    /// The draft's derived facts; notifies only when what this view draws from them changed.
    fn draft_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.input.read(cx).value();
        let has_text = !value.trim().is_empty();
        let preview = decibell_richtext::composer::preview_visible(value).then(|| SharedString::from(value.to_string()));
        if preview.is_none() && self.preview.is_some() {
            self.preview_cache.clear();
        }
        if has_text != self.has_text || preview != self.preview {
            self.has_text = has_text;
            self.preview = preview;
            cx.notify();
        }
    }

    /// The stores the gate reads changed (permissions, timeout, slowmode interval, the DM's
    /// send error and send in flight): draw again only when the answer did.
    pub(crate) fn refresh_gate(&mut self, cx: &mut Context<Self>) {
        let gate = self.target.gate(cx);
        let send_error = self.target.send_error(cx);
        let sending = self.target.sending(cx);
        if sending != self.sending {
            self.sending = sending;
            cx.notify();
        }
        if gate != self.gate || send_error != self.send_error {
            let placeholder = self.target.placeholder(&gate, cx);
            self.input.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
            let timeout = gate.timed_out_until != self.gate.timed_out_until;
            let slowmode = gate.slowmode != self.gate.slowmode;
            self.gate = gate;
            self.send_error = send_error;
            if timeout {
                self.restart_timeout_tick(cx);
            }
            if slowmode {
                self.restart_slow_tick(cx);
            }
            cx.notify();
        }
    }

    // ── Sending ─────────────────────────────────────────────────────────────────────────────

    /// Enter or the send button (`handleSend`).
    pub fn send(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        let reply_to = self.reply.as_ref().map_or(0, |m| m.id);
        match self.target.send(&text, reply_to, cx) {
            SendOutcome::Sent { .. } => {
                self.input.update(cx, |input, cx| input.clear(cx));
                self.set_reply(None, cx);
                if self.variant == Variant::Dm {
                    // DmChatPanel closes the picker on a send.
                    self.picker = None;
                }
            }
            // Slowmode: the text stays, the pill shakes (SlowmodeEvent::Nudged). A windowed
            // slice jumped to the present: the draft stays for the next Enter.
            SendOutcome::HeldBySlowmode | SendOutcome::JumpedToPresent | SendOutcome::Nothing => {}
        }
    }

    /// A GIF from the picker: its URL as a message of its own; the draft stays, a reply target
    /// is consumed by it (`sendGif`).
    pub fn send_gif(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let reply_to = self.reply.as_ref().map_or(0, |m| m.id);
        if let SendOutcome::Sent { .. } = self.target.send_gif(url, reply_to, cx) {
            self.set_reply(None, cx);
        }
        self.close_picker(window, cx);
    }

    /// ArrowUp on an empty composer, or with nothing focused: edit our latest message.
    pub fn edit_latest(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.target.latest_own_editable(cx) else { return };
        if let Some(list) = self.list.as_ref().and_then(WeakEntity::upgrade) {
            list.update(cx, |list, cx| list.start_edit(id, cx));
        }
    }

    // ── Replies ─────────────────────────────────────────────────────────────────────────────

    /// The reply arrow on a message (`startReply`): the bar, the rail, the focus.
    pub fn start_reply(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if id <= 0 {
            return;
        }
        let Some(slice) = self.target.slice(cx) else { return };
        let Some(message) = slice.read(cx).find(id).cloned() else { return };
        self.reply = Some(message);
        // The bar follows edits of its parent while it is loaded.
        self.reply_watch = Some(cx.observe(&slice, |_, _, cx| cx.notify()));
        if let Some(list) = self.list.as_ref().and_then(WeakEntity::upgrade) {
            list.update(cx, |list, cx| list.set_reply_target(Some(id), cx));
        }
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn set_reply(&mut self, message: Option<Arc<Message>>, cx: &mut Context<Self>) {
        if message.is_none() && self.reply.is_none() {
            return;
        }
        self.reply = message;
        if self.reply.is_none() {
            self.reply_watch = None;
            if let Some(list) = self.list.as_ref().and_then(WeakEntity::upgrade) {
                list.update(cx, |list, cx| list.set_reply_target(None, cx));
            }
        }
        cx.notify();
    }

    /// Escape in the input (`escapeReply`): the picker closes first (its own Escape), else a
    /// pending reply is dropped. `true` swallows the key.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.picker.is_some() {
            self.close_picker(window, cx);
            return true;
        }
        if self.snippet != Snippet::Closed {
            self.close_snippet(window, cx);
            return true;
        }
        if self.reply.is_some() {
            self.set_reply(None, cx);
            return true;
        }
        false
    }

    // ── Keys from the pane ──────────────────────────────────────────────────────────────────

    /// `useTypeToFocusComposer`: a key that reached the pane or the window's root with no text
    /// field focused. Not with Ctrl / Alt / Super held, a modal up, the composer hidden by its
    /// host or replaced by a notice, nor for a key the focused hotkey fallback takes (a bare-key
    /// push to talk: the renderer would press it *and* type it, `docs/electron-quirks.md` #17).
    pub(crate) fn key_from_pane(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return false;
        }
        if decibell_text::input::text_field_focused(window) {
            return false;
        }
        if let Some(model) = model(cx) {
            // A modal owns the keyboard while it is up.
            if model.overlays.read(cx).active_modal.is_some() {
                return false;
            }
            if model.live.hotkeys.read(cx).would_take(event.code, &event.keystroke) {
                return false;
            }
        }
        if self.gate_blocks() || self.target.hidden(cx) {
            return false;
        }
        if event.keystroke.key == "up" {
            self.edit_latest(window, cx);
            return true;
        }
        self.input.update(cx, |input, cx| input.type_to_focus(event, window, cx))
    }

    fn gate_blocks(&self) -> bool {
        !self.gate.can_send || self.timed_out()
    }

    fn timed_out(&self) -> bool {
        self.gate.timed_out_until > now_secs()
    }

    // ── Emoji, GIFs, snippets ───────────────────────────────────────────────────────────────

    pub fn toggle_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            self.close_picker(window, cx);
        } else {
            self.open_picker(window, cx);
        }
    }

    pub fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            return;
        }
        let started = Instant::now();
        let weak = cx.weak_entity();
        let anchor = self.emoji_anchor.clone();
        let picker = cx.new(|cx| Picker::new(weak, anchor, window, cx));
        picker.update(cx, |picker, _| picker.opened_at = Some(started));
        self.picker = Some(picker);
        cx.notify();
    }

    /// Closes the picker and gives back what it alone held (the emoji it rasterised, the GIF
    /// previews it fetched and decoded), then returns the focus to the input.
    pub fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = self.picker.take() {
            picker.update(cx, |picker, cx| picker.release(window, cx));
            self.input.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
            // What the picker freed goes back to the system once its frame is gone (glibc keeps
            // freed heap otherwise; a picker's images and elements are a few MB).
            #[cfg(all(target_os = "linux", target_env = "gnu"))]
            cx.on_next_frame(window, |_, _, _| {
                // SAFETY: malloc_trim only walks and trims the allocator's own free lists.
                unsafe {
                    libc::malloc_trim(0);
                }
            });
        }
    }

    /// An emoji from the picker: inserted at the caret as an atom; the picker stays open.
    pub fn insert_emoji(&mut self, native: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.insert_text(native, cx);
        });
    }

    fn toggle_snippet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.snippet {
            Snippet::Panel => self.close_snippet(window, cx),
            Snippet::Menu => {
                self.snippet = Snippet::Closed;
                cx.notify();
            }
            Snippet::Closed => {
                self.snippet = Snippet::Menu;
                cx.notify();
            }
        }
    }

    pub fn open_snippet(&mut self, kind: SnippetKind, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        self.panel = Some(cx.new(|cx| SnippetPanel::new(kind, weak, window, cx)));
        self.snippet = Snippet::Panel;
        cx.notify();
    }

    pub fn close_snippet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.snippet == Snippet::Panel {
            self.input.update(cx, |input, cx| input.focus(window, cx));
        }
        self.snippet = Snippet::Closed;
        self.panel = None;
        cx.notify();
    }

    /// `insertSnippet`: a fence or a formula appended to the draft on a line of its own.
    pub fn insert_snippet(&mut self, snippet: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            let current = input.value().to_string();
            let sep = if !current.is_empty() && !current.ends_with('\n') { "\n" } else { "" };
            input.focus(window, cx);
            input.set_value(&format!("{current}{sep}{snippet}"), cx);
        });
        self.close_snippet(window, cx);
    }

    // ── Attachments ─────────────────────────────────────────────────────────────────────────

    /// The attach button (`handlePickFiles`): the system's file dialog, every file kind.
    pub fn pick_files(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let _ = this.update(cx, |this, cx| {
                for path in paths {
                    this.target.attach(UploadSource::Path(path), cx);
                }
            });
        })
        .detach();
    }

    /// Files from anywhere (the dialog, a drop, a paste).
    pub fn attach(&mut self, sources: Vec<UploadSource>, cx: &mut Context<Self>) {
        for source in sources {
            self.target.attach(source, cx);
        }
    }

    /// A paste that holds more than text (`usePasteToAttach`): its files and pictures become
    /// attachments and nothing is typed. DMs have no attachments: the text (if any) pastes.
    fn paste(&mut self, item: &ClipboardItem, _: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.variant != Variant::Channel {
            return false;
        }
        let mut sources = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    let (name, mime) = crate::drop::pasted_image_name(image.format);
                    sources.push(UploadSource::Memory { name: name.into(), mime: mime.into(), data: bytes::Bytes::from(image.bytes.clone()) });
                }
                ClipboardEntry::ExternalPaths(paths) => sources.extend(paths.paths().iter().cloned().map(UploadSource::Path)),
                ClipboardEntry::String(_) => {}
            }
        }
        if sources.is_empty() {
            return false;
        }
        self.attach(sources, cx);
        true
    }

    fn remove_attachment(id: SharedString, cx: &mut App) {
        if let Some(model) = model(cx) {
            model.chat.composer.update(cx, |c, cx| c.remove(&id, cx));
        }
    }

    /// A file drag moved over the window: the pulse, and the overlay while over the card.
    fn drag_moved(&mut self, over_card: bool, cx: &mut Context<Self>) {
        if self.file_drag != Some(over_card) {
            self.file_drag = Some(over_card);
            if let Some(model) = model(cx) {
                model.chat.drag.update(cx, |drag, cx| {
                    drag.set_active(true, cx);
                    drag.set_hovered(over_card.then(|| SharedString::new_static("active-input")), cx);
                });
            }
            cx.notify();
        }
    }

    fn drag_ended(&mut self, cx: &mut Context<Self>) {
        if self.file_drag.take().is_some() {
            if let Some(model) = model(cx) {
                model.chat.drag.update(cx, |drag, cx| drag.set_active(false, cx));
            }
            cx.notify();
        }
    }

    /// Files dropped anywhere on the window while this composer shows (`useDragDrop`: the
    /// active channel), unless another drop target claimed the drag (`DragState::hovered`).
    fn dropped(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        self.drag_ended(cx);
        if self.variant != Variant::Channel {
            return;
        }
        self.attach(paths.paths().iter().cloned().map(UploadSource::Path).collect(), cx);
    }

    // ── Timers ──────────────────────────────────────────────────────────────────────────────

    fn slow_remaining(&self, cx: &App) -> u32 {
        let (Some(key), Some(model)) = (self.target.slowmode_key(), model(cx)) else { return 0 };
        model.chat.slowmode.read(cx).remaining(&key, self.gate.slowmode, cx.background_executor().now())
    }

    /// `useSlowmode`'s tick: wake on the wait's whole-second boundaries, so the shown seconds
    /// change on time and the last one ends exactly when the window does. None at rest.
    fn restart_slow_tick(&mut self, cx: &mut Context<Self>) {
        self.slow_tick = None;
        let (Some(key), Some(model)) = (self.target.slowmode_key(), model(cx)) else { return };
        if self.gate.slowmode == 0 {
            return;
        }
        let Some(until) = model.chat.slowmode.read(cx).until(&key) else { return };
        if until <= cx.background_executor().now() {
            return;
        }
        self.slow_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                let now = cx.background_executor().now();
                if until <= now {
                    break;
                }
                let left = until.duration_since(now).as_millis() as u64;
                let step = match left % 1000 {
                    0 => 1000,
                    part => part,
                };
                cx.background_executor().timer(Duration::from_millis(step)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |this, _| this.slow_tick = None);
        }));
    }

    /// The timed-out notice counts down once a second while the timeout runs.
    fn restart_timeout_tick(&mut self, cx: &mut Context<Self>) {
        self.timeout_tick = None;
        if !self.timed_out() {
            return;
        }
        let until = self.gate.timed_out_until;
        self.timeout_tick = Some(cx.spawn(async move |this, cx| {
            while now_secs() < until {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.timeout_tick = None;
                cx.notify();
            });
        }));
    }

    fn chat_style(&mut self, window: &Window, cx: &mut App) -> Rc<ChatStyle> {
        let stamp = {
            let theme = cx.theme();
            (theme.id.index(), theme.scaled.text_body.to_bits(), window.scale_factor().to_bits())
        };
        match &self.chat_style {
            Some((kept, style)) if *kept == stamp => style.clone(),
            _ => {
                let style = ChatStyle::new(window.scale_factor(), cx);
                self.chat_style = Some((stamp, style.clone()));
                style
            }
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// `formatRemaining`: "45s", "3m", "2h", "1d".
pub fn format_remaining(total: i64) -> String {
    let s = total.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", (s as f64 / 60.).ceil() as i64)
    } else if s < 86_400 {
        format!("{}h", (s as f64 / 3600.).ceil() as i64)
    } else {
        format!("{}d", (s as f64 / 86_400.).ceil() as i64)
    }
}

/// Records an element's box each frame (where a popup anchors).
fn track_bounds(cell: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    // `inset_0`: an absolute child without insets sits where it would in flow (after its
    // siblings), not on its parent's box.
    canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {}).absolute().inset_0()
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.stats.renders += 1;
        // A drag that ended (dropped elsewhere, left the window) leaves no pulse behind, and the
        // drag state ends after this frame.
        if self.file_drag.is_some() && !cx.has_active_drag() {
            self.file_drag = None;
            window.defer(cx, |_, cx| {
                if let Some(model) = cx.try_global::<Model>().cloned() {
                    model.chat.drag.update(cx, |drag, cx| drag.set_active(false, cx));
                }
            });
        }
        let dm = self.variant == Variant::Dm;
        let theme = cx.theme();
        let (bg_light, border, accent, muted, radius_lg) = (theme.color_bg_light, theme.color_border, theme.color_accent, theme.color_text_muted, theme.radius_lg);
        let ring = theme.shadow_ring.clone();

        // No permission, or timed out: a notice instead of the card (ChatPanel).
        if self.gate_blocks() {
            let text = if self.timed_out() {
                format!("You are timed out. You can send messages again in {}.", format_remaining(self.gate.timed_out_until - now_secs()))
            } else {
                format!("You don't have permission to send messages in #{}.", self.gate.name)
            };
            return div()
                .w_full()
                .px_3()
                .py_2()
                .child(
                    div()
                        .flex()
                        .min_h(px(54.))
                        .items_center()
                        .rounded(radius_lg)
                        .border_1()
                        .border_color(border)
                        .bg(bg_light)
                        .px(px(14.))
                        .py(px(10.))
                        .child(Label::new(text).size(13.).color(muted)),
                )
                .into_any_element();
        }

        let focused = self.card_focus.contains_focused(window, cx);
        let over_card = self.file_drag == Some(true);
        let dragging = self.file_drag.is_some() && !dm;
        let entity = cx.entity();

        // The reply bar.
        let reply = self.reply.clone().map(|kept| {
            let parent = reply_parent(&*self.target, &kept, cx);
            let weak = cx.weak_entity();
            chrome::reply_bar(
                self.target.server(),
                &parent,
                Box::new(move |_, _, cx| {
                    let _ = weak.update(cx, |this, cx| this.set_reply(None, cx));
                }),
                cx,
            )
        });

        // The preview strip.
        let preview = self.preview.clone().map(|text| {
            let style = self.chat_style(window, cx);
            let theme = cx.theme();
            let collapsed = self.preview_collapsed;
            div()
                .flex()
                .flex_col()
                .border_b_1()
                .border_color(theme.color_border_divider)
                .pb_2()
                .child(
                    div()
                        .id("preview-toggle")
                        .group("preview-toggle")
                        .mb_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .cursor_pointer()
                        .tooltip(decibell_ui::title_tooltip(if collapsed { "Show preview" } else { "Hide preview" }))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.preview_collapsed = !this.preview_collapsed;
                            cx.notify();
                        }))
                        .child(
                            decibell_ui::Icon::new(if collapsed { decibell_ui::IconName::ComposerChevron } else { decibell_ui::IconName::ComposerChevronOpen })
                                .size(px(10.))
                                .color(theme.color_text_muted)
                                .group_hover("preview-toggle", theme.color_text_secondary),
                        )
                        .child(
                            Label::new("PREVIEW")
                                .size(10.)
                                .weight(600.)
                                .tracking(0.07)
                                .color(theme.color_text_muted)
                                .nowrap(),
                        ),
                )
                .when(!collapsed, |this| {
                    this.child(
                        ScrollArea::new("preview-body")
                            .track_scroll(&self.preview_scroll)
                            .max_h(px(PREVIEW_MAX_HEIGHT))
                            .child(DraftBody::new(text, style, self.preview_cache.clone(), Some(entity.entity_id()))),
                    )
                })
        });

        // The attachment chips (channels).
        let chips = match (self.target.slowmode_key(), model(cx)) {
            (Some((server, channel)), Some(model)) => {
                let composer = model.chat.composer.read(cx);
                chrome::chips(composer.for_channel(server, &channel), |id, _, cx| Self::remove_attachment(id, cx), cx)
            }
            _ => None,
        };
        let live_pendings = match (self.target.slowmode_key(), model(cx)) {
            (Some((server, channel)), Some(model)) => model
                .chat
                .composer
                .read(cx)
                .for_channel(server, &channel)
                .any(|p| p.status != decibell_model::chat::composer::PendingStatus::Failed),
            _ => false,
        };

        // The buttons beside the input.
        let remaining = self.slow_remaining(cx);
        // Each nudge while shown shakes the pill once (the animation is keyed by the count).
        let nudge = self.nudges;
        let pill = (!dm && self.gate.slowmode > 0).then(|| chrome::slowmode_pill(self.gate.slowmode, remaining, nudge, cx));
        let picker_open = self.picker.is_some();
        let snippet_open = self.snippet != Snippet::Closed;
        let menu = if self.snippet == Snippet::Menu {
            let weak = cx.weak_entity();
            Some(crate::panel::snippet_menu(self.snippet_anchor.get(), weak, cx))
        } else {
            None
        };
        let snippet_button = div()
            .relative()
            .child(
                chrome::square_button("snippet", decibell_ui::IconName::ComposerCode, 17., snippet_open, false, Some("Insert code or math"), cx)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_snippet(window, cx))),
            )
            .child(track_bounds(self.snippet_anchor.clone()))
            .children(menu);
        // `absolute bottom-full right-0 mb-2`: the picker's right edge on the button's, 8 px above.
        let picker = self.picker.clone().map(|picker| decibell_ui::layer(2, crate::panel::Above::new(px(8.), true, picker)));
        let emoji_button = div()
            .relative()
            .child(
                chrome::square_button("emoji", decibell_ui::IconName::ComposerEmoji, 18., picker_open, dm, Some("Emoji"), cx)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_picker(window, cx))),
            )
            .child(track_bounds(self.emoji_anchor.clone()))
            .children(picker);
        // DmSendButton: `disabled={sending || !hasDraft}`.
        let sendable = if dm { self.has_text && !self.sending } else { self.has_text || live_pendings };
        // Disabled without anything to send; in a slowmode wait it looks off but still takes the
        // click, which holds the send and shakes the pill.
        let send = chrome::send_button(sendable, remaining > 0, dm, cx).on_click(cx.listener(move |this, _, window, cx| {
            if sendable {
                this.send(window, cx);
            }
        }));
        let attach = (!dm && self.gate.can_attach).then(|| {
            let theme = cx.theme();
            let (hover_bg, accent_fg) = (theme.color_accent_soft, theme.color_accent);
            div()
                .id("attach")
                .group("attach")
                .size(px(chrome::BUTTON))
                .flex_none()
                .self_end()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .bg(theme.color_surface_hover)
                .cursor_pointer()
                .hover(move |s| s.bg(hover_bg))
                .tooltip(decibell_ui::title_tooltip("Attach files"))
                .on_click(cx.listener(|this, _, window, cx| this.pick_files(window, cx)))
                .child(decibell_ui::Icon::new(decibell_ui::IconName::ComposerAttach).size(px(16.)).color(muted).group_hover("attach", accent_fg))
        });

        let input_style = TextField::input_style(FontRole::Sans, 14., 1.4, cx);
        let input = ScrollArea::new("composer-input")
            .track_scroll(&self.input_scroll)
            .flex_1()
            .min_w_0()
            .max_h(px(INPUT_MAX_HEIGHT))
            .child(TextArea::new(&self.input, input_style));

        let row = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .children(attach)
            .child(input)
            .child(div().flex().flex_none().self_end().gap_1().children(pill).child(snippet_button).child(emoji_button).child(send));

        let theme = cx.theme();
        let card = div()
            .id("composer-card")
            .track_focus(&self.card_focus)
            .relative()
            .flex()
            .flex_col()
            .min_h(px(54.))
            .gap(px(10.))
            .rounded(radius_lg)
            .border_1()
            .px(px(14.))
            .py(px(10.))
            .map(|card| {
                if over_card {
                    card.border_color(accent).bg(theme.color_accent_soft.opacity(0.5))
                } else if dragging {
                    card.border_color(gpui::transparent_black()).bg(bg_light)
                } else {
                    card.border_color(if focused { accent } else { border }).bg(bg_light)
                }
            })
            .when(focused && !dragging, |card| card.shadow(ring))
            .on_click(cx.listener(|this, _, window, cx| {
                // A click on the card's padding puts the caret in the input (the contentEditable
                // fills its row in the renderer).
                if !this.input.read(cx).focus_handle(cx).is_focused(window) {
                    this.input.update(cx, |input, cx| input.focus(window, cx));
                }
            }))
            .children(reply)
            .children(preview)
            .children(chips)
            .child(row)
            .when(dragging && !over_card, |card| card.child(chrome::drop_pulse(cx)))
            .when(over_card, |card| card.child(chrome::drop_overlay(&self.gate.name, cx)))
            .child(track_bounds(self.card_bounds.clone()))
            .when_some(self.panel.clone().filter(|_| self.snippet == Snippet::Panel), |card, panel| card.child(crate::panel::panel_layer(panel)));

        let send_error = self.send_error.clone().filter(|_| dm);
        if dm {
            decibell_shell::probes::text("composer-send-error", send_error.as_ref().unwrap_or(&SharedString::default()));
        }
        let theme = cx.theme();
        div()
            .w_full()
            .flex()
            .flex_col()
            // `useDragDrop` (window-level): a file drag anywhere in the window pulses the card.
            .on_drag_move::<ExternalPaths>(cx.listener(|this, event: &gpui::DragMoveEvent<ExternalPaths>, _, cx| {
                let over = this.card_bounds.get().contains(&event.event.position);
                this.drag_moved(over, cx);
            }))
            // `px-4 text-xs text-error`: 12 px on Tailwind's 16 px line.
            .children(send_error.map(|error| div().px_4().child(Label::new(error).size(12.).line_height(16.).color(theme.color_error))))
            .child(div().px_3().py_2().child(card))
            .when(dragging, |this| this.child(drop_catcher(window, cx)))
            .into_any_element()
    }
}

impl Composer {
    /// A press outside the snippet trigger's menu closes it.
    pub(crate) fn dismiss_snippet_menu(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.snippet == Snippet::Menu {
            self.snippet = Snippet::Closed;
            cx.notify();
        }
    }
}

/// A window-sized, invisible layer that exists only while a file is dragged over the window:
/// it takes the drop anywhere (`useDragDrop` attaches to the active channel), unless a drop
/// target of its own claimed the drag (`DragState::hovered` set to another key).
fn drop_catcher(window: &Window, cx: &mut Context<Composer>) -> AnyElement {
    decibell_ui::layer(
        0,
        gpui::anchored().position_mode(gpui::AnchoredPositionMode::Window).position(gpui::point(px(0.), px(0.))).child(
            div()
                .id("drop-catcher")
                .w(window.viewport_size().width)
                .h(window.viewport_size().height)
                .can_drop(|value, _, cx| {
                    value.downcast_ref::<ExternalPaths>().is_some()
                        && cx.try_global::<Model>().is_none_or(|m| m.chat.drag.read(cx).hovered.as_ref().is_none_or(|key| key == "active-input"))
                })
                .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.dropped(paths, cx))),
        ),
    )
    .into_any_element()
}
