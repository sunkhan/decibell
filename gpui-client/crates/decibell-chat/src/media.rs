//! The places in a row that other crates fill: attachments, uploads in flight, link embeds
//! (invite cards included), the inline editor, and the pane's composer.
//!
//! A row's height is known before it is painted, so a slot answers two questions: **how tall**
//! it is for a message at a width (final: it must not depend on bytes that are still loading),
//! and **what to draw** in the box that was reserved. Until a crate registers a slot the list
//! reserves the same box itself (the sizing rules of `attachmentSizing.ts` /
//! `AttachmentList.tsx`) and draws a neutral placeholder, so plugging the real content in later
//! moves nothing.
//!
//! ```ignore
//! // in decibell-attachments-ui's init(cx):
//! decibell_chat::slots(cx).attachments = Some(Rc::new(MyAttachments));
//! // …and when something a height depends on changed (a preview resolved, an upload ended):
//! decibell_chat::invalidate_slots(cx);
//! ```

use std::rc::Rc;
use std::sync::Arc;

use decibell_attachments::AttachmentKind;
use decibell_model::ServerId;
use decibell_model::chat::Message;
use gpui::{AnyElement, AnyView, App, AppContext as _, Entity, Global, KeyDownEvent, Window};

/// What a slot is sized against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SlotSpace {
    /// The width of the row's content column, px.
    pub width: f32,
    /// The chat view (`chatViewSize`: the pane's width, its height less a resting composer),
    /// which the renderer scales media against. Zero when unknown.
    pub view_width: f32,
    pub view_height: f32,
    pub scale: f32,
}

/// One place in a row. See the module doc.
pub trait RowSlot: 'static {
    /// The slot's height for `message` (px, its own margins included); 0 draws nothing.
    fn height(&self, message: &Message, server: Option<ServerId>, space: SlotSpace, cx: &App) -> f32;
    /// The element for the reserved box (laid out at `space.width` × the height answered above).
    fn element(&self, message: &Arc<Message>, server: Option<ServerId>, space: SlotSpace, window: &mut Window, cx: &mut App) -> AnyElement;
}

/// The editor's `submit(text)` and `cancel()`.
pub type SubmitEdit = Rc<dyn Fn(String, &mut Window, &mut App)>;
pub type CancelEdit = Rc<dyn Fn(&mut Window, &mut App)>;

/// The inline editor that replaces a message's content while it is edited (`InlineEditor`).
pub trait EditorSlot: 'static {
    /// Height at `width` (px, at the window's `scale`) for the text being edited (the list asks
    /// again after [`invalidate_slots`], which the editor calls when its text wraps differently).
    fn height(&self, message: &Message, width: f32, scale: f32, cx: &App) -> f32;
    /// `submit(text)` saves, `cancel()` leaves edit mode.
    fn element(
        &self,
        message: &Arc<Message>,
        width: f32,
        submit: SubmitEdit,
        cancel: CancelEdit,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement;
}

/// What the composer slot builds for a channel: the view under the list, and its "type anywhere
/// to compose" hook (`useTypeToFocusComposer`), which the pane calls with the keys that reach it
/// while no text field has focus; it returns whether the composer took the key.
pub struct ComposerParts {
    pub view: AnyView,
    pub type_to_focus: Rc<dyn Fn(&KeyDownEvent, &mut Window, &mut App) -> bool>,
}

/// Builds the composer for a channel, given the list it sits under (the composer follows the
/// list's [`crate::ListEvent::Reply`] and asks it to [`crate::MessageList::start_edit`]).
pub type ComposerBuilder =
    Rc<dyn Fn(ServerId, decibell_model::ChannelId, &Entity<crate::MessageList>, &mut Window, &mut App) -> ComposerParts>;

/// The registered slots. Unset ones fall back to the list's own placeholders.
#[derive(Default)]
pub struct ChatSlots {
    /// `AttachmentList`.
    pub attachments: Option<Rc<dyn RowSlot>>,
    /// `BubbleInflightAttachments` (a pending bubble's uploads).
    pub inflight: Option<Rc<dyn RowSlot>>,
    /// `LinkEmbeds` (link previews, media links, invite cards). Not drawn while editing.
    pub embeds: Option<Rc<dyn RowSlot>>,
    pub editor: Option<Rc<dyn EditorSlot>>,
    pub composer: Option<ComposerBuilder>,
    /// Bumped by [`invalidate_slots`]; lists observe it.
    pub(crate) epoch: Option<Entity<SlotEpoch>>,
}

impl Global for ChatSlots {}

/// Notified when a slot's answer may have changed.
pub struct SlotEpoch(pub u64);

/// The slot registry (created on first use).
pub fn slots(cx: &mut App) -> &mut ChatSlots {
    cx.default_global::<ChatSlots>()
}

