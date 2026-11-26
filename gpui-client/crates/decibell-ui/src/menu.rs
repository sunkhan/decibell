//! Context menus and the menus of settings popovers: `features/servers/tabs/settingsUi.tsx`
//! (`Popover`, `MenuItem`, `MenuDivider`), `features/voice/UserContextMenu.tsx` (section labels,
//! the volume slider row, "Move to…" unfolding in place), `features/voice/DeviceContextMenu.tsx`
//! (checkable rows).

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, ElementId, FocusHandle, IntoElement, Pixels, Point, RenderOnce, SharedString,
    Transformation, Window, div, prelude::*, px, radians, svg,
};

use crate::icon::icon_path;
use crate::interaction::focus_handle;
use crate::motion::{self, Transition, mix};
use crate::popover::{DismissEvent, Dismissal, MENU_MARGIN, Placed, Placement, Side, drop_in, visible_window_bounds};
use crate::{ActiveTheme as _, FontRole, Icon, IconName, Label};

type Handler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;
type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// One row of a [`ContextMenu`]: `flex items-center gap-2.5 rounded-md px-2.5 py-2 text-[13px]
/// font-medium text-text-primary hover:bg-surface-hover focus-visible:bg-surface-hover`, a 15 px
/// `text-text-muted` icon; danger rows `text-error hover:bg-error/10`; disabled at 40 %.
pub struct MenuItem {
    label: SharedString,
    icon: Option<IconName>,
    danger: bool,
    disabled: bool,
    checked: Option<bool>,
    hint: Option<SharedString>,
    expanded: Option<bool>,
    keep_open: bool,
    on_click: Option<ClickHandler>,
}

impl MenuItem {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            danger: false,
            disabled: false,
            checked: None,
            hint: None,
            expanded: None,
            keep_open: false,
            on_click: None,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// A choice row (`DeviceContextMenu`): a 12 px accent tick when checked, its space kept when not.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Muted text at the row's end (a shortcut).
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// A row that unfolds the rows after it in place ("Move to…"): a chevron that turns 90° and
    /// takes the accent while `expanded`. Clicking it keeps the menu open.
    pub fn expander(mut self, expanded: bool) -> Self {
        self.expanded = Some(expanded);
        self.keep_open = true;
        self
    }

    /// Don't close the menu after running the action (a toggle such as Mute).
    pub fn keep_open(mut self) -> Self {
        self.keep_open = true;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

/// What a [`ContextMenu`] holds.
pub enum MenuEntry {
    Item(MenuItem),
    /// `mx-2.5 my-1 h-px bg-border-divider`.
    Separator,
    /// A section heading: `px-2.5 pb-1 pt-1.5 text-[10px] font-semibold uppercase
    /// tracking-[0.07em] text-text-muted` ("Moderation").
    Label(SharedString),
    /// Anything else (a header with an avatar, the volume slider row).
    Custom(AnyElement),
}

impl From<MenuItem> for MenuEntry {
    fn from(item: MenuItem) -> Self {
        MenuEntry::Item(item)
    }
}

/// A menu at a point (a right click, or under a button): `rounded-lg border border-border
/// bg-bg-light p-1 shadow-float`, entering with `dropIn`, kept inside the window (flipped above or
/// to the left of the anchor when it does not fit, as `useMenuPosition` does) and scrolling when
/// taller than the window. The first row takes focus; ↑ / ↓ move, Enter / Space run, Escape, a
/// press outside or the window losing focus close it (`on_dismiss`). A row's action closes it
/// unless the row says otherwise.
///
/// ```ignore
/// ContextMenu::new("member-menu", position)
///     .entry(MenuItem::new("Copy ID").icon(IconName::SettingsKey).on_click(…))
///     .entry(MenuEntry::Separator)
///     .entry(MenuItem::new("Kick").icon(IconName::SettingsUserX).danger().on_click(…))
///     .on_dismiss(cx.listener(|this, _: &DismissEvent, _, cx| { this.menu = None; cx.notify() }))
/// ```
#[derive(IntoElement)]
pub struct ContextMenu {
    id: ElementId,
    anchor: Point<Pixels>,
    prefer: Side,
    width: Option<Pixels>,
    entries: Vec<MenuEntry>,
    on_dismiss: Option<Handler>,
    sidebar: bool,
}

impl ContextMenu {
    pub fn new(id: impl Into<ElementId>, anchor: Point<Pixels>) -> Self {
        Self {
            id: id.into(),
            anchor,
            prefer: Side::Below,
            width: None,
            entries: Vec::new(),
            on_dismiss: None,
            sidebar: false,
        }
    }

    /// The channel list's menu (`ServerChannelsSidebar`'s context menu and its `ContextMenuItem`
    /// rows): `rounded-md p-[5px]`, rows `rounded-sm px-2.5 py-[8px] text-[13px]` in
    /// `text-text-secondary` (danger: `text-text-muted`, `hover:text-error hover:bg-error/10`),
    /// no icons. Drawn in the sidebar's DOM, it takes the chrome's palette: build it inside
    /// [`crate::chrome_scope`].
    pub fn sidebar(mut self) -> Self {
        self.sidebar = true;
        self
    }

    pub fn prefer(mut self, side: Side) -> Self {
        self.prefer = side;
        self
    }

    /// A fixed width (`w-[220px]`); else as wide as the widest row.
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }

    pub fn entry(mut self, entry: impl Into<MenuEntry>) -> Self {
        self.entries.push(entry.into());
        self
    }

    pub fn entries(mut self, entries: impl IntoIterator<Item = MenuEntry>) -> Self {
        self.entries.extend(entries);
        self
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&DismissEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(move |window, cx| handler(&DismissEvent, window, cx)));
        self
    }
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.id;
        let on_dismiss: Handler = self.on_dismiss.unwrap_or_else(|| Rc::new(|_, _| {}));
        let dismissal = Dismissal::new(window, cx, &id, on_dismiss);
        let dismiss = dismissal.handler();

