//! The dropdown picker (`components/Select.tsx`): a select-only combobox whose menu is drawn by
//! the app in the theme, not by the OS.

use std::f32::consts::PI;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use decibell_text::input::{InputState, TextInput};
use decibell_theme::ThemeId;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, DispatchPhase, ElementId, Entity, Focusable as _, Hsla,
    IntoElement, MouseButton, Pixels, RenderOnce, ScrollEvent, SharedString, Transformation,
    Window, canvas, deferred, div, linear_color_stop, linear_gradient, prelude::*, px,
    radians, svg,
};

use crate::icon::icon_path;
use crate::interaction::{focus_handle, ring};
use crate::motion::{self, CubicBezier, Transition, mix};
use crate::popover::{Placed, Placement, visible_window_bounds};
use crate::{ActiveTheme as _, FontRole, Icon, IconName, Label, ScrollAlign, ScrollArea, ScrollHandle};

const MARGIN: f32 = 8.;
const GAP: f32 = 4.;
const MAX_MENU_H: f32 = 320.;
const TYPEAHEAD: Duration = Duration::from_millis(600);
const PAGE: usize = 8;
/// The filter row: `py-2` around a 13 px input (line-height `normal`: Inter's 1.21), and its
/// `border-b`.
const SEARCH_ROW_H: f32 = 16. + 13. * 1.2109 + 1.;
/// `.dsel-group`: `pb-1 pt-2`, 11 px.
const GROUP_LABEL_H: f32 = 12. + 16.5;
/// "No matches": `px-2.5 py-2 text-[12px]`.
const EMPTY_H: f32 = 16. + 18.;
/// nocturne's `.dsel-menu` sheen: `linear-gradient(to bottom, rgb(255 244 228 / 0.035), transparent 56px)`
/// (globals.css, a component rule rather than a token).
const NOCTURNE_SHEEN: Hsla = Hsla {
    h: 35. / 360.,
    s: 1.,
    l: 0.947,
    a: 0.035,
};

/// A leading glyph on an option or the trigger.
#[derive(Debug, Clone, Copy)]
pub enum SelectIcon {
    Icon(IconName),
    /// A colour dot (a role's colour), 10 px.
    Dot(Hsla),
}

/// One choice.
#[derive(Clone)]
pub struct SelectOption<T> {
    pub value: T,
    pub label: SharedString,
    /// A muted second line.
    pub hint: Option<SharedString>,
    pub icon: Option<SelectIcon>,
    pub disabled: bool,
}

