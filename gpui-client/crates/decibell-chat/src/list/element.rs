//! The list's element: one box that paints the visible rows itself.
//!
//! `prepaint` brings the view's state up to date for this frame (rows in step with the slice,
//! the viewport's rows measured, the position); `paint` draws the rows that meet the viewport
//! straight from their cached layouts, the scrollbar, and registers the list's four pointer
//! listeners. Rows are not elements: nothing is built per row per frame. The only elements made
//! here are the ones other crates put into a row's slots (attachments, embeds, the editor).

use std::time::Instant;

use decibell_model::Model;
use decibell_text::PaintOptions;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId, Entity, GlobalElementId,
    Hitbox, HitboxBehavior, InspectorElementId, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    ScrollEvent, Style, Window, fill, point, px, relative, size,
};

use super::view::MessageList;
use crate::media::{SlotSpace, slots};
use crate::row::{RowPaint, Target};
use crate::style::{BAR, ChatStyle};

/// The smallest thumb (the kit's `ScrollArea`, which measured Chromium's).
const MIN_THUMB: f32 = 20.;

pub struct ListElement {
    view: Entity<MessageList>,
}

impl ListElement {
    pub fn new(view: Entity<MessageList>) -> Self {
        Self { view }
    }
}

pub struct ListPrepaint {
    hitbox: Hitbox,
    /// Elements other crates draw in rows' slots, already laid out at their boxes.
    slots: Vec<AnyElement>,
}

/// The bar in the list's viewport, px (the kit's `BarGeometry`: square arrow zones, the thumb
/// in between).
#[derive(Clone, Copy, Debug)]
pub struct BarGeometry {
    pub track_top: f32,
    pub track_len: f32,
    pub thumb_top: f32,
    pub thumb_len: f32,
}

impl BarGeometry {
    pub fn new(height: f32, content: f32, progress: f32) -> Self {
        let track_len = (height - 2. * BAR).max(0.);
        let thumb_len = (track_len * height / content.max(1.)).round().max(MIN_THUMB).min(track_len);
        Self { track_top: BAR, track_len, thumb_top: BAR + ((track_len - thumb_len) * progress).round(), thumb_len }
    }

    pub(super) fn of(list: &MessageList) -> Self {
        let (min, max) = list.core.scroller.range();
        let progress = if max > min { ((list.drawn.position - min) / (max - min)).clamp(0., 1.) } else { 0. };
        Self::new(list.core.viewport_height as f32, list.core.content_height() as f32, progress as f32)
    }

    /// The share of the scroll range (0–1) that puts the thumb's top at `thumb_top`.
    pub fn position_for(&self, thumb_top: f32) -> f32 {
        let travel = self.track_len - self.thumb_len;
        if travel <= 0. { 0. } else { ((thumb_top - self.track_top) / travel).clamp(0., 1.) }
    }
}

/// The box the list reserves for the editor until the text-input crate fills the slot:
/// `InlineEditor`'s one-line textarea (`border px-3 py-2`, the body's line) and its hint line
/// (`mt-1 text-meta`).
pub(crate) fn placeholder_editor_height(style: &ChatStyle) -> f32 {
    let line = crate::style::layout_unit(style.rich.size * style.rich.leading);
    let hint = crate::style::layout_unit(style.time.size * 1.5);
    2. + 16. + line + 4. + hint
}

impl Element for ListElement {
    type RequestLayoutState = ();
    type PrepaintState = ListPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> ListPrepaint {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let slots = self.view.update(cx, |list, cx| {
            list.frame_prepaint(bounds, window, cx);
            list.after_autoscroll(cx);
            let slots = list.slot_elements(window, cx);
            if let Some(bench) = &mut list.bench {
                bench.phase(0);
            }
            slots
        });
        ListPrepaint { hitbox, slots }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), prepaint: &mut ListPrepaint, window: &mut Window, cx: &mut App) {
        let started = Instant::now();
        let hitbox = &prepaint.hitbox;
        self.view.update(cx, |list, cx| {
            if let Some(bench) = &mut list.bench {
                bench.phase(3);
            }
            // The content moved under a resting pointer: what it is over may have changed.
            if hitbox.is_hovered(window) {
                let position = window.mouse_position();
                list.update_hover(position, true, cx);
            }
            list.frame_paint(bounds, hitbox, window, cx);
            if let Some(bench) = &mut list.bench {
                bench.phase(1);
            }
        });
        for slot in &mut prepaint.slots {
            slot.paint(window, cx);
        }

        // Four listeners for the whole list (one boxed closure each: what GPUI needs to route
        // an event to an element).
        let view = self.view.clone();
        let target = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |list, cx| list.mouse_move(event, &target, window, cx));
            }
        });
        let view = self.view.clone();
        let target = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |list, cx| list.mouse_down(event, &target, window, cx));
            }
        });
        let view = self.view.clone();
        let target = hitbox.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |list, cx| list.mouse_up(event, &target, window, cx));
            }
        });
        let view = self.view.clone();
        let target = hitbox.clone();
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |list, cx| list.scroll_wheel(event, &target, window, cx));
            }
        });
        self.view.update(cx, |list, _| {
            list.stats.paint_us = started.elapsed().as_micros() as u32;
            if let Some(bench) = &mut list.bench {
                bench.phase(2);
                bench.frame_painted(list.stats.prepaint_us, list.stats.paint_us, list.stats.layouts, std::mem::take(&mut list.stats.between_us));
            }
        });
    }
}

