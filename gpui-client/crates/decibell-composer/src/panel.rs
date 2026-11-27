//! "Rich text mode" (`RichComposer.tsx`): the button beside the emoji picker opens a small menu
//! (Code, Math), and each opens a floating panel above the card with a real text area. Insert
//! serialises to the marker syntax a user could type (```` ```lang ```` fences, `$$…$$`), appends
//! it to the draft on a line of its own, and the live preview strip shows the result.
//!
//! Code mode's keys: Tab / Shift+Tab indent / outdent (line-wise over a selection that spans
//! lines; two spaces), Enter keeps the current line's indent, Ctrl+Enter inserts, Escape cancels.
//! Math mode: Ctrl+Enter inserts, Escape cancels; the formula renders live under the field.
//!
//! The code area doesn't wrap (`wrap="off"`): it scrolls sideways (the wheel's horizontal
//! deltas, Shift with the wheel), following the caret, and colours the tokens as they are typed
//! (the renderer's transparent `<textarea>` over a `highlightNodes` `<pre>`: here the field's own
//! text is styled, `decibell_text::code_colorizer`, the code blocks' palette). Its horizontal
//! overflow has no scrollbar drawn (the renderer's textarea shows Chromium's).

use decibell_text::input::{InputEvent, InputState, InputStyle, Selection, TextArea};
use decibell_text::{Engine, Formula, MathBlock, Wrapping};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, ScrollArea, ScrollHandle, Select, SelectOption, SelectSize, SelectVariant, TextField};
use gpui::Focusable as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Element, ElementId, Entity, GlobalElementId, InspectorElementId, IntoElement, KeyDownEvent,
    LayoutId, Pixels, Render, SharedString, Style, Subscription, WeakEntity, Window, div, point, prelude::*, px,
};

use crate::Composer;
use crate::ink::Ink;

/// `LANGS`: the registered grammars by their shortest alias, with their proper names.
pub const LANGS: &[(&str, &str)] = &[
    ("plain", "Plain text"),
    ("js", "JavaScript"),
    ("ts", "TypeScript"),
    ("python", "Python"),
    ("rust", "Rust"),
    ("c", "C"),
    ("cpp", "C++"),
    ("csharp", "C#"),
    ("java", "Java"),
    ("kotlin", "Kotlin"),
    ("swift", "Swift"),
    ("go", "Go"),
    ("php", "PHP"),
    ("ruby", "Ruby"),
    ("bash", "Bash"),
    ("powershell", "PowerShell"),
    ("sql", "SQL"),
    ("json", "JSON"),
    ("yaml", "YAML"),
    ("toml", "TOML"),
    ("html", "HTML"),
    ("css", "CSS"),
    ("scss", "SCSS"),
    ("markdown", "Markdown"),
    ("dockerfile", "Dockerfile"),
    ("diff", "Diff"),
    ("lua", "Lua"),
];

const INDENT: &str = "  ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnippetKind {
    Code,
    Math,
}

/// The snippet that `insert` appends (`insert()`): ```` ```lang\nbody\n``` ```` with "plain"
/// giving no tag and the body's trailing line breaks dropped; `$$tex$$` with the source trimmed.
/// `None` when there is nothing to insert.
pub fn snippet(kind: SnippetKind, text: &str, lang: &str) -> Option<String> {
    match kind {
        SnippetKind::Code => {
            let body = text.trim_end_matches('\n');
            if body.trim().is_empty() {
                return None;
            }
            Some(format!("```{}\n{body}\n```", if lang == "plain" { "" } else { lang }))
        }
        SnippetKind::Math => {
            let tex = text.trim();
            (!tex.is_empty()).then(|| format!("$${tex}$$"))
        }
    }
}