pub(crate) fn slot_epoch(cx: &mut App) -> Entity<SlotEpoch> {
    if let Some(epoch) = slots(cx).epoch.clone() {
        return epoch;
    }
    let epoch = cx.new(|_| SlotEpoch(0));
    slots(cx).epoch = Some(epoch.clone());
    epoch
}

/// A slot's height for some message may have changed: every open list measures its rows with
/// slots again (the row being read keeps its place).
pub fn invalidate_slots(cx: &mut App) {
    slot_epoch(cx).update(cx, |epoch, cx| {
        epoch.0 += 1;
        cx.notify();
    });
}

// ── The sizing rules the placeholders reserve (attachmentSizing.ts) ─────────────────────────────

const IMAGE_MAX_W: f32 = 550.;
const IMAGE_MAX_H: f32 = 350.;
const GIF_MAX_W: f32 = 440.;
const GIF_MAX_H: f32 = 280.;
const FALLBACK_W: f32 = 260.;
const FALLBACK_H: f32 = 180.;
const H_RESERVE: f32 = 80.;
const V_RESERVE: f32 = 60.;
/// `MEDIA_ACTION_RESERVE_PX`: the download button beside a media box.
const ACTION_RESERVE: f32 = 38.;
const GRID_GAP: f32 = 4.;
const GRID_ROW: f32 = 180.;
const GRID_MAX_W: f32 = 540.;
const GRID_MIN_W: f32 = 320.;
/// The download button (`h-8 w-8`), `gap-1.5` from the media.
const DOWNLOAD: f32 = 32.;
/// `mt-1 flex flex-col gap-1` around the list; `mt-2` on a grid / video card.
const LIST_MARGIN: f32 = 4.;
const LIST_GAP: f32 = 4.;
const MEDIA_MARGIN: f32 = 8.;

fn max_image_width(view_width: f32, gif: bool, reserve: f32) -> f32 {
    let cap = if gif { GIF_MAX_W } else { IMAGE_MAX_W };
    cap.min(18. * view_width.sqrt()).min(view_width - H_RESERVE - reserve).max(120.)
}

fn max_image_height(view_height: f32, gif: bool) -> f32 {
    let cap = if gif { GIF_MAX_H } else { IMAGE_MAX_H };
    cap.min(16. * view_height.sqrt()).min(view_height - V_RESERVE).max(120.)
}

/// JavaScript's `Math.round` (halves go up).
fn js_round(value: f32) -> f32 {
    (value + 0.5).floor()
}

/// `reserveBox`: the box an image of `w` × `h` gets in a view.
pub fn reserve_box(w: u32, h: u32, gif: bool, space: SlotSpace) -> (f32, f32) {
    if w == 0 || h == 0 {
        return (FALLBACK_W, FALLBACK_H);
    }
    let (w, h) = (w as f32, h as f32);
    let known = space.view_width > 0.;
    let max_w = if known { max_image_width(space.view_width, gif, ACTION_RESERVE) } else if gif { GIF_MAX_W } else { IMAGE_MAX_W };
    let max_h = if known { max_image_height(space.view_height, gif) } else if gif { GIF_MAX_H } else { IMAGE_MAX_H };
    let scale = (max_w / w).min(max_h / h).min(1.);
    (js_round(w * scale).max(1.), js_round(h * scale).max(1.))
}

/// `gridRowCounts`.
pub fn grid_rows(n: usize) -> &'static [usize] {
    match n {
        2 => &[2],
        3 => &[3],
        4 => &[2, 2],
        5 => &[2, 3],
        6 => &[3, 3],
        7 => &[1, 3, 3],
        8 => &[2, 3, 3],
        9 => &[3, 3, 3],
        10 => &[1, 3, 3, 3],
        _ => &[],
    }
}

fn grid_width(view_width: f32) -> f32 {
    if view_width <= 0. {
        return GRID_MAX_W;
    }
    let cap = max_image_width(view_width, false, ACTION_RESERVE);
    let room = view_width - H_RESERVE - ACTION_RESERVE;
    GRID_MAX_W.min(GRID_MIN_W.min(room).max(cap))
}

/// One box the placeholder draws, relative to the slot's top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub kind: MediaBoxKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaBoxKind {
    /// A picture (or a grid cell): `rounded-lg border bg-bg-secondary`.
    Image,
    /// The grid's frame.
    Grid,
    /// A card (video, audio, document, a purged attachment): approximate until the attachments
    /// crate fills the slot.
    Card,
    /// The download button beside the media.
    Download,
}

