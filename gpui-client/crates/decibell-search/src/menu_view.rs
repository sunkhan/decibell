//! The dropdown under the search box (`SearchMenu`, `MenuBody`, `DatePicker` in the renderer):
//! right-aligned to the box, 6 px under it, at least 360 px wide, never taking the focus (a press
//! in it keeps the field focused). Drawn by [`SearchBox`] while its menu is open.
//!
//! The renderer portals the menu to `<body>`, so its text takes the body's letter-spacing, not
//! the box's `tracking-normal`: labels here keep `Label`'s default (the body's).

use std::time::Duration;

use decibell_common::UserAvatar;
use decibell_model::chat::search::{Day, HasKind};
use decibell_model::regional::Regional;
use decibell_text::{Engine, FontSpec, TextStyle, Wrapping};
use decibell_ui::popover::visible_window_bounds;
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, ScrollArea, Switch, motion};
use gpui::{
    AnimationExt as _, AnyElement, Animation, ClickEvent, Context, Hsla, IntoElement, MouseMoveEvent, SharedString, Window,
    div, prelude::*, px,
};

use crate::calendar;
use crate::format::{format_day, to_date, today};
use crate::menu::Row;
use crate::runs::Runs;
use crate::search_box::{SearchBox, bounds_probe};
use crate::tokens::{Op, category_of};

/// The menu's least width (`Math.max(r.width, 360)`).
const MENU_WIDTH: f32 = 360.;

/// `OP_INFO`.
fn op_info(op: Op) -> (IconName, &'static str) {
    match op {
        Op::From => (IconName::SettingsUser, "a member"),
        Op::In => (IconName::SettingsHash, "a channel"),
        Op::Has => (IconName::SettingsPaperclip, "link, file, image, video, audio or document"),
        Op::Before | Op::During | Op::After => (IconName::SettingsCalendar, "a date"),
    }
}

/// `HAS_ICON`.
pub(crate) fn has_icon(kind: HasKind) -> IconName {
    match kind {
        HasKind::Link => IconName::SettingsLink,
        HasKind::File => IconName::SettingsPaperclip,
        HasKind::Image => IconName::SettingsImage,
        HasKind::Video => IconName::SettingsFilm,
        HasKind::Audio => IconName::SettingsMusic,
        HasKind::Document => IconName::SettingsFile,
    }
}

/// The colours the menu draws with (copied out of the theme so listeners can be made while
/// they are in use).
#[derive(Clone, Copy)]
struct Ink {
    primary: Hsla,
    muted: Hsla,
    secondary: Hsla,
    bright: Hsla,
    hover: Hsla,
    divider: Hsla,
    border: Hsla,
    lighter: Hsla,
    accent: Hsla,
    on_accent: Hsla,
}

