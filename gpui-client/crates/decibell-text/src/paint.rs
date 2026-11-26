//! Painting a [`Layout`]: glyph by glyph through GPUI's glyph atlas, with the layout's own
//! positions (so letter-spacing, inline boxes and Chromium's line boxes hold), plus chips,
//! decorations and the selection. Nothing here allocates.
//!
//! Pixel snapping: each line's baseline is rounded to a whole device pixel once, and the
//! line's rectangles move with it, so text and its chip never drift apart; glyphs sit on a
//! quarter-pixel grid horizontally (GPUI's subpixel variants, the same grid Skia uses).

use gpui::{App, Bounds, Corners, GlyphId, Hsla, Pixels, Point, Window, fill, point, px, size};

use crate::layout::{DecoKind, ELLIPSIS_ITEM, FIRST_MARK_ITEM, Layout};
use crate::paragraph::flag;

/// What varies between two paints of the same layout.
#[derive(Clone, Copy, Debug, Default)]
pub struct PaintOptions {
    /// Selected text, as byte offsets into the paragraph's text.
    pub selection: Option<(u32, u32)>,
    pub selection_background: Hsla,
    /// The colour selected glyphs are drawn in; `None` keeps their own.
    pub selection_color: Option<Hsla>,
    /// Underline every run carrying this tag (the link under the pointer).
    pub underline_tag: Option<u32>,
    /// Draw every run in this colour instead of its own. A label's colour is given here,
    /// so that a hover or a theme switch recolours it without a new layout.
    pub color: Option<Hsla>,
}

impl Layout {
    /// Paints the layout with its top-left corner at `origin`. Paint phase only. Inline
    /// boxes are the caller's to fill ([`Layout::boxes`]).
    pub fn paint(&self, origin: Point<Pixels>, options: &PaintOptions, window: &mut Window, cx: &mut App) {
        let _ = cx;
        let paragraph = &*self.paragraph;
        let scale = window.scale_factor();
        let origin_x = f32::from(origin.x);
        let origin_y = f32::from(origin.y);
        let snap = |value: f32| (value * scale).round() / scale;
        let selection = options.selection.filter(|(from, to)| from < to);

        let mut deco = 0;
        for (line_index, line) in self.lines.iter().enumerate() {
            let baseline = snap(origin_y + line.baseline);
            let shift = baseline - line.baseline;

            // Chip backgrounds.
            let line_decos = {
                let first = deco;
                while deco < self.decos.len() && self.decos[deco].line == line_index as u32 {
                    deco += 1;
                }
                &self.decos[first..deco]
            };
            for rect in line_decos.iter().filter(|rect| rect.kind == DecoKind::Chip) {
                let bounds = Bounds::new(
                    point(px(origin_x + rect.x), px(shift + rect.y)),
                    size(px(rect.width), px(rect.height)),
                );
                window.paint_quad(fill(bounds, rect.color).corner_radii(Corners::all(px(rect.radius))));
            }

            // The selection, one rectangle per line, as high as the line box.
            if let Some((from, to)) = selection
                && let Some((left, right)) = self.selection_span(line_index, from, to)
            {
                let top = snap(origin_y + line.top);
                let bottom = snap(origin_y + line.top + line.height);
                window.paint_quad(fill(
                    Bounds::new(point(px(origin_x + left), px(top)), size(px(right - left), px(bottom - top))),
                    options.selection_background,
                ));
            }

            for rect in line_decos.iter().filter(|rect| rect.kind == DecoKind::Underline) {
                self.paint_rule(rect, origin_x, shift, selection, options, window);
            }

            for frag in &self.frags[line.frags.0 as usize..line.frags.1 as usize] {
                if frag.item >= FIRST_MARK_ITEM {
                    let mark = if frag.item == ELLIPSIS_ITEM { paragraph.ellipsis } else { paragraph.hyphen };
                    if let Some(mark) = mark {
                        let _ = window.paint_glyph(
                            point(px(origin_x + frag.x), px(baseline)),
                            mark.font,
                            GlyphId(mark.glyph as u32),
                            px(crate::paragraph::shaping_size(paragraph.base.size, self.scale)),
                            options.color.unwrap_or(paragraph.base.color),
                        );
                    }
                    continue;
                }
                let item = &paragraph.items[frag.item as usize];
                if frag.glyphs.0 >= frag.glyphs.1 {
                    continue;
                }
                let style = &paragraph.styles[item.style as usize];
                let font_size = px(crate::paragraph::shaping_size(style.size, self.scale));
                let x = origin_x + frag.dx;
                let glyphs = &paragraph.glyphs[frag.glyphs.0 as usize..frag.glyphs.1 as usize];
                if item.flags & flag::EMOJI_FONT != 0 {
                    for glyph in glyphs {
                        let at = point(px(x + glyph.x), px(baseline + glyph.y as f32 / 64.));
                        let _ = window.paint_emoji(at, item.font, GlyphId(glyph.id as u32), font_size);
                    }
                    continue;
                }
                let run_color = options.color.unwrap_or(style.color);
                let selected_color = match (selection, options.selection_color) {
                    (Some(range), Some(color)) if style.selectable => Some((range, color)),
                    _ => None,
                };
                for glyph in glyphs {
                    let color = match selected_color {
                        Some(((from, to), color)) if glyph.cluster >= from && glyph.cluster < to => color,
                        _ => run_color,
                    };
                    let at = point(px(x + glyph.x), px(baseline + glyph.y as f32 / 64.));
                    let _ = window.paint_glyph(at, item.font, GlyphId(glyph.id as u32), font_size, color);
                }
                if options.underline_tag == Some(item.tag) && item.flags & flag::IS_BOX == 0 {
                    let thickness = (style.size * scale / 10.).floor().max(1.);
                    let gap = (thickness / 2.).ceil().max(1.);
                    window.paint_quad(fill(
                        Bounds::new(
                            point(px(origin_x + frag.x), px(baseline + gap / scale)),
                            size(px(frag.width), px(thickness / scale)),
                        ),
                        run_color,
                    ));
                }
            }

            for rect in line_decos.iter().filter(|rect| rect.kind == DecoKind::Strike) {
                self.paint_rule(rect, origin_x, shift, selection, options, window);
            }
        }
    }

