//! Shared by `tests/fidelity.rs` and `examples/fidelity.rs`: an engine over GPUI's real Linux
//! text system with only the bundled fonts, and the corpus' segments → paragraphs.

#![allow(dead_code)]

use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;

use decibell_text::{Chip, Engine, Layout, Paragraph, ParagraphBuilder, TextStyle, Wrapping};
use gpui::{Hsla, TextSystem};
use serde_json::Value;

pub const BOLD: u64 = 1;
pub const ITALIC: u64 = 2;
pub const UNDERLINE: u64 = 4;
pub const STRIKE: u64 = 8;
pub const CODE: u64 = 16;
pub const LINK: u64 = 32;
pub const META: u64 = 64;

pub fn assets() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

/// GPUI's cosmic-text system with the bundled static faces and nothing else.
pub fn engine() -> Rc<Engine> {
    let platform = gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Decibell Inter");
    let mut fonts = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(assets().join("fonts/static"))
        .expect("gpui-client/assets/fonts/static")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "ttf"))
        .collect();
    files.sort();
    for file in files {
        fonts.push(Cow::Owned(std::fs::read(file).unwrap()));
    }
    let text_system = Arc::new(TextSystem::new(Arc::new(platform)));
    text_system.add_fonts(fonts).unwrap();
    Engine::new(text_system)
}

/// One of the corpus' named styles (a theme's message body).
#[derive(Clone)]
pub struct BodyStyle {
    pub family: String,
    pub mono: String,
    pub size: f32,
    pub leading: f32,
    pub letter_spacing_em: f32,
    pub meta: f32,
}

impl BodyStyle {
    pub fn from_json(value: &Value) -> Self {
        Self {
            family: value["family"].as_str().unwrap().to_string(),
            mono: value["mono"].as_str().unwrap().to_string(),
            size: value["size"].as_f64().unwrap() as f32,
            leading: value["lh"].as_f64().unwrap() as f32,
            letter_spacing_em: value["ls_em"].as_f64().unwrap() as f32,
            meta: value["meta"].as_f64().unwrap() as f32,
        }
    }

    pub fn base(&self, engine: &Engine) -> TextStyle {
        self.style(engine, 0)
    }

    /// The style of a segment with `flags`, as the renderer's CSS computes it.
    pub fn style(&self, engine: &Engine, flags: u64) -> TextStyle {
        let color = Hsla { h: 0., s: 0., l: 0.9, a: 1. };
        let weight = if flags & BOLD != 0 { 700 } else { 400 };
        let italic = flags & ITALIC != 0;
        let code = flags & CODE != 0;
        let family = if code { &self.mono } else { &self.family };
        let face = engine.face(&decibell_text::FontSpec {
            family: family.clone().into(),
            weight,
            italic,
        });
        // font-size: 0.85em on <code>; the unitless line-height and the computed (px)
        // letter-spacing are inherited.
        let size = if flags & META != 0 {
            self.meta
        } else if code {
            self.size * 0.85
        } else {
            self.size
        };
        let mut style = TextStyle::new(face, size, color)
            .leading(self.leading)
            .letter_spacing(self.size * self.letter_spacing_em);
        style.underline = flags & UNDERLINE != 0;
        style.strikethrough = flags & STRIKE != 0;
        if code {
            style.chip = Some(Chip {
                background: Hsla { h: 0., s: 0., l: 0.1, a: 1. },
                radius: 6.,
                pad_x: 6.,
                pad_y: 2.,
            });
        }
        if flags & META != 0 {
            style.margin_start = 4.;
            style.selectable = false;
        }
        style
    }
}

pub fn wrapping(name: &str) -> Wrapping {
    match name {
        "body" => Wrapping::MESSAGE,
        "words" => Wrapping::PRE_WRAP,
        "normal" => Wrapping::NORMAL,
        "nowrap" => Wrapping::NOWRAP,
        other => panic!("unknown wrap {other}"),
    }
}

