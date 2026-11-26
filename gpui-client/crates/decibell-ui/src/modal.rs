//! Dialogs: [`Modal`] (the backdrop, the fade, Escape and backdrop-click requests, the focus trap)
//! and [`ConfirmModal`] (`components/ConfirmModal.tsx`).

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, ElementId, FocusHandle, Hsla, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, Window, deferred, div, prelude::*, px,
};

use crate::motion::{self, Transition};
use crate::popover::{Placed, Placement, visible_window_bounds};
use crate::{ActiveTheme as _, Button, ButtonVariant, Label};

/// Why a modal asks to close. The owner decides: close it (`open = false`), or refuse and say
/// why (a settings window with unsaved changes shakes its save bar instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseRequest {
    Escape,
    Backdrop,
}

type CloseHandler = Rc<dyn Fn(CloseRequest, &mut Window, &mut App) + 'static>;

/// The renderer's dialog backdrop: `rgba(0,0,0,0.65)` (inline in ConfirmModal / SettingsModal,
/// not a theme token), faded over `duration-300`.
const BACKDROP: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.,
    a: 0.65,
};

struct ModalState {
    /// Where focus was before the modal opened, to go back to as it closes.
    restore: Option<FocusHandle>,
    was_open: bool,
}

/// A modal layer over the whole window: the backdrop darkens and the content fades in over
/// 300 ms as `open` turns true, and out as it turns false (render it whether open or not, as
/// ConfirmModal is: it stays drawn through the fade-out, then draws nothing). The renderer's
/// `scale(0.95 → 1)` is left out: GPUI cannot scale a box. While open: focus moves into it and
/// Tab / Shift-Tab cycle inside it; Escape and a press on the backdrop call `on_close` (the owner
/// may refuse); closing gives focus back. A modal opened from inside another one draws over it
/// and takes the keys first. The content must `.occlude()` (as [`dialog_card`] does) so that a
/// press on it is not a press on the backdrop.
///
/// ```ignore
/// Modal::new("leave-server", self.confirming)
///     .on_close(cx.listener(|this, _: &CloseRequest, _, cx| { this.confirming = false; cx.notify() }))
///     .child(card)
/// ```
#[derive(IntoElement)]
pub struct Modal {
    id: ElementId,
    open: bool,
    on_close: Option<CloseHandler>,
    padding: Option<(f32, f32)>,
    children: Vec<AnyElement>,
}

impl Modal {
    pub fn new(id: impl Into<ElementId>, open: bool) -> Self {
        Self {
            id: id.into(),
            open,
            on_close: None,
            padding: None,
            children: Vec::new(),
        }
    }

    pub fn on_close(mut self, handler: impl Fn(&CloseRequest, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(move |request, window, cx| handler(&request, window, cx)));
        self
    }

    /// The backdrop's padding (x, y), which sets a large window's margin.
    pub fn padding(mut self, x: f32, y: f32) -> Self {
        self.padding = Some((x, y));
        self
    }
}

impl ParentElement for Modal {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

impl RenderOnce for Modal {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.id;
        let t = motion::transition_from(
            window,
            cx,
            (id.clone(), "shown"),
            0.,
            if self.open { 1. } else { 0. },
            Transition::MODAL,
        );
        let focus = crate::interaction::focus_handle(window, cx, (id.clone(), "focus"), false);
        let state = window.use_keyed_state((id.clone(), "modal"), cx, |_, _| ModalState {
            restore: None,
            was_open: false,
        });
        let was_open = state.read(cx).was_open;
        if self.open && !was_open {
            // Opening: remember where focus was, then take it.
            let restore = window.focused(cx);
            state.update(cx, |state, _| {
                state.restore = restore;
                state.was_open = true;
            });
            let focus = focus.clone();
            window.defer(cx, move |window, cx| {
                if !focus.contains_focused(window, cx) {
                    focus.focus(window, cx);
                }
            });
        } else if !self.open && was_open {
            let restore = state.update(cx, |state, _| {
                state.was_open = false;
                state.restore.take()
            });
            if let Some(restore) = restore {
                window.defer(cx, move |window, cx| restore.focus(window, cx));
            }
        }
        if !self.open && t <= 0. {
            return div().into_any_element();
        }