/// Tab / Shift+Tab over the lines a selection touches (`onCodeKeyDown`'s line-wise branch): the
/// new text and selection.
pub fn indent_lines(value: &str, from: usize, to: usize, outdent: bool) -> (String, usize, usize) {
    let block_start = value[..from].rfind('\n').map_or(0, |at| at + 1);
    let head = &value[..block_start];
    let tail = &value[to..];
    let mut delta_first: isize = 0;
    let mut delta_total: isize = 0;
    let lines: Vec<String> = value[block_start..to]
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            if outdent {
                let cut = line.bytes().take(2).take_while(|b| *b == b' ').count();
                if index == 0 {
                    delta_first = -(cut as isize);
                }
                delta_total -= cut as isize;
                line[cut..].to_string()
            } else {
                if index == 0 {
                    delta_first = INDENT.len() as isize;
                }
                delta_total += INDENT.len() as isize;
                format!("{INDENT}{line}")
            }
        })
        .collect();
    let text = format!("{head}{}{tail}", lines.join("\n"));
    let start = (from as isize + delta_first).max(block_start as isize) as usize;
    let end = (to as isize + delta_total).max(block_start as isize) as usize;
    (text, start, end)
}

/// The code area's padding (`p-2.5`).
const CODE_PAD: f32 = 10.;

/// Scrolls the code area sideways so the caret (field coordinates) is inside it with the padding
/// around it, as a textarea keeps its caret in view. Whether it moved.
fn reveal_caret_x(scroll: &gpui::ScrollHandle, caret: gpui::Bounds<gpui::Pixels>) -> bool {
    let view = f32::from(scroll.bounds().size.width);
    if view <= 0. {
        return false;
    }
    let offset = scroll.offset();
    let scrolled = -f32::from(offset.x);
    let (left, right) = (f32::from(caret.left()) + CODE_PAD, f32::from(caret.right()) + CODE_PAD);
    let target = if left - CODE_PAD < scrolled {
        (left - CODE_PAD).max(0.)
    } else if right + CODE_PAD > scrolled + view {
        right + CODE_PAD - view
    } else {
        return false;
    };
    scroll.set_offset(point(px(-target), offset.y));
    true
}

/// The code blocks' palette (`.rich-codeblock .hljs-*`) on the code area's text.
fn code_palette(cx: &mut App) -> decibell_text::CodeStyle {
    let text = TextField::input_style(FontRole::Mono, 12.5, 1.5, cx).text;
    let theme = cx.theme();
    let family = theme.font(FontRole::Mono);
    let (keyword, title, string, number, comment, deletion, operator) = (
        theme.color_accent_bright,
        theme.color_accent,
        theme.color_success,
        theme.color_warning,
        theme.color_text_muted,
        theme.color_error,
        theme.color_text_secondary,
    );
    let italic = Engine::global(cx).face(&decibell_text::FontSpec { family, weight: 400, italic: true });
    decibell_text::CodeStyle { text, italic, keyword, title, string, number, comment, deletion, operator }
}

/// The code or math panel.
pub struct SnippetPanel {
    kind: SnippetKind,
    composer: WeakEntity<Composer>,
    field: Entity<InputState>,
    scroll: ScrollHandle,
    /// The code area's sideways scroll (it doesn't wrap).
    scroll_x: gpui::ScrollHandle,
    lang: SharedString,
    _subscriptions: Vec<Subscription>,
}

impl SnippetPanel {
    pub fn new(kind: SnippetKind, composer: WeakEntity<Composer>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = match kind {
            SnippetKind::Code => "// paste or type code — Tab indents, Ctrl+Enter inserts",
            SnippetKind::Math => "\\int_0^1 x^2\\,dx — Ctrl+Enter inserts",
        };
        let field = cx.new(|cx| InputState::multi_line(window, cx).placeholder(placeholder));
        field.update(cx, |field, _| {
            // The panel handles these keys itself (they propagate to its listener).
            field.pass_keys(move |keystroke| {
                let key = keystroke.key.as_str();
                let m = keystroke.modifiers;
                match kind {
                    SnippetKind::Code => matches!(key, "tab" | "enter" | "escape") && !m.alt,
                    SnippetKind::Math => key == "escape" || (key == "enter" && (m.control || m.platform)),
                }
            });
        });
        let scroll = ScrollHandle::new();
        let scroll_x = gpui::ScrollHandle::new();
        let reveal = scroll.clone();
        let reveal_x = scroll_x.clone();
        // The hook runs inside the field's update, which may run inside this panel's own (Tab):
        // the panel is told to draw again by id, never updated from here.
        let panel = cx.entity_id();
        field.update(cx, |field, _| {
            field.on_reveal_caret(move |caret, cx| {
                reveal.scroll_into_view(caret.top(), caret.size.height, decibell_ui::ScrollAlign::Nearest);
                if kind == SnippetKind::Code && reveal_caret_x(&reveal_x, caret) {
                    cx.notify(panel);
                }
            })
        });
        // `autoFocus`.
        let focus = field.clone();
        window.defer(cx, move |window, cx| focus.update(cx, |f, cx| f.focus(window, cx)));
        let subscriptions = vec![cx.subscribe(&field, |_, _, event: &InputEvent, cx| {
            if *event == InputEvent::Change {
                cx.notify();
            }
        })];
        Self { kind, composer, field, scroll, scroll_x, lang: "plain".into(), _subscriptions: subscriptions }
    }

