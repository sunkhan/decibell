//! Small pieces the views here share: the renderer's literals that recur (the dialog's ✕, the
//! gradient letter of a community without a picture), its `<input>` boxes, JavaScript's
//! `parseInt` and `localeCompare`, and text measured before it is drawn.

use std::cmp::Ordering;
use std::sync::Arc;

use base64::Engine as _;
use decibell_text::{Engine, TextStyle, Wrapping};
use decibell_text::input::{InputState, TextInput};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, TextField};
use gpui::{Focusable as _, 
    App, ClickEvent, ElementId, Entity, Hsla, Image, ImageFormat, IntoElement, Pixels, Stateful, Div, Window, div,
    prelude::*, px,
};

/// `h1`–`h6 { letter-spacing: -0.02em }` (globals.css base layer): every dialog title here is one.
pub const HEADING_TRACKING: f32 = -0.02;

/// `color-mix(in srgb, c 72%, #000)`.
pub fn darker(color: Hsla) -> Hsla {
    let mut rgba: gpui::Rgba = color.into();
    rgba.r *= 0.72;
    rgba.g *= 0.72;
    rgba.b *= 0.72;
    rgba.into()
}

/// `stringToGradient(name)`'s base colour: `var(--color-av-<rampIndex(name)>)`.
pub fn gradient_base(name: &str, cx: &App) -> Hsla {
    let t = cx.theme();
    [t.color_av_1, t.color_av_2, t.color_av_3, t.color_av_4, t.color_av_5, t.color_av_6, t.color_av_7, t.color_av_8]
        [decibell_ui::identity::ramp_index(name).clamp(1, 8) - 1]
}

/// `data:image/<type>;base64,<bytes>` (central's pictures) → an image GPUI decodes when drawn.
pub fn decode_data_url(url: &str) -> Option<Arc<Image>> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    let format = match mime {
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::Webp,
        "image/gif" => ImageFormat::Gif,
        _ => return None,
    };
    let bytes = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

/// JavaScript's `parseInt(s, 10)`: leading white space, an optional sign, then as many decimal
/// digits as there are; `None` is `NaN` (no digit).
pub fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim_start();
    let (negative, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits: &str = &rest[..rest.bytes().take_while(u8::is_ascii_digit).count()];
    if digits.is_empty() {
        return None;
    }
    // Past i64 the number is huge either way; the callers compare it with small bounds.
    let value = digits.parse::<i64>().unwrap_or(i64::MAX);
    Some(if negative { -value } else { value })
}

/// `a.localeCompare(b)` for names in the renderer's locale (en): letters compare without case
/// first, lower case before upper case when that is all that differs.
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    let fold = |s: &str| s.chars().flat_map(char::to_lowercase).collect::<String>();
    fold(a).cmp(&fold(b)).then_with(|| {
        // Equal but for case: ICU's tertiary level puts lower case first.
        a.chars()
            .zip(b.chars())
            .find(|(x, y)| x != y)
            .map(|(x, y)| y.is_lowercase().cmp(&x.is_lowercase()))
            .unwrap_or_else(|| a.len().cmp(&b.len()))
    })
}

/// A dialog's ✕: `flex h-7 w-7 items-center justify-center rounded-sm text-text-muted
/// hover:bg-surface-hover hover:text-text-secondary` around the 14 px cross (stroke 2.5).
pub fn close_button(id: impl Into<ElementId>, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let group: gpui::SharedString = "dialog-close".into();
    div()
        .id(id)
        .group(group.clone())
        .flex()
        .flex_none()
        .size_7()
        .items_center()
        .justify_center()
        .rounded(t.radius_sm)
        .cursor_pointer()
        .hover(|s| s.bg(t.color_surface_hover))
        .on_click(on_click)
        .child(Icon::new(IconName::Close).size(px(14.)).color(t.color_text_muted).group_hover(group, t.color_text_secondary))
}

/// The text style of a `Label` of this font, size, line height and weight (for measuring it
/// before it is drawn): the theme's body letter-spacing unless `tracking` (em) says otherwise.
pub fn text_style(font: FontRole, size: f32, line_height: f32, cx: &mut App) -> TextStyle {
    label_style(font, size, line_height, 400, None, cx)
}

pub fn label_style(font: FontRole, size: f32, line_height: f32, weight: u16, tracking: Option<f32>, cx: &mut App) -> TextStyle {
    let engine = Engine::global(cx);
    let theme = cx.theme();
    let face = engine.face(&decibell_text::FontSpec { family: theme.font(font), weight, italic: false });
    let mut style = TextStyle::new(face, size, theme.color_text_primary);
    style.line_height = decibell_text::LineHeight::Px(line_height);
    // `Label`: the body's letter-spacing is inherited as px of the 16 px root size.
    style.letter_spacing = match tracking {
        Some(em) => em * size,
        None => theme.palette.t_ls_body * 16.,
    };
    style
}