        let bounds = visible_window_bounds(window, cx);
        let on_close = self.on_close;
        let open = self.open;
        let (pad_x, pad_y) = self.padding.unwrap_or((16., 16.));
        let layer = div()
            .id(id.clone())
            .track_focus(&focus)
            .occlude()
            .w(bounds.size.width)
            .h(bounds.size.height)
            .flex()
            .items_center()
            .justify_center()
            .px(px(pad_x))
            .py(px(pad_y))
            .bg(BACKDROP.opacity(t))
            .on_mouse_down(MouseButton::Left, {
                let on_close = on_close.clone();
                move |_, window, cx| {
                    // Only a press on the backdrop itself: the content occludes it.
                    if open && let Some(handler) = &on_close {
                        handler(CloseRequest::Backdrop, window, cx);
                    }
                }
            })
            .on_action({
                let focus = focus.clone();
                move |_: &crate::FocusNext, window, cx| trap_tab(&focus, false, window, cx)
            })
            .on_action({
                let focus = focus.clone();
                move |_: &crate::FocusPrev, window, cx| trap_tab(&focus, true, window, cx)
            })
            .on_key_down({
                move |event, window, cx| {
                    let keystroke = &event.keystroke;
                    match keystroke.key.as_str() {
                        "escape" if !keystroke.modifiers.modified() => {
                            cx.stop_propagation();
                            if open && let Some(handler) = &on_close {
                                handler(CloseRequest::Escape, window, cx);
                            }
                        }
                        _ => {}
                    }
                }
            })
            .child(
                div()
                    .id((id.clone(), "content"))
                    .flex()
                    .justify_center()
                    .size_full()
                    .items_center()
                    .opacity(t)
                    .children(self.children),
            );
        deferred(Placed::new(Placement::At(bounds.origin)).child(layer))
            .with_priority(10)
            .into_any_element()
    }
}

/// Tab / Shift-Tab inside a modal: move to the next tab stop, skipping (and so cycling past)
/// everything outside it. Nothing focusable inside: the modal itself keeps focus.
fn trap_tab(modal: &FocusHandle, backwards: bool, window: &mut Window, cx: &mut App) {
    for _ in 0..256 {
        if backwards {
            window.focus_prev(cx);
        } else {
            window.focus_next(cx);
        }
        if modal.contains_focused(window, cx) && !modal.is_focused(window) {
            return;
        }
        if window.focused(cx).is_none() {
            break;
        }
    }
    modal.focus(window, cx);
}

/// The look of a dialog card: `w-full max-w-[400px] rounded-xl border border-border
/// bg-bg-secondary p-6 shadow-modal` (`max_width` overrides the 400).
pub fn dialog_card(max_width: f32, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    div()
        .occlude()
        .w_full()
        .max_w(px(max_width))
        .rounded(theme.radius_xl)
        .border_1()
        .border_color(theme.color_border)
        .bg(theme.color_bg_secondary)
        .p_6()
        .shadow(theme.shadow_modal.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfirmTone {
    /// The destructive button (`bg-error`).
    #[default]
    Danger,
    /// An ordinary choice (the primary button).
    Accent,
}

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// The small confirmation dialog (delete message, leave server): title, body, Cancel and the
/// confirm button; Escape or the backdrop cancels, Enter confirms. `busy` shows a spinner on the
/// confirm button and ignores Enter.
///
/// ```ignore
/// ConfirmModal::new("delete-message", self.confirm.is_some(), "Delete message", "Delete")
///     .message("Delete this message? This cannot be undone.")
///     .on_confirm(cx.listener(|this, _: &ClickEvent, window, cx| this.delete(window, cx)))
///     .on_cancel(cx.listener(|this, _: &ClickEvent, _, cx| { this.confirm = None; cx.notify() }))
/// ```
#[derive(IntoElement)]
pub struct ConfirmModal {
    id: ElementId,
    open: bool,
    title: SharedString,
    confirm_label: SharedString,
    message: Option<SharedString>,
    body: Vec<AnyElement>,
    tone: ConfirmTone,
    confirm_disabled: bool,
    busy: bool,
    secondary: Option<(SharedString, Handler)>,
    on_confirm: Option<Handler>,
    on_cancel: Option<Handler>,
}

impl ConfirmModal {
    pub fn new(
        id: impl Into<ElementId>,
        open: bool,
        title: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            open,
            title: title.into(),
            confirm_label: confirm_label.into(),
            message: None,
            body: Vec::new(),
            tone: ConfirmTone::Danger,
            confirm_disabled: false,
            busy: false,
            secondary: None,
            on_confirm: None,
            on_cancel: None,
        }
    }

    /// The body as one paragraph (`mt-2 text-[13px] text-text-secondary`). It is kept while the
    /// dialog fades out, so an owner clearing its state on confirm doesn't blank it.
    pub fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub fn tone(mut self, tone: ConfirmTone) -> Self {
        self.tone = tone;
        self
    }

    /// Greys the confirm button out; Enter does nothing.
    pub fn confirm_disabled(mut self, disabled: bool) -> Self {
        self.confirm_disabled = disabled;
        self
    }

    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// A middle choice between Cancel and the confirm button (the card widens to `max-w-lg`).
    pub fn secondary(mut self, label: impl Into<SharedString>, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.secondary = Some((label.into(), Rc::new(handler)));
        self
    }

    pub fn on_confirm(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_confirm = Some(Rc::new(handler));
        self
    }

    /// Cancel, Escape and a press on the backdrop.
    pub fn on_cancel(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_cancel = Some(Rc::new(handler));
        self
    }
}

impl ParentElement for ConfirmModal {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.body.extend(elements)
    }
}

