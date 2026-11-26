//! Building blocks shared by the settings tabs (`tabs/settingsUi.tsx`, `channelSettings/ui.tsx`):
//! the toolbar search field, menu rows, the role chip, the error banner, the channel settings'
//! groups, rows, notices and pills, and the permission row.

use std::cell::Cell;
use std::rc::Rc;

use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, Switch, TextField};
use gpui::{
    AnyElement, App, AppContext as _, Focusable as _, ClickEvent, Context, ElementId, Entity, Hsla, IntoElement, RenderOnce, SharedString,
    Subscription, Window, div, prelude::*, px,
};

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A role's colour (`roleColor`): 0xRRGGBB, or the muted token for "no colour".
pub fn role_color(color: u32, cx: &App) -> Hsla {
    decibell_common::role_color(color).unwrap_or(cx.theme().color_text_muted)
}

/// A colour dot of `size` px.
pub fn dot(color: Hsla, size: f32) -> gpui::Div {
    div().flex_none().size(px(size)).rounded_full().bg(color)
}

/// The toolbar search box (`SearchField`): a field with the magnifier, a ✕ while it holds text,
/// and Escape clearing it first (the window closes only on an empty one).
pub struct SearchBox {
    pub state: Entity<InputState>,
    has_text: Rc<Cell<bool>>,
    _changes: Subscription,
}

impl SearchBox {
    /// `on_change` gets the text after every edit.
    pub fn new<V: 'static>(
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<V>,
        on_change: impl Fn(&mut V, &str, &mut Context<V>) + 'static,
    ) -> SearchBox {
        let has_text: Rc<Cell<bool>> = Rc::default();
        let state = cx.new(|cx| InputState::single_line(window, cx).placeholder(placeholder));
        let weak = state.downgrade();
        let flag = has_text.clone();
        state.update(cx, |s, _| {
            s.on_escape(move |_, cx| {
                if !flag.get() {
                    return false;
                }
                let weak = weak.clone();
                cx.defer(move |cx| {
                    if let Some(state) = weak.upgrade() {
                        state.update(cx, |s, cx| s.clear(cx));
                    }
                });
                true
            });
        });
        let flag = has_text.clone();
        let changes = cx.subscribe(&state, move |this, state, event: &InputEvent, cx| {
            if *event == InputEvent::Change {
                let text = state.read(cx).value().to_string();
                flag.set(!text.is_empty());
                on_change(this, &text, cx);
            }
        });
        SearchBox { state, has_text, _changes: changes }
    }

    pub fn text(&self, cx: &App) -> String {
        self.state.read(cx).value().to_string()
    }

    pub fn clear(&self, cx: &mut App) {
        self.state.update(cx, |s, cx| s.clear(cx));
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |s, cx| s.focus(window, cx));
    }

    pub fn field(&self, id: impl Into<ElementId>) -> SearchField {
        SearchField { id: id.into(), state: self.state.clone(), has_text: self.has_text.get() }
    }
}

#[derive(IntoElement)]
pub struct SearchField {
    id: ElementId,
    state: Entity<InputState>,
    has_text: bool,
}

impl RenderOnce for SearchField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let state = self.state.clone();
        let (muted, primary, hover) = (theme.color_text_muted, theme.color_text_primary, theme.color_surface_hover);
        let id = self.id.clone();
        div()
            .relative()
            .flex()
            .h_10()
            .w_full()
            .min_w_0()
            .items_center()
            .child(TextField::new(self.id).editor(&self.state).leading_icon(IconName::SettingsSearch))
            .when(self.has_text, |this| {
                this.child(
                    div()
                        .id((id, "clear"))
                        .group("search-clear")
                        .absolute()
                        .right_2()
                        .flex()
                        .size_6()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius_sm)
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(move |_, _, cx| state.update(cx, |s, cx| s.clear(cx)))
                        .child(Icon::new(IconName::SettingsX).size(px(12.)).stroke_width(2.5).color(muted).group_hover("search-clear", primary)),
                )
            })
    }
}