    pub fn kind(&self) -> SnippetKind {
        self.kind
    }

    pub fn field(&self) -> &Entity<InputState> {
        &self.field
    }

    /// The code area's sideways scroll: (how far it is scrolled, its visible width), px.
    pub fn sideways(&self) -> (f32, f32) {
        (-f32::from(self.scroll_x.offset().x), f32::from(self.scroll_x.bounds().size.width))
    }

    pub fn set_lang(&mut self, lang: &str, cx: &mut Context<Self>) {
        self.lang = SharedString::from(lang.to_string());
        if self.kind == SnippetKind::Code {
            let colorizer = decibell_text::code_colorizer(lang, code_palette(cx));
            self.field.update(cx, |field, cx| field.set_colorizer(colorizer, cx));
        }
        cx.notify();
    }

    fn snippet(&self, cx: &App) -> Option<String> {
        snippet(self.kind, self.field.read(cx).value(), &self.lang)
    }

    pub fn insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snippet) = self.snippet(cx) else { return };
        if let Some(composer) = self.composer.upgrade() {
            composer.update(cx, |composer, cx| composer.insert_snippet(&snippet, window, cx));
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(composer) = self.composer.upgrade() {
            composer.update(cx, |composer, cx| composer.close_snippet(window, cx));
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;
        match keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                self.cancel(window, cx);
            }
            "enter" if m.control || m.platform => {
                cx.stop_propagation();
                self.insert(window, cx);
            }
            "enter" if self.kind == SnippetKind::Code => {
                // Auto-indent: carry the current line's leading white space.
                cx.stop_propagation();
                self.field.update(cx, |field, cx| {
                    let value = field.value();
                    let caret = field.selection().head.min(field.selection().anchor);
                    let line_start = value[..caret].rfind('\n').map_or(0, |at| at + 1);
                    let indent: String = value[line_start..caret].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                    field.insert_text(&format!("\n{indent}"), cx);
                });
            }
            "tab" if self.kind == SnippetKind::Code => {
                cx.stop_propagation();
                self.tab(m.shift, cx);
            }
            _ => {}
        }
    }

    /// Tab / Shift+Tab in the code area: two spaces at the caret, or the touched lines indented or
    /// outdented. Reached as a key or as the window's focus actions, which the app binds to Tab
    /// and dispatches before key listeners (the panel takes them while the code area has the focus).
    fn tab(&mut self, outdent: bool, cx: &mut Context<Self>) {
        self.field.update(cx, |field, cx| {
            let selection = field.selection();
            let (from, to) = (selection.anchor.min(selection.head), selection.anchor.max(selection.head));
            let value = field.value().to_string();
            let multiline = value[from..to].contains('\n');
            if !outdent && !multiline {
                field.insert_text(INDENT, cx);
                return;
            }
            let (text, start, end) = indent_lines(&value, from, to, outdent);
            field.set_value(&text, cx);
            field.select(Selection { anchor: start, head: end }, cx);
        });
    }
}

