//! `inputs`: every kind of field `decibell_text::input` draws, in the kit's `TextField`
//! shell and bare (the composer's input), with fixture values. Click one to edit it.

use decibell_text::input::{InputEvent, InputState, Selection, TextArea, numeric};
use decibell_ui::{ActiveTheme as _, Field, FontRole, IconName, Label, TextField};
use gpui::{Context, Entity, IntoElement, Render, Window, div, prelude::*, px};

pub struct Inputs {
    pub name: Entity<InputState>,
    pub empty: Entity<InputState>,
    pub password: Entity<InputState>,
    pub error: Entity<InputState>,
    pub disabled: Entity<InputState>,
    pub search: Entity<InputState>,
    pub long: Entity<InputState>,
    pub number: Entity<InputState>,
    pub capped: Entity<InputState>,
    pub read_only: Entity<InputState>,
    pub selected: Entity<InputState>,
    pub composing: Entity<InputState>,
    pub area: Entity<InputState>,
    pub tall: Entity<InputState>,
    pub composer: Entity<InputState>,
    pub composer_empty: Entity<InputState>,
    /// What the composer last "sent" (Enter), shown under it.
    pub sent: Option<String>,
}

const CODE: &str = "```rust\nfn main() {\n    println!(\"an open fence: Enter adds a line\");\n}";
const TALL: &str = "A text area grows with its lines\nuntil it reaches its maximum height,\nthen it scrolls\nand keeps the caret in view:\nline five\nline six\nline seven\nline eight\nline nine\nthe last line";

impl Inputs {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let single = |cx: &mut Context<Self>, window: &mut Window, value: &str, placeholder: &'static str| {
            cx.new(|cx| InputState::single_line(window, cx).placeholder(placeholder).with_value(value))
        };
        let composer = cx.new(|cx| {
            InputState::composer(window, cx)
                .placeholder("Message #general")
                .with_value("Emoji are atoms 😀 one step each 👨‍👩‍👧 and 🇹🇷, a typed :tada: becomes one")
        });
        cx.subscribe(&composer, |this, composer, event: &InputEvent, cx| {
            if *event == InputEvent::Submit {
                this.sent = Some(composer.read(cx).value().to_string());
                composer.update(cx, |composer, cx| composer.clear(cx));
                cx.notify();
            }
        })
        .detach();
        let selected = single(cx, window, "Some of this text is selected", "");
        selected.update(cx, |state, cx| state.select(Selection { anchor: 8, head: 17 }, cx));
        let composing = single(cx, window, "Preedit: ", "");
        composing.update(cx, |state, cx| {
            use gpui::EntityInputHandler as _;
            state.replace_and_mark_text_in_range(None, "にほんご", None, window, cx);
        });
        Self {
            name: single(cx, window, "Daily Gaming", "Server name"),
            empty: single(cx, window, "", "What's this channel about?"),
            password: cx.new(|cx| InputState::password(window, cx).placeholder("Password").with_value("hunter2 şifre")),
            error: single(cx, window, "gaming", "Invite code"),
            disabled: single(cx, window, "Can't touch this", ""),
            search: single(cx, window, "", "Search members"),
            long: single(
                cx,
                window,
                "A value longer than its box scrolls sideways to keep the caret in view while you type or move",
                "",
            ),
            number: cx.new(|cx| InputState::single_line(window, cx).accept(numeric).max_length(6).with_value("1024")),
            capped: cx.new(|cx| InputState::single_line(window, cx).max_bytes(32).with_value("32 bytes: ığüşöç 日本語 clamped as typed")),
            read_only: cx.new(|cx| InputState::single_line(window, cx).with_value("decibell://invite/K7Q2M9").read_only(true)),
            selected,
            composing,
            area: cx.new(|cx| InputState::multi_line(window, cx).placeholder("A text area").with_value(CODE)),
            tall: cx.new(|cx| InputState::multi_line(window, cx).with_value(TALL)),
            composer,
            composer_empty: cx.new(|cx| InputState::composer(window, cx).placeholder("Message #general")),
            sent: None,
        }
    }
}

impl Render for Inputs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The composer's input as RichComposer frames it: channel font and size, line-height 1.4.
        let channel_size = cx.theme().scaled.text_channel;
        let composer_style = TextField::input_style(FontRole::Channel, channel_size, 1.4, cx);
        let theme = cx.theme();
        let muted = theme.color_text_muted;
        let section = |title: &'static str| {
            Label::new(title)
                .font(FontRole::Section)
                .size(11.)
                .weight(600.)
                .tracking(0.12)
                .color(muted)
        };
        let cell = |label: &'static str, field: TextField| div().w(px(260.)).child(Field::new(label).child(field));
        let row = || div().flex().flex_wrap().items_start().gap_6();
        let composer_box = |state: &Entity<InputState>| {
            div()
                .w(px(420.))
                .px_4()
                .py_3()
                .rounded(theme.radius_md)
                .bg(theme.color_bg_lighter)
                .child(TextArea::new(state, composer_style))
        };

        div()
            .id("inputs")
            .size_full()
            .overflow_y_scroll()
            .bg(theme.color_bg_primary)
            .text_color(theme.color_text_primary)
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(section("SINGLE LINE · VALUE, PLACEHOLDER, PASSWORD, ERROR"))
            .child(
                row()
                    .child(cell("Value", TextField::new("name").editor(&self.name)))
                    .child(cell("Placeholder", TextField::new("empty").editor(&self.empty)))
                    .child(cell("Password", TextField::new("password").editor(&self.password)))
                    .child(cell("Error", TextField::new("error").editor(&self.error).error(true))),
            )
            .child(section("DISABLED, LEADING ICON, OVERFLOW, READ-ONLY"))
            .child(
                row()
                    .child(cell("Disabled", TextField::new("disabled").editor(&self.disabled).disabled(true)))
                    .child(cell(
                        "Search",
                        TextField::new("search").editor(&self.search).leading_icon(IconName::SettingsSearch),
                    ))
                    .child(cell("Scrolls sideways", TextField::new("long").editor(&self.long)))
                    .child(cell("Read-only", TextField::new("read-only").editor(&self.read_only))),
            )
            .child(section("RULES · NUMBER (6 UNITS), 32-BYTE CAP · SELECTION, IME PREEDIT"))
            .child(
                row()
                    .child(cell("Number", TextField::new("number").editor(&self.number)))
                    .child(cell("Byte cap", TextField::new("capped").editor(&self.capped)))
                    .child(cell("Selection (focused)", TextField::new("selected").editor(&self.selected)))
                    .child(cell("Composing", TextField::new("composing").editor(&self.composing))),
            )
            .child(section("TEXT AREA · GROWS, THEN SCROLLS AT 120 PX"))
            .child(
                row()
                    .child(div().w(px(360.)).child(TextField::new("area").editor(&self.area)))
                    .child(div().w(px(360.)).child(TextField::new("tall").editor(&self.tall).max_height(120.))),
            )
            .child(section("COMPOSER INPUT · EMOJI ATOMS, ENTER SENDS, SHIFT+ENTER BREAKS"))
            .child(
                row()
                    .child(composer_box(&self.composer))
                    .child(composer_box(&self.composer_empty)),
            )
            .children(self.sent.clone().map(|sent| Label::new(format!("sent: {sent:?}")).size(12.).color(muted)))
    }
}
