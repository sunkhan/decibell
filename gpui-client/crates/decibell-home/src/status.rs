//! "Set a custom status" (`CustomStatusModal.tsx`, `modal::CUSTOM_STATUS`): an emoji and a line
//! of text shown under our name while we're online, cleared after a chosen time. Opened from the
//! status menu; each opening starts from the saved message. The text is cut at 128 code points as
//! it is typed (`STATUS_TEXT_MAX`) and normalised the way central will on save.
//!
//! **The emoji picker** is the composer crate's: it registers with [`set_emoji_picker`] and is
//! drawn under (or over) the field while open. Until then the emoji button opens a small chooser
//! that takes a typed emoji or a `:shortcode:` (the same expansion the composer's input does).

use std::rc::Rc;

use decibell_common::Watch;
use decibell_model::live::presence::StatusMessage;
use decibell_model::live::presence::statuses::STATUS_TEXT_MAX;
use decibell_model::stores::overlays::modal;
use decibell_model::{Model, actions};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{
    ActiveTheme as _, CloseRequest, FontRole, Icon, IconName, Label, Modal, Select, SelectOption, TextField, dialog_card,
    title_tooltip,
};
use gpui::{Focusable as _, 
    AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, Entity, Global, IntoElement, Pixels, Render,
    SharedString, Subscription, Window, div, prelude::*, px,
};

use crate::ui::{Look, button, caps_label};

/// "Clear after" (`ClearAfter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearAfter {
    Today,
    FourHours,
    OneHour,
    ThirtyMinutes,
    Never,
}

const CLEAR_AFTER_OPTIONS: [(ClearAfter, &str); 5] = [
    (ClearAfter::Today, "Today"),
    (ClearAfter::FourHours, "4 hours"),
    (ClearAfter::OneHour, "1 hour"),
    (ClearAfter::ThirtyMinutes, "30 minutes"),
    (ClearAfter::Never, "Don't clear"),
];

/// Seconds until the message should go; 0 = never. "Today" runs to local midnight, at least a
/// minute (`clearAfterSeconds`).
pub fn clear_after_seconds(choice: ClearAfter) -> u32 {
    match choice {
        ClearAfter::FourHours => 4 * 3600,
        ClearAfter::OneHour => 3600,
        ClearAfter::ThirtyMinutes => 30 * 60,
        ClearAfter::Never => 0,
        ClearAfter::Today => {
            use chrono::{Duration, Local, TimeZone};
            let now = Local::now();
            let midnight = (now.date_naive() + Duration::days(1)).and_hms_opt(0, 0, 0).and_then(|t| Local.from_local_datetime(&t).earliest());
            match midnight {
                Some(m) => ((m - now).num_milliseconds() as f64 / 1000.).round().max(60.) as u32,
                None => 86_400,
            }
        }
    }
}

/// The chosen emoji, and the window and app it is chosen in.
pub type OnPick = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;
/// The picker closed without a choice.
pub type OnClose = Rc<dyn Fn(&mut Window, &mut App)>;

/// What a registered picker is asked to draw.
pub struct EmojiPickerRequest {
    /// The field the picker belongs to (window coordinates): below it, else above, else as low
    /// as the window allows.
    pub field: Bounds<Pixels>,
    /// The chosen emoji (the dialog closes the picker).
    pub on_pick: OnPick,
    /// Escape, a press outside.
    pub on_close: OnClose,
}

pub type EmojiPickerBuilder = Rc<dyn Fn(EmojiPickerRequest, &mut Window, &mut App) -> AnyElement>;

#[derive(Default)]
struct PickerSlot(Option<EmojiPickerBuilder>);

impl Global for PickerSlot {}

/// Register the emoji picker the custom status dialog opens (the composer crate's).
pub fn set_emoji_picker(cx: &mut App, build: impl Fn(EmojiPickerRequest, &mut Window, &mut App) -> AnyElement + 'static) {
    cx.default_global::<PickerSlot>().0 = Some(Rc::new(build));
}

/// The first emoji in `text` once `:shortcodes:` are expanded (the fallback chooser).
pub fn first_emoji(text: &str) -> Option<SharedString> {
    let expanded = decibell_richtext::emoji::expand_shortcodes(text.trim());
    let range = decibell_richtext::emoji::find_iter(&expanded).next()?;
    Some(expanded[range].to_string().into())
}

pub struct CustomStatusModal {
    open: bool,
    /// Counts openings (each one starts from the saved message).
    opening: u64,
    text: Entity<InputState>,
    emoji: SharedString,
    clear_after: ClearAfter,
    picker_open: bool,
    /// The fallback chooser's field.
    chooser: Entity<InputState>,
    field_bounds: decibell_common::BoundsCell,
    _watch: Watch,
    _subscriptions: Vec<Subscription>,
}

