//! A wrapping line of mixed text (the audit log's "**alice** changed the roles of **bob**"):
//! runs in their own weights, colours and fonts, inline boxes for an avatar or a role's dot,
//! laid out by `decibell_text` at the container's width (Chromium's line breaking and line
//! boxes), the layout kept per width.

use std::cell::RefCell;
use std::rc::Rc;

use decibell_text::{Engine, FontSpec, Layout, LineHeight, PaintOptions, Paragraph, ParagraphBuilder, TextStyle, Wrapping};
use decibell_ui::{ActiveTheme as _, FontRole};
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, Style, Window, fill, point, px, size,
};

/// One piece of a sentence.
#[derive(Clone, Debug, PartialEq)]
pub enum Run {
    /// Text in the sentence's own style.
    Text(String),
    /// `font-semibold text-text-primary`.
    Strong(String),
    /// `font-mono text-[12px] text-text-primary` (an invite code).
    Code(String),
    /// Text in the sentence's font in another colour (a count in `text-text-secondary`).
    Colored(String, Hsla),
    /// A box `width` × `height` px with `tag`, its bottom on the baseline (`translate-y` is the
    /// painter's): an avatar, a role's dot.
    Box { tag: u32, width: f32, height: f32 },
}

/// The style of a sentence's plain text.
#[derive(Clone, Copy)]
pub struct Base {
    pub font: FontRole,
    pub size: f32,
    pub line_height: f32,
    pub color: Hsla,
    pub strong: Hsla,
}

fn style(engine: &Engine, role: FontRole, weight: u16, size: f32, line_height: f32, color: Hsla, cx: &App) -> TextStyle {
    let theme = cx.theme();
    let face = engine.face(&FontSpec { family: theme.font(role), weight, italic: false });
    let mut style = TextStyle::new(face, size, color);
    style.line_height = LineHeight::Px(line_height);
    // The body's letter-spacing, inherited as px of the 16 px root size (see `decibell_ui::Label`).
    style.letter_spacing = theme.palette.t_ls_body * 16.;
    style
}

/// Shape `runs` (once per theme / scale: keep the result).
pub fn build(runs: &[Run], base: Base, scale: f32, cx: &mut App) -> Rc<Paragraph> {
    let engine = Engine::global(cx);
    let plain = style(&engine, base.font, 400, base.size, base.line_height, base.color, cx);
    let strong = style(&engine, base.font, 600, base.size, base.line_height, base.strong, cx);
    let code = style(&engine, FontRole::Mono, 400, 12., base.line_height, base.strong, cx);
    let mut builder = ParagraphBuilder::new(&engine, &plain, Wrapping::NORMAL, scale);
    for run in runs {
        match run {
            Run::Text(t) => {
                builder.text(t, &plain, 0);
            }
            Run::Strong(t) => {
                builder.text(t, &strong, 0);
            }
            Run::Code(t) => {
                builder.text(t, &code, 0);
            }
            Run::Colored(t, color) => {
                let colored = plain.color(*color);
                builder.text(t, &colored, 0);
            }
            Run::Box { tag, width, height } => {
                builder.inline_box(*width, *height, 0., *tag);
            }
        }
    }
    Rc::new(builder.build())
}

/// A shaped sentence and its layouts by width.
pub struct Shaped {
    pub paragraph: Rc<Paragraph>,
    layouts: RefCell<Vec<(f32, Rc<Layout>)>>,
}

impl Shaped {
    pub fn new(paragraph: Rc<Paragraph>) -> Rc<Shaped> {
        Rc::new(Shaped { paragraph, layouts: RefCell::new(Vec::new()) })
    }

    fn layout(&self, width: f32, engine: &Engine) -> Rc<Layout> {
        let mut layouts = self.layouts.borrow_mut();
        if let Some((_, layout)) = layouts.iter().find(|(w, _)| (*w - width).abs() < 0.01) {
            return layout.clone();
        }
        let layout = Rc::new(Layout::new(self.paragraph.clone(), width, engine));
        if layouts.len() >= 4 {
            layouts.remove(0);
        }
        layouts.push((width, layout.clone()));
        layout
    }
}

/// The element: the sentence at its container's width; boxes filled with elements (moved by
/// their offset) or painted as dots.
pub struct Sentence {
    shaped: Rc<Shaped>,
    elements: Vec<(u32, AnyElement, f32)>,
    dots: Vec<(u32, Hsla)>,
    layout: Option<Rc<Layout>>,
}

impl Sentence {
    pub fn new(shaped: &Rc<Shaped>) -> Self {
        Sentence { shaped: shaped.clone(), elements: Vec::new(), dots: Vec::new(), layout: None }
    }

    /// Fill box `tag` with `element`, moved down by `dy` px.
    pub fn element(mut self, tag: u32, element: impl IntoElement, dy: f32) -> Self {
        self.elements.push((tag, element.into_any_element(), dy));
        self
    }

    /// Paint box `tag` as a dot of `color` (a role's).
    pub fn dot(mut self, tag: u32, color: Hsla) -> Self {
        self.dots.push((tag, color));
        self
    }
}

impl Element for Sentence {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let engine = Engine::global(cx);
        let shaped = self.shaped.clone();
        let scale = window.scale_factor();
        let id = window.request_measured_layout(Style::default(), move |known, available, _, _| {
            let limit = match (known.width, available.width) {
                (Some(w), _) => f32::from(w),
                (None, AvailableSpace::Definite(w)) => f32::from(w),
                (None, AvailableSpace::MinContent) => 0.,
                (None, AvailableSpace::MaxContent) => f32::INFINITY,
            };
            let layout = shaped.layout(limit, &engine);
            let snap = |v: f32| (v * scale).ceil() / scale;
            size(px(snap(layout.width())), px(snap(layout.height())))
        });
        (id, ())
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
        let engine = Engine::global(cx);
        // At the element's width (its widest line, snapped up): the same line breaks as measured.
        let layout = self.shaped.layout(f32::from(bounds.size.width), &engine);
        for (tag, element, dy) in &mut self.elements {
            if let Some((_, [x, y, w, h])) = layout.boxes().find(|(t, _)| t == tag) {
                element.layout_as_root(size(AvailableSpace::Definite(px(w)), AvailableSpace::Definite(px(h))), window, cx);
                element.prepaint_at(bounds.origin + point(px(x), px(y + *dy)), window, cx);
            }
        }
        self.layout = Some(layout);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(layout) = self.layout.clone() else { return };
        layout.paint(bounds.origin, &PaintOptions::default(), window, cx);
        for (tag, color) in &self.dots {
            if let Some((_, [x, y, w, h])) = layout.boxes().find(|(t, _)| t == tag) {
                // An 8 px dot, `align-middle`: centred on the box.
                let d = w.min(h);
                let rect = Bounds { origin: bounds.origin + point(px(x), px(y + (h - d) / 2.)), size: size(px(d), px(d)) };
                window.paint_quad(fill(rect, *color).corner_radii(px(d / 2.)));
            }
        }
        for (_, element, _) in &mut self.elements {
            element.paint(window, cx);
        }
    }
}
