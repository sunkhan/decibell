//! What the DM pane draws under the conversation until the composer crate registers its own
//! ([`super::set_dm_composer`]): `DmChatPanel`'s input bar — the `px-3 py-2` gutter, the card
//! (`min-h-[54px] rounded-lg border bg-bg-light px-3.5 py-2.5`, accent border and ring while
//! focused), the field ("Message @name", Enter sends, Shift+Enter breaks the line, ArrowUp in an
//! empty box edits our last message, Escape drops a pending reply), the emoji button (inert here:
//! the picker is the composer crate's) and the send button (greyed while the draft is blank).
//! The draft lives in `chat.drafts` as the renderer's did, and comes back when a send is refused.

use decibell_chat::{ListEvent, MessageList};
use decibell_model::actions::chat::SendOutcome;
use decibell_model::chat::{DraftKey, DraftsEvent};
use decibell_model::{Model, Uid, actions};
use decibell_text::input::{InputEvent, InputState, TextArea};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, ScrollArea, TextField, title_tooltip};
use gpui::{Focusable as _, ClickEvent, Context, Entity, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*, px};

pub struct FallbackComposer {
    peer: Uid,
    input: Entity<InputState>,
    list: Entity<MessageList>,
    /// The message the next send answers (`replyingTo`), set by the list's reply arrow.
    reply_to: Option<i64>,
    _subscriptions: Vec<Subscription>,
}

impl FallbackComposer {
    pub fn new(peer: Uid, name: SharedString, list: Entity<MessageList>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let draft = model.chat.drafts.read(cx).get(&DraftKey::Dm(peer));
        let input = cx.new(|cx| InputState::composer(window, cx).placeholder(format!("Message @{name}")).with_value(&draft));
        let weak = cx.weak_entity();
        input.update(cx, |state, _| {
            // Esc drops a pending reply (and is swallowed then).
            state.on_escape(move |_, cx| {
                weak.update(cx, |this, cx| {
                    let had = this.reply_to.take().is_some();
                    if had {
                        this.list.update(cx, |list, cx| list.set_reply_target(None, cx));
                    }
                    had
                })
                .unwrap_or(false)
            });
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Submit => this.send(window, cx),
                InputEvent::Change => {
                    let text: SharedString = input.read(cx).value().to_string().into();
                    actions::dm::set_draft(this.peer, text, cx);
                    cx.notify();
                }
                InputEvent::ArrowUpEmpty => this.edit_last(cx),
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
                _ => {}
            }),
            cx.subscribe(&list, |this, _, event: &ListEvent, cx| {
                if let ListEvent::Reply(id) = event {
                    this.reply_to = Some(*id);
                    cx.notify();
                }
            }),
            // A send the server refused (or never confirmed) puts its text back.
            cx.subscribe_in(&model.chat.drafts, window, |this, drafts, event: &DraftsEvent, window, cx| {
                if let DraftsEvent::Restored(DraftKey::Dm(peer)) = event
                    && *peer == this.peer
                {
                    let text = drafts.read(cx).get(&DraftKey::Dm(*peer));
                    this.input.update(cx, |state, cx| {
                        state.set_value(&text, cx);
                        state.focus(window, cx);
                    });
                }
            }),
        ];
        Self { peer, input, list, reply_to: None, _subscriptions: subscriptions }
    }

    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// `handleSend`.
    fn send(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        match actions::dm::send_dm(self.peer, &text, self.reply_to.unwrap_or(0), cx) {
            SendOutcome::Sent { .. } => {
                self.input.update(cx, |state, cx| state.clear(cx));
                if self.reply_to.take().is_some() {
                    self.list.update(cx, |list, cx| list.set_reply_target(None, cx));
                }
                cx.notify();
            }
            SendOutcome::JumpedToPresent | SendOutcome::HeldBySlowmode | SendOutcome::Nothing => {}
        }
    }

    /// ArrowUp in an empty box: edit our newest message (`editLatestOwn`).
    fn edit_last(&mut self, cx: &mut Context<Self>) {
        let Some(slice) = Model::global(cx).dm.list.read(cx).slice(self.peer).cloned() else { return };
        let id = {
            let slice = slice.read(cx);
            slice.rows().iter().zip(slice.facts()).rev().find(|(m, f)| f.own && m.id > 0).map(|(m, _)| m.id)
        };
        if let Some(id) = id {
            self.list.update(cx, |list, cx| list.start_edit(id, cx));
        }
    }
}

impl Render for FallbackComposer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        let has_draft = !self.input.read(cx).is_blank();
        // `.rich-input`: 14 px (`text-sm`) at line-height 1.4.
        let style = TextField::input_style(FontRole::Sans, 14., 1.4, cx);
        let t = cx.theme();
        let (accent, accent_hover, on_accent, muted, secondary) =
            (t.color_accent, t.color_accent_hover, t.color_on_accent, t.color_text_muted, t.color_text_secondary);
        let send_bg = if has_draft { accent } else { t.color_surface_active };
        let send_fg = if has_draft { on_accent } else { muted };
        div().w_full().px_3().py_2().child(
            div()
                .relative()
                .flex()
                .flex_col()
                .gap_2p5()
                .min_h(px(54.))
                .rounded(t.radius_lg)
                .border_1()
                .border_color(if focused { accent } else { t.color_border })
                .bg(t.color_bg_light)
                .px_3p5()
                .py_2p5()
                .when(focused, |this| this.shadow(t.shadow_ring.clone()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .child(
                            div().flex_1().min_w_0().child(
                                ScrollArea::new("dm-composer-input").without_keys().w_full().max_h(px(160.)).child(TextArea::new(&self.input, style)),
                            ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .self_end()
                                .gap_1()
                                .child(
                                    div()
                                        .id("dm-emoji")
                                        .group("dm-emoji")
                                        .flex()
                                        .size(px(34.))
                                        .items_center()
                                        .justify_center()
                                        .rounded(t.radius_sm)
                                        .cursor_pointer()
                                        .tooltip(title_tooltip("Emoji"))
                                        .child(Icon::new(IconName::ShellSmile).size(px(18.)).color(muted).group_hover("dm-emoji", secondary)),
                                )
                                .child(
                                    div()
                                        .id("dm-send")
                                        .flex()
                                        .size(px(34.))
                                        .items_center()
                                        .justify_center()
                                        .rounded(t.radius_sm)
                                        .bg(send_bg)
                                        .map(|this| {
                                            if has_draft {
                                                this.cursor_pointer()
                                                    .hover(move |s| s.bg(accent_hover))
                                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.send(window, cx)))
                                            } else {
                                                this.cursor_not_allowed()
                                            }
                                        })
                                        .child(Icon::new(IconName::HomeSend).size(px(16.)).color(send_fg)),
                                ),
                        ),
                ),
        )
    }
}
