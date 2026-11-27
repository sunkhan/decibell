//! The elements that put a layout on screen: [`Label`] for UI text and [`TextBlock`] for a
//! layout the caller already has.

use std::cell::RefCell;
use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, CursorStyle, DispatchPhase, Element, ElementId, Entity,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, SharedString, Size, Style, WeakEntity, Window, px, rgb, size,
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
    selection: Option<Entity<TextSelection>>,
    /// Emoji drawn as Twemoji at this size, px ([`Label::emoji`]).
    emoji: Option<f32>,
}

/// What is selected in a selectable [`Label`] (`user-select: text`): byte offsets into its text.
/// One label holds a selection at a time, as one document does in Chromium; a press anywhere
/// else clears it.
#[derive(Default)]
pub struct TextSelection {
    anchor: u32,
    focus: u32,
    dragging: bool,
    text: SharedString,
}

impl TextSelection {
    /// The selected byte range, when something is selected.
    pub fn range(&self) -> Option<(u32, u32)> {
        let (from, to) = (self.anchor.min(self.focus), self.anchor.max(self.focus));
        (from < to && to as usize <= self.text.len()).then_some((from, to))
    }

    /// The selected text.
    pub fn selected_text(&self) -> Option<String> {
        let (from, to) = self.range()?;
        self.text.get(from as usize..to as usize).map(str::to_string)
    }

    fn set(&mut self, anchor: u32, focus: u32) -> bool {
        let changed = (self.anchor, self.focus) != (anchor, focus);
        (self.anchor, self.focus) = (anchor, focus);
        changed
    }
}

thread_local! {
    /// The label holding the window's text selection.
    static ACTIVE: RefCell<Option<WeakEntity<TextSelection>>> = const { RefCell::new(None) };
}

/// The text selected in a label, if any (what Ctrl+C copies).
pub fn selected_label_text(cx: &App) -> Option<String> {
    let active = ACTIVE.with(|a| a.borrow().clone())?.upgrade()?;
    active.read(cx).selected_text()
}

/// Ctrl+C with no text field focused: copies the label selection to the clipboard (Chromium copies
/// a document selection the same way). `false` when nothing is selected.
pub fn copy_label_selection(cx: &mut App) -> bool {
    let Some(text) = selected_label_text(cx) else { return false };
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    true
}

/// Chromium's default selection colours (the renderer has no `::selection` rule).
fn selection_colors() -> (gpui::Hsla, gpui::Hsla) {
    (rgb(0x0538A4).into(), rgb(0xFFFFFF).into())
}

impl Label {
    pub fn new(text: impl Into<SharedString>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
            wrapping: Wrapping::NORMAL,
            inherit_color: false,
            selection: None,
            emoji: None,
        }
    }

    /// Text the user can select with the pointer (drag, double click a word, triple click all)
    /// and copy with Ctrl+C (the window's root asks [`copy_label_selection`]); the I-beam
    /// shows over it. `selection` holds it: make it once per label (`window.use_keyed_state`).
    pub fn selectable(mut self, selection: Entity<TextSelection>) -> Self {
        self.selection = Some(selection);
        self
    }

    /// Draw the emoji in the text as Twemoji, `size` px square, sitting 0.2 em under the
    /// baseline (`Twemoji.tsx`: an `inline-block align-[-0.2em]` of the size), where the label
    /// would otherwise take them from the system's emoji font: a custom status, a DM preview.
    /// An emoji the pack lacks stays text. The pictures are rasterised when the layout is built
    /// (once per text, size and scale), so a frame only places them.
    pub fn emoji(mut self, size: f32) -> Self {
        self.emoji = Some(size);
        self
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
        let key = label_key(&self.text, &self.style, self.wrapping, scale);
        match self.emoji {
            None => key,
            Some(size) => {
                let mut hasher = KeyHasher::default();
                (key, size.to_bits()).hash(&mut hasher);
                hasher.finish()
            }
        }
    }
}

/// The inline boxes of a label's Twemoji carry this plus the emoji's byte offset in the text.
const EMOJI_TAG: u32 = 1 << 31;

/// A label's text with its emoji as inline boxes (`emoji`: their size) or as plain text.
fn label_text(builder: &mut ParagraphBuilder, engine: &Engine, text: &str, style: &TextStyle, emoji: Option<f32>, scale: f32) {
    let Some(size) = emoji else {
        builder.text(text, style, 0);
        return;
    };
    let device_px = (size * scale).round() as u32;
    let mut last = 0;
    for range in decibell_richtext::emoji::find_iter(text) {
        let Some(asset) = decibell_richtext::emoji::twemoji_asset(&text[range.clone()]) else { continue };
        if engine.emoji.prepare(asset, device_px).is_none() {
            continue;
        }
        if range.start > last {
            builder.text(&text[last..range.start], style, 0);
        }
        builder.inline_box(size, size, 0.2 * style.size, EMOJI_TAG | range.start as u32);
        last = range.end;
    }
    if last < text.len() {
        builder.text(&text[last..], style, 0);
    }
}