impl SearchBox {
    pub(crate) fn render_menu(&self, engine: &Engine, window: &mut Window, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let ink = Ink {
            primary: theme.color_text_primary,
            muted: theme.color_text_muted,
            secondary: theme.color_text_secondary,
            bright: theme.color_text_bright,
            hover: theme.color_surface_hover,
            divider: theme.color_border_divider,
            border: theme.color_border,
            lighter: theme.color_bg_lighter,
            accent: theme.color_accent,
            on_accent: theme.color_on_accent,
        };
        let (radius_sm, radius_md, radius_lg) = (theme.radius_sm, theme.radius_md, theme.radius_lg);
        let (light, shadow) = (theme.color_bg_light, theme.shadow_float.clone());

        let token = self.token.as_ref().map(|t| t.op);
        let body: Vec<AnyElement> = match token {
            Some(op) if op.is_date() => self.date_body(op, ink, radius_sm, radius_md, cx),
            Some(op) => self.picker_body(op, ink, radius_sm, radius_md, cx),
            None => self.plain_body(ink, radius_sm, radius_md, engine, cx),
        };

        // `top: round(r.bottom + 6)`, right-aligned to the box (the box is centred in this view).
        let viewport = visible_window_bounds(window, cx);
        let max_height = (viewport.size.height - px(80.)).min(px(520.));
        let reduce = cx.reduce_motion();
        let epoch = self.menu_epoch;
        let menu = div()
            .id("search-menu")
            .occlude()
            .w(px(MENU_WIDTH))
            .max_h(max_height)
            .flex()
            .flex_col()
            .rounded(radius_lg)
            .border_1()
            .border_color(ink.border)
            .bg(light)
            .shadow(shadow)
            // Never takes the focus: a press here keeps the field focused (`onMouseDown`
            // preventDefault), so nothing inside focuses itself.
            .capture_any_mouse_down(|_, window, _| window.prevent_default())
            .child(bounds_probe(self.menu_bounds.clone()))
            .child(
                ScrollArea::new("search-menu-scroll")
                    .without_keys()
                    .flex_1()
                    .child(div().flex().flex_col().py_1p5().children(body)),
            );
        let placed = div().absolute().top(px(MENU_TOP)).right_0().child(menu);
        let placed = if reduce {
            placed.into_any_element()
        } else {
            // `animate-[dropIn_0.15s_ease]`: from 4 px up and transparent (GPUI can't scale a box).
            placed
                .with_animation(("search-menu-in", epoch), Animation::new(Duration::from_millis(150)).with_easing(motion::EASE.easing()), |el, t| {
                    el.opacity(t).top(px(MENU_TOP - 4. * (1. - t)))
                })
                .into_any_element()
        };
        decibell_ui::layer(1, placed).into_any_element()
    }