impl Render for SnippetPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = Ink::of(cx);
        let shadow = cx.theme().shadow_float.clone();
        let focused = self.field.read(cx).focus_handle(cx).is_focused(window);
        let empty = self.field.read(cx).value().trim().is_empty();
        let kind = self.kind;
        let title = match kind {
            SnippetKind::Code => "CODE BLOCK",
            SnippetKind::Math => "MATH (LATEX)",
        };
        let lang_select = (kind == SnippetKind::Code).then(|| {
            let weak = cx.weak_entity();
            Select::options("snippet-lang", Some(self.lang.to_string()), LANGS.iter().map(|(v, l)| SelectOption::new(v.to_string(), *l)).collect())
                .size(SelectSize::Sm)
                .variant(SelectVariant::Sunken)
                .menu_min_width(px(180.))
                .on_change(move |value: &String, _, cx| {
                    let _ = weak.update(cx, |this, cx| this.set_lang(value, cx));
                })
        });
        let close = div()
            .id("snippet-close")
            .group("snippet-close")
            .size_6()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(ink.radius_sm)
            .cursor_pointer()
            .hover(|s| s.bg(ink.color_surface_hover))
            .tooltip(decibell_ui::title_tooltip("Cancel"))
            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx)))
            .child(Icon::new(IconName::ComposerX).size(px(12.)).color(ink.color_text_muted).group_hover("snippet-close", ink.color_text_secondary));

        let mono = |size: f32, wrapping: Wrapping, cx: &mut App| -> InputStyle {
            let mut style = TextField::input_style(FontRole::Mono, size, 1.5, cx);
            style.wrapping = wrapping;
            style
        };
        let field: AnyElement = match kind {
            SnippetKind::Code => div()
                .relative()
                .h(px(176.))
                .w_full()
                .overflow_hidden()
                .rounded(ink.radius_md)
                .border_1()
                .border_color(if focused { ink.color_accent } else { ink.color_border })
                .bg(ink.color_bg_darkest)
                .child(
                    ScrollArea::new("snippet-code").track_scroll(&self.scroll).size_full().child(
                        // `wrap="off"`: the lines run on and the area scrolls sideways.
                        div()
                            .id("snippet-code-x")
                            .w_full()
                            .flex()
                            .overflow_x_scroll()
                            // The wheel's vertical turn stays the outer area's; Shift turns it
                            // sideways (Chromium's textarea).
                            .restrict_scroll_to_axis()
                            .track_scroll(&self.scroll_x)
                            .on_scroll(cx.listener(|this, event: &gpui::ScrollEvent, window, cx| {
                                if !event.modifiers.shift {
                                    return;
                                }
                                let delta = event.delta.pixel_delta(window.line_height());
                                let turn = if delta.x == px(0.) { delta.y } else { delta.x };
                                let max = this.scroll_x.max_offset().x;
                                let mut offset = this.scroll_x.offset();
                                offset.x = (offset.x + turn).clamp(-max, px(0.));
                                this.scroll_x.set_offset(offset);
                                cx.stop_propagation();
                                cx.notify();
                            }))
                            // As wide as the longest line, at least the area.
                            .child(div().flex_none().min_w_full().p(px(CODE_PAD)).child(TextArea::new(&self.field, mono(12.5, Wrapping::PRE, cx)))),
                    ),
                )
                .into_any_element(),
            SnippetKind::Math => div()
                .w_full()
                .rounded(ink.radius_md)
                .border_1()
                .border_color(if focused { ink.color_accent } else { ink.color_border })
                .bg(ink.color_bg_darkest)
                .p(px(10.))
                // `rows={2}`: two lines high at least.
                .min_h(px(10. + 2. * 21. + 10. + 2.))
                .child(TextArea::new(&self.field, mono(14., Wrapping::PRE_WRAP, cx)))
                .into_any_element(),
        };
        let math_preview = (kind == SnippetKind::Math && !empty).then(|| {
            let tex = self.field.read(cx).value().trim().to_string();
            let engine = Engine::global(cx);
            let style = crate::preview::chat_style(window.scale_factor(), cx);
            // KaTeX at 1.21 × the box's 16 px.
            let formula: AnyElement = match engine.formula(&tex, true, 16., window.scale_factor()) {
                Formula::Math(math) => MathBlock::new(math, style.math).into_any_element(),
                Formula::Error(_) | Formula::Internal => Label::new(tex).font(FontRole::Mono).size(16.).color(ink.color_error).into_any_element(),
            };
            div()
                .mt_2()
                .rounded(ink.radius_md)
                .border_1()
                .border_color(ink.color_border_divider)
                .bg(ink.color_bg_mid)
                .px_3()
                .py(px(10.))
                .overflow_hidden()
                .child(formula)
        });
        let hint = match kind {
            SnippetKind::Code => "Tab indents · Shift+Tab outdents · Ctrl+Enter inserts",
            SnippetKind::Math => "Rendered live above · Ctrl+Enter inserts",
        };
        let cancel = div()
            .id("snippet-cancel")
            .px(px(10.))
            .py_1()
            .rounded(ink.radius_md)
            .cursor_pointer()
            .hover(|s| s.bg(ink.color_surface_hover))
            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx)))
            .child(Label::new("Cancel").size(12.).color(ink.color_text_muted).nowrap());
        let insert = div()
            .id("snippet-insert")
            .px_3()
            .py_1()
            .rounded(ink.radius_md)
            .bg(ink.color_accent)
            .when(empty, |this| this.opacity(0.4).cursor(gpui::CursorStyle::OperationNotAllowed))
            .when(!empty, |this| this.cursor_pointer().hover(|s| s.bg(ink.color_accent_hover)))
            .on_click(cx.listener(|this, _, window, cx| this.insert(window, cx)))
            .child(Label::new("Insert").size(12.).weight(500.).color(ink.color_on_accent).nowrap());
        div()
            .id("snippet-panel")
            .occlude()
            .w_full()
            .rounded(ink.radius_lg)
            .border_1()
            .border_color(ink.color_border)
            .bg(ink.color_bg_light)
            .p_3()
            .shadow(shadow)
            .on_key_down(cx.listener(Self::key_down))
            .when(kind == SnippetKind::Code, |this| {
                this.on_action(cx.listener(|this, _: &decibell_ui::FocusNext, _, cx| this.tab(false, cx)))
                    .on_action(cx.listener(|this, _: &decibell_ui::FocusPrev, _, cx| this.tab(true, cx)))
            })
            .child(
                div()
                    .mb_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Label::new(title).size(11.).weight(600.).tracking(0.07).color(ink.color_text_muted).nowrap())
                            .children(lang_select.map(|select| div().w(px(121.)).child(select))),
                    )
                    .child(close),
            )
            .child(field)
            .children(math_preview)
            .child(
                div()
                    .mt_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(Label::new(hint).size(10.5).color(ink.color_text_muted).nowrap())
                    .child(div().flex().items_center().gap_1p5().child(cancel).child(insert)),
            )
    }
}

