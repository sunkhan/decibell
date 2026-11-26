//! The elements that put a layout on screen: [`Label`] for UI text and [`TextBlock`] for a
//! layout the caller already has.

use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;

use gpui::{
    App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId,
    Pixels, SharedString, Size, Style, Window, px, size,
};

use crate::cache::KeyHasher;
use crate::style::hash_style;
use crate::{Engine, Layout, PaintOptions, ParagraphBuilder, TextStyle, WhiteSpace, Wrapping};

/// Text in one style: every label, title, name and count of the UI.
///
/// ```ignore
/// Label::new("general", style)                          // wraps at its container's width
/// Label::new(name, style).truncate()                    // one line, "…" when cut
/// Label::new(topic, style).wrapping(Wrapping::NORMAL.clamp(2))
/// ```
///
/// The layout is cached in the engine by (text, style, wrapping, width, scale), so a label
/// that does not change costs a hash per frame and never shapes again.
pub struct Label {
    text: SharedString,
    style: TextStyle,
    wrapping: Wrapping,
    inherit_color: bool,
}

impl Label {
    pub fn new(text: impl Into<SharedString>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
            wrapping: Wrapping::NORMAL,
            inherit_color: false,
        }
    }

    /// Draw in the text colour the element inherits (GPUI's text style at the label's
    /// place in the tree: a parent's `.text_color(…)`, also under `.hover(…)`) instead of
    /// the style's own.
    pub fn inherit_color(mut self) -> Self {
        self.inherit_color = true;
        self
    }

    pub fn wrapping(mut self, wrapping: Wrapping) -> Self {
        self.wrapping = wrapping;
        self
    }

    /// One line, cut with "…" (Tailwind `truncate`). Give the label's flex parent
    /// `min_w_0()` as in CSS, or the row will not shrink.
    pub fn truncate(mut self) -> Self {
        self.wrapping = Wrapping::TRUNCATE;
        self
    }

    /// One line, never cut (`whitespace-nowrap`).
    pub fn nowrap(mut self) -> Self {
        self.wrapping = Wrapping::NOWRAP;
        self
    }

    /// At most `lines` lines, the last ending in "…" (`line-clamp-N`).
    pub fn clamp(mut self, lines: u16) -> Self {
        self.wrapping = self.wrapping.clamp(lines);
        self
    }

    fn key(&self, scale: f32) -> u64 {
        label_key(&self.text, &self.style, self.wrapping, scale)
    }
}

/// The colour is not part of a label's layout: it is given when painting.
fn label_key(text: &str, style: &TextStyle, wrapping: Wrapping, scale: f32) -> u64 {
    let mut hasher = KeyHasher::default();
    text.hash(&mut hasher);
    hash_style(&style.color(gpui::Hsla::default()), &mut hasher);
    wrapping.hash(&mut hasher);
    scale.to_bits().hash(&mut hasher);
    hasher.finish()
}

/// The cached layout of a label at a width.
fn label_layout(engine: &Engine, key: u64, text: &str, style: &TextStyle, wrapping: Wrapping, width: f32, scale: f32) -> Rc<Layout> {
    engine.labels.layout(key, width, scale, || {
        let paragraph = engine.labels.paragraph(key, || {
            let mut builder = ParagraphBuilder::new(engine, style, wrapping, scale);
            builder.text(text, style, 0);
            builder.build()
        });
        Layout::new(paragraph, width, engine)
    })
}

impl Engine {
    /// The layout a [`Label`] with this text and style has at `width` px: for code that
    /// needs a label's size before it draws one.
    pub fn label_layout(&self, text: &str, style: &TextStyle, wrapping: Wrapping, width: f32, scale: f32) -> Rc<Layout> {
        label_layout(self, label_key(text, style, wrapping, scale), text, style, wrapping, width, scale)
    }
}