    /// A row (`row` in `MenuBody`): `mx-1.5 h-9 gap-2.5 rounded-md px-2 text-[13px]`, the active
    /// one `bg-surface-hover`; the pointer makes a row active, a click picks it.
    fn row(&self, index: usize, row: Row, ink: Ink, radius_md: gpui::Pixels, cx: &Context<Self>) -> gpui::Stateful<gpui::Div> {
        let active = self.active == index as i32;
        div()
            .id(("search-menu-row", index))
            .mx_1p5()
            .flex()
            .flex_none()
            .h_9()
            .items_center()
            .gap_2p5()
            .rounded(radius_md)
            .px_2()
            .cursor_pointer()
            .when(active, |this| this.bg(ink.hover))
            .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| this.set_active(index as i32, cx)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.pick(row.clone(), window, cx)))
    }

    fn plain_body(&self, ink: Ink, radius_sm: gpui::Pixels, radius_md: gpui::Pixels, engine: &Engine, cx: &Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let mut index = 0usize;
        if let Some(Row::Run(text)) = self.rows.first() {
            // `min-w-0 flex-1 truncate`: one line of two runs.
            let (plain, bold) = run_styles(ink, engine, cx);
            let runs = if text.is_empty() {
                vec![("Search with these filters".to_string(), plain)]
            } else {
                vec![("Search for ".to_string(), plain), (text.to_string(), bold)]
            };
            let content = div().min_w_0().flex_1().child(Runs::new(plain, Wrapping::TRUNCATE).runs(runs));
            out.push(
                self.row(index, Row::Run(text.clone()), ink, radius_md, cx)
                    .child(Icon::new(IconName::SettingsSearch).size(px(15.)).color(ink.muted))
                    .child(content)
                    .child(keycap("Enter", ink, radius_sm))
                    .into_any_element(),
            );
            index += 1;
        }
        out.push(section_label("Filters", ink, cx));
        for row in &self.rows {
            let Row::Op(op) = row else { continue };
            let (icon, hint) = op_info(*op);
            out.push(
                self.row(index, row.clone(), ink, radius_md, cx)
                    .child(Icon::new(icon).size(px(15.)).color(ink.muted))
                    .child(div().flex_none().child(Label::new(format!("{}:", op.name())).size(13.).weight(600.).color(ink.primary).nowrap()))
                    .child(div().min_w_0().flex_1().child(Label::new(hint).size(13.).color(ink.muted).truncate()))
                    .into_any_element(),
            );
            index += 1;
        }
        if self.rows.contains(&Row::Inside) {
            out.push(section_label("Match", ink, cx));
            out.push(
                self.row(index, Row::Inside, ink, radius_md, cx)
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .child(div().flex_none().child(Label::new("Inside words").size(13.).weight(500.).color(ink.primary).nowrap()))
                            .child(div().ml_2().min_w_0().child(Label::new("abc also finds xabcy").size(13.).color(ink.muted).truncate())),
                    )
                    // The row toggles it; the switch only shows the state.
                    .child(Switch::new("search-inside-switch", self.draft.matches_inside()))
                    .into_any_element(),
            );
        }
        out.push(menu_hint(&[("↑↓", "choose"), ("Enter", "search"), ("Ctrl F", "from anywhere")], ink, radius_sm));
        out
    }

    fn picker_body(&self, op: Op, ink: Ink, radius_sm: gpui::Pixels, radius_md: gpui::Pixels, cx: &Context<Self>) -> Vec<AnyElement> {
        let title = match op {
            Op::From => "From",
            Op::In => "In channel",
            _ => "Has",
        };
        let mut out = vec![section_label(title, ink, cx)];
        if self.rows.is_empty() {
            let text = match op {
                Op::From => "No member by that name.",
                Op::In => "No channel by that name.",
                _ => "Nothing by that name.",
            };
            out.push(div().px_3().py_2().child(Label::new(text).size(13.).color(ink.muted)).into_any_element());
        }
        let all_channels = self.server.and_then(|s| self.model.community(s, cx)).map(|c| c.channels.read(cx).list().to_vec()).unwrap_or_default();
        for (index, row) in self.rows.iter().enumerate() {
            let el = self.row(index, row.clone(), ink, radius_md, cx);
            let el = match row {
                Row::Member(m) => {
                    let username = decibell_common::user_name(m.uid, &m.name, cx);
                    let name = decibell_common::display_name(self.server, m.uid, &username, cx);
                    let differs = name != username;
                    el.child(UserAvatar::new(m.uid, username.clone(), 22.))
                        .child(div().min_w_0().child(Label::new(name).size(13.).weight(500.).color(ink.primary).truncate()))
                        .child(if differs {
                            div().min_w_0().flex_1().child(Label::new(username).size(12.).color(ink.muted).truncate()).into_any_element()
                        } else {
                            div().flex_1().into_any_element()
                        })
                }
                Row::Channel(c) => {
                    let icon = if c.encrypted { IconName::SettingsLock } else { IconName::SettingsHash };
                    let side: AnyElement = if Some(&c.id) == self.channel.as_ref() {
                        Label::new("this channel").size(12.).color(ink.muted).nowrap().into_any_element()
                    } else {
                        // `max-w-[40%] truncate`.
                        div()
                            .max_w(gpui::relative(0.4))
                            .child(Label::new(category_of(&all_channels, &c.id)).size(12.).color(ink.muted).truncate())
                            .into_any_element()
                    };
                    el.child(Icon::new(icon).size(px(15.)).color(ink.muted))
                        .child(div().min_w_0().flex_1().child(Label::new(c.name.clone()).size(13.).weight(500.).color(ink.primary).truncate()))
                        .child(side)
                }
                Row::Has(kind) => el
                    .child(Icon::new(has_icon(*kind)).size(px(15.)).color(ink.muted))
                    .child(div().flex_1().child(Label::new(kind.label()).size(13.).weight(500.).color(ink.primary).nowrap())),
                _ => continue,
            };
            out.push(el.into_any_element());
        }
        out.push(menu_hint(&[("↑↓", "choose"), ("Enter", "add"), ("Esc", "close")], ink, radius_sm));
        out
    }

    fn date_body(&self, op: Op, ink: Ink, radius_sm: gpui::Pixels, radius_md: gpui::Pixels, cx: &Context<Self>) -> Vec<AnyElement> {
        let title = match op {
            Op::Before => "Before",
            Op::After => "After",
            _ => "On",
        };
        let mut out = vec![section_label(title, ink, cx)];
        for (index, row) in self.rows.iter().enumerate() {
            let Row::Day { day, label } = row else { continue };
            out.push(
                self.row(index, row.clone(), ink, radius_md, cx)
                    .child(Icon::new(IconName::SettingsCalendar).size(px(15.)).color(ink.muted))
                    .child(div().flex_1().child(Label::new(*label).size(13.).weight(500.).color(ink.primary).nowrap()))
                    .child(Label::new(format_day(*day)).size(12.).color(ink.muted).nowrap())
                    .into_any_element(),
            );
        }
        out.push(div().mx_3().my_1p5().h(px(1.)).bg(ink.divider).into_any_element());
        out.push(self.calendar(ink, radius_sm, cx));
        out.push(menu_hint(&[("←→↑↓", "pick a day"), ("Enter", "add"), ("2026-10-03", "or type it")], ink, radius_sm));
        out
    }

    /// `DatePicker`: the month of the highlighted day, the system's first weekday first; the
    /// pointer moves the highlight, a click picks; days after today are off.
    fn calendar(&self, ink: Ink, radius_sm: gpui::Pixels, cx: &Context<Self>) -> AnyElement {
        let regional = Regional::get();
        let first = regional.first_weekday();
        let today = today(cx);
        let active = self.cal_day;
        let date = to_date(active);
        let title = regional.month_year(date.year, date.month);
        let next_off = calendar::next_month_disabled(active, today);
        let nav = |id: &'static str, icon: IconName, months: i32, disabled: bool| {
            div()
                .id(id)
                .group(id)
                .flex()
                .flex_none()
                .size_7()
                .items_center()
                .justify_center()
                .rounded(radius_sm)
                .when(disabled, |this| this.opacity(0.3))
                .when(!disabled, |this| {
                    this.cursor_pointer().hover(|s| s.bg(ink.hover)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let day = calendar::shift_month(this.cal_day, months, today);
                        this.set_cal_day(day, cx);
                    }))
                })
                .child({
                    let icon = Icon::new(icon).size(px(14.)).stroke_width(2.5).color(ink.muted);
                    if disabled { icon } else { icon.group_hover(id, ink.primary) }
                })
        };
        let heads = calendar::weekday_heads(first).into_iter().map(|head| {
            div()
                .flex()
                .h_6()
                .items_center()
                .justify_center()
                .child(Label::new(head).size(11.).weight(600.).color(ink.muted).nowrap())
        });
        let cells = calendar::cells(active, first).into_iter().map(|cell| {
            let day: Day = cell.day;
            let future = day > today;
            let on = day == active;
            let number: SharedString = chrono::Datelike::day(&day).to_string().into();
            let color = if on {
                ink.on_accent
            } else if future || !cell.in_month {
                ink.muted
            } else {
                ink.primary
            };
            div()
                .id(("search-day", chrono::Datelike::num_days_from_ce(&day).max(0) as usize))
                .relative()
                .flex()
                .h_8()
                .items_center()
                .justify_center()
                .rounded(radius_sm)
                .when(on, |this| this.bg(ink.accent))
                .when(future && !on, |this| this.opacity(0.3))
                .when(!on && !future, |this| this.hover(|s| s.bg(ink.hover)))
                .when(!future, |this| {
                    this.cursor_pointer()
                        .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| this.set_cal_day(day, cx)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.pick_day(day, window, cx)))
                })
                .child(Label::new(number).size(13.).weight(if on { 600. } else { 400. }).color(color).nowrap())
                .when(day == today && !on, |this| {
                    this.child(div().absolute().bottom_1().size_1().rounded_full().bg(ink.accent))
                })
        });
        div()
            .px_2()
            .pb_1()
            .child(
                div()
                    .mb_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .child(div().flex_1().child(Label::new(title).size(13.).weight(600.).color(ink.primary).nowrap()))
                    .child(nav("search-month-prev", IconName::SearchChevronLeft, -1, false))
                    .child(nav("search-month-next", IconName::SearchChevronRight, 1, next_off)),
            )
            .child(grid7(heads.map(IntoElement::into_any_element).chain(cells.map(IntoElement::into_any_element)).collect()))
            .into_any_element()
    }
}

