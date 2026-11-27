//! The text styles and line heights the content's layouts measure with, built once per theme,
//! text size and scale (what `decibell_chat::style::ChatStyle` is for the rows).
//!
//! A slot's height is computed before anything is drawn, from these; the elements then draw
//! the same strings with the same styles at the same widths, so the two can't disagree.

use std::cell::RefCell;
use std::rc::Rc;

use decibell_text::{Engine, Face, FontSpec, LineHeight, OverflowWrap, TextStyle, Wrapping};
use decibell_ui::{ActiveTheme as _, FontRole, Theme};
use gpui::{App, Hsla};

/// The renderer's `<body>` letter-spacing is an em value of the browser's 16 px, inherited as px.
const BODY_FONT_SIZE: f32 = 16.;

/// See the module doc.
pub struct ContentStyle {
    pub scale: f32,
    /// `font-channel text-sm font-medium` (a document's or an upload's name): 14 px / 20 px.
    pub file_name: TextStyle,
    /// `text-[11px]` (a document's size).
    pub file_meta: TextStyle,
    /// `text-sender font-medium` (an audio file's name).
    pub audio_name: TextStyle,
    /// `font-meta text-[10.5px]` (audio meta, the video card's bar).
    pub meta_105: TextStyle,
    /// `font-meta text-[10px]` (the audio transport time).
    pub meta_10: TextStyle,
    /// `text-[10px]` (an upload's status line).
    pub upload_status: TextStyle,
    /// `text-[12px] italic` (a purged attachment).
    pub tombstone: TextStyle,
    /// `text-[11px]` (a grid video's scrim line).
    pub scrim: TextStyle,
    /// `text-[11px] text-error` (an audio card's "Failed to load" line).
    pub error: TextStyle,
    /// Site card: `font-meta text-micro` (site name), `text-body font-semibold leading-body`
    /// (title, accent), `text-meta leading-body` (description).
    pub site_name: TextStyle,
    pub site_title: TextStyle,
    pub site_description: TextStyle,
    /// Invite card: the label (`font-meta text-micro font-semibold uppercase tracking-section`),
    /// the title (`text-body font-semibold`), the description (`text-meta leading-body`), the
    /// members line (`font-meta text-meta`), the button (`text-[13px] font-semibold`).
    pub invite_label: TextStyle,
    pub invite_title: TextStyle,
    pub invite_description: TextStyle,
    pub invite_meta: TextStyle,
    pub button: TextStyle,
    /// Chromium's broken-image alt text: what the `<button>` inherits (the message text).
    pub alt: TextStyle,
    pub engine: Rc<Engine>,
}

fn face(engine: &Engine, theme: &Theme, role: FontRole, weight: f32, italic: bool) -> Face {
    engine.face(&FontSpec { family: theme.font(role), weight: weight.round() as u16, italic })
}