impl<T> SelectOption<T> {
    pub fn new(value: T, label: impl Into<SharedString>) -> Self {
        Self {
            value,
            label: label.into(),
            hint: None,
            icon: None,
            disabled: false,
        }
    }

    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn icon(mut self, icon: SelectIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// A labelled group of options (an empty label: no heading).
#[derive(Clone)]
pub struct SelectGroup<T> {
    pub label: SharedString,
    pub options: Vec<SelectOption<T>>,
}

impl<T> SelectGroup<T> {
    pub fn new(label: impl Into<SharedString>, options: Vec<SelectOption<T>>) -> Self {
        Self {
            label: label.into(),
            options,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectSize {
    /// Form fields: 40 px.
    #[default]
    Md,
    /// Inline and toolbar pickers: 30 px.
    Sm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectVariant {
    /// An input on a card: `bg-bg-lighter text-text-primary`.
    #[default]
    Field,
    /// A control on a raised panel: `bg-bg-darkest text-text-secondary`.
    Sunken,
}

/// The three ways the themes dress the menu (`.dsel-*` in globals.css).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// nocturne, matinee: accent-soft selection with a 2 px bar, the check at the end.
    Lacquer,
    /// graphite, graphite-light: the active row filled with the accent, the check first.
    Crisp,
    /// console*: monospace, a `›` marker on the active row, `▾` for the chevron, `// ` groups.
    Terminal,
}

impl Family {
    fn of(id: ThemeId) -> Self {
        match id {
            ThemeId::Nocturne | ThemeId::Matinee => Family::Lacquer,
            ThemeId::Graphite | ThemeId::GraphiteLight => Family::Crisp,
            _ => Family::Terminal,
        }
    }
}

/// Where the menu went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuSide {
    Below,
    Above,
}

struct SelectState {
    open: bool,
    /// Index into the visible options.
    active: Option<usize>,
    /// Whether the keyboard (or opening) moved `active` last: only then is it scrolled into view.
    /// Scrolling for the pointer revealed the half-visible row under it, the synthetic mouse
    /// move after the scroll made the next row active, and the list ran away (Select.tsx).
    reveal: Option<ScrollAlign>,
    /// The filter row's text as last seen (the row is a `TextInput`; see `render`).
    query: String,
    typeahead: String,
    typeahead_at: Option<Instant>,
    /// The trigger's bounds in the last frame.
    trigger: Bounds<Pixels>,
    scroll: ScrollHandle,
    /// Whether the window was active while the menu was open, last frame.
    window_active: Option<bool>,
    /// The view the select was last drawn in: a state held by the owner (a `SelectHandle`) is
    /// not observed by that view, which may be cached, so changes notify it directly.
    view: Option<gpui::EntityId>,
}

impl SelectState {
    fn new(open: bool) -> Self {
        Self {
            open,
            active: None,
            reveal: None,
            query: String::new(),
            typeahead: String::new(),
            typeahead_at: None,
            trigger: Bounds::default(),
            scroll: ScrollHandle::new(),
            window_active: None,
            view: None,
        }
    }

    fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.active = None;
        self.reveal = None;
        self.window_active = None;
    }
}

type ChangeHandler<T> = Rc<dyn Fn(&T, &mut Window, &mut App) + 'static>;

/// Changes the select's state and redraws the view it is drawn in.
fn update_state<R>(state: &Entity<SelectState>, cx: &mut App, f: impl FnOnce(&mut SelectState) -> R) -> R {
    let (result, view) = state.update(cx, |state, cx| {
        let result = f(state);
        cx.notify();
        (result, state.view)
    });
    if let Some(view) = view {
        cx.notify(view);
    }
    result
}

/// A select's open state, shared with its owner (else it lives in the select's element state):
/// to open or close it from code, or to ask what it shows.
///
/// ```ignore
/// let handle = SelectHandle::new(cx);
/// Select::options("device", value, options).track(&handle)
/// handle.open(cx); handle.is_open(cx)
/// ```
#[derive(Clone)]
pub struct SelectHandle(Entity<SelectState>);

impl SelectHandle {
    pub fn new(cx: &mut App) -> Self {
        Self(cx.new(|_| SelectState::new(false)))
    }

    pub fn is_open(&self, cx: &App) -> bool {
        self.0.read(cx).open
    }

    /// The highlighted row (in the menu's keyboard order) while open.
    pub fn active(&self, cx: &App) -> Option<usize> {
        self.0.read(cx).active
    }

    /// How far the option list is scrolled.
    pub fn list_offset(&self, cx: &App) -> Pixels {
        self.0.read(cx).scroll.offset()
    }

    pub fn close(&self, cx: &mut App) {
        update_state(&self.0, cx, |state| state.close());
    }
}

/// A dropdown picker. Keyboard as the native control: ↓ ↑ Enter Space open it; then arrows,
/// Home / End, Page Up / Down move, Enter / Space choose, typing jumps to a label ("bbb" cycles
/// the b's), Escape closes without reaching what is underneath (a modal), Tab closes. The menu
/// opens below the trigger, or above it when there is more room there, as tall as its options
/// up to 320 px, at least as wide as the trigger; a press outside, the wheel outside it or the
/// window losing focus closes it. Past 10 options it has a filter row (a `TextInput`, focused
/// while the menu is open: typing filters). Each theme family styles
/// it as globals.css does (`.dsel-*`).
///
/// ```ignore
/// Select::new("output-device", Some(self.device.clone()), vec![SelectGroup::new("", options)])
///     .on_change(cx.listener(|this, device: &DeviceId, _, cx| { this.device = device.clone(); cx.notify() }))
/// ```
#[derive(IntoElement)]
pub struct Select<T: Clone + PartialEq + 'static> {
    id: ElementId,
    value: Option<T>,
    groups: Vec<SelectGroup<T>>,
    placeholder: SharedString,
    size: SelectSize,
    variant: SelectVariant,
    disabled: bool,
    searchable: Option<bool>,
    menu_min_width: Option<Pixels>,
    icon: Option<IconName>,
    initially_open: bool,
    handle: Option<SelectHandle>,
    on_change: Option<ChangeHandler<T>>,
}

impl<T: Clone + PartialEq + 'static> Select<T> {
    /// `value: None` shows the placeholder.
    pub fn new(id: impl Into<ElementId>, value: Option<T>, groups: Vec<SelectGroup<T>>) -> Self {
        Self {
            id: id.into(),
            value,
            groups,
            placeholder: SharedString::new_static("Select…"),
            size: SelectSize::Md,
            variant: SelectVariant::Field,
            disabled: false,
            searchable: None,
            menu_min_width: None,
            icon: None,
            initially_open: false,
            handle: None,
            on_change: None,
        }
    }

    /// Share the open state with the owner.
    pub fn track(mut self, handle: &SelectHandle) -> Self {
        self.handle = Some(handle.clone());
        self
    }

    /// Opens the menu the first time the select is drawn (galleries and tests).
    pub fn initially_open(mut self) -> Self {
        self.initially_open = true;
        self
    }

    /// The options without groups.
    pub fn options(id: impl Into<ElementId>, value: Option<T>, options: Vec<SelectOption<T>>) -> Self {
        Self::new(id, value, vec![SelectGroup::new("", options)])
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn size(mut self, size: SelectSize) -> Self {
        self.size = size;
        self
    }

    pub fn variant(mut self, variant: SelectVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// A filter row above the options (default: past 10 options).
    pub fn searchable(mut self, searchable: bool) -> Self {
        self.searchable = Some(searchable);
        self
    }

    /// The menu is at least the trigger's width; this widens it for a narrow trigger.
    pub fn menu_min_width(mut self, width: Pixels) -> Self {
        self.menu_min_width = Some(width);
        self
    }

    /// A leading glyph in the trigger (`text-text-muted`).
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&T, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

/// The visible options, flattened: (group index, option index) per row of the keyboard order.
struct Visible {
    groups: Vec<(usize, Vec<usize>)>,
    flat: Vec<(usize, usize)>,
}

fn visible<T>(groups: &[SelectGroup<T>], query: &str) -> Visible {
    let query = query.trim().to_lowercase();
    let matches = |option: &SelectOption<T>| {
        query.is_empty()
            || option.label.to_lowercase().contains(&query)
            || option
                .hint
                .as_ref()
                .is_some_and(|hint| hint.to_lowercase().contains(&query))
    };
    let mut out = Visible {
        groups: Vec::new(),
        flat: Vec::new(),
    };
    for (g, group) in groups.iter().enumerate() {
        let options: Vec<usize> = group
            .options
            .iter()
            .enumerate()
            .filter(|(_, option)| matches(option))
            .map(|(o, _)| o)
            .collect();
        if !options.is_empty() {
            out.flat.extend(options.iter().map(|&o| (g, o)));
            out.groups.push((g, options));
        }
    }
    out
}

/// The first enabled row from `start` going `step` (±1).
fn enabled_from<T>(groups: &[SelectGroup<T>], flat: &[(usize, usize)], start: isize, step: isize) -> Option<usize> {
    let mut i = start;
    while i >= 0 && (i as usize) < flat.len() {
        let (g, o) = flat[i as usize];
        if !groups[g].options[o].disabled {
            return Some(i as usize);
        }
        i += step;
    }
    None
}

/// Type-to-jump: `buffer` is what was typed within 600 ms of each other.
fn typeahead<T>(groups: &[SelectGroup<T>], flat: &[(usize, usize)], active: Option<usize>, buffer: &str) -> Option<usize> {
    let first = buffer.chars().next()?;
    let same = buffer.chars().all(|c| c == first);
    let needle: String = if same { first.to_string() } else { buffer.to_owned() };
    let from = match (same, active) {
        (true, Some(active)) => active + 1,
        (false, Some(active)) => active,
        _ => 0,
    };
    (0..flat.len())
        .map(|k| (from + k) % flat.len())
        .find(|&i| {
            let (g, o) = flat[i];
            let option = &groups[g].options[o];
            !option.disabled && option.label.to_lowercase().starts_with(&needle)
        })
}

impl<T: Clone + PartialEq + 'static> RenderOnce for Select<T> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.id.clone();
        let focus = focus_handle(window, cx, (id.clone(), "focus"), !self.disabled);
        let initially_open = self.initially_open && !self.disabled;
        let state: Entity<SelectState> = match &self.handle {
            Some(handle) => handle.0.clone(),
            None => window.use_keyed_state((id.clone(), "select"), cx, |_, _| SelectState::new(initially_open)),
        };
        let view = window.current_view();
        if state.read(cx).view != Some(view) {
            state.update(cx, |state, _| state.view = Some(view));
        }
        if initially_open && state.read(cx).open && state.read(cx).active.is_none() && state.read(cx).trigger.size.width == px(0.) {
            open_menu(&state, &self.groups, self.value.as_ref(), cx);
        }
        let total: usize = self.groups.iter().map(|group| group.options.len()).sum();
        let searchable = self.searchable.unwrap_or(total > 10);
        let groups = Rc::new(self.groups);
        // The filter row is a real text field (`decibell_text::input`), focused while the menu
        // is open: its text is the query, a change of it re-targets the first match, and a
        // closed menu forgets it and gives the focus back to the trigger. The select's key
        // handler sits on the element around both, so it still sees the keys first.
        let filter = searchable.then(|| {
            window.use_keyed_state((id.clone(), "filter"), cx, |window, cx| {
                InputState::single_line(window, cx).placeholder("Filter…")
            })
        });
        if let Some(filter) = &filter {
            let open = state.read(cx).open;
            if !open && !filter.read(cx).is_empty() {
                filter.update(cx, |filter, cx| filter.clear(cx));
            }
            if filter.read(cx).value() != state.read(cx).query {
                let typed = filter.read(cx).value().to_string();
                state.update(cx, |state, _| {
                    state.query = typed;
                    state.active = None;
                });
            }
            let filter_focus = filter.focus_handle(cx);
            if open && focus.is_focused(window) {
                let filter = filter.clone();
                window.defer(cx, move |window, cx| filter.update(cx, |filter, cx| filter.focus(window, cx)));
            } else if !open && filter_focus.is_focused(window) {
                let focus = focus.clone();
                window.defer(cx, move |window, cx| focus.focus(window, cx));
            }
        }
        let (open, query) = {
            let state = state.read(cx);
            (state.open, state.query.clone())
        };
        let shown = visible(&groups, &query);
        let flat = Rc::new(shown.flat.clone());
        let selected = self.value.as_ref().and_then(|value| {
            groups
                .iter()
                .flat_map(|group| group.options.iter())
                .find(|option| &option.value == value)
                .cloned()
        });
        let theme = cx.theme();
        let family = Family::of(theme.id);
        let font = if family == Family::Terminal {
            FontRole::Meta
        } else {
            FontRole::Sans
        };
        let size = self.size;
        let (text_size, line_height) = match size {
            SelectSize::Md => (13., 18.),
            SelectSize::Sm => (12., 16.),
        };
        let open_t = motion::transition_flag(window, cx, (id.clone(), "open-t"), open, Transition::TAILWIND);
        let focused = focus.is_focused(window) && window.last_input_was_keyboard();
        let hover = crate::interaction::Hover::new(window, cx, (id.clone(), "trigger"), Some(Transition::TAILWIND));
        let theme = cx.theme();
        let (bg, text) = match self.variant {
            SelectVariant::Field => (theme.color_bg_lighter, theme.color_text_primary),
            SelectVariant::Sunken => (theme.color_bg_darkest, theme.color_text_secondary),
        };
        let lit = (open || focused) && !self.disabled;
        let border = if lit {
            theme.color_accent
        } else {
            mix(
                theme.color_border,
                theme.color_accent.opacity(0.4),
                if self.disabled { 0. } else { hover.progress },
            )
        };
        let shadow = if lit { ring(cx) } else { Vec::new() };
        let muted = theme.color_text_muted;
        let chevron_color = if open { theme.color_accent } else { muted };
        let on_change = self.on_change.clone();

        // Keys, on the trigger, before anything else sees them (a modal's Escape, the trigger's
        // own Enter / Space click).
        let on_key = {
            let state = state.clone();
            let groups = groups.clone();
            let flat = flat.clone();
            let on_change = on_change.clone();
            let value = self.value.clone();
            let disabled = self.disabled;
            move |event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut App| {
                if disabled {
                    return;
                }
                let keystroke = &event.keystroke;
                let key = keystroke.key.as_str();
                let open = state.read(cx).open;
                if !open {
                    if matches!(key, "down" | "up" | "enter" | "space") && !keystroke.modifiers.modified() {
                        cx.stop_propagation();
                        window.prevent_default();
                        open_menu(&state, &groups, value.as_ref(), cx);
                    }
                    return;
                }
                let active = state.read(cx).active;
                let mut handled = true;
                let mut choose_now = None;
                update_state(&state, cx, |state| {
                    let move_to = |state: &mut SelectState, to: Option<usize>| {
                        if to.is_some() {
                            state.active = to;
                            state.reveal = Some(ScrollAlign::Nearest);
                        }
                    };
                    match key {
                        "down" => move_to(state, enabled_from(&groups, &flat, active.map_or(0, |a| a as isize + 1), 1)),
                        "up" => move_to(state, enabled_from(&groups, &flat, active.map_or(0, |a| a as isize - 1), -1)),
                        "home" if !searchable => move_to(state, enabled_from(&groups, &flat, 0, 1)),
                        "end" if !searchable => move_to(state, enabled_from(&groups, &flat, flat.len() as isize - 1, -1)),
                        "pagedown" | "pageup" => {
                            let step: isize = if key == "pagedown" { 1 } else { -1 };
                            let target = (active.unwrap_or(0) as isize + step * PAGE as isize)
                                .clamp(0, flat.len() as isize - 1);
                            move_to(state, enabled_from(&groups, &flat, target, -step));
                        }
                        "enter" => choose_now = active,
                        "escape" => state.close(),
                        "tab" => {
                            state.close();
                            handled = false;
                        }
                        // The filter row's own keys.
                        "backspace" if searchable => handled = false,
                        _ => {
                            let text = keystroke.key_char.as_deref().filter(|_| {
                                !keystroke.modifiers.control && !keystroke.modifiers.alt && !keystroke.modifiers.platform
                            });
                            match text {
                                Some(_) if searchable => handled = false,
                                Some(text) => {
                                    let now = Instant::now();
                                    let recent = state.typeahead_at.is_some_and(|at| now - at < TYPEAHEAD);
                                    if key == "space" && !recent {
                                        choose_now = active;
                                    } else {
                                        if !recent {
                                            state.typeahead.clear();
                                        }
                                        state.typeahead.push_str(&text.to_lowercase());
                                        state.typeahead_at = Some(now);
                                        move_to(state, typeahead(&groups, &flat, active, &state.typeahead));
                                    }
                                }
                                None => handled = false,
                            }
                        }
                    }
                });
                if let Some(index) = choose_now {
                    choose(&state, &groups, &flat, index, value.as_ref(), on_change.as_ref(), window, cx);
                }
                if handled {
                    cx.stop_propagation();
                    window.prevent_default();
                }
            }
        };

        // A filter change re-targets the first match.
        if open && searchable && state.read(cx).active.is_none() {
            let first = enabled_from(&groups, &flat, 0, 1);
            state.update(cx, |state, _| {
                state.active = first;
                state.reveal = Some(ScrollAlign::Nearest);
            });
        }

        let trigger_label = match &selected {
            Some(option) => Label::new(option.label.clone()).font(font).size(text_size).line_height(line_height).color(text).truncate(),
            None => Label::new(self.placeholder.clone()).font(font).size(text_size).line_height(line_height).color(muted).truncate(),
        };
        let chevron = if family == Family::Terminal {
            Label::new("▾").font(FontRole::Meta).size(text_size).line_height(line_height).color(chevron_color).into_any_element()
        } else {
            let px_size = if size == SelectSize::Sm { 10. } else { 12. };
            let turn = motion::transition_flag(window, cx, (id.clone(), "chevron"), open, Transition::TAILWIND);
            svg()
                .path(icon_path(IconName::SelectChevron, None))
                .size(px(px_size))
                .flex_none()
                .text_color(chevron_color)
                .with_transformation(Transformation::rotate(radians(PI * turn)))
                .into_any_element()
        };
        let _ = open_t;

        let theme = cx.theme();
        let trigger = div()
            .id(id.clone())
            .track_focus(&focus)
            .relative()
            .flex()
            .w_full()
            .min_w_0()
            .items_center()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(border)
            .bg(bg)
            .shadow(shadow)
            .map(|this| match size {
                SelectSize::Md => this.gap_2().px_3().py_2p5(),
                SelectSize::Sm => this.gap_1p5().px_2p5().py_1p5(),
            })
            .on_hover(hover.listener())
            .map(|this| {
                if self.disabled {
                    this.opacity(0.6).cursor_not_allowed()
                } else {
                    let state = state.clone();
                    let groups = groups.clone();
                    let value = self.value.clone();
                    this.cursor_pointer()
                        .on_click(move |_, _, cx| {
                            if state.read(cx).open {
                                update_state(&state, cx, |state| state.close());
                            } else {
                                open_menu(&state, &groups, value.as_ref(), cx);
                            }
                        })
                }
            })
            .when_some(self.icon, |this, icon| this.child(Icon::new(icon).size(px(14.)).color(muted)))
            .when_some(selected.as_ref().and_then(|option| option.icon), |this, icon| this.child(select_icon(icon, text)))
            .child(div().flex_1().min_w_0().child(trigger_label))
            .child(chevron);

        let menu = open.then(|| {
            render_menu(MenuArgs {
                id: id.clone(),
                state: state.clone(),
                groups: groups.clone(),
                shown,
                flat: flat.clone(),
                value: self.value.clone(),
                on_change,
                size,
                family,
                font,
                searchable,
                filter: filter.clone(),
                menu_min_width: self.menu_min_width,
            }, window, cx)
        });

        div()
            .relative()
            .w_full()
            .min_w_0()
            .when(!self.disabled, |this| this.capture_key_down(on_key))
            .child(trigger)
            .child({
                // Records the trigger's border box for the menu's width and side.
                let state = state.clone();
                canvas(
                    move |bounds, _, cx| state.update(cx, |state, _| state.trigger = bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
            })
            .children(menu)
    }
}

fn open_menu<T: Clone + PartialEq>(state: &Entity<SelectState>, groups: &[SelectGroup<T>], value: Option<&T>, cx: &mut App) {
    let all = visible(groups, "").flat;
    let selected = value.and_then(|value| {
        all.iter()
            .position(|&(g, o)| &groups[g].options[o].value == value && !groups[g].options[o].disabled)
    });
    let active = selected.or_else(|| enabled_from(groups, &all, 0, 1));
    update_state(state, cx, |state| {
        state.open = true;
        state.query.clear();
        state.active = active;
        state.reveal = Some(ScrollAlign::Center);
    });
}

#[allow(clippy::too_many_arguments)]
fn choose<T: Clone + PartialEq>(
    state: &Entity<SelectState>,
    groups: &[SelectGroup<T>],
    flat: &[(usize, usize)],
    index: usize,
    value: Option<&T>,
    on_change: Option<&ChangeHandler<T>>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(&(g, o)) = flat.get(index) else {
        return;
    };
    let option = &groups[g].options[o];
    if option.disabled {
        return;
    }
    update_state(state, cx, |state| state.close());
    if value != Some(&option.value)
        && let Some(handler) = on_change
    {
        handler(&option.value, window, cx);
    }
}

fn select_icon(icon: SelectIcon, color: Hsla) -> AnyElement {
    match icon {
        SelectIcon::Icon(name) => Icon::new(name).size(px(16.)).color(color).into_any_element(),
        SelectIcon::Dot(dot) => div().size(px(10.)).flex_none().rounded_full().bg(dot).into_any_element(),
    }
}

struct MenuArgs<T: Clone + PartialEq + 'static> {
    id: ElementId,
    state: Entity<SelectState>,
    groups: Rc<Vec<SelectGroup<T>>>,
    shown: Visible,
    flat: Rc<Vec<(usize, usize)>>,
    value: Option<T>,
    on_change: Option<ChangeHandler<T>>,
    size: SelectSize,
    family: Family,
    font: FontRole,
    searchable: bool,
    filter: Option<Entity<InputState>>,
    menu_min_width: Option<Pixels>,
}

/// A row's height, from its classes (`py-2` + the 13 px line, + an 11 px hint line).
fn option_height(size: SelectSize, hint: bool) -> f32 {
    let base = match size {
        SelectSize::Md => 16. + 19.5,
        SelectSize::Sm => 12. + 18.,
    };
    if hint { base + 16.5 } else { base }
}

fn render_menu<T: Clone + PartialEq + 'static>(args: MenuArgs<T>, window: &mut Window, cx: &mut App) -> AnyElement {
    let MenuArgs { id, state, groups, shown, flat, value, on_change, size, family, font, searchable, filter, menu_min_width } = args;

    // Geometry: every row's height is fixed by its classes, so the menu's natural height (and
    // where the active row is) is known before layout.
    let mut content = 8.; // the list's p-1
    let mut active_top = None;
    let (active, reveal) = {
        let state = state.read(cx);
        (state.active, state.reveal)
    };
    let mut index = 0;
    for (g, options) in &shown.groups {
        if !groups[*g].label.is_empty() {
            content += GROUP_LABEL_H;
        }
        for &o in options {
            let height = option_height(size, groups[*g].options[o].hint.is_some());
            if Some(index) == active {
                active_top = Some((content - 4., height));
            }
            content += height;
            index += 1;
        }
    }
    if shown.flat.is_empty() {
        content += EMPTY_H;
    }
    let natural = (2. + if searchable { SEARCH_ROW_H } else { 0. } + content).min(MAX_MENU_H);

    let (trigger, scroll) = {
        let state = state.read(cx);
        (state.trigger, state.scroll.clone())
    };
    // The side and the height come from where the trigger was last frame (the menu itself is
    // placed against this frame's trigger); before the trigger has been laid out, below.
    let visible_window = visible_window_bounds(window, cx);
    let known = trigger.size.width > px(0.);
    let below = if known {
        f32::from(visible_window.bottom() - trigger.bottom()) - GAP - MARGIN
    } else {
        MAX_MENU_H
    };
    let above = f32::from(trigger.top() - visible_window.top()) - GAP - MARGIN;
    let side = if below >= natural || below >= above { MenuSide::Below } else { MenuSide::Above };
    let width = f32::from(trigger.size.width).max(menu_min_width.map_or(0., f32::from));
    let max_height = MAX_MENU_H.min(if side == MenuSide::Below { below } else { above });
    let height = natural.min(max_height).max(0.);

    // Keep the active row in view: centred on open, nearest after a key (never for the pointer).
    if let (Some(align), Some((row_top, row_height))) = (reveal, active_top) {
        scroll.scroll_into_view(px(row_top), px(row_height), align);
        state.update(cx, |state, _| state.reveal = None);
    }

    let theme = cx.theme();
    let mut rows: Vec<AnyElement> = Vec::new();
    let mut index = 0;
    for (g, options) in &shown.groups {
        let group = &groups[*g];
        if !group.label.is_empty() {
            let label: SharedString = group.label.to_uppercase().into();
            rows.push(
                div()
                    .flex()
                    .px_2p5()
                    .pt_2()
                    .pb_1()
                    .when(family == Family::Terminal, |this| {
                        this.child(Label::new("// ").font(FontRole::Section).size(11.).weight(600.).color(theme.color_accent))
                    })
                    .child(Label::new(label).font(FontRole::Section).size(11.).weight(600.).color(theme.color_text_muted))
                    .into_any_element(),
            );
        }
        for &o in options {
            let option = &group.options[o];
            let row_index = index;
            index += 1;
            let is_active = Some(row_index) == active;
            let is_selected = value.as_ref() == Some(&option.value);
            rows.push(option_row(OptionRow {
                id: ElementId::NamedChild(Arc::new(id.clone()), SharedString::from(format!("opt-{row_index}"))),
                option,
                index: row_index,
                active: is_active,
                selected: is_selected,
                size,
                family,
                font,
                state: state.clone(),
                groups: groups.clone(),
                flat: flat.clone(),
                value: value.clone(),
                on_change: on_change.clone(),
            }, cx));
        }
    }
    if shown.flat.is_empty() {
        rows.push(
            div()
                .px_2p5()
                .py_2()
                .child(Label::new("No matches").font(font).size(12.).color(theme.color_text_muted))
                .into_any_element(),
        );
    }

    let filter_style = crate::TextField::input_style(font, 13., 1.2109, cx);
    let theme = cx.theme();
    let search = filter.map(|filter| {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_2p5()
            .py_2()
            .border_b_1()
            .border_color(theme.color_border_divider)
            .child(Icon::new(IconName::Search).size(px(13.)).color(theme.color_text_muted))
            .child(div().flex_1().min_w_0().child(TextInput::new(&filter, filter_style)))
    });

    let dismiss = {
        let state = state.clone();
        move |_: &mut Window, cx: &mut App| {
            if state.read(cx).open {
                update_state(&state, cx, |state| state.close());
            }
        }
    };
    // The window losing focus closes it (a change of focus, not an inactive window it opened in).
    let active_now = window.is_window_active();
    let was_active = state.read(cx).window_active;
    if was_active == Some(true) && !active_now {
        let dismiss = dismiss.clone();
        window.defer(cx, move |window, cx| dismiss(window, cx));
    } else if was_active != Some(active_now) {
        state.update(cx, |state, _| state.window_active = Some(active_now));
    }
    let theme = cx.theme();

    let menu_id = ElementId::NamedChild(Arc::new(id.clone()), SharedString::new_static("menu"));
    let menu = div()
        .id(menu_id.clone())
        .occlude()
        .relative()
        .flex()
        .flex_col()
        .flex_none()
        .map(|menu| if known { menu.w(px(width)) } else { menu.min_w(gpui::relative(1.)) })
        .h(px(height))
        .overflow_hidden()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.color_border)
        .bg(theme.color_bg_light)
        .shadow(theme.shadow_float.clone())
        .on_mouse_down_out({
            let state = state.clone();
            let dismiss = dismiss.clone();
            move |event, window, cx| {
                // The trigger toggles the menu itself.
                if !state.read(cx).trigger.contains(&event.position) {
                    dismiss(window, cx);
                }
            }
        })
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .when(theme.id == ThemeId::Nocturne, |this| {
            this.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(56.))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(NOCTURNE_SHEEN, 0.),
                        linear_color_stop(NOCTURNE_SHEEN.opacity(0.), 1.),
                    )),
            )
        })
        .children(search)
        .child(
            ScrollArea::new(ElementId::NamedChild(Arc::new(id.clone()), SharedString::new_static("list")))
                .track_scroll(&scroll)
                .without_keys()
                .flex_1()
                .min_h_0()
                .child(div().p_1().children(rows)),
        )
        .child({
            // The wheel outside the menu closes it (it would move the trigger from under it).
            let dismiss = dismiss.clone();
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, window, _| {
                    window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                        if phase == DispatchPhase::Capture && !bounds.contains(&event.position) {
                            dismiss(window, cx);
                        }
                    });
                },
            )
            .absolute()
            .inset_0()
        });

    let anim_id = ElementId::NamedChild(Arc::new(id), SharedString::new_static("menu-in"));
    let placed = Placed::new(Placement::Attached {
        above: side == MenuSide::Above,
        gap: px(GAP),
    });
    let animated: AnyElement = match family {
        Family::Lacquer => {
            let dy = if side == MenuSide::Above { 4. } else { -4. };
            menu.with_animation(
                anim_id,
                Animation::new(Duration::from_millis(140)).with_easing(motion::POP.easing()),
                move |menu, t| menu.opacity(t).top(px(dy * (1. - t))),
            )
            .into_any_element()
        }
        Family::Crisp => menu
            .with_animation(
                anim_id,
                Animation::new(Duration::from_millis(120))
                    .with_easing(CubicBezier::new(0.2, 0.9, 0.3, 1.2).easing()),
                |menu, t| menu.opacity(t.clamp(0., 1.)),
            )
            .into_any_element(),
        Family::Terminal => {
            // dselPrint: the menu printed in five steps, from the top (or from the bottom above).
            let from_bottom = side == MenuSide::Above;
            div()
                .map(|clip| if known { clip.w(px(width)) } else { clip.min_w(gpui::relative(1.)) })
                .h(px(height))
                .child(menu)
                .with_animation(
                    anim_id,
                    Animation::new(Duration::from_millis(120)).with_steps(5),
                    move |clip, t| {
                        let shown = height * t;
                        let clip = clip.overflow_hidden().h(px(shown));
                        if from_bottom {
                            clip.top(px(height - shown)).pt(px(0.)).flex().flex_col().justify_end()
                        } else {
                            clip
                        }
                    },
                )
                .into_any_element()
        }
    };
    deferred(placed.child(animated)).with_priority(3).into_any_element()
}