/// The menu's top, from the view's top: the box is centred in the 47 px of the header above its
/// border, so its bottom is at 39.5; `Math.round(r.bottom + 6)` lands 46 px down.
const MENU_TOP: f32 = 46.;

/// "Search for" (`text-text-secondary`, 13 px) and the words (`font-semibold text-text-bright`), with
/// the body's letter-spacing the menu inherits.
fn run_styles(ink: Ink, engine: &Engine, cx: &Context<SearchBox>) -> (TextStyle, TextStyle) {
    let theme = cx.theme();
    let tracking = theme.palette.t_ls_body * 16.;
    let face = |weight| engine.face(&FontSpec { family: theme.font(FontRole::Sans), weight, italic: false });
    (
        TextStyle::new(face(400), 13., ink.secondary).leading(1.5).letter_spacing(tracking),
        TextStyle::new(face(600), 13., ink.bright).leading(1.5).letter_spacing(tracking),
    )
}

/// `grid grid-cols-7 gap-0.5`: seven equal columns, 2 px apart.
fn grid7(cells: Vec<AnyElement>) -> AnyElement {
    let mut rows = Vec::new();
    let mut cells = cells.into_iter().peekable();
    while cells.peek().is_some() {
        let row: Vec<AnyElement> = cells.by_ref().take(7).map(|cell| div().flex_1().min_w_0().child(cell).into_any_element()).collect();
        rows.push(div().flex().gap_0p5().children(row).into_any_element());
    }
    div().flex().flex_col().gap_0p5().children(rows).into_any_element()
}

