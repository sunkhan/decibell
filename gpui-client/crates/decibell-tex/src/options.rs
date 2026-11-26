//! Port of KaTeX `src/Options.ts`.

use std::rc::Rc;

use crate::font_metrics::{FontMetrics, get_global_metrics};
use crate::style::Style;

const SIZE_STYLE_MAP: [[u8; 3]; 11] = [
    [1, 1, 1],
    [2, 1, 1],
    [3, 1, 1],
    [4, 2, 1],
    [5, 2, 1],
    [6, 3, 1],
    [7, 4, 2],
    [8, 6, 3],
    [9, 7, 6],
    [10, 8, 7],
    [11, 10, 9],
];

pub const SIZE_MULTIPLIERS: [f64; 11] =
    [0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.2, 1.44, 1.728, 2.074, 2.488];

fn size_at_style(size: u8, style: Style) -> u8 {
    if style.size < 2 {
        size
    } else {
        SIZE_STYLE_MAP[size as usize - 1][style.size as usize - 1]
    }
}

pub const BASESIZE: u8 = 6;

/// `Options`. `font`, `font_family`, `font_weight`, `font_shape` hold KaTeX's strings (`""`
/// when unset).
#[derive(Clone, Debug)]
pub struct Options {
    pub style: Style,
    pub color: Option<Rc<str>>,
    pub size: u8,
    pub text_size: u8,
    pub phantom: bool,
    pub font: &'static str,
    pub font_family: &'static str,
    pub font_weight: &'static str,
    pub font_shape: &'static str,
    pub size_multiplier: f64,
    pub max_size: f64,
    pub min_rule_thickness: f64,
}

static CLASSES_RESET: [&str; 12] = [
    "reset-size0",
    "reset-size1",
    "reset-size2",
    "reset-size3",
    "reset-size4",
    "reset-size5",
    "reset-size6",
    "reset-size7",
    "reset-size8",
    "reset-size9",
    "reset-size10",
    "reset-size11",
];
static CLASSES_SIZE: [&str; 12] = [
    "size0", "size1", "size2", "size3", "size4", "size5", "size6", "size7", "size8", "size9",
    "size10", "size11",
];

/// `"reset-size" + n`.
pub fn reset_size_class(n: u8) -> &'static str {
    CLASSES_RESET[n as usize]
}

/// `"size" + n`.
pub fn size_class(n: u8) -> &'static str {
    CLASSES_SIZE[n as usize]
}

impl Options {
    pub fn new(style: Style, max_size: f64, min_rule_thickness: f64) -> Options {
        Options {
            style,
            color: None,
            size: BASESIZE,
            text_size: BASESIZE,
            phantom: false,
            font: "",
            font_family: "",
            font_weight: "",
            font_shape: "",
            size_multiplier: SIZE_MULTIPLIERS[BASESIZE as usize - 1],
            max_size,
            min_rule_thickness,
        }
    }

    fn with_size(mut self, size: u8) -> Options {
        self.size = size;
        self.size_multiplier = SIZE_MULTIPLIERS[size as usize - 1];
        self
    }

    /// `havingStyle(style)`.
    pub fn having_style(&self, style: Style) -> Options {
        if self.style == style {
            self.clone()
        } else {
            let mut o = self.clone();
            o.style = style;
            o.with_size(size_at_style(self.text_size, style))
        }
    }

    /// `havingCrampedStyle()`.
    pub fn having_cramped_style(&self) -> Options {
        self.having_style(self.style.cramp())
    }

    /// `havingSize(size)`.
    pub fn having_size(&self, size: u8) -> Options {
        if self.size == size && self.text_size == size {
            self.clone()
        } else {
            let mut o = self.clone();
            o.style = self.style.text();
            o.text_size = size;
            o.with_size(size)
        }
    }

    /// `havingBaseStyle(style)` (`None` = `this.style.text()`).
    pub fn having_base_style(&self, style: Option<Style>) -> Options {
        let style = style.unwrap_or_else(|| self.style.text());
        let want_size = size_at_style(BASESIZE, style);
        if self.size == want_size && self.text_size == BASESIZE && self.style == style {
            self.clone()
        } else {
            let mut o = self.clone();
            o.style = style;
            o.with_size(want_size)
        }
    }

    /// `havingBaseSizing()`.
    pub fn having_base_sizing(&self) -> Options {
        let size = match self.style.id {
            4 | 5 => 3,
            6 | 7 => 1,
            _ => 6,
        };
        let mut o = self.clone();
        o.style = self.style.text();
        o.with_size(size)
    }

    pub fn with_color(&self, color: &str) -> Options {
        let mut o = self.clone();
        o.color = Some(Rc::from(color));
        o
    }

    pub fn with_phantom(&self) -> Options {
        let mut o = self.clone();
        o.phantom = true;
        o
    }

    pub fn with_font(&self, font: &'static str) -> Options {
        let mut o = self.clone();
        o.font = font;
        o
    }

    pub fn with_text_font_family(&self, font_family: &'static str) -> Options {
        let mut o = self.clone();
        o.font_family = font_family;
        o.font = "";
        o
    }

    pub fn with_text_font_weight(&self, font_weight: &'static str) -> Options {
        let mut o = self.clone();
        o.font_weight = font_weight;
        o.font = "";
        o
    }

    pub fn with_text_font_shape(&self, font_shape: &'static str) -> Options {
        let mut o = self.clone();
        o.font_shape = font_shape;
        o.font = "";
        o
    }

    /// `sizingClasses(oldOptions)`.
    pub fn sizing_classes(&self, old: &Options) -> Vec<&'static str> {
        if old.size != self.size {
            vec![
                "katex-sizing",
                reset_size_class(old.size),
                size_class(self.size),
            ]
        } else {
            vec![]
        }
    }

    /// `baseSizingClasses()`.
    pub fn base_sizing_classes(&self) -> Vec<&'static str> {
        if self.size != BASESIZE {
            vec![
                "katex-sizing",
                reset_size_class(self.size),
                size_class(BASESIZE),
            ]
        } else {
            vec![]
        }
    }

    pub fn font_metrics(&self) -> &'static FontMetrics {
        get_global_metrics(self.size)
    }

    /// `getColor()`.
    pub fn get_color(&self) -> Option<Rc<str>> {
        if self.phantom {
            Some(Rc::from("transparent"))
        } else {
            self.color.clone()
        }
    }
}
