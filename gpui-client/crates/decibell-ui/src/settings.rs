//! The settings frame (`components/SettingsWindow.tsx`), shared by Settings, Server Settings and
//! Channel Settings, and the save bar of a settings draft (`features/servers/tabs/settingsUi.tsx`
//! `SaveBar`).

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, ClickEvent, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, ParentElement, Pixels, RenderOnce, SharedString,
    Window, canvas, div, point, prelude::*, px,
};

use crate::interaction::{Hover, focus_handle};
use crate::modal::{CloseRequest, Modal};
use crate::motion::{self, Keyframes, Transition, mix};
use crate::popover::visible_window_bounds;
use crate::tooltip::Tooltip;
use crate::{
    ActiveTheme as _, Button, ButtonVariant, FontRole, Icon, IconButton, IconName, Label,
    ScrollArea, ScrollHandle,
};

/// The width below which the settings frame folds its sidebar into an icon rail (`max-lg:`).
pub const RAIL_BREAKPOINT: f32 = decibell_theme::tailwind::BREAKPOINT_LG;

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type CloseHandler = Rc<dyn Fn(CloseRequest, &mut Window, &mut App) + 'static>;

/// A tab in the settings sidebar: `rounded-md px-3 py-[9px] text-[14px] transition-colors`; the
/// active one `bg-accent-soft font-medium text-text-primary` with its icon `text-accent-bright`,
/// the others `text-text-secondary hover:bg-surface-hover hover:text-text-primary` with the icon
/// `text-text-muted group-hover:text-text-secondary`, a danger one (Leave Server) `text-text-muted
/// hover:bg-error/10 hover:text-error`. `dot`: unsaved changes.
pub struct NavItem {
    id: ElementId,
    icon: IconName,
    label: SharedString,
    active: bool,
    dot: bool,
    danger: bool,
    on_click: Option<Handler>,
}