/// A row of a settings popover menu (`MenuItem`): icon, label, danger tone.
pub fn menu_item(id: impl Into<ElementId>, icon: IconName, label: impl Into<SharedString>, danger: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    let id = id.into();
    let group: SharedString = format!("menu-item-{id}").into();
    let (fg, icon_color, hover_bg) = if danger {
        (theme.color_error, theme.color_error, theme.color_error.opacity(0.1))
    } else {
        (theme.color_text_primary, theme.color_text_muted, theme.color_surface_hover)
    };
    div()
        .id(id)
        .group(group)
        .flex()
        .w_full()
        .items_center()
        .gap_2p5()
        .rounded(theme.radius_md)
        .px_2p5()
        .py_2()
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        .child(Icon::new(icon).size(px(15.)).color(icon_color))
        .child(div().min_w_0().flex_1().child(Label::new(label.into()).size(13.).weight(500.).color(fg).truncate()))
}

/// `MenuDivider`: `mx-2.5 my-1 h-px bg-border-divider`.
pub fn menu_divider(cx: &App) -> gpui::Div {
    div().mx_2p5().my_1().h(px(1.)).bg(cx.theme().color_border_divider)
}

/// A role as a chip (`RoleChip`): colour dot + name. With `on_remove`, hovering swaps the dot
/// for a ✕ in the same 12 px slot.
#[derive(IntoElement)]
pub struct RoleChip {
    id: ElementId,
    name: SharedString,
    color: u32,
    on_remove: Option<Handler>,
}

impl RoleChip {
    pub fn new(id: impl Into<ElementId>, name: impl Into<SharedString>, color: u32) -> Self {
        RoleChip { id: id.into(), name: name.into(), color, on_remove: None }
    }

    pub fn on_remove(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_remove = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for RoleChip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = role_color(self.color, cx);
        let group: SharedString = format!("chip-{}", self.id).into();
        let slot = match self.on_remove {
            Some(handler) => div()
                .id((self.id.clone(), "remove"))
                .flex()
                .flex_none()
                .size_3()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .tooltip(decibell_ui::title_tooltip(format!("Remove {}", self.name)))
                .on_click(move |e, window, cx| {
                    cx.stop_propagation();
                    handler(e, window, cx)
                })
                .child(dot(color, 10.).group_hover(group.clone(), |s| s.invisible()))
                .child(
                    div()
                        .absolute()
                        .invisible()
                        .group_hover(group.clone(), |s| s.visible())
                        .child(Icon::new(IconName::SettingsX).size(px(12.)).stroke_width(3.).color(theme.color_text_primary)),
                )
                .into_any_element(),
            None => div().flex().flex_none().size_3().items_center().justify_center().child(dot(color, 10.)).into_any_element(),
        };
        div()
            .id(self.id)
            .group(group)
            .relative()
            .flex()
            .h_6()
            .min_w_0()
            .max_w(px(160.))
            .flex_none()
            .items_center()
            .gap_1p5()
            .rounded(theme.radius_sm)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_light)
            .pl_1p5()
            .pr_2()
            .child(slot)
            .child(div().min_w_0().child(Label::new(self.name).size(12.).weight(500.).color(theme.color_text_secondary).truncate()))
    }
}

/// The error banner over a tab: `flex items-center gap-2 rounded-md border border-error/30
/// bg-error/10 px-3 py-2.5 text-[13px] text-error`, a ✕ when it can be dismissed.
pub fn error_banner(id: impl Into<ElementId>, text: impl Into<SharedString>, on_dismiss: Option<Handler>, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    let error = theme.color_error;
    div()
        .flex()
        .items_center()
        .gap_2()
        .rounded(theme.radius_md)
        .border_1()
        .border_color(error.opacity(0.3))
        .bg(error.opacity(0.1))
        .px_3()
        .py_2p5()
        .child(Icon::new(IconName::SettingsAlert).size(px(15.)).color(error))
        .child(div().min_w_0().flex_1().child(Label::new(text.into()).size(13.).color(error)))
        .when_some(on_dismiss, |this, handler| {
            this.child(
                div()
                    .id(id.into())
                    .rounded(theme.radius_sm)
                    .p_1()
                    .cursor_pointer()
                    .hover(move |s| s.bg(error.opacity(0.1)))
                    .on_click(move |e, window, cx| handler(e, window, cx))
                    .child(Icon::new(IconName::SettingsX).size(px(12.)).stroke_width(2.5).color(error)),
            )
        })
}