impl CustomStatusModal {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let text = cx.new(|cx| InputState::single_line(window, cx).placeholder("What's happening?"));
        let chooser = cx.new(|cx| InputState::single_line(window, cx).placeholder("Type an emoji or :shortcode:"));
        let mut watch = Watch::new();
        watch.store_if(&model.overlays, cx, |this: &mut CustomStatusModal, overlays, _| this.open != overlays.is_open(modal::CUSTOM_STATUS));
        let subscriptions = vec![
            cx.observe_in(&model.overlays, window, |this, overlays, window, cx| {
                let open = overlays.read(cx).is_open(modal::CUSTOM_STATUS);
                if open && !this.open {
                    this.start(window, cx);
                }
                this.open = open;
            }),
            cx.subscribe_in(&text, window, |this, text, event: &InputEvent, _, cx| match event {
                InputEvent::Submit => this.save(cx),
                InputEvent::Change => {
                    // At most 128 code points as typed (`Array.from(value).slice(0, 128)`).
                    let value = text.read(cx).value();
                    if value.chars().count() > STATUS_TEXT_MAX {
                        let cut: String = value.chars().take(STATUS_TEXT_MAX).collect();
                        text.update(cx, |state, cx| state.set_value(&cut, cx));
                    }
                    cx.notify();
                }
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&chooser, window, |this, chooser, event: &InputEvent, window, cx| match event {
                InputEvent::Submit => {
                    if let Some(emoji) = first_emoji(chooser.read(cx).value()) {
                        this.pick(emoji, window, cx);
                    }
                }
                InputEvent::Escape => {
                    this.picker_open = false;
                    this.text.update(cx, |state, cx| state.focus(window, cx));
                    cx.notify();
                }
                _ => {}
            }),
        ];
        let mut this = Self {
            open: false,
            opening: 0,
            text,
            emoji: SharedString::default(),
            clear_after: ClearAfter::Today,
            picker_open: false,
            chooser,
            field_bounds: decibell_common::BoundsCell::new(),
            _watch: watch,
            _subscriptions: subscriptions,
        };
        if model.overlays.read(cx).is_open(modal::CUSTOM_STATUS) {
            this.open = true;
            this.start(window, cx);
        }
        let entity = cx.entity();
        crate::hooks::remember(&entity, cx);
        this
    }

