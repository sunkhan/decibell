//! The inflight slot (`BubbleInflightAttachments.tsx`): in our own optimistic bubble, one strip
//! per upload still in the composer's store: the preview (pictures) or the file glyph, the name,
//! a progress bar and the status. When the echo arrives the bubble stops naming its pendings and
//! the attachment list takes over.
//!
//! Progress ticks redraw the lists showing the bubble (no re-measure); a pending appearing or
//! going changes the slot's height, which re-measures the rows (`invalidate_slots`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use decibell_attachments::AttachmentKind;
use decibell_chat::{RowSlot, SlotSpace};
use decibell_model::chat::Message;
use decibell_model::chat::composer::{ComposerEvent, PendingAttachment, PendingStatus};
use decibell_model::{Model, ServerId};
use decibell_ui::{ActiveTheme as _, Icon, IconName};
use gpui::{AnyElement, App, Corners, EntityId, SharedString, Subscription, Window, div, prelude::*, px};

use crate::pictures::{Fit, PicKey, Pictures, Source, Spec};
use crate::style::{ContentStyle, label};
use crate::tile::Tile;

/// `mt-2 flex flex-col gap-2` around the strips.
const MARGIN: f32 = 8.;
const GAP: f32 = 8.;
/// `max-w-[420px]`.
const MAX_W: f32 = 420.;

/// The strips a message shows now: its pendings still in the composer's store.
fn visible<'a>(message: &Message, pendings: &'a decibell_model::chat::composer::ComposerAttachments) -> Vec<&'a PendingAttachment> {
    message.pending_attachments.iter().filter_map(|id| pendings.get(id)).collect()
}

fn item_height(p: &PendingAttachment, style: &ContentStyle) -> f32 {
    let preview: f32 = if p.kind == AttachmentKind::Image && p.preview.is_some() { 48. } else { 40. };
    let text = style.line(&style.file_name) + 4. + 4. + 4. + style.line(&style.upload_status);
    2. + 24. + preview.max(text)
}

/// The slot. See the module doc.
pub struct InflightSlot {
    /// Pending ids each list shows (from `near`), to redraw it on progress.
    shown: Rc<RefCell<HashMap<EntityId, HashSet<SharedString>>>>,
    _subscription: Option<Subscription>,
}

impl InflightSlot {
    pub fn new(cx: &mut App) -> InflightSlot {
        let shown: Rc<RefCell<HashMap<EntityId, HashSet<SharedString>>>> = Rc::default();
        let known: Rc<RefCell<HashSet<SharedString>>> = Rc::default();
        let subscription = cx.try_global::<Model>().cloned().map(|model| {
            let (shown, known) = (shown.clone(), known.clone());
            cx.subscribe(&model.chat.composer, move |composer, _: &ComposerEvent, cx| {
                let now: HashSet<SharedString> = composer.read(cx).iter().map(|p| p.id.clone()).collect();
                let gone = known.borrow().iter().any(|id| !now.contains(id));
                *known.borrow_mut() = now;
                if gone {
                    decibell_chat::invalidate_slots(cx);
                    return;
                }
                let lists: Vec<EntityId> = shown.borrow().iter().filter(|(_, ids)| !ids.is_empty()).map(|(list, _)| *list).collect();
                for list in lists {
                    cx.notify(list);
                }
            })
        });
        InflightSlot { shown, _subscription: subscription }
    }
}

impl RowSlot for InflightSlot {
    fn height(&self, message: &Message, _: Option<ServerId>, space: SlotSpace, cx: &App) -> f32 {
        if message.pending_attachments.is_empty() {
            return 0.;
        }
        let Some(model) = cx.try_global::<Model>() else { return 0. };
        let composer = model.chat.composer.read(cx);
        let items = visible(message, composer);
        if items.is_empty() {
            return 0.;
        }
        let style = ContentStyle::get(cx, space.scale);
        MARGIN + items.iter().map(|p| item_height(p, &style)).sum::<f32>() + GAP * (items.len() - 1) as f32
    }