/// A lock notice (`Notice`): `rounded-md border border-border-divider bg-bg-light px-3.5 py-3
/// text-[13px] leading-[1.55] text-text-secondary` with a 15 px muted icon.
pub fn notice(icon: IconName, text: impl Into<SharedString>, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .gap_2p5()
        .rounded(theme.radius_md)
        .border_1()
        .border_color(theme.color_border_divider)
        .bg(theme.color_bg_light)
        .px(px(14.))
        .py_3()
        .child(Icon::new(icon).size(px(15.)).color(theme.color_text_muted))
        .child(div().min_w_0().flex_1().child(Label::new(text.into()).size(13.).line_height(13. * 1.55).color(theme.color_text_secondary)))
}

/// A labelled card of rows split by hairlines (`Group`).
pub fn group(label: impl Into<SharedString>, aside: Option<AnyElement>, rows: Vec<AnyElement>, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    let mut card = div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.color_border_divider)
        .bg(theme.color_bg_light);
    let count = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        card = card.child(row);
        if index + 1 < count {
            card = card.child(div().h(px(1.)).bg(theme.color_border_divider));
        }
    }
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .mb_2p5()
                .flex()
                .h_3()
                .items_center()
                .justify_between()
                .gap_3()
                .child(decibell_ui::SectionLabel::new(label.into()))
                .children(aside),
        )
        .child(card)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Good,
    Danger,
}

/// One setting in a [`group`] (`Row`): icon tile, title and description, the control on the
/// right; `below` spans the width under the text (a slider).
pub fn row(
    icon: Option<IconName>,
    tone: Tone,
    title: impl Into<SharedString>,
    description: Option<AnyElement>,
    control: Option<AnyElement>,
    below: Option<AnyElement>,
    cx: &App,
) -> gpui::Div {
    let theme = cx.theme();
    div()
        .px_4()
        .py(px(14.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .when_some(icon, |this, icon| {
                    let (bg, fg) = match tone {
                        Tone::Good => (theme.color_success.opacity(0.15), theme.color_success),
                        Tone::Danger => (theme.color_error.opacity(0.1), theme.color_error),
                        Tone::Neutral => (theme.color_bg_lighter, theme.color_text_secondary),
                    };
                    this.child(
                        div()
                            .flex()
                            .flex_none()
                            .size_9()
                            .items_center()
                            .justify_center()
                            .rounded(theme.radius_md)
                            .bg(bg)
                            .child(Icon::new(icon).size(px(17.)).color(fg)),
                    )
                })
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(Label::new(title.into()).size(14.).weight(500.).color(theme.color_text_primary))
                        .when_some(description, |this, d| this.child(div().mt_0p5().child(d))),
                )
                .children(control),
        )
        .children(below)
}

/// A row description: `text-[12px] leading-[1.55] text-text-muted`.
pub fn description(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    Label::new(text.into()).size(12.).line_height(12. * 1.55).color(cx.theme().color_text_muted).into_any_element()
}

/// A small status pill for read-only values (`Pill`).
pub fn pill(good: bool, icon: Option<IconName>, text: &'static str, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    let (bg, fg) = if good {
        (theme.color_success.opacity(0.15), theme.color_success)
    } else {
        (theme.color_surface_active, theme.color_text_secondary)
    };
    div()
        .flex()
        .flex_none()
        .h_6()
        .items_center()
        .gap_1p5()
        .rounded(theme.radius_sm)
        .bg(bg)
        .px_2()
        .when_some(icon, |this, icon| this.child(Icon::new(icon).size(px(12.)).stroke_width(2.5).color(fg)))
        .child(Label::new(text).size(12.).weight(600.).color(fg))
}

