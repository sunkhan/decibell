//! The live send-preview (`MessagePreview.tsx`): the draft drawn exactly as a sent message's body,
//! by the message list's own row layout (`decibell_chat::row::RowLayout`: the same parser, the same
//! code block and formula chrome, the same heights), shown only while the draft has formatting
//! beyond plain text and autolinks (`decibell_richtext::composer::preview_visible`).
//!
//! [`DraftBody`] is an element: it lays the draft out at the width it is given (in GPUI's layout
//! pass, so its height is exact before anything is painted) and paints the row's body there. A row
//! layout is kept per (text, width, style), so a frame that only repaints shapes nothing.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use decibell_chat::row::{RowLayout, RowPaint, RowSpec};
use decibell_chat::style::ChatStyle;
use decibell_chat::SlotSpace;
use decibell_model::chat::{Message, RowFacts};
use decibell_model::stores::settings::TimeFormat;
use decibell_text::Engine;
use gpui::{
    App, AvailableSpace, Bounds, ContentMask, Element, ElementId, EntityId, GlobalElementId, InspectorElementId, LayoutId,
    Pixels, SharedString, Size, Style, Window, point, px, relative, size,
};

/// The row's content column starts this far in (`ROW_PAD_LEFT + AVATAR + GUTTER`); its right
/// padding is `ROW_PAD_RIGHT`.
const CONTENT_X: f32 = decibell_chat::style::ROW_PAD_LEFT + decibell_chat::style::AVATAR + decibell_chat::style::GUTTER;
const RIGHT: f32 = decibell_chat::style::ROW_PAD_RIGHT;

/// A draft's body at a width: the row layout and the body's box in it.
struct Built {
    text: SharedString,
    width: f32,
    style_key: u64,
    layout: Rc<RowLayout>,
}

/// The cache a composer keeps for its preview (one draft at a time).
#[derive(Default, Clone)]
pub struct PreviewCache(Rc<RefCell<Option<Built>>>);

impl PreviewCache {
    /// Drops the kept layout (the strip went away).
    pub fn clear(&self) {
        self.0.borrow_mut().take();
    }

    fn layout(&self, text: &SharedString, width: f32, style: &Rc<ChatStyle>, view: Option<EntityId>, cx: &mut App) -> Rc<RowLayout> {
        if let Some(built) = self.0.borrow().as_ref()
            && built.text == *text
            && built.width == width
            && built.style_key == style.key
        {
            return built.layout.clone();
        }
        let message = Arc::new(Message {
            id: 0,
            nonce: None,
            channel: SharedString::default(),
            sender: SharedString::default(),
            sender_uid: 0,
            content: text.clone(),
            timestamp: None,
            day: 0,
            attachments: Arc::from(Vec::new()),
            pending_attachments: Arc::from(Vec::new()),
            edited_at: 0,
            reply: None,
            pending: false,
            sent_after_id: 0,
            encrypted: false,
            decrypt_error: None,
            suppress_embeds: false,
            preview: false,
        });
        // A grouped row: no header, no avatar; only the body (and its code / math blocks).
        let facts = RowFacts { grouped: true, ..RowFacts::default() };
        let spec = RowSpec {
            message: &message,
            facts,
            server: None,
            display_name: SharedString::default(),
            user_name: SharedString::default(),
            name_color: style.text_primary,
            avatar_color: style.text_primary,
            reply: None,
            reply_colors: (style.text_primary, style.text_primary),
            expanded: 0,
            text_hidden: false,
            editing: false,
            editor_height: 0.,
            today: 0,
            time_format: TimeFormat::Auto,
            space: SlotSpace { width, view_width: 0., view_height: 0., scale: style.scale },
            slot_heights: [None, None, None],
        };
        let engine = Engine::global(cx);
        // A code block's colours arrive later when highlighting is slow: draw the strip again.
        let ready = move || -> Box<dyn FnOnce(&mut App)> {
            Box::new(move |cx: &mut App| {
                if let Some(view) = view {
                    cx.notify(view);
                }
            })
        };
        let layout = Rc::new(RowLayout::build(&spec, width + CONTENT_X + RIGHT, style, &engine, cx, &ready));
        *self.0.borrow_mut() = Some(Built { text: text.clone(), width, style_key: style.key, layout: layout.clone() });
        layout
    }
}

/// The draft as a message body, as wide as its parent gives it, as tall as its content.
pub struct DraftBody {
    text: SharedString,
    style: Rc<ChatStyle>,
    cache: PreviewCache,
    view: Option<EntityId>,
}

impl DraftBody {
    /// `view`: the entity to notify when a code block's colours arrive.
    pub fn new(text: SharedString, style: Rc<ChatStyle>, cache: PreviewCache, view: Option<EntityId>) -> Self {
        Self { text, style, cache, view }
    }
}

impl Element for DraftBody {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, _: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let (text, chat_style, cache, view) = (self.text.clone(), self.style.clone(), self.cache.clone(), self.view);
        let id = window.request_measured_layout(style, move |known: Size<Option<Pixels>>, available: Size<AvailableSpace>, _, cx| {
            let width = known.width.map(f32::from).unwrap_or(match available.width {
                AvailableSpace::Definite(width) => f32::from(width),
                _ => 400.,
            });
            let layout = cache.layout(&text, width.max(1.), &chat_style, view, cx);
            size(px(width), px(layout.body_bounds()[3]))
        });
        (id, ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        let width = f32::from(bounds.size.width).max(1.);
        let layout = self.cache.layout(&self.text, width, &self.style, self.view, cx);
        let body = layout.body_bounds();
        let origin = point(bounds.origin.x - px(body[0]), bounds.origin.y - px(body[1]));
        let row_width = width + CONTENT_X + RIGHT;
        let style = self.style.clone();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            layout.paint(origin, row_width, &RowPaint::default(), &style, window, cx);
        });
    }
}

/// The preview strip's style for the window's scale, kept per (theme, scale) by the caller.
pub fn chat_style(scale: f32, cx: &mut App) -> Rc<ChatStyle> {
    ChatStyle::new(scale, cx)
}