/// A floating layer over the box it is put in (`absolute bottom-full mb-2 left-0 / right-0`): this
/// element fills its parent (position it in a `relative` box), lays its child out at the child's
/// own size, and moves it so the child's bottom sits `gap` above the parent's top, its left or
/// right edge on the parent's. Anchored in the same frame (no box remembered from the last one).
/// Put it in `deferred` to draw it over everything.
pub struct Above {
    gap: Pixels,
    right: bool,
    child: AnyElement,
}

impl Above {
    pub fn new(gap: Pixels, right: bool, child: impl IntoElement) -> Self {
        Self { gap, right, child: child.into_any_element() }
    }
}

impl Element for Above {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        // This element is the anchor's box; the child is laid out on its own (below), so the
        // anchor's size never squeezes it.
        let mut style = Style { position: gpui::Position::Absolute, ..Style::default() };
        style.inset = gpui::Edges::all(px(0.).into());
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, anchor: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        // Its own size: as wide as it says (a `w_full` child is the anchor's width).
        let space = gpui::size(gpui::AvailableSpace::Definite(anchor.size.width), gpui::AvailableSpace::MaxContent);
        let size = self.child.layout_as_root(space, window, cx);
        let left = if self.right { anchor.right() - size.width } else { anchor.left() };
        let top = anchor.top() - self.gap - size.height;
        self.child.prepaint_at(point(left.round(), top.round()), window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.paint(window, cx);
    }
}