/// How many lines `text` takes at `width` px in `style` (wrapping as ordinary UI text does).
pub fn line_count(text: &str, style: &TextStyle, width: f32, scale: f32, cx: &mut App) -> usize {
    if text.is_empty() {
        return 0;
    }
    let engine = Engine::global(cx);
    engine.label_layout(text, style, Wrapping::NORMAL, width.max(1.), scale).line_count()
}

/// Each line `text` takes at `width` px: what a centred paragraph (`text-align: center`) draws one
/// label per line from.
pub fn lines(text: &str, style: &TextStyle, width: f32, scale: f32, cx: &mut App) -> Vec<String> {
    let engine = Engine::global(cx);
    let layout = engine.label_layout(text, style, Wrapping::NORMAL, width.max(1.), scale);
    let source = layout.paragraph().text().to_string();
    (0..layout.line_count())
        .filter_map(|i| layout.line(i))
        .map(|line| source.get(line.text.0 as usize..line.text.1 as usize).unwrap_or("").trim_end().to_string())
        .collect()
}

/// One of the renderer's `<input>` boxes: border, background, radius and padding of its classes,
/// the editor inside. `focus:border-accent`, and `focus:shadow-ring` where the TSX has it.
#[derive(Clone, Copy)]
pub struct InputLook {
    pub background: Hsla,
    pub radius: Pixels,
    pub pad_x: f32,
    pub pad_y: f32,
    /// Font size and line box of the text (`text-sm`: 14 / 20; `text-[13px]`: 13 / 19.5).
    pub size: f32,
    pub line: f32,
    pub font: FontRole,
    /// `placeholder:font-sans` on a monospace field.
    pub placeholder_font: FontRole,
    /// No `placeholder:text-*` class: preflight's `color-mix(currentcolor 50%, transparent)`.
    pub plain_placeholder: bool,
    pub ring: bool,
    /// Drawn inside `.chrome-scope` (the channel sidebar's dialogs): the chrome palette's colours.
    pub chrome: bool,
}

impl InputLook {
    pub fn new(background: Hsla, radius: Pixels) -> Self {
        Self {
            background,
            radius,
            pad_x: 12.,
            pad_y: 8.,
            size: 13.,
            line: 19.5,
            font: FontRole::Sans,
            placeholder_font: FontRole::Sans,
            plain_placeholder: false,
            ring: false,
            chrome: false,
        }
    }

    pub fn chrome(mut self) -> Self {
        self.chrome = true;
        self
    }

    /// The placeholder in the text's colour at half strength (no `placeholder:` class).
    pub fn plain_placeholder(mut self) -> Self {
        self.plain_placeholder = true;
        self
    }

    /// `text-sm`: 14 px on a 20 px line.
    pub fn text_sm(mut self) -> Self {
        self.size = 14.;
        self.line = 20.;
        self
    }

    pub fn pad(mut self, x: f32, y: f32) -> Self {
        self.pad_x = x;
        self.pad_y = y;
        self
    }

    pub fn mono(mut self) -> Self {
        self.font = FontRole::Mono;
        self
    }

    pub fn ring(mut self) -> Self {
        self.ring = true;
        self
    }
}

/// The box for `state` in `look`.
pub fn input_box(id: impl Into<ElementId>, state: &Entity<InputState>, look: InputLook, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let empty = state.read(cx).is_empty();
    let font = if empty { look.placeholder_font } else { look.font };
    let mut style = TextField::input_style(font, look.size, look.line / look.size, cx);
    let t = if look.chrome { &cx.theme().chrome } else { &**cx.theme() };
    style.text.color = t.color_text_primary;
    style.placeholder_color = if look.plain_placeholder { t.color_text_primary.opacity(0.5) } else { t.color_text_muted };
    let (accent, border, ring) = (t.color_accent, t.color_border, t.shadow_ring.clone());
    let focus = state.read(cx).focus_handle(cx);
    let focused = focus.is_focused(window);
    div()
        .id(id)
        .track_focus(&focus)
        .flex()
        .items_center()
        .w_full()
        .min_w_0()
        .h(px(look.line + 2. * look.pad_y + 2.))
        .px(px(look.pad_x))
        .rounded(look.radius)
        .border_1()
        .border_color(if focused { accent } else { border })
        .bg(look.background)
        .when(focused && look.ring, |this| this.shadow(ring))
        .cursor_text()
        .child(div().flex_1().min_w_0().child(TextInput::new(state, style)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_reads_like_javascript() {
        assert_eq!(parse_int("8082"), Some(8082));
        assert_eq!(parse_int("  8082abc"), Some(8082));
        assert_eq!(parse_int("-3"), Some(-3));
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("abc"), None);
        assert_eq!(parse_int("+"), None);
    }

    #[test]
    fn names_sort_as_locale_compare_does() {
        let mut names = vec!["bob", "Alice", "alice", "carol", "Bob"];
        names.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(names, ["alice", "Alice", "bob", "Bob", "carol"]);
    }

    #[test]
    fn pictures_come_as_data_urls() {
        let url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
        assert!(decode_data_url(url).is_some());
        assert!(decode_data_url("https://example.test/a.png").is_none());
    }
}