/// The bordered secondary button ("Show 198 more", "Load older entries", "Sync now"):
/// `rounded-sm border border-border px-4 py-2 text-[13px] font-semibold text-text-secondary
/// hover:bg-surface-hover hover:text-text-primary disabled:opacity-60`.
pub fn outline_button(id: impl Into<ElementId>, label: impl Into<SharedString>, disabled: bool, small: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    let (fg, fg_hover, hover) = if small {
        (theme.color_text_primary, theme.color_text_primary, theme.color_surface_hover)
    } else {
        (theme.color_text_secondary, theme.color_text_primary, theme.color_surface_hover)
    };
    div()
        .id(id.into())
        .flex_none()
        .rounded(theme.radius_sm)
        .border_1()
        .border_color(theme.color_border)
        .map(|this| if small { this.px_3().py_1p5() } else { this.px_4().py_2() })
        .text_color(fg)
        .child(Label::new(label.into()).size(13.).weight(600.))
        .map(|this| {
            if disabled {
                this.opacity(if small { 0.5 } else { 0.6 })
            } else {
                this.cursor_pointer().hover(move |s| s.bg(hover).text_color(fg_hover))
            }
        })
}

/// A permission with its switch (RolesTab's `PermRow`): label, description, the switch at the
/// right; the whole row toggles.
pub fn perm_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    checked: bool,
    disabled: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    let id = id.into();
    let hover = theme.color_bg_lighter;
    let on_toggle = Rc::new(on_toggle);
    let row_toggle = on_toggle.clone();
    div()
        .id(id.clone())
        .flex()
        .items_center()
        .gap_4()
        .px_4()
        .py(px(14.))
        .when(!disabled, |this| {
            this.cursor_pointer().hover(move |s| s.bg(hover)).on_click(move |_, window, cx| row_toggle(window, cx))
        })
        .child(
            div()
                .min_w_0()
                .flex_1()
                .child(Label::new(label.into()).size(14.).weight(500.).color(theme.color_text_primary))
                .child(div().mt_0p5().child(Label::new(description.into()).size(12.).line_height(12. * 1.55).color(theme.color_text_muted))),
        )
        .child(Switch::new((id, "switch"), checked).disabled(disabled).on_toggle(move |_, window, cx| {
            cx.stop_propagation();
            on_toggle(window, cx)
        }))
}

/// `text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted` (Overview's labels).
pub fn caps_label(text: &'static str, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    Label::new(text.to_uppercase()).size(11.).weight(600.).tracking(0.07).color(theme.color_text_muted)
}

/// A card heading: `text-[14px] font-semibold text-text-primary` (or in `text-error`).
pub fn heading(text: &'static str, color: Hsla) -> impl IntoElement {
    Label::new(text).size(14.).weight(600.).color(color)
}

/// The display-font title of an editor header (`font-display text-[18px] font-semibold`).
pub fn editor_title(text: impl Into<SharedString>, color: Hsla) -> Label {
    Label::new(text.into()).font(FontRole::Display).size(18.).line_height(27.).weight(600.).color(color).truncate()
}

/// The meta line under it: `font-meta text-meta tabular-nums text-text-muted`.
pub fn meta(text: impl Into<SharedString>, cx: &App) -> Label {
    let theme = cx.theme();
    Label::new(text.into()).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted)
}

/// `"1 member"` / `"2 members"` with the count in the system's grouping.
pub fn plural(n: i64, word: &str) -> String {
    let count = decibell_model::regional::Regional::get().group_signed(n);
    if n == 1 { format!("{count} {word}") } else { format!("{count} {word}s") }
}

/// A text field of another size than the kit's 40 px one (Overview's 14 px name, the nickname
/// field, the colour picker's hex): the box `rounded-md border border-border bg-bg-lighter`, the
/// accent border and ring while focused, the text in `font` at `size` / `weight`.
#[derive(IntoElement)]
pub struct InputBox {
    id: ElementId,
    state: Entity<InputState>,
    font: FontRole,
    size: f32,
    weight: u16,
    leading: f32,
    color: Option<Hsla>,
    pad: (f32, f32),
    height: Option<f32>,
    radius_sm: bool,
    ring: bool,
    focus_border: Option<Hsla>,
    hover: bool,
    bg: Option<Hsla>,
    always_focused_look: bool,
    multiline_rows: Option<u32>,
}