/// A box of whole device pixels around a size, so that layout rounding cannot make the box
/// narrower than its text.
fn snapped(width: f32, height: f32, scale: f32) -> Size<Pixels> {
    size(px((width * scale).ceil() / scale), px((height * scale).ceil() / scale))
}

impl Element for Label {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let engine = Engine::global(cx);
        let scale = window.scale_factor();
        let key = self.key(scale);
        let (text, style, wrapping) = (self.text.clone(), self.style, self.wrapping);
        let layout_id = window.request_measured_layout(Style::default(), move |known, available, _, _| {
            // A label that does not wrap has one size; one that wraps is as wide as it is
            // allowed to be and as high as that makes it.
            let wraps = matches!(wrapping.white_space, WhiteSpace::Normal | WhiteSpace::PreWrap);
            let limit = match (known.width, available.width) {
                (Some(width), _) => f32::from(width),
                (None, AvailableSpace::Definite(width)) => f32::from(width),
                (None, AvailableSpace::MinContent) => 0.,
                (None, AvailableSpace::MaxContent) => f32::INFINITY,
            };
            let width = if wraps { limit } else { f32::INFINITY };
            let layout = label_layout(&engine, key, &text, &style, wrapping, width, scale);
            let natural = snapped(layout.width(), layout.height(), scale);
            if !wraps && wrapping.ellipsis {
                // A truncating label gives way down to nothing, like `min-width: 0`.
                return size(natural.width.min(px(limit)), natural.height);
            }
            natural
        });
        (layout_id, ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

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
        let engine = Engine::global(cx);
        let scale = window.scale_factor();
        let key = self.key(scale);
        let width = f32::from(bounds.size.width);
        let wraps = self.wrapping.wraps();
        let mut layout = label_layout(&engine, key, &self.text, &self.style, self.wrapping, if wraps { width } else { f32::INFINITY }, scale);
        if !wraps && self.wrapping.ellipsis && layout.width() > width + 0.5 / scale {
            layout = label_layout(&engine, key, &self.text, &self.style, self.wrapping, width, scale);
        }
        let color = if self.inherit_color { window.text_style().color } else { self.style.color };
        let options = PaintOptions {
            color: Some(color),
            ..PaintOptions::default()
        };
        layout.paint(bounds.origin, &options, window, cx);
    }
}

/// A layout the caller made, shown as it is: a code block's text, a paragraph a view keeps.
/// The element is exactly as large as the layout (rounded up to whole device pixels).
pub struct TextBlock {
    layout: Rc<Layout>,
    options: PaintOptions,
}

impl TextBlock {
    pub fn new(layout: Rc<Layout>) -> Self {
        Self {
            layout,
            options: PaintOptions::default(),
        }
    }

    /// The selection and hover state to paint with.
    pub fn options(mut self, options: PaintOptions) -> Self {
        self.options = options;
        self
    }
}

impl Element for TextBlock {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        let size = snapped(self.layout.width(), self.layout.height(), window.scale_factor());
        style.size.width = size.width.into();
        style.size.height = size.height.into();
        style.flex_shrink = 0.;
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.layout.paint(bounds.origin, &self.options, window, cx);
    }
}

/// A display formula (`$$…$$`) at its place: the formula starts at the element's left edge
/// (the renderer left-aligns display math) and the element is as wide as its container and
/// as high as the formula. A formula wider than the container overflows to the right:
/// [`Math::width`] against the container's width tells the caller to scroll or clip it.
pub struct MathBlock {
    math: Rc<crate::Math>,
    colors: crate::MathColors,
}

impl MathBlock {
    pub fn new(math: Rc<crate::Math>, colors: crate::MathColors) -> Self {
        Self { math, colors }
    }
}

impl Element for MathBlock {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = px((self.math.height() * window.scale_factor()).ceil() / window.scale_factor()).into();
        style.flex_shrink = 0.;
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        let baseline = gpui::point(bounds.origin.x, bounds.origin.y + px(self.math.ascent));
        self.math.paint(baseline, f32::from(bounds.size.width), &self.colors, window, cx);
    }
}