pub fn paragraph(engine: &Engine, body: &BodyStyle, wrap: &str, segs: &[Value], scale: f32) -> Paragraph {
    let base = body.base(engine);
    let mut builder = ParagraphBuilder::new(engine, &base, wrapping(wrap), scale);
    for (index, seg) in segs.iter().enumerate() {
        let seg = seg.as_array().unwrap();
        if seg[0].as_str() == Some("box") {
            let (width, height) = (seg[1].as_f64().unwrap() as f32, seg[2].as_f64().unwrap() as f32);
            let below = seg[3].as_f64().unwrap() as f32 * body.size;
            builder.inline_box(width, height, below, index as u32);
        } else {
            let style = body.style(engine, seg[0].as_u64().unwrap());
            builder.text(seg[1].as_str().unwrap(), &style, index as u32);
        }
    }
    builder.build()
}

/// What the harness page records per line, from our layout: (start in UTF-16 units, width,
/// top of the first character's box or `None` for an empty line).
pub fn lines(layout: &Layout, engine: &Engine, body: &BodyStyle, segs: &[Value], scale: f32) -> Vec<(u32, f32, Option<f32>)> {
    let paragraph = layout.paragraph();
    let text = paragraph.text();
    let mut out = Vec::new();
    let mut utf16 = 0u32;
    let mut byte = 0usize;
    for index in 0..layout.line_count() {
        let line = layout.line(index).unwrap();
        utf16 += text[byte..line.text.0 as usize].encode_utf16().count() as u32;
        byte = line.text.0 as usize;
        let content = &text[line.text.0 as usize..line.text.1 as usize];
        let empty = content.is_empty() || content == "\n";
        let top = (!empty).then(|| {
            let (tag, _) = paragraph.locate(line.text.0 + 1).unwrap();
            let seg = segs[tag as usize].as_array().unwrap();
            if seg[0].as_str() == Some("box") {
                let below = ((seg[3].as_f64().unwrap() as f32 * body.size * scale * 64.).floor() / 64.) / scale;
                line.baseline + below - seg[2].as_f64().unwrap() as f32
            } else {
                let style = body.style(engine, seg[0].as_u64().unwrap());
                let (ascent, _) = style.face.line_metrics(style.size, scale);
                line.baseline - ascent / scale
            }
        });
        // The harness measures to the right edge of the line's last character that is not
        // a space (a chip's closing padding is outside it).
        let trimmed = content.trim_end_matches([' ', '\t', '\n']);
        let end = line.text.0 + trimmed.len() as u32;
        let width = layout.caret(decibell_text::Pos { offset: end, affinity: decibell_text::Affinity::Upstream }).0;
        out.push((utf16, if trimmed.is_empty() { 0. } else { width }, top));
    }
    out
}

/// Whether the harness can compare this case line by line: its page finds lines by x going
/// back, which holds only for left-to-right text, and it loads only the bundled fonts'
/// own scripts reliably (anything else is drawn by whatever font the machine has).
pub fn comparable(segs: &[Value]) -> bool {
    segs.iter().all(|seg| {
        seg[1].as_str().is_none_or(|text| {
            text.chars().all(|c| {
                let c = c as u32;
                // Latin, Latin-1, Latin Extended-A/B, general punctuation, arrows, maths.
                c < 0x250 || (0x2000..0x2400).contains(&c) || c == 0xfffc
            })
        })
    })
}

/// Line starts as the harness page reports them. With `white-space: normal` the space a
/// line was wrapped at has no box of its own, and the page attributes it to either line:
/// there a line starts at its first character that is not a space.
pub fn line_starts(lines: &[(u32, f32, Option<f32>)], text: &str, wrap: &str) -> Vec<u32> {
    if wrap != "normal" {
        return lines.iter().map(|line| line.0).collect();
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    lines
        .iter()
        .map(|line| {
            let mut start = line.0 as usize;
            while units.get(start) == Some(&(b' ' as u16)) {
                start += 1;
            }
            start as u32
        })
        .collect()
}