    fn element(&self, message: &Arc<Message>, _: Option<ServerId>, space: SlotSpace, _: &mut Window, cx: &mut App) -> AnyElement {
        let Some(model) = cx.try_global::<Model>().cloned() else { return div().into_any_element() };
        let style = ContentStyle::get(cx, space.scale);
        let items: Vec<PendingAttachment> = visible(message, model.chat.composer.read(cx)).into_iter().cloned().collect();
        let mut root = div().w(px(space.width)).pt(px(MARGIN)).flex().flex_col().gap(px(GAP));
        for p in &items {
            root = root.child(strip(p, space, &style, cx));
        }
        root.into_any_element()
    }

    fn near(&self, list: EntityId, _: Option<ServerId>, _: SlotSpace, near: &[Arc<Message>], _: std::ops::Range<usize>, _: &mut App) {
        let ids: HashSet<SharedString> = near.iter().flat_map(|m| m.pending_attachments.iter().cloned()).collect();
        let mut shown = self.shown.borrow_mut();
        if ids.is_empty() {
            shown.remove(&list);
        } else {
            shown.insert(list, ids);
        }
    }
}

/// The preview of a pending picture through the pipeline (decoded from the composer's 320 px
/// JPEG, which is handed over as a local file).
pub fn preview_key(image: &Arc<gpui::Image>, size: f32, scale: f32, cx: &mut App) -> Option<PicKey> {
    let pictures = Pictures::global(cx)?;
    let source = Source::Local(image.id);
    let bytes = bytes::Bytes::copy_from_slice(&image.bytes);
    pictures.update(cx, |p, _| p.provide(source.clone(), bytes));
    Some(PicKey::new(source, Spec { width: size, height: size, scale, fit: Fit::Cover, upscale: true }))
}

fn strip(p: &PendingAttachment, space: SlotSpace, style: &Rc<ContentStyle>, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let (bg, border, light, accent, error, muted) =
        (theme.color_bg_secondary, theme.color_border, theme.color_bg_light, theme.color_accent, theme.color_error, theme.color_text_muted);
    let (radius_lg, radius_md) = (theme.radius_lg, theme.radius_md);
    let pct = if p.total_bytes > 0 { ((p.transferred_bytes as f64 / p.total_bytes as f64) * 100.).round().min(100.) as u32 } else { 0 };
    let status: SharedString = match p.status {
        PendingStatus::Failed => p.error.clone().unwrap_or_else(|| SharedString::new_static("Upload failed")),
        PendingStatus::Ready => SharedString::new_static("Ready"),
        _ if p.paused => format!("Paused · {pct}%").into(),
        _ => format!("{pct}%").into(),
    };
    let fill = if p.status == PendingStatus::Failed {
        error
    } else if p.paused {
        muted
    } else {
        accent
    };
    let picture = (p.kind == AttachmentKind::Image).then(|| p.preview.clone()).flatten();
    let preview: AnyElement = match picture.and_then(|image| preview_key(&image, 48., space.scale, cx)) {
        Some(key) => {
            let tile = Tile { key: Some(key), placeholder: SharedString::default(), corners: Corners::all(radius_md), background: None, alt: SharedString::default(), veil: None };
            div().size_12().flex_none().opacity(0.7).child(tile.element()).into_any_element()
        }
        None => div()
            .size_10()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius_md)
            .bg(light)
            .child(Icon::new(IconName::TransfersKindDocument).size(px(20.)).color(muted))
            .into_any_element(),
    };
    let width = space.width.min(MAX_W);
    div()
        .w(px(width))
        .h(px(item_height(p, style)))
        .flex()
        .items_center()
        .gap_3()
        .p_3()
        .rounded(radius_lg)
        .border_1()
        .border_color(border)
        .bg(bg)
        .child(preview)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(label(p.filename.clone(), &style.file_name, decibell_text::Wrapping::TRUNCATE))
                .child(
                    div()
                        .mt_1()
                        .h_1()
                        .w_full()
                        .rounded_full()
                        .bg(light)
                        .child(div().h_full().w(gpui::relative(pct as f32 / 100.)).rounded_full().bg(fill)),
                )
                .child(div().mt_1().child(label(status, &style.upload_status, decibell_text::Wrapping::TRUNCATE))),
        )
        .into_any_element()
}