/// The trigger's little menu (`absolute bottom-full right-0 mb-2 w-40 rounded-md border bg-bg-light
/// p-1 shadow-float`): Code, Math. Closed by a press outside the trigger cluster.
pub fn snippet_menu(anchor: Bounds<Pixels>, composer: WeakEntity<Composer>, cx: &mut Context<Composer>) -> impl IntoElement + use<> {
    let ink = Ink::of(cx);
    let shadow = cx.theme().shadow_float.clone();
    let item = |id: &'static str, kind: SnippetKind, label: &'static str, cx: &mut Context<Composer>| {
        let glyph: AnyElement = match kind {
            SnippetKind::Code => Icon::new(IconName::ComposerCode).size(px(14.)).color(ink.color_accent).into_any_element(),
            // `font-serif text-[14px] leading-none text-accent`: Σ.
            SnippetKind::Math => div().w(px(14.)).flex().justify_center().child(Label::new("Σ").size(14.).line_height(14.).color(ink.color_accent).nowrap()).into_any_element(),
        };
        let group = SharedString::new_static(id);
        div()
            .id(id)
            .group(group.clone())
            .flex()
            .w_full()
            .items_center()
            .gap(px(10.))
            .rounded(ink.radius_sm)
            .px(px(10.))
            .py_1p5()
            .cursor_pointer()
            .hover(|s| s.bg(ink.color_surface_hover))
            .on_click(cx.listener(move |this, _, window, cx| this.open_snippet(kind, window, cx)))
            .child(glyph)
            .child(
                div()
                    .child(div().group_hover(group.clone(), |s| s.opacity(0.)).child(Label::new(label).size(13.).color(ink.color_text_secondary).nowrap()))
                    .child(div().absolute().top_0().opacity(0.).group_hover(group, |s| s.opacity(1.)).child(Label::new(label).size(13.).color(ink.color_text_primary).nowrap()))
                    .relative(),
            )
    };
    let trigger = anchor;
    let menu = div()
        .id("snippet-menu")
        .occlude()
        .w_40()
        .rounded(ink.radius_md)
        .border_1()
        .border_color(ink.color_border)
        .bg(ink.color_bg_light)
        .p_1()
        .shadow(shadow)
        .on_mouse_down_out(move |event, window, cx| {
            if !trigger.contains(&event.position) {
                let _ = composer.update(cx, |this, cx| this.dismiss_snippet_menu(window, cx));
            }
        })
        .child(item("snippet-code", SnippetKind::Code, "Code", cx))
        .child(item("snippet-math", SnippetKind::Math, "Math", cx));
    decibell_ui::layer(2, Above::new(px(8.), true, menu))
}

/// The panel over the card (put in the card): as wide as the card, its bottom 8 px above it.
pub fn panel_layer(panel: Entity<SnippetPanel>) -> impl IntoElement + use<> {
    decibell_ui::layer(1, Above::new(px(8.), false, div().w_full().child(panel)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_serialise_as_the_renderer_does() {
        assert_eq!(snippet(SnippetKind::Code, "fn main() {}\n\n", "plain").as_deref(), Some("```\nfn main() {}\n```"));
        assert_eq!(snippet(SnippetKind::Code, "x = 1", "python").as_deref(), Some("```python\nx = 1\n```"));
        assert_eq!(snippet(SnippetKind::Code, "  \n", "rust"), None);
        assert_eq!(snippet(SnippetKind::Math, "  x^2 ", "plain").as_deref(), Some("$$x^2$$"));
        assert_eq!(snippet(SnippetKind::Math, " ", "plain"), None);
    }

    #[test]
    fn tab_indents_and_outdents_the_lines_a_selection_touches() {
        let value = "a\n  b\nc";
        // Select from inside "a" to inside "b": both lines indent; the selection moves with them.
        let (text, start, end) = indent_lines(value, 0, 4, false);
        assert_eq!(text, "  a\n    b\nc");
        assert_eq!((start, end), (2, 8));
        let (text, start, end) = indent_lines("  a\n    b\nc", 2, 8, true);
        assert_eq!(text, "a\n  b\nc");
        assert_eq!((start, end), (0, 4));
    }
}