impl NavItem {
    pub fn new(id: impl Into<ElementId>, icon: IconName, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon,
            label: label.into(),
            active: false,
            dot: false,
            danger: false,
            on_click: None,
        }
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn dot(mut self, dot: bool) -> Self {
        self.dot = dot;
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

/// The settings window: a modal frame (`max-h-280 max-w-400 rounded-xl border bg-bg-dark
/// shadow-modal`, the backdrop's padding `max(56px, 5vw)` × `max(48px, 6vh)`), a 240 px sidebar
/// of tabs and a scrolling pane whose header (title + ✕) scrolls with it. Below 1024 px the
/// window goes nearly edge to edge (16 px) and the sidebar folds into a 64 px rail of icons, each
/// labelled beside it on hover or keyboard focus, and the header shows `context` (the server or
/// channel name) next to the title. Escape, ✕ and a press on the backdrop call `on_close`: the
/// owner closes it, or refuses (and shakes its [`SaveBar`]). The pane keeps the bar's gutter
/// (`scrollbar-gutter: stable`).
///
/// ```ignore
/// SettingsWindow::new("settings", self.open, &self.scroll)
///     .nav_label("Settings")
///     .item(NavItem::new("account", IconName::SettingsTabAccount, "Account").active(true).on_click(…))
///     .title("Account")
///     .on_close(cx.listener(|this, _: &CloseRequest, _, cx| this.close(cx)))
///     .child(account_tab)
/// ```
#[derive(IntoElement)]
pub struct SettingsWindow {
    id: ElementId,
    open: bool,
    scroll: ScrollHandle,
    nav_label: Option<SharedString>,
    items: Vec<NavItem>,
    foot: Vec<NavItem>,
    title: SharedString,
    context: Option<SharedString>,
    on_close: Option<CloseHandler>,
    children: Vec<AnyElement>,
}

impl SettingsWindow {
    pub fn new(id: impl Into<ElementId>, open: bool, scroll: &ScrollHandle) -> Self {
        Self {
            id: id.into(),
            open,
            scroll: scroll.clone(),
            nav_label: None,
            items: Vec::new(),
            foot: Vec::new(),
            title: SharedString::default(),
            context: None,
            on_close: None,
            children: Vec::new(),
        }
    }

    /// The small uppercase label over the tabs (hidden on the rail).
    pub fn nav_label(mut self, label: impl Into<SharedString>) -> Self {
        self.nav_label = Some(label.into());
        self
    }

    pub fn item(mut self, item: NavItem) -> Self {
        self.items.push(item);
        self
    }

    /// Tabs pushed to the sidebar's foot under a divider (Leave Server).
    pub fn foot_item(mut self, item: NavItem) -> Self {
        self.foot.push(item);
        self
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    /// Shown beside the title on the rail, where the sidebar's heading is hidden.
    pub fn context(mut self, context: impl Into<SharedString>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Escape, a press on the backdrop, and ✕ (as `CloseRequest::Escape`).
    pub fn on_close(mut self, handler: impl Fn(&CloseRequest, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(move |request, window, cx| handler(&request, window, cx)));
        self
    }
}

impl ParentElement for SettingsWindow {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

impl RenderOnce for SettingsWindow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let visible = visible_window_bounds(window, cx);
        let width = f32::from(visible.size.width);
        let height = f32::from(visible.size.height);
        let rail = width < RAIL_BREAKPOINT;
        let (pad_x, pad_y) = if rail {
            (16., 16.)
        } else {
            (56f32.max(width * 0.05), 48f32.max(height * 0.06))
        };
        let theme = cx.theme();
        let id = self.id;

        let mut nav = div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .gap_0p5()
            .w(px(if rail { 64. } else { 240. }))
            .border_r_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_darkest)
            .px_3()
            .py_6()
            .when(!rail, |this| {
                this.when_some(self.nav_label, |this, label| {
                    this.child(
                        div().mb_2().px_3().child(
                            Label::new(SharedString::from(label.to_uppercase()))
                                .size(11.)
                                .weight(600.)
                                .color(theme.color_text_muted)
                                .truncate(),
                        ),
                    )
                })
            });
        for item in self.items {
            nav = nav.child(nav_item(item, rail, window, cx));
        }
        if !self.foot.is_empty() {
            let theme = cx.theme();
            nav = nav
                .child(div().flex_1())
                .child(div().mx_1p5().mb_1().h(px(1.)).bg(theme.color_border_divider));
            for item in self.foot {
                nav = nav.child(nav_item(item, rail, window, cx));
            }
        }

        let theme = cx.theme();
        let on_close = self.on_close.clone();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .pt_7()
            .pb_5()
            .map(|this| if rail { this.px_6() } else { this.px_8() })
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_baseline()
                    .gap_2p5()
                    .child(
                        Label::new(self.title)
                            .font(FontRole::Display)
                            .size(18.)
                            .line_height(27.)
                            .weight(600.)
                            .color(theme.color_text_primary),
                    )
                    .when(rail, |this| {
                        this.when_some(self.context, |this, context| {
                            this.child(
                                div().min_w_0().child(
                                    Label::new(context)
                                        .size(14.)
                                        .color(theme.color_text_muted)
                                        .truncate(),
                                ),
                            )
                        })
                    }),
            )
            .child(
                IconButton::new((id.clone(), "close"), IconName::Close).on_click({
                    let on_close = on_close.clone();
                    move |_, window, cx| {
                        if let Some(handler) = &on_close {
                            handler(CloseRequest::Escape, window, cx);
                        }
                    }
                }),
            );

        let pane = ScrollArea::new((id.clone(), "pane"))
            .track_scroll(&self.scroll)
            .gutter_stable()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .pb_7()
                    .map(|this| if rail { this.px_6() } else { this.px_8() })
                    .children(self.children),
            );

        let frame = div()
            .id((id.clone(), "frame"))
            .occlude()
            .flex()
            .size_full()
            .max_h(px(1120.))
            .max_w(px(1600.))
            .overflow_hidden()
            .rounded(theme.radius_xl)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_dark)
            .shadow(theme.shadow_modal.clone())
            .child(nav)
            .child(pane);

        Modal::new(id, self.open)
            .padding(pad_x, pad_y)
            .on_close(move |request, window, cx| {
                if let Some(handler) = &on_close {
                    handler(*request, window, cx);
                }
            })
            .child(frame)
    }
}

