//! One wrapping paragraph of several styled runs: CSS's `Leave <span class="font-semibold
//! text-text-primary">name</span>? …`. The kit's [`crate::Label`] takes one style; this lays a
//! `decibell_text` paragraph out at the width the element is given, as `Label` does.
//!
//! ```ignore
//! Runs::new(vec![
//!     ("Leave ".into(), run_style(FontRole::Sans, 13., 400., secondary, cx)),
//!     (name.to_string(), run_style(FontRole::Sans, 13., 600., primary, cx)),
//!     ("? You will need a new invite to rejoin.".into(), run_style(FontRole::Sans, 13., 400., secondary, cx)),
//! ])
//! ```

use std::rc::Rc;

use decibell_text::{Engine, FontSpec, Layout, PaintOptions, ParagraphBuilder, TextStyle, Wrapping};
use gpui::{
    App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, LayoutId,
    Pixels, Style, Window, px, size,
};

use crate::{ActiveTheme as _, FontRole};

/// A run's style: the theme's face for `role` at `weight`, the body's letter-spacing (what text
/// inherits in the renderer), line height 1.5.
pub fn run_style(role: FontRole, size: f32, weight: f32, color: Hsla, cx: &mut App) -> TextStyle {
    let family = cx.theme().font(role);
    let tracking = cx.theme().palette.t_ls_body;
    let engine = Engine::global(cx);
    let face = engine.face(&FontSpec { family, weight: weight.round() as u16, italic: false });
    let mut style = TextStyle::new(face, size, color);
    style.letter_spacing = tracking * 16.;
    style
}

pub struct Runs {
    runs: Vec<(String, TextStyle)>,
    built: Option<Rc<decibell_text::Paragraph>>,
}

impl Runs {
    pub fn new(runs: Vec<(String, TextStyle)>) -> Self {
        Self { runs, built: None }
    }

    fn paragraph(&mut self, engine: &Engine, scale: f32) -> Rc<decibell_text::Paragraph> {
        if let Some(built) = &self.built
            && built.scale() == scale
        {
            return built.clone();
        }
        let base = self.runs.first().map(|(_, style)| *style).expect("at least one run");
        let mut builder = ParagraphBuilder::new(engine, &base, Wrapping::NORMAL, scale);
        for (i, (text, style)) in self.runs.iter().enumerate() {
            builder.text(text, style, i as u32);
        }
        let paragraph = Rc::new(builder.build());
        self.built = Some(paragraph.clone());
        paragraph
    }
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
        let paragraph = self.paragraph(&engine, scale);
        let id = window.request_measured_layout(Style::default(), move |known, available, _, _| {
            let width = match (known.width, available.width) {
                (Some(width), _) => f32::from(width),
                (None, AvailableSpace::Definite(width)) => f32::from(width),
                (None, AvailableSpace::MinContent) => 0.,
                (None, AvailableSpace::MaxContent) => f32::INFINITY,
            };
            let layout = Layout::new(paragraph.clone(), width, &engine);
            size(px((layout.width() * scale).ceil() / scale), px((layout.height() * scale).ceil() / scale))
        });
        (id, ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        let engine = Engine::global(cx);
        let paragraph = self.paragraph(&engine, window.scale_factor());
        let layout = Layout::new(paragraph, f32::from(bounds.size.width), &engine);
        layout.paint(bounds.origin, &PaintOptions::default(), window, cx);
    }
}