/// `label(text)`: `px-3 pb-1 pt-2 font-section text-section font-medium uppercase leading-none
/// tracking-section text-text-muted`.
fn section_label(text: &str, ink: Ink, cx: &Context<SearchBox>) -> AnyElement {
    let theme = cx.theme();
    let size = theme.scaled.text_section;
    div()
        .px_3()
        .pb_1()
        .pt_2()
        .child(div().child(
            Label::new(text.to_uppercase())
                .font(FontRole::Section)
                .size(size)
                .weight(500.)
                .line_height(size)
                .tracking(theme.palette.tracking_section)
                .color(ink.muted)
                .nowrap(),
        ))
        .into_any_element()
}

/// `Keycap`: `rounded-sm border border-border bg-bg-lighter px-1.5 py-px text-[11px] font-semibold
/// text-text-muted`.
fn keycap(text: &'static str, ink: Ink, radius_sm: gpui::Pixels) -> AnyElement {
    div()
        .flex_none()
        .rounded(radius_sm)
        .border_1()
        .border_color(ink.border)
        .bg(ink.lighter)
        .px_1p5()
        .py(px(1.))
        .child(Label::new(text).size(11.).weight(600.).color(ink.muted).nowrap())
        .into_any_element()
}

/// `MenuHint`: the keys at the menu's foot.
fn menu_hint(keys: &[(&'static str, &'static str)], ink: Ink, radius_sm: gpui::Pixels) -> AnyElement {
    div()
        .mt_1p5()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_3()
        .gap_y_1()
        .border_t_1()
        .border_color(ink.divider)
        .px_3()
        .pb_0p5()
        .pt_2()
        .children(keys.iter().map(|(key, what)| {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(keycap(key, ink, radius_sm))
                .child(Label::new(*what).size(11.).color(ink.muted).nowrap())
        }))
        .into_any_element()
}