impl ContentStyle {
    fn build(engine: Rc<Engine>, theme: &Theme, scale: f32) -> ContentStyle {
        let tracking = theme.palette.t_ls_body * BODY_FONT_SIZE;
        let text = |role: FontRole, size: f32, weight: f32, color: Hsla| -> TextStyle {
            let mut style = TextStyle::new(face(&engine, theme, role, weight, false), size, color);
            style.line_height = LineHeight::Factor(1.5);
            style.letter_spacing = tracking;
            style
        };
        let scaled = &theme.scaled;
        let leading = theme.palette.t_lh_body;
        let mut tombstone = text(FontRole::Sans, 12., 400., theme.color_text_muted);
        tombstone.face = face(&engine, theme, FontRole::Sans, 400., true);
        let mut invite_label = text(FontRole::Meta, scaled.text_micro, 600., theme.color_text_muted);
        invite_label.letter_spacing = theme.palette.tracking_section * scaled.text_micro;
        ContentStyle {
            scale,
            // Tailwind's `text-sm`: 0.875 rem with its own 1.25 rem line height.
            file_name: text(FontRole::Channel, 14., 500., theme.color_text_primary).line_height_px(20.),
            file_meta: text(FontRole::Sans, 11., 400., theme.color_text_muted),
            audio_name: text(FontRole::Sans, scaled.text_sender, 500., theme.color_text_primary),
            meta_105: text(FontRole::Meta, 10.5, 400., theme.color_text_muted),
            meta_10: text(FontRole::Meta, 10., 400., theme.color_text_muted),
            upload_status: text(FontRole::Sans, 10., 400., theme.color_text_muted),
            tombstone,
            scrim: text(FontRole::Sans, 11., 400., theme.color_text_secondary),
            error: text(FontRole::Sans, 11., 400., theme.color_error),
            site_name: text(FontRole::Meta, scaled.text_micro, 400., theme.color_text_muted),
            site_title: text(FontRole::Sans, scaled.text_body, 600., theme.color_accent).leading(leading),
            site_description: text(FontRole::Sans, scaled.text_meta, 400., theme.color_text_secondary).leading(leading),
            invite_label,
            invite_title: text(FontRole::Sans, scaled.text_body, 600., theme.color_text_bright),
            invite_description: text(FontRole::Sans, scaled.text_meta, 400., theme.color_text_secondary).leading(leading),
            invite_meta: text(FontRole::Meta, scaled.text_meta, 400., theme.color_text_muted),
            button: text(FontRole::Sans, 13., 600., theme.color_on_accent),
            alt: text(FontRole::Sans, scaled.text_body, 400., theme.color_text_primary),
            engine,
        }
    }

    /// The style for the active theme at `scale` (rebuilt when the theme, the text size or the
    /// scale changes).
    pub fn get(cx: &App, scale: f32) -> Rc<ContentStyle> {
        type Stamp = (usize, u32, u32);
        thread_local! {
            static CACHE: RefCell<Option<(Stamp, Rc<ContentStyle>)>> = const { RefCell::new(None) };
        }
        let theme = cx.theme();
        let stamp = (theme.id.index(), theme.scaled.text_body.to_bits(), scale.to_bits());
        CACHE.with(|cache| {
            if let Some((at, style)) = &*cache.borrow()
                && *at == stamp
            {
                return style.clone();
            }
            let engine = crate::engine(cx);
            let style = Rc::new(ContentStyle::build(engine, theme, scale));
            *cache.borrow_mut() = Some((stamp, style.clone()));
            style
        })
    }

    /// The height of `text` in `style` at `width` (line boxes as Blink stacks them).
    pub fn height(&self, text: &str, style: &TextStyle, wrapping: Wrapping, width: f32) -> f32 {
        if text.is_empty() {
            return 0.;
        }
        self.engine.label_layout(text, style, wrapping, width.max(1.), self.scale).height()
    }

    /// One line's box.
    pub fn line(&self, style: &TextStyle) -> f32 {
        self.height("Xg", style, Wrapping::NOWRAP, f32::INFINITY)
    }
}

/// `line-clamp-N` with `[overflow-wrap:anywhere]`.
pub fn clamp_anywhere(lines: u16) -> Wrapping {
    let mut wrapping = Wrapping::NORMAL.clamp(lines);
    wrapping.overflow_wrap = OverflowWrap::Anywhere;
    wrapping
}

/// A label drawing `text` in `style` with `wrapping` (the kit's `Label` doesn't take every
/// wrapping the cards need).
pub fn label(text: impl Into<gpui::SharedString>, style: &TextStyle, wrapping: Wrapping) -> decibell_text::Label {
    decibell_text::Label::new(text, *style).wrapping(wrapping)
}

/// The same with another colour.
pub fn label_in(text: impl Into<gpui::SharedString>, style: &TextStyle, wrapping: Wrapping, color: Hsla) -> decibell_text::Label {
    decibell_text::Label::new(text, style.color(color)).wrapping(wrapping)
}