impl RenderOnce for ConfirmModal {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.id.clone();
        let open = self.open;
        // The last message shown while open, for the fade-out.
        let frozen = window.use_keyed_state((id.clone(), "frozen"), cx, |_, _| None::<SharedString>);
        if open && self.message.is_some() && frozen.read(cx).as_ref() != self.message.as_ref() {
            let message = self.message.clone();
            frozen.update(cx, |frozen, _| *frozen = message);
        }
        let message = if open { self.message.clone() } else { frozen.read(cx).clone() };
        let confirmable = !self.confirm_disabled && !self.busy;
        let on_confirm = self.on_confirm.clone();
        let on_cancel = self.on_cancel.clone();
        let theme = cx.theme();
        let secondary_text = theme.color_text_secondary;

        let confirm = {
            let on_confirm = on_confirm.clone();
            Button::new((id.clone(), "confirm"), self.confirm_label.clone())
                .variant(match self.tone {
                    ConfirmTone::Danger => ButtonVariant::Danger,
                    ConfirmTone::Accent => ButtonVariant::Primary,
                })
                .disabled(self.confirm_disabled)
                .loading(self.busy)
                .on_click(move |event, window, cx| {
                    if open && confirmable && let Some(handler) = &on_confirm {
                        handler(event, window, cx);
                    }
                })
        };
        let cancel = {
            let on_cancel = on_cancel.clone();
            Button::new((id.clone(), "cancel"), "Cancel")
                .variant(ButtonVariant::Secondary)
                .on_click(move |event, window, cx| {
                    if open && let Some(handler) = &on_cancel {
                        handler(event, window, cx);
                    }
                })
        };
        let secondary = self.secondary.map(|(label, handler)| {
            Button::new((id.clone(), "secondary"), label)
                .variant(ButtonVariant::Secondary)
                .on_click(move |event, window, cx| {
                    if open {
                        handler(event, window, cx);
                    }
                })
        });
        let wide = secondary.is_some();

        let card = dialog_card(if wide { 512. } else { 400. }, cx)
            .on_key_down({
                let on_confirm = on_confirm.clone();
                move |event, window, cx| {
                    if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified() {
                        cx.stop_propagation();
                        window.prevent_default();
                        if open && confirmable && let Some(handler) = &on_confirm {
                            handler(&ClickEvent::default(), window, cx);
                        }
                    }
                }
            })
            .child(
                Label::new(self.title)
                    .size(16.)
                    .weight(600.)
                    .color(theme.color_text_primary),
            )
            .when_some(message, |this, message| {
                this.child(div().mt_2().child(Label::new(message).size(13.).color(secondary_text)))
            })
            .children(self.body)
            .child(
                div()
                    .mt_5()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(cancel)
                    .children(secondary)
                    .child(confirm),
            );

        Modal::new(id, open)
            .on_close(move |_, window, cx| {
                if let Some(handler) = &on_cancel {
                    handler(&ClickEvent::default(), window, cx);
                }
            })
            .child(card)
    }
}