        // Every enabled row's focus handle, in order, for the arrow keys.
        let mut handles: Vec<FocusHandle> = Vec::new();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, entry) in self.entries.into_iter().enumerate() {
            let row_id = ElementId::NamedChild(Arc::new(id.clone()), SharedString::from(index.to_string()));
            match entry {
                MenuEntry::Item(item) => {
                    let focus = focus_handle(window, cx, (row_id.clone(), "focus"), false);
                    if !item.disabled {
                        handles.push(focus.clone());
                    }
                    rows.push(menu_row(row_id, item, focus, dismiss.clone(), self.sidebar, window, cx));
                }
                MenuEntry::Separator => {
                    let theme = cx.theme();
                    rows.push(
                        div()
                            .mx_2p5()
                            .my_1()
                            .h(px(1.))
                            .bg(theme.color_border_divider)
                            .into_any_element(),
                    );
                }
                MenuEntry::Label(label) => {
                    let theme = cx.theme();
                    rows.push(
                        div()
                            .px_2p5()
                            .pb_1()
                            .pt_1p5()
                            .child(
                                Label::new(SharedString::from(label.to_uppercase()))
                                    .size(10.)
                                    .weight(600.)
                                    .color(theme.color_text_muted),
                            )
                            .into_any_element(),
                    );
                }
                MenuEntry::Custom(element) => rows.push(element),
            }
        }

        // The first row takes focus as the menu opens (settingsUi's Popover).
        let first_focus = window.use_keyed_state((id.clone(), "first-focus"), cx, |_, _| false);
        if !*first_focus.read(cx)
            && let Some(first) = handles.first().cloned()
        {
            first_focus.update(cx, |done, _| *done = true);
            window.defer(cx, move |window, cx| first.focus(window, cx));
        }

        let theme = cx.theme();
        let max_height = visible_window_bounds(window, cx).size.height - MENU_MARGIN * 2.;
        let menu = div()
            .id(id.clone())
            .occlude()
            .flex()
            .flex_col()
            .min_w(px(180.))
            .when_some(self.width, |this, width| this.w(width))
            .max_h(max_height)
            .overflow_hidden()
            .rounded(if self.sidebar { theme.radius_md } else { theme.radius_lg })
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_light)
            .shadow(theme.shadow_float.clone())
            .map(|this| if self.sidebar { this.p(px(5.)) } else { this.p_1() })
            .on_key_down(move |event, window, cx| {
                let key = event.keystroke.key.as_str();
                if !matches!(key, "down" | "up" | "home" | "end") || handles.is_empty() {
                    return;
                }
                cx.stop_propagation();
                let at = handles.iter().position(|handle| handle.is_focused(window));
                let len = handles.len();
                let next = match (key, at) {
                    ("home", _) => 0,
                    ("end", _) => len - 1,
                    ("down", Some(at)) => (at + 1) % len,
                    ("up", Some(at)) => (at + len - 1) % len,
                    ("down", None) => 0,
                    _ => len - 1,
                };
                handles[next].focus(window, cx);
            })
            .children(rows);
        crate::layer::layer(
            4,
            Placed::new(Placement::Menu {
                anchor: self.anchor,
                prefer: self.prefer,
            })
            .child(drop_in(id, dismissal.attach(menu))),
        )
    }
}

