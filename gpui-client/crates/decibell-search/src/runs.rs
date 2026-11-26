//! [`Runs`]: a line (or wrapped text) of several styled runs, laid out at the width it is given,
//! as an inline `<span>` mix is: "Search for **gg**", a file name with its matched words marked,
//! the encrypted-channels notice with its names in bold. (`decibell_ui::Label` is one style.)

use std::rc::Rc;

use decibell_text::{Engine, Layout, PaintOptions, ParagraphBuilder, TextStyle, WhiteSpace, Wrapping};
use gpui::{
    App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId, Pixels, Style,
    Window, px, size,
};

/// Text of several runs. Wrapping text is as wide as its container; a truncated line gives way
/// down to nothing (`min-width: 0`).
pub struct Runs {
    runs: Rc<[(String, TextStyle)]>,
    base: TextStyle,
    wrapping: Wrapping,
}

impl Runs {
    pub fn new(base: TextStyle, wrapping: Wrapping) -> Self {
        Self { runs: Rc::from(Vec::new()), base, wrapping }
    }

    pub fn runs(mut self, runs: Vec<(String, TextStyle)>) -> Self {
        self.runs = runs.into();
        self
    }
}

fn build(engine: &Engine, runs: &[(String, TextStyle)], base: &TextStyle, wrapping: Wrapping, width: f32, scale: f32) -> Layout {
    let mut builder = ParagraphBuilder::new(engine, base, wrapping, scale);
    for (text, style) in runs {
        builder.text(text, style, 0);
    }
    Layout::new(Rc::new(builder.build()), width, engine)
}

impl Element for Runs {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let engine = Engine::global(cx);
        let scale = window.scale_factor();
        let (runs, base, wrapping) = (self.runs.clone(), self.base, self.wrapping);
        let id = window.request_measured_layout(Style::default(), move |known, available, _, _| {
            let wraps = matches!(wrapping.white_space, WhiteSpace::Normal | WhiteSpace::PreWrap);
            let limit = match (known.width, available.width) {
                (Some(width), _) => f32::from(width),
                (None, AvailableSpace::Definite(width)) => f32::from(width),
                (None, AvailableSpace::MinContent) => 0.,
                (None, AvailableSpace::MaxContent) => f32::INFINITY,
            };
            let layout = build(&engine, &runs, &base, wrapping, if wraps { limit } else { f32::INFINITY }, scale);
            let snap = |v: f32| (v * scale).ceil() / scale;
            let natural = size(px(snap(layout.width())), px(snap(layout.height())));
            if !wraps && wrapping.ellipsis { size(natural.width.min(px(limit)), natural.height) } else { natural }
        });
        (id, ())
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
        let width = f32::from(bounds.size.width);
        let wraps = matches!(self.wrapping.white_space, WhiteSpace::Normal | WhiteSpace::PreWrap);
        let mut layout = build(&engine, &self.runs, &self.base, self.wrapping, if wraps { width } else { f32::INFINITY }, scale);
        if !wraps && self.wrapping.ellipsis && layout.width() > width + 0.5 / scale {
            layout = build(&engine, &self.runs, &self.base, self.wrapping, width, scale);
        }
        layout.paint(bounds.origin, &PaintOptions::default(), window, cx);
    }
}