impl MessageList {
    /// Screen origin of row `index` for this frame: whole device pixels.
    fn row_origin(&self, index: usize, bounds: Bounds<Pixels>, scale: f32) -> gpui::Point<Pixels> {
        let device = (self.core.top(index) * scale as f64).round() - (self.drawn.position * scale as f64).round();
        point(bounds.left(), bounds.top() + px(device as f32 / scale + self.drawn.slide))
    }

    /// The elements of filled slots for the rows on screen (none while no crate registered a
    /// slot and nothing is edited: the common frame builds no element at all).
    fn slot_elements(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> Vec<AnyElement> {
        let registry = slots(cx);
        let providers = [registry.attachments.clone(), registry.inflight.clone(), registry.embeds.clone()];
        let editor = registry.editor.clone();
        if providers.iter().all(Option::is_none) && self.editing.is_none() {
            return Vec::new();
        }
        let Some(style) = self.style.clone() else { return Vec::new() };
        let server = self.source.server();
        let (bounds, width) = (self.bounds, self.width);
        let space = SlotSpace { width, view_width: 0., view_height: 0., scale: style.scale };
        let mut elements = Vec::new();
        for index in self.drawn.first..self.drawn.last.min(self.core.len()) {
            let entry = &self.core.entries[index];
            let Some(layout) = entry.layout.clone() else { continue };
            let origin = self.row_origin(index, bounds, style.scale);
            let mut place = |mut element: AnyElement, b: [f32; 4], window: &mut Window, cx: &mut App| {
                let space = size(AvailableSpace::Definite(px(b[2])), AvailableSpace::Definite(px(b[3])));
                element.prepaint_as_root(point(origin.x + px(b[0]), origin.y + px(b[1])), space, window, cx);
                elements.push(element);
            };
            for (slot, provider) in providers.iter().enumerate() {
                if let (Some(provider), Some(b)) = (provider, layout.slot_bounds(slot)) {
                    let space = SlotSpace { width: b[2], ..space };
                    place(provider.element(&entry.message, server, space, window, cx), b, window, cx);
                }
            }
            if let (Some(b), Some(editor)) = (layout.editor_bounds(), &editor) {
                let view = cx.weak_entity();
                let submit: crate::media::SubmitEdit = {
                    let view = view.clone();
                    std::rc::Rc::new(move |text: String, _: &mut Window, cx: &mut App| {
                        let _ = view.update(cx, |list, cx| list.submit_edit(&text, cx));
                    })
                };
                let cancel: crate::media::CancelEdit = std::rc::Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = view.update(cx, |list, cx| list.cancel_edit(cx));
                });
                place(editor.element(&entry.message, b[2], submit, cancel, window, cx), b, window, cx);
            }
        }
        elements
    }

    pub(super) fn frame_paint(&mut self, bounds: Bounds<Pixels>, hitbox: &Hitbox, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(style) = self.style.clone() else { return };
        let scale = style.scale;
        let width = self.width;
        let can_delete_others = self.source.can_delete_others(cx);
        let slots_filled = cx.try_global::<crate::media::ChatSlots>().is_some_and(|slots| slots.attachments.is_some());
        let has_editor = cx.try_global::<crate::media::ChatSlots>().is_some_and(|slots| slots.editor.is_some());
        let avatars = cx.try_global::<Model>().map(|model| model.live.presence.avatars.clone());

        // The cursor: what the pointer is over.
        match self.hover.map(|(_, target)| target) {
            Some(Target::Text) => window.set_cursor_style(CursorStyle::IBeam, hitbox),
            Some(Target::Link(_) | Target::Sender | Target::ReplyLine | Target::ReplyName | Target::Action(_) | Target::CodeCopy(_) | Target::CodeToggle(_) | Target::Media) => {
                window.set_cursor_style(CursorStyle::PointingHand, hitbox)
            }
            _ => {}
        }

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for index in self.drawn.first..self.drawn.last.min(self.core.len()) {
                let entry = &self.core.entries[index];
                let Some(layout) = &entry.layout else { continue };
                let origin = self.row_origin(index, bounds, scale);
                let hover = self.hover.filter(|(row, _)| *row == index);
                // Scroll offsets of this row's code blocks, if any was scrolled (rare).
                let mut code_scroll = [(0u32, 0f32, 0f32); 4];
                let mut scrolled = 0;
                if !self.code_scroll.is_empty() && !layout.codes.is_empty() {
                    for (key, span, x, y) in &self.code_scroll {
                        if key == &entry.key && scrolled < code_scroll.len() {
                            code_scroll[scrolled] = (*span, *x, *y);
                            scrolled += 1;
                        }
                    }
                }
                // A sender's picture, once the avatar store has decoded it.
                let avatar = match (&avatars, layout.grouped) {
                    (Some(avatars), false) => avatars.read(cx).image(layout.sender()).cloned().and_then(|image| image.use_render_image(window, cx)),
                    _ => None,
                };
                let paint = RowPaint {
                    hovered: hover.is_some(),
                    target: hover.map_or(Target::None, |(_, target)| target),
                    highlighted: self.highlighted == Some(entry.message.id) && entry.message.id > 0,
                    reply_target: self.reply_target == Some(entry.message.id) && entry.message.id > 0,
                    can_delete: entry.facts.can_delete(can_delete_others),
                    selection: self.selection_in(index),
                    copied: self.copied.as_ref().filter(|(key, _)| key == &entry.key).map(|(_, span)| *span),
                    code_scroll: &code_scroll[..scrolled],
                    avatar: avatar.as_ref(),
                    reply_avatar: None,
                    slots_filled,
                };
                layout.paint(origin, width, &paint, &style, window, cx);
                if let (Some(b), false) = (layout.editor_bounds(), has_editor) {
                    paint_placeholder_editor(&entry.message.content, point(origin.x + px(b[0]), origin.y + px(b[1])), b[2], &style, window, cx);
                }
            }
        });

        // The scrollbar: the kit's look (the thumb 2 device px in, the arrows as rows of pixels).
        if self.core.content_height() > self.core.viewport_height + 0.5 {
            let geometry = BarGeometry::of(self);
            let device = |value: f32| px(value / scale);
            let thickness = (BAR * scale).round();
            let left = bounds.right() - device(thickness);
            window.paint_quad(fill(
                Bounds { origin: point(left + device(2.), bounds.top() + px(geometry.thumb_top)), size: size(device(thickness - 4.), px(geometry.thumb_len)) },
                style.scrollbar,
            ));
            let rows = ((thickness + 1.) / 4.).floor();
            let first_row = ((thickness - rows + 1.) / 2.).ceil();
            let first_width = if thickness as i32 % 2 == 0 { 2. } else { 1. };
            for row in 0..rows as i32 {
                let row_width = first_width + 2. * row as f32;
                let x = left + device((thickness - row_width) / 2.);
                for y in [bounds.top() + device(first_row + row as f32), bounds.bottom() - device(first_row + row as f32 + 1.)] {
                    window.paint_quad(fill(Bounds { origin: point(x, y), size: size(device(row_width), device(1.)) }, style.scrollbar));
                }
            }
        }

        // A scroll came to rest: layouts far from the viewport are let go.
        if !self.core.scroller.is_animating() {
            self.release_far_layouts();
        }
    }
}

/// `InlineEditor` as a still: the bordered field with the message's text and the hint under it.
fn paint_placeholder_editor(text: &str, origin: gpui::Point<Pixels>, width: f32, style: &ChatStyle, window: &mut Window, cx: &mut App) {
    use gpui::{BorderStyle, Corners, Edges, quad};
    let engine = decibell_text::Engine::global(cx);
    let body = style.rich.body(&engine);
    let line = engine.label_layout(text, &body, decibell_text::Wrapping::TRUNCATE, (width - 26.).max(1.), style.scale);
    let field = Bounds::new(origin, size(px(width), px(18. + line.height())));
    window.paint_quad(quad(field, Corners::all(style.radius_md), style.bg_light, Edges::all(px(1.)), style.accent, BorderStyle::default()));
    line.paint(point(origin.x + px(13.), origin.y + px(9.)), &PaintOptions::default(), window, cx);
    let hint = engine.label_layout("escape to cancel • enter to save", &style.time, decibell_text::Wrapping::NOWRAP, f32::INFINITY, style.scale);
    let options = PaintOptions { color: Some(style.text_muted), ..PaintOptions::default() };
    hint.paint(point(origin.x, origin.y + px(18. + line.height() + 4.)), &options, window, cx);
}