/// The Twemoji a label's inline box shows: the emoji at the byte offset its tag carries.
fn emoji_at(text: &str, tag: u32) -> Option<decibell_richtext::EmojiAsset> {
    if tag & EMOJI_TAG == 0 {
        return None;
    }
    let rest = text.get((tag & !EMOJI_TAG) as usize..)?;
    let range = decibell_richtext::emoji::find_iter(rest).next().filter(|range| range.start == 0)?;
    decibell_richtext::emoji::twemoji_asset(&rest[range])
}

/// Paints a label's Twemoji into the boxes its layout placed (`label_text`).
fn paint_emoji(layout: &Layout, text: &str, origin: gpui::Point<Pixels>, engine: &Engine, window: &mut Window, cx: &mut App) {
    for (tag, [x, y, width, height]) in layout.boxes() {
        let Some(asset) = emoji_at(text, tag) else { continue };
        let bounds = Bounds::new(gpui::point(origin.x + px(x), origin.y + px(y)), size(px(width), px(height)));
        engine.emoji.paint(asset, bounds, window, cx);
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
#[allow(clippy::too_many_arguments)]
fn label_layout(engine: &Engine, key: u64, text: &str, style: &TextStyle, wrapping: Wrapping, emoji: Option<f32>, width: f32, scale: f32) -> Rc<Layout> {
    engine.labels.layout(key, width, scale, || {
        let paragraph = engine.labels.paragraph(key, || {
            let mut builder = ParagraphBuilder::new(engine, style, wrapping, scale);
            label_text(&mut builder, engine, text, style, emoji, scale);
            builder.build()
        });
        Layout::new(paragraph, width, engine)
    })
}

impl Engine {
    /// The layout a [`Label`] with this text and style has at `width` px: for code that
    /// needs a label's size before it draws one.
    pub fn label_layout(&self, text: &str, style: &TextStyle, wrapping: Wrapping, width: f32, scale: f32) -> Rc<Layout> {
        label_layout(self, label_key(text, style, wrapping, scale), text, style, wrapping, None, width, scale)
    }
}

/// A box of whole device pixels around a size, so that layout rounding cannot make the box
/// narrower than its text.
fn snapped(width: f32, height: f32, scale: f32) -> Size<Pixels> {
    size(px((width * scale).ceil() / scale), px((height * scale).ceil() / scale))
}

impl Element for Label {
    type RequestLayoutState = ();
    /// A selectable label's hitbox. GPUI takes hitboxes in prepaint only (`debug_assert_prepaint`:
    /// inserting one in paint aborted every debug build that drew such a label).
    type PrepaintState = Option<Hitbox>;

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
        let (text, style, wrapping, emoji) = (self.text.clone(), self.style, self.wrapping, self.emoji);
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
            let layout = label_layout(&engine, key, &text, &style, wrapping, emoji, width, scale);
            let natural = snapped(layout.width(), layout.height(), scale);
            if !wraps && wrapping.ellipsis {
                // A truncating label gives way down to nothing, like `min-width: 0`.
                return size(natural.width.min(px(limit)), natural.height);
            }
            natural
        });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Option<Hitbox> {
        self.selection.as_ref().map(|_| window.insert_hitbox(bounds, HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let engine = Engine::global(cx);
        let scale = window.scale_factor();
        let key = self.key(scale);
        let width = f32::from(bounds.size.width);
        let wraps = self.wrapping.wraps();
        let mut layout = label_layout(&engine, key, &self.text, &self.style, self.wrapping, self.emoji, if wraps { width } else { f32::INFINITY }, scale);
        if !wraps && self.wrapping.ellipsis && layout.width() > width + 0.5 / scale {
            layout = label_layout(&engine, key, &self.text, &self.style, self.wrapping, self.emoji, width, scale);
        }
        let color = if self.inherit_color { window.text_style().color } else { self.style.color };
        let mut options = PaintOptions {
            color: Some(color),
            ..PaintOptions::default()
        };
        if let Some(selection) = self.selection.clone() {
            let text = self.text.clone();
            if selection.read(cx).text != text {
                selection.update(cx, |s, _| *s = TextSelection { text, ..TextSelection::default() });
            }
            if let Some(range) = selection.read(cx).range() {
                let (background, foreground) = selection_colors();
                options.selection = Some(range);
                options.selection_background = background;
                options.selection_color = Some(foreground);
            }
            if let Some(hitbox) = hitbox.as_ref() {
                listen_for_selection(&selection, &layout, bounds, hitbox, window);
            }
        }
        layout.paint(bounds.origin, &options, window, cx);
        if self.emoji.is_some() {
            paint_emoji(&layout, &self.text, bounds.origin, &engine, window, cx);
        }
    }
}

/// A selectable label's pointer: a press on it starts a selection (two presses: the word, three:
/// all), a drag moves its end, a press anywhere else clears it.
fn listen_for_selection(selection: &Entity<TextSelection>, layout: &Rc<Layout>, bounds: Bounds<Pixels>, hitbox: &Hitbox, window: &mut Window) {
    window.set_cursor_style(CursorStyle::IBeam, hitbox);
    let at = {
        let layout = layout.clone();
        move |position: gpui::Point<Pixels>| layout.hit(f32::from(position.x - bounds.origin.x), f32::from(position.y - bounds.origin.y))
    };
    let (sel, hit, on) = (selection.clone(), at.clone(), hitbox.clone());
    let layout_down = layout.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        let inside = on.is_hovered(window);
        if phase == DispatchPhase::Capture {
            // Chromium clears a document selection on a press elsewhere.
            if !inside && sel.read(cx).range().is_some() {
                sel.update(cx, |s, cx| {
                    s.set(0, 0);
                    s.dragging = false;
                    cx.notify();
                });
            }
            return;
        }
        if !inside || event.button != MouseButton::Left {
            return;
        }
        let offset = hit(event.position).pos.offset;
        let len = sel.read(cx).text.len() as u32;
        let (anchor, focus) = match event.click_count {
            1 => (offset, offset),
            2 => layout_down.word_at(offset),
            _ => (0, len),
        };
        if let Some(previous) = ACTIVE.with(|a| a.replace(Some(sel.downgrade()))).and_then(|w| w.upgrade())
            && previous != sel
        {
            previous.update(cx, |s, cx| {
                if s.set(0, 0) {
                    cx.notify();
                }
            });
        }
        sel.update(cx, |s, cx| {
            s.dragging = event.click_count == 1;
            if s.set(anchor, focus) {
                cx.notify();
            }
        });
    });
    let (sel, hit) = (selection.clone(), at);
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble || !sel.read(cx).dragging || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let focus = hit(event.position).pos.offset;
        sel.update(cx, |s, cx| {
            let anchor = s.anchor;
            if s.set(anchor, focus) {
                cx.notify();
            }
        });
    });
    let sel = selection.clone();
    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && sel.read(cx).dragging {
            sel.update(cx, |s, _| s.dragging = false);
        }
    });
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

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn a_selection_is_a_byte_range_of_the_text() {
        let mut s = TextSelection { text: "10.1.2.3 · Linux".into(), ..TextSelection::default() };
        assert_eq!(s.range(), None);
        s.set(8, 0);
        assert_eq!(s.range(), Some((0, 8)), "a drag backwards selects the same");
        assert_eq!(s.selected_text().as_deref(), Some("10.1.2.3"));
        s.set(3, 3);
        assert_eq!(s.selected_text(), None, "a caret selects nothing");
        s.set(0, 200);
        assert_eq!(s.range(), None, "past the text (it changed): nothing");
    }
}