    fn paint_rule(
        &self,
        rect: &crate::layout::Deco,
        origin_x: f32,
        shift: f32,
        selection: Option<(u32, u32)>,
        options: &PaintOptions,
        window: &mut Window,
    ) {
        let color = match (selection, options.selection_color) {
            (Some((from, to)), Some(color)) if rect.text.0 >= from && rect.text.1 <= to => color,
            _ => options.color.unwrap_or(rect.color),
        };
        window.paint_quad(fill(
            Bounds::new(
                point(px(origin_x + rect.x), px(shift + rect.y)),
                size(px(rect.width), px(rect.height)),
            ),
            color,
        ));
    }

    /// Every inline box with its tag and its rectangle relative to the layout's top-left
    /// (x, y, width, height in px).
    pub fn boxes(&self) -> impl Iterator<Item = (u32, [f32; 4])> + '_ {
        let paragraph = &*self.paragraph;
        self.frags.iter().filter_map(move |frag| {
            if frag.item >= FIRST_MARK_ITEM {
                return None;
            }
            let item = &paragraph.items[frag.item as usize];
            if item.flags & flag::IS_BOX == 0 {
                return None;
            }
            let inline_box = paragraph.boxes.iter().find(|b| b.unit == frag.units.0)?;
            let line = &self.lines[frag.line as usize];
            // The drop below the baseline on the layout's grid, as the line box used it.
            let below = (inline_box.below * self.scale * 64.).floor() / 64. / self.scale;
            let bottom = line.baseline + below;
            Some((
                inline_box.tag,
                [frag.x, bottom - inline_box.height, inline_box.width, inline_box.height],
            ))
        })
    }
}