impl InputBox {
    pub fn new(id: impl Into<ElementId>, state: &Entity<InputState>) -> Self {
        InputBox {
            id: id.into(),
            state: state.clone(),
            font: FontRole::Sans,
            size: 13.,
            weight: 400,
            leading: 1.5,
            color: None,
            pad: (12., 8.),
            height: None,
            radius_sm: false,
            ring: true,
            focus_border: None,
            hover: false,
            bg: None,
            always_focused_look: false,
            multiline_rows: None,
        }
    }
    pub fn font(mut self, font: FontRole) -> Self {
        self.font = font;
        self
    }
    pub fn text(mut self, size: f32, weight: u16) -> Self {
        self.size = size;
        self.weight = weight;
        self
    }
    pub fn leading(mut self, leading: f32) -> Self {
        self.leading = leading;
        self
    }
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
    pub fn pad(mut self, x: f32, y: f32) -> Self {
        self.pad = (x, y);
        self
    }
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }
    pub fn small_radius(mut self) -> Self {
        self.radius_sm = true;
        self
    }
    pub fn no_ring(mut self) -> Self {
        self.ring = false;
        self
    }
    pub fn focus_border(mut self, color: Hsla) -> Self {
        self.focus_border = Some(color);
        self
    }
    pub fn hover(mut self) -> Self {
        self.hover = true;
        self
    }
    pub fn bg(mut self, bg: Hsla) -> Self {
        self.bg = Some(bg);
        self
    }
    /// Drawn focused whatever the focus (the inline nickname field: `border-accent shadow-ring`).
    pub fn focused_look(mut self) -> Self {
        self.always_focused_look = true;
        self
    }
    /// A text area this many lines tall (`rows={n}`).
    pub fn rows(mut self, rows: u32) -> Self {
        self.multiline_rows = Some(rows);
        self
    }
}

impl RenderOnce for InputBox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let engine = decibell_text::Engine::global(cx);
        let mut style = TextField::input_style(self.font, self.size, self.leading, cx);
        let theme = cx.theme();
        let face = engine.face(&decibell_text::FontSpec { family: theme.font(self.font), weight: self.weight, italic: false });
        style.text = style.text.face(face).color(self.color.unwrap_or(theme.color_text_primary));
        let state = self.state.read(cx);
        let focus = state.focus_handle(cx);
        let multiline = state.rules().multiline;
        let disabled = state.is_disabled();
        let focused = focus.is_focused(window) || self.always_focused_look;
        let border = if focused { self.focus_border.unwrap_or(theme.color_accent) } else { theme.color_border };
        let hover_border = theme.color_accent.opacity(0.4);
        let line = self.size * self.leading;
        let input = if multiline {
            decibell_text::input::TextArea::new(&self.state, style).into_any_element()
        } else {
            decibell_text::input::TextInput::new(&self.state, style).into_any_element()
        };
        div()
            .id(self.id)
            .track_focus(&focus)
            .flex()
            .w_full()
            .min_w_0()
            .when(!multiline, |this| this.items_center())
            .rounded(if self.radius_sm { theme.radius_sm } else { theme.radius_md })
            .border_1()
            .border_color(border)
            .bg(self.bg.unwrap_or(theme.color_bg_lighter))
            .px(px(self.pad.0))
            .py(px(self.pad.1))
            .when_some(self.height, |this, h| this.h(px(h)))
            .when_some(self.multiline_rows, |this, rows| this.min_h(px(rows as f32 * line + self.pad.1 * 2. + 2.)))
            .when(focused && self.ring, |this| this.shadow(theme.shadow_ring.clone()))
            .when(self.hover && !focused && !disabled, |this| this.hover(move |s| s.border_color(hover_border)))
            .when(disabled, |this| this.opacity(0.6).cursor_not_allowed())
            .when(!disabled, |this| this.cursor_text())
            .child(div().flex_1().min_w_0().child(input))
    }
}