// GPUI's Linux text system with the bundled fonts (the crate's other engine tests do the same).
#[cfg(all(test, any(target_os = "linux", target_os = "freebsd")))]
mod tests {
    use std::borrow::Cow;
    use std::sync::Arc;

    use gpui::{Hsla, TextSystem};

    use super::*;
    use crate::FontSpec;

    fn engine() -> Rc<Engine> {
        let platform = gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Decibell Inter");
        let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/static");
        let mut files: Vec<_> = std::fs::read_dir(fonts)
            .expect("gpui-client/assets/fonts/static")
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "ttf"))
            .collect();
        files.sort();
        let text_system = Arc::new(TextSystem::new(Arc::new(platform)));
        text_system.add_fonts(files.iter().map(|file| Cow::Owned(std::fs::read(file).unwrap())).collect()).unwrap();
        Engine::new(text_system)
    }

    /// A label with `emoji(size)` lays its emoji out as boxes of that size, each naming the emoji it
    /// shows; text between them stays text, and a label without it keeps every emoji as text.
    #[test]
    fn emoji_become_boxes_of_their_size() {
        if crate::emoji::packed_emoji_count() == 0 {
            eprintln!("SKIP: the Twemoji pack is empty (assets/twemoji missing at build time)");
            return;
        }
        let engine = engine();
        let face = engine.face(&FontSpec::new("Decibell Inter", 400));
        let style = TextStyle::new(face, 11., Hsla::default());
        let text = "🎮 playing 🙂 now";
        let with = label_layout(&engine, 1, text, &style, Wrapping::TRUNCATE, Some(13.), f32::INFINITY, 1.);
        let boxes: Vec<(u32, [f32; 4])> = with.boxes().collect();
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        for (tag, [_, _, width, height]) in &boxes {
            assert_eq!((*width, *height), (13., 13.));
            assert!(emoji_at(text, *tag).is_some(), "tag {tag:#x} names no emoji");
        }
        assert_eq!(emoji_at(text, boxes[0].0).map(|a| a.chars()), Some("🎮"));
        assert_eq!(emoji_at(text, boxes[1].0).map(|a| a.chars()), Some("🙂"));
        assert!(boxes[0].1[0] < boxes[1].1[0], "in reading order");
        let without = label_layout(&engine, 2, text, &style, Wrapping::TRUNCATE, None, f32::INFINITY, 1.);
        assert_eq!(without.boxes().count(), 0);
        // Cut short: the boxes past the cut aren't placed.
        let cut = label_layout(&engine, 1, text, &style, Wrapping::TRUNCATE, Some(13.), 40., 1.);
        assert!(cut.boxes().count() < 2, "a cut label places only the emoji before the \"…\"");
    }
}