/// The attachment list's box for `message` as `AttachmentList.tsx` lays it out, with the
/// placeholder boxes to draw. Exact for pictures and grids of pictures; cards for the other
/// kinds are the renderer's resting sizes (a video card's height follows its poster).
pub fn attachment_boxes(message: &Message, space: SlotSpace, out: &mut Vec<MediaBox>) -> f32 {
    if message.attachments.is_empty() {
        return 0.;
    }
    let mut sorted: Vec<&decibell_model::chat::Attachment> = message.attachments.iter().collect();
    sorted.sort_by_key(|a| a.position);
    let (grid, rest): (Vec<_>, Vec<_>) = sorted
        .into_iter()
        .partition(|a| a.purged_at == 0 && matches!(a.kind, AttachmentKind::Image | AttachmentKind::Video));
    let mut y = LIST_MARGIN;
    let mut first = true;
    let gap = |y: &mut f32, first: &mut bool| {
        if !*first {
            *y += LIST_GAP;
        }
        *first = false;
    };
    if grid.len() >= 2 && !grid_rows(grid.len()).is_empty() {
        gap(&mut y, &mut first);
        let width = grid_width(space.view_width);
        let rows = grid_rows(grid.len());
        let height = rows.len() as f32 * GRID_ROW + (rows.len() - 1) as f32 * GRID_GAP + 2.;
        let top = y + MEDIA_MARGIN;
        out.push(MediaBox { x: 0., y: top, width, height, kind: MediaBoxKind::Grid });
        let mut row_y = top + 1.;
        for cols in rows {
            let cell = (width - 2. - (*cols - 1) as f32 * GRID_GAP) / *cols as f32;
            for col in 0..*cols {
                out.push(MediaBox {
                    x: 1. + col as f32 * (cell + GRID_GAP),
                    y: row_y,
                    width: cell,
                    height: GRID_ROW,
                    kind: MediaBoxKind::Image,
                });
            }
            row_y += GRID_ROW + GRID_GAP;
        }
        out.push(MediaBox { x: width + 6., y: top + (height - DOWNLOAD) / 2., width: DOWNLOAD, height: DOWNLOAD, kind: MediaBoxKind::Download });
        y = top + height.max(DOWNLOAD);
    } else {
        for attachment in &grid {
            gap(&mut y, &mut first);
            let gif = attachment.mime.as_ref() == "image/gif";
            let (width, height) = reserve_box(attachment.width, attachment.height, gif, space);
            if attachment.kind == AttachmentKind::Video {
                // The video card: `mt-2`, the poster box, a caption bar (`py-2`, 10.5 px text).
                let top = y + MEDIA_MARGIN;
                let card = height + 2. + 16. + 15.75;
                out.push(MediaBox { x: 0., y: top, width, height: card, kind: MediaBoxKind::Card });
                y = top + card.max(DOWNLOAD);
            } else {
                // `flex items-center` with the 32 px download button: the taller one sets the row.
                let row = height.max(DOWNLOAD);
                out.push(MediaBox { x: 0., y: y + (row - height) / 2., width, height, kind: MediaBoxKind::Image });
                out.push(MediaBox { x: width + 6., y: y + (row - DOWNLOAD) / 2., width: DOWNLOAD, height: DOWNLOAD, kind: MediaBoxKind::Download });
                y += row;
            }
        }
    }
    for attachment in &rest {
        gap(&mut y, &mut first);
        let width = space.width.min(400.);
        let height = match (attachment.purged_at > 0, attachment.kind) {
            // PurgedTombstone: `p-3 border text-[12px]`.
            (true, _) => 12. * 2. + 2. + 18.,
            // The audio card: `mt-2 p-3 gap-2.5`, a 40 px head and a 32 px transport row.
            (false, AttachmentKind::Audio) => MEDIA_MARGIN + 12. * 2. + 2. + 40. + 10. + 32.,
            // The document card: `p-3`, a 40 px icon.
            (false, _) => 12. * 2. + 2. + 40.,
        };
        out.push(MediaBox { x: 0., y, width, height, kind: MediaBoxKind::Card });
        y += height;
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(view_width: f32, view_height: f32) -> SlotSpace {
        SlotSpace { width: 900., view_width, view_height, scale: 1. }
    }

    /// The numbers `reserveBoxFor` gives in the renderer (worked by hand from attachmentSizing.ts).
    #[test]
    fn boxes_follow_the_renderer() {
        // 1280 × 800 window, members list open: the panel is 706 px wide, 696 − 70 tall.
        let view = space(706., 626.);
        // maxW = min(550, 18·√706 = 478.27, 706 − 80 − 38) = 478.27; maxH = min(350, 16·√626 = 400.3, 566) = 350.
        assert_eq!(reserve_box(1600, 900, false, view), (478., 269.));
        assert_eq!(reserve_box(900, 1200, false, view), (263., 350.));
        assert_eq!(reserve_box(640, 480, false, view), (467., 350.));
        assert_eq!(reserve_box(0, 0, false, view), (260., 180.));
        // Unknown view: the fixed caps.
        assert_eq!(reserve_box(1600, 900, false, space(0., 0.)), (550., 309.));
        assert!((grid_width(706.) - 18. * 706f32.sqrt()).abs() < 1e-3);
        assert_eq!(grid_rows(7), &[1, 3, 3]);
    }
}