fn menu_row(
    id: ElementId,
    item: MenuItem,
    focus: FocusHandle,
    dismiss: Handler,
    sidebar: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    if sidebar {
        return sidebar_row(id, item, focus, dismiss, window, cx);
    }
    let expand_t = item.expanded.map(|expanded| {
        motion::transition_flag(window, cx, (id.clone(), "expanded"), expanded, Transition::TAILWIND)
    });
    let theme = cx.theme();
    let focused = focus.is_focused(window) && window.last_input_was_keyboard();
    let (text, icon_color, hover_bg) = if item.danger {
        (theme.color_error, theme.color_error, theme.color_error.opacity(0.1))
    } else {
        (theme.color_text_primary, theme.color_text_muted, theme.color_surface_hover)
    };
    let disabled = item.disabled;
    let keep_open = item.keep_open;
    let on_click = item.on_click.clone();

    div()
        .id(id)
        .track_focus(&focus)
        .flex()
        .w_full()
        .items_center()
        .gap_2p5()
        .rounded(theme.radius_md)
        .px_2p5()
        .py_2()
        .when(focused, |this| this.bg(hover_bg))
        .map(|this| {
            if disabled {
                this.opacity(0.4)
            } else {
                this.cursor_pointer()
                    .hover(move |style| style.bg(hover_bg))
                    .on_click(move |event, window, cx| {
                        if let Some(handler) = &on_click {
                            handler(event, window, cx);
                        }
                        if !keep_open {
                            dismiss(window, cx);
                        }
                    })
            }
        })
        .when_some(item.checked, |this, checked| {
            this.child(div().flex_none().size(px(12.)).when(checked, |this| {
                this.child(
                    Icon::new(IconName::Checkmark)
                        .size(px(12.))
                        .stroke_width(3.)
                        .color(theme.color_accent),
                )
            }))
        })
        .when_some(item.icon, |this, icon| this.child(Icon::new(icon).size(px(15.)).color(icon_color)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Label::new(item.label).size(13.).weight(500.).color(text).truncate()),
        )
        .when_some(item.hint, |this, hint| {
            this.child(
                Label::new(hint)
                    .font(FontRole::Meta)
                    .size(11.)
                    .color(theme.color_text_muted),
            )
        })
        .when_some(expand_t, |this, t| {
            this.child(
                svg()
                    .path(icon_path(IconName::ChevronRight, None))
                    .size(px(12.))
                    .flex_none()
                    .text_color(mix(theme.color_text_muted, theme.color_accent, t.round()))
                    .with_transformation(Transformation::rotate(radians(std::f32::consts::FRAC_PI_2 * t))),
            )
        })
        .into_any_element()
}

/// `ContextMenuItem` (ServerChannelsSidebar): `flex w-full items-center rounded-sm px-2.5 py-[8px]
/// text-[13px]`, `text-text-secondary hover:bg-surface-hover hover:text-text-primary`; danger
/// `text-text-muted hover:bg-error/10 hover:text-error`. The label takes the row's colour (it
/// changes on hover).
fn sidebar_row(id: ElementId, item: MenuItem, focus: FocusHandle, dismiss: Handler, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let focused = focus.is_focused(window) && window.last_input_was_keyboard();
    let (fg, fg_hover, bg_hover) = if item.danger {
        (theme.color_text_muted, theme.color_error, theme.color_error.opacity(0.1))
    } else {
        (theme.color_text_secondary, theme.color_text_primary, theme.color_surface_hover)
    };
    let keep_open = item.keep_open;
    let on_click = item.on_click.clone();
    div()
        .id(id)
        .track_focus(&focus)
        .flex()
        .w_full()
        .items_center()
        .rounded(theme.radius_sm)
        .px_2p5()
        .py(px(8.))
        .text_color(fg)
        .when(focused, |this| this.bg(bg_hover).text_color(fg_hover))
        .map(|this| {
            if item.disabled {
                this.opacity(0.4)
            } else {
                this.cursor_pointer().hover(move |style| style.bg(bg_hover).text_color(fg_hover)).on_click(move |event, window, cx| {
                    if let Some(handler) = &on_click {
                        handler(event, window, cx);
                    }
                    if !keep_open {
                        dismiss(window, cx);
                    }
                })
            }
        })
        .child(Label::new(item.label).size(13.).truncate())
        .into_any_element()
}