fn nav_item(item: NavItem, rail: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let focus = focus_handle(window, cx, (item.id.clone(), "focus"), true);
    let hover = Hover::new(window, cx, item.id.clone(), Some(Transition::TAILWIND));
    let bounds = window.use_keyed_state((item.id.clone(), "bounds"), cx, |_, _| Bounds::<Pixels>::default());
    let focus_visible = focus.is_focused(window) && window.last_input_was_keyboard();
    let theme = cx.theme();
    let h = if item.active { 0. } else { hover.progress };
    let (text, icon, bg, weight) = if item.danger {
        (
            mix(theme.color_text_muted, theme.color_error, h),
            mix(theme.color_text_muted, theme.color_error, h),
            theme.color_error.opacity(0.1 * h),
            400.,
        )
    } else if item.active {
        (theme.color_text_primary, theme.color_accent_bright, theme.color_accent_soft, 500.)
    } else {
        (
            mix(theme.color_text_secondary, theme.color_text_primary, h),
            mix(theme.color_text_muted, theme.color_text_secondary, h),
            theme.color_surface_hover.opacity(h),
            400.,
        )
    };
    let ring = theme.shadow_ring.clone();
    let accent = theme.color_accent;
    let label = item.label.clone();
    let on_click = item.on_click.clone();
    let tooltip_shown = rail && (*hover.state.read(cx) || focus_visible);
    let anchor = *bounds.read(cx);

    div()
        .id(item.id.clone())
        .track_focus(&focus)
        .relative()
        .flex()
        .items_center()
        .gap_2p5()
        .rounded(theme.radius_md)
        .bg(bg)
        .cursor_pointer()
        .map(|this| {
            if rail {
                this.h_10().justify_center()
            } else {
                this.px_3().py(px(9.))
            }
        })
        .focus_visible(move |style| style.shadow(ring))
        .on_hover(hover.listener())
        .when_some(on_click, |this, handler| this.on_click(move |event, window, cx| handler(event, window, cx)))
        .child({
            let bounds = bounds.clone();
            canvas(move |b, _, cx| bounds.update(cx, |bounds, _| *bounds = b), |_, _, _, _| {})
                .absolute()
                .inset_0()
        })
        .child(Icon::new(item.icon).size(px(16.)).color(icon))
        .when(!rail, |this| {
            this.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(item.label).size(14.).line_height(21.).weight(weight).color(text).truncate()),
            )
        })
        .when(item.dot, |this| {
            this.child(
                div()
                    .flex_none()
                    .size_1p5()
                    .rounded_full()
                    .bg(accent)
                    .when(rail, |this| this.absolute().right_1().top_1()),
            )
        })
        .when(rail, |this| {
            this.child(
                Tooltip::new(
                    ElementId::NamedChild(Arc::new(item.id), SharedString::new_static("rail-label")),
                    label,
                    anchor,
                )
                .shown(tooltip_shown),
            )
        })
        .into_any_element()
}

/// The floating save bar under a settings draft: always holding its 56 px (`mt-8 h-14`) so its
/// arrival never makes the tab taller; while `show`, a bar (`rounded-lg border bg-bg-darkest
/// pl-4 pr-2.5 shadow-float`, entering with `fadeUp` 0.2 s) with the message, Reset and Save
/// ("Saving…" while `saving`). Each refused leave bumps `nudge`: the bar shakes (`barNudge`
/// 0.36 s) and says `nudge_message`. It sticks to the bottom of the pane (`sticky bottom-4`)
/// while the draft runs past it: pass the pane's scroll handle.
///
/// ```ignore
/// SaveBar::new("roles-save", &self.pane_scroll, self.dirty, self.nudge)
///     .saving(self.saving)
///     .on_reset(cx.listener(|this, _, _, cx| this.reset(cx)))
///     .on_save(cx.listener(|this, _, _, cx| this.save(cx)))
/// ```
#[derive(IntoElement)]
pub struct SaveBar {
    id: ElementId,
    scroll: ScrollHandle,
    show: bool,
    nudge: u32,
    saving: bool,
    save_disabled: bool,
    message: SharedString,
    nudge_message: SharedString,
    reset_label: SharedString,
    save_label: SharedString,
    on_reset: Option<Handler>,
    on_save: Option<Handler>,
}