    /// One form per opening, from the saved message (`CustomStatusForm key={opening}`).
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let own = Model::global(cx).live.presence.own.read(cx);
        let saved = own.message.clone();
        let never = saved.is_some() && own.clears_at.is_none();
        self.opening += 1;
        self.emoji = saved.as_ref().map(|m| m.emoji.clone()).unwrap_or_default();
        self.clear_after = if never { ClearAfter::Never } else { ClearAfter::Today };
        self.picker_open = false;
        let text = saved.map(|m| m.text).unwrap_or_default();
        self.text.update(cx, |state, cx| {
            state.set_value(&text, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn close(cx: &mut App) {
        let overlays = Model::global(cx).overlays.clone();
        overlays.update(cx, |o, cx| o.close_modal(cx));
    }

    /// "Clear after" (the select's choice).
    pub fn set_clear_after(&mut self, choice: ClearAfter, cx: &mut Context<Self>) {
        self.clear_after = choice;
        cx.notify();
    }

    pub fn text(&self) -> &Entity<InputState> {
        &self.text
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        let text = self.text.read(cx).value().to_string();
        actions::presence::save_status_message(StatusMessage::of(&text, &self.emoji), clear_after_seconds(self.clear_after), cx);
        CustomStatusModal::close(cx);
    }

    fn pick(&mut self, emoji: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.emoji = emoji;
        self.picker_open = false;
        self.text.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn toggle_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker_open = !self.picker_open;
        if self.picker_open && cx.try_global::<PickerSlot>().and_then(|s| s.0.clone()).is_none() {
            self.chooser.update(cx, |state, cx| {
                state.clear(cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    /// The picker (registered), or the fallback chooser under the field.
    fn picker(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.picker_open {
            return None;
        }
        if let Some(build) = cx.try_global::<PickerSlot>().and_then(|s| s.0.clone()) {
            let this = cx.weak_entity();
            let close = cx.weak_entity();
            let request = EmojiPickerRequest {
                field: self.field_bounds.get(),
                on_pick: Rc::new(move |emoji, window, cx| {
                    let _ = this.update(cx, |this, cx| this.pick(emoji, window, cx));
                }),
                on_close: Rc::new(move |_, cx| {
                    let _ = close.update(cx, |this, cx| {
                        this.picker_open = false;
                        cx.notify();
                    });
                }),
            };
            return Some(build(request, window, cx));
        }
        let t = cx.theme();
        Some(
            div()
                .mt_1p5()
                .rounded(t.radius_md)
                .border_1()
                .border_color(t.color_border)
                .bg(t.color_bg_light)
                .p_2()
                .child(TextField::new("status-emoji-chooser").editor(&self.chooser))
                .child(div().mt_1().child(Label::new("Enter picks it; Escape goes back.").size(11.).color(t.color_text_muted)))
                .into_any_element(),
        )
    }
}

impl Render for CustomStatusModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.open;
        let focused = self.text.read(cx).focus_handle(cx).is_focused(window);
        let chars = self.text.read(cx).value().chars().count();
        let has_content = !self.text.read(cx).value().trim().is_empty() || !self.emoji.is_empty();
        let picker = self.picker(window, cx);
        let style = TextField::input_style(FontRole::Sans, 14., 1.2109, cx);
        let caps_mind = caps_label("What's on your mind?", 11., cx);
        let caps_clear = caps_label("Clear after", 11., cx);
        let clear_after = self.clear_after;
        let select = Select::options(
            ("status-clear-after", self.opening),
            Some(clear_after),
            CLEAR_AFTER_OPTIONS.iter().map(|(value, label)| SelectOption::new(*value, *label)).collect(),
        )
        .on_change(cx.listener(|this, value: &ClearAfter, _, cx| {
            this.clear_after = *value;
            cx.notify();
        }));
        let cancel = button("status-cancel", "Cancel", Look::secondary(cx).with(|l| {
            // `rounded-md border border-border bg-transparent … text-text-primary`.
            l.radius = cx.theme().radius_md;
            l.fg = cx.theme().color_text_primary;
        }), false)
        .on_click(|_: &ClickEvent, _, cx| CustomStatusModal::close(cx));
        let save = button("status-save", "Save", Look::primary(cx), false)
            .relative()
            .children(decibell_shell::probes::probe("status-save"))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx)));
        let emoji = self.emoji.clone();
        let t = cx.theme();
        let (border, accent, bg_dark, muted, primary, hover) =
            (t.color_border, t.color_accent, t.color_bg_dark, t.color_text_muted, t.color_text_primary, t.color_surface_hover);
        let field_bounds = self.field_bounds.clone();
        let field = div()
            .relative()
            .flex()
            .items_center()
            .gap_1()
            .rounded(t.radius_md)
            .border_1()
            .border_color(if focused { accent } else { border })
            .bg(bg_dark)
            .px_1p5()
            .when(focused, |this| this.shadow(t.shadow_ring.clone()))
            .child(field_bounds.probe())
            .child(
                div()
                    .id("status-emoji")
                    .group("status-emoji")
                    .flex()
                    .flex_none()
                    .size_8()
                    .items_center()
                    .justify_center()
                    .rounded(t.radius_sm)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .tooltip(title_tooltip(if emoji.is_empty() { "Add an emoji" } else { "Change emoji" }))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_picker(window, cx)))
                    .child(if emoji.is_empty() {
                        Icon::new(IconName::ShellSmile).size(px(18.)).color(muted).group_hover("status-emoji", primary).into_any_element()
                    } else {
                        // Twemoji in the renderer; the text engine's emoji font here (decibell-common's note).
                        Label::new(emoji.clone()).size(20.).line_height(20.).into_any_element()
                    }),
            )
            .child(div().flex_1().min_w_0().py_2p5().child(decibell_text::input::TextInput::new(&self.text, style)))
            .when(has_content, |this| {
                this.child(
                    div()
                        .id("status-clear")
                        .group("status-clear")
                        .flex()
                        .flex_none()
                        .size_7()
                        .items_center()
                        .justify_center()
                        .rounded(t.radius_sm)
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .tooltip(title_tooltip("Clear"))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.emoji = SharedString::default();
                            this.text.update(cx, |state, cx| {
                                state.clear(cx);
                                state.focus(window, cx);
                            });
                            cx.notify();
                        }))
                        .child(Icon::new(IconName::Close).size(px(14.)).color(muted).group_hover("status-clear", primary)),
                )
            });
        let card = dialog_card(440., cx)
            .child(Label::new("Set a custom status").size(16.).weight(600.).color(primary))
            .child(div().mt_1().child(Label::new("Shows under your name while you're online.").size(13.).line_height(13. * 1.375).color(muted)))
            .child(div().mb_2().mt_5().child(caps_mind))
            .child(field)
            .children(picker)
            .child(
                div()
                    .mt_1p5()
                    .flex()
                    .justify_end()
                    .child(Label::new(SharedString::from(format!("{}/{}", crate::fmt::count(chars), crate::fmt::count(STATUS_TEXT_MAX)))).font(FontRole::Meta).size(t.scaled.text_micro).color(muted)),
            )
            .child(div().mb_2().mt_3().child(caps_clear))
            .child(select)
            .child(div().mt_6().flex().flex_wrap().justify_end().gap_2().child(cancel).child(save));
        Modal::new("custom-status", open)
            .on_close(cx.listener(|this, _: &CloseRequest, _, cx| {
                // Esc closes, unless the emoji picker is open (it closes itself).
                if this.picker_open {
                    this.picker_open = false;
                    cx.notify();
                    return;
                }
                CustomStatusModal::close(cx);
            }))
            .child(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_chooser_takes_an_emoji_or_a_shortcode() {
        assert_eq!(first_emoji("🎮").as_deref(), Some("🎮"));
        assert_eq!(first_emoji("  :smile: rest").as_deref(), Some("😄"));
        assert_eq!(first_emoji("no emoji"), None);
        assert!(clear_after_seconds(ClearAfter::Today) >= 60);
        assert_eq!(clear_after_seconds(ClearAfter::ThirtyMinutes), 1800);
        assert_eq!(clear_after_seconds(ClearAfter::Never), 0);
    }
}