struct OptionRow<'a, T: Clone + PartialEq + 'static> {
    id: ElementId,
    option: &'a SelectOption<T>,
    index: usize,
    active: bool,
    selected: bool,
    size: SelectSize,
    family: Family,
    font: FontRole,
    state: Entity<SelectState>,
    groups: Rc<Vec<SelectGroup<T>>>,
    flat: Rc<Vec<(usize, usize)>>,
    value: Option<T>,
    on_change: Option<ChangeHandler<T>>,
}

fn option_row<T: Clone + PartialEq + 'static>(row: OptionRow<'_, T>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let OptionRow { id, option, index, active, selected, size, family, font, state, groups, flat, value, on_change } = row;
    let (text_size, line_height) = match size {
        SelectSize::Md => (13., 19.5),
        SelectSize::Sm => (12., 18.),
    };
    // `.dsel-option` colours, family by family.
    let mut text = theme.color_text_secondary;
    let mut bg = Hsla::transparent_black();
    if active {
        bg = theme.color_surface_hover;
        text = theme.color_text_primary;
    }
    if selected {
        text = theme.color_text_bright;
    }
    let mut check = theme.color_accent;
    let mut hint = theme.color_text_muted;
    let mut label = text;
    match family {
        Family::Lacquer if selected => {
            bg = if active { theme.color_accent_mid } else { theme.color_accent_soft };
        }
        Family::Crisp if active => {
            bg = theme.color_accent;
            text = theme.color_on_accent;
            label = text;
            check = theme.color_on_accent;
            hint = theme.color_on_accent.opacity(0.8);
        }
        Family::Terminal => {
            if active {
                bg = theme.color_accent_soft;
                text = theme.color_text_bright;
            }
            label = if selected { theme.color_accent_bright } else { text };
        }
        _ => label = text,
    }
    let check_glyph = move || {
        div()
            .flex_none()
            .size(px(14.))
            .when(selected, |this| {
                this.child(Icon::new(IconName::SelectCheck).size(px(14.)).color(check))
            })
    };
    let disabled = option.disabled;

    div()
        .id(id)
        .relative()
        .flex()
        .items_center()
        .gap_2()
        .rounded(theme.radius_md)
        .bg(bg)
        .map(|this| match size {
            SelectSize::Md => this.px_2p5().py_2(),
            SelectSize::Sm => this.px_2().py_1p5(),
        })
        .when(disabled, |this| this.opacity(0.45))
        .when(!disabled, |this| {
            let hover_state = state.clone();
            let click_state = state.clone();
            this.cursor_pointer()
                .on_mouse_move(move |_, _, cx| {
                    if hover_state.read(cx).active != Some(index) {
                        update_state(&hover_state, cx, |state| {
                            state.active = Some(index);
                            state.reveal = None;
                        });
                    }
                })
                .on_click(move |_, window, cx| {
                    choose(&click_state, &groups, &flat, index, value.as_ref(), on_change.as_ref(), window, cx)
                })
        })
        // nocturne / matinee: the selected row's 2 px bar.
        .when(family == Family::Lacquer && selected, |this| {
            this.child(
                div()
                    .absolute()
                    .left_0()
                    .top(gpui::relative(0.22))
                    .bottom(gpui::relative(0.22))
                    .w(px(2.))
                    .rounded(px(2.))
                    .bg(theme.color_accent),
            )
        })
        .when(family == Family::Terminal, |this| {
            this.child(
                div().flex_none().w(px(text_size * 0.6)).when(active, |this| {
                    this.child(Label::new("›").font(FontRole::Meta).size(text_size).line_height(line_height).color(theme.color_accent))
                }),
            )
        })
        .when(family == Family::Crisp, |this| this.child(check_glyph()))
        .when_some(option.icon, |this, icon| this.child(select_icon(icon, text)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(Label::new(option.label.clone()).font(font).size(text_size).line_height(line_height).color(label).truncate())
                .when_some(option.hint.clone(), |this, text| {
                    this.child(Label::new(text).font(font).size(11.).color(hint).truncate())
                }),
        )
        .when(family != Family::Crisp, |this| this.child(check_glyph()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Vec<SelectGroup<u32>> {
        vec![
            SelectGroup::new(
                "",
                vec![
                    SelectOption::new(1, "Bass"),
                    SelectOption::new(2, "Beta").disabled(true),
                    SelectOption::new(3, "Bell"),
                    SelectOption::new(4, "Cello").hint("strings"),
                ],
            ),
            SelectGroup::new("More", vec![SelectOption::new(5, "Brass")]),
        ]
    }

    #[test]
    fn keyboard_moves_skip_disabled_options() {
        let groups = options();
        let flat = visible(&groups, "").flat;
        assert_eq!(enabled_from(&groups, &flat, 1, 1), Some(2));
        assert_eq!(enabled_from(&groups, &flat, 1, -1), Some(0));
        assert_eq!(enabled_from(&groups, &flat, 5, 1), None);
    }

    #[test]
    fn typeahead_cycles_and_narrows() {
        let groups = options();
        let flat = visible(&groups, "").flat;
        // "b" from Bass: the next b (Beta is disabled) is Bell, then Brass, then back to Bass.
        assert_eq!(typeahead(&groups, &flat, Some(0), "b"), Some(2));
        assert_eq!(typeahead(&groups, &flat, Some(2), "bb"), Some(4));
        assert_eq!(typeahead(&groups, &flat, Some(4), "bbb"), Some(0));
        // "br" narrows to Brass.
        assert_eq!(typeahead(&groups, &flat, Some(0), "br"), Some(4));
        assert_eq!(typeahead(&groups, &flat, None, "z"), None);
    }

    #[test]
    fn the_filter_matches_labels_and_hints() {
        let groups = options();
        assert_eq!(visible(&groups, "STR").flat, vec![(0, 3)]);
        assert_eq!(visible(&groups, "br").groups.len(), 1);
        assert!(visible(&groups, "nothing").flat.is_empty());
    }
}