impl SaveBar {
    pub fn new(id: impl Into<ElementId>, scroll: &ScrollHandle, show: bool, nudge: u32) -> Self {
        Self {
            id: id.into(),
            scroll: scroll.clone(),
            show,
            nudge,
            saving: false,
            save_disabled: false,
            message: SharedString::new_static("You have unsaved changes."),
            nudge_message: SharedString::new_static("Save or reset your changes first."),
            reset_label: SharedString::new_static("Reset"),
            save_label: SharedString::new_static("Save changes"),
            on_reset: None,
            on_save: None,
        }
    }

    pub fn saving(mut self, saving: bool) -> Self {
        self.saving = saving;
        self
    }

    pub fn save_disabled(mut self, disabled: bool) -> Self {
        self.save_disabled = disabled;
        self
    }

    pub fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = message.into();
        self
    }

    pub fn nudge_message(mut self, message: impl Into<SharedString>) -> Self {
        self.nudge_message = message.into();
        self
    }

    pub fn labels(mut self, reset: impl Into<SharedString>, save: impl Into<SharedString>) -> Self {
        self.reset_label = reset.into();
        self.save_label = save.into();
        self
    }

    pub fn on_reset(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_reset = Some(Rc::new(handler));
        self
    }

    pub fn on_save(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_save = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SaveBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let id = self.id;
        let bar = self.show.then(|| {
            let on_reset = self.on_reset.clone();
            let on_save = self.on_save.clone();
            let bar = div()
                .flex()
                .w_full()
                .h_full()
                .items_center()
                .gap_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.color_border)
                .bg(theme.color_bg_darkest)
                .pl_4()
                .pr_2p5()
                .shadow(theme.shadow_float.clone())
                .child(
                    div().flex_1().min_w_0().child(
                        Label::new(if self.nudge > 0 { self.nudge_message } else { self.message })
                            .size(13.)
                            .weight(500.)
                            .color(theme.color_text_primary)
                            .truncate(),
                    ),
                )
                .child(
                    Button::new((id.clone(), "reset"), self.reset_label)
                        .variant(ButtonVariant::Ghost)
                        .disabled(self.saving)
                        .on_click(move |event, window, cx| {
                            if let Some(handler) = &on_reset {
                                handler(event, window, cx);
                            }
                        }),
                )
                .child(
                    Button::new(
                        (id.clone(), "save"),
                        if self.saving { SharedString::new_static("Saving…") } else { self.save_label },
                    )
                    .disabled(self.saving || self.save_disabled)
                    .on_click(move |event, window, cx| {
                        if let Some(handler) = &on_save {
                            handler(event, window, cx);
                        }
                    }),
                );
            // A fresh animation per nudge (the renderer keys the bar on it).
            let animation_id = ElementId::NamedChild(
                Arc::new(id.clone()),
                SharedString::from(format!("bar-{}", self.nudge)),
            );
            if self.nudge > 0 {
                bar.with_animation(
                    animation_id,
                    Animation::new(Duration::from_millis(360)),
                    |bar, t| bar.left(px(Keyframes::BAR_NUDGE.value(t))),
                )
                .into_any_element()
            } else {
                bar.with_animation(
                    animation_id,
                    Animation::new(Duration::from_millis(200)).with_easing(motion::EASE.easing()),
                    |bar, t| bar.opacity(t).top(px(6. * (1. - t))),
                )
                .into_any_element()
            }
        });
        div()
            .mt_8()
            .h(px(56.))
            .flex()
            .justify_center()
            .flex_none()
            .child(StickyBottom {
                scroll: self.scroll,
                inset: px(16.),
                child: div().size_full().children(bar).into_any_element(),
            })
    }
}

/// `position: sticky; bottom: inset` inside a [`ScrollArea`]: laid out in the flow, drawn no
/// lower than `inset` above the viewport's bottom.
struct StickyBottom {
    scroll: ScrollHandle,
    inset: Pixels,
    child: AnyElement,
}

impl Element for StickyBottom {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = self.child.request_layout(window, cx);
        let style = gpui::Style {
            size: gpui::size(gpui::relative(1.).into(), gpui::relative(1.).into()),
            ..Default::default()
        };
        (window.request_layout(style, [child], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let viewport = self.scroll.viewport_bounds();
        let limit = viewport.bottom() - self.inset;
        let lift = if viewport.size.height > px(0.) && bounds.bottom() > limit {
            limit - bounds.bottom()
        } else {
            px(0.)
        };
        window.with_element_offset(point(px(0.), lift), |window| self.child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}
