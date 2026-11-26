//! `attachmentSizing.ts`: the boxes media get, as a function of the chat view's size. Ported rule
//! for rule (the list crate's placeholders use the same numbers: `decibell_chat::media`).

use decibell_chat::SlotSpace;

pub const IMAGE_MAX_W: f32 = 550.;
pub const IMAGE_MAX_H: f32 = 350.;
pub const GIF_MAX_W: f32 = 440.;
pub const GIF_MAX_H: f32 = 280.;
pub const FALLBACK_W: f32 = 260.;
pub const FALLBACK_H: f32 = 180.;
const H_RESERVE: f32 = 80.;
const V_RESERVE: f32 = 60.;
/// `MEDIA_ACTION_RESERVE_PX`: the download button beside attachment media.
pub const ACTION_RESERVE: f32 = 38.;
pub const GRID_GAP: f32 = 4.;
pub const GRID_ROW: f32 = 180.;
const GRID_MAX_W: f32 = 540.;
const GRID_MIN_W: f32 = 320.;

/// Which ceiling a preview gets (`MediaKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Gif,
}

pub fn media_kind_of(mime: &str) -> MediaKind {
    if mime == "image/gif" { MediaKind::Gif } else { MediaKind::Image }
}

pub fn max_image_width(view_width: f32, kind: MediaKind, reserve: f32) -> f32 {
    let cap = if kind == MediaKind::Gif { GIF_MAX_W } else { IMAGE_MAX_W };
    cap.min(18. * view_width.sqrt()).min(view_width - H_RESERVE - reserve).max(120.)
}

pub fn max_image_height(view_height: f32, kind: MediaKind) -> f32 {
    let cap = if kind == MediaKind::Gif { GIF_MAX_H } else { IMAGE_MAX_H };
    cap.min(16. * view_height.sqrt()).min(view_height - V_RESERVE).max(120.)
}

/// JavaScript's `Math.round`.
pub fn js_round(value: f32) -> f32 {
    (value + 0.5).floor()
}

/// `reserveBoxFor`: (width, height, known).
pub fn reserve_box_for(w: u32, h: u32, space: SlotSpace, kind: MediaKind, reserve: f32) -> (f32, f32, bool) {
    if w == 0 || h == 0 {
        return (FALLBACK_W, FALLBACK_H, false);
    }
    let known_view = space.view_width > 0.;
    let max_w = if known_view {
        max_image_width(space.view_width, kind, reserve)
    } else if kind == MediaKind::Gif {
        GIF_MAX_W
    } else {
        IMAGE_MAX_W
    };
    let max_h = if known_view {
        max_image_height(space.view_height, kind)
    } else if kind == MediaKind::Gif {
        GIF_MAX_H
    } else {
        IMAGE_MAX_H
    };
    let (w, h) = (w as f32, h as f32);
    let scale = (max_w / w).min(max_h / h).min(1.);
    (js_round(w * scale).max(1.), js_round(h * scale).max(1.), true)
}

/// `reserveBox`: an attachment's box (the download button's room kept).
pub fn reserve_box(w: u32, h: u32, mime: &str, space: SlotSpace) -> (f32, f32) {
    let (w, h, _) = reserve_box_for(w, h, space, media_kind_of(mime), ACTION_RESERVE);
    (w, h)
}

/// `gridRowCounts`.
pub fn grid_rows(n: usize) -> Vec<usize> {
    match n {
        2 => vec![2],
        3 => vec![3],
        4 => vec![2, 2],
        5 => vec![2, 3],
        6 => vec![3, 3],
        7 => vec![1, 3, 3],
        8 => vec![2, 3, 3],
        9 => vec![3, 3, 3],
        10 => vec![1, 3, 3, 3],
        n => vec![n],
    }
}

/// `gridWidth`.
pub fn grid_width(view_width: f32) -> f32 {
    if view_width <= 0. {
        return GRID_MAX_W;
    }
    let cap = max_image_width(view_width, MediaKind::Image, ACTION_RESERVE);
    let room = view_width - H_RESERVE - ACTION_RESERVE;
    GRID_MAX_W.min(GRID_MIN_W.min(room).max(cap))
}

/// The thumbnail size the renderer's `previewUrlFor` asks for a box: the long edge × the
/// device pixel ratio, through `pickThumbnailSize`. `None`: the original (no thumbnail, or a GIF,
/// whose thumbnail is a still of its first frame).
pub fn preview_thumb(mask: u32, thumb_bytes: u32, mime: &str, box_w: f32, box_h: f32, scale: f32) -> Option<u32> {
    if mime == "image/gif" || thumb_bytes == 0 {
        return None;
    }
    let target = if box_w > 0. { js_round(box_w.max(box_h) * scale) as u32 } else { 640 };
    decibell_attachments::thumbs::pick_thumbnail_size(mask, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(view_width: f32, view_height: f32) -> SlotSpace {
        SlotSpace { width: 900., view_width, view_height, scale: 1. }
    }

    #[test]
    fn boxes_match_the_list_crate() {
        let view = space(706., 626.);
        for (w, h) in [(1600, 900), (900, 1200), (640, 480), (0, 0), (480, 270)] {
            assert_eq!(reserve_box(w, h, "image/jpeg", view), decibell_chat::media::reserve_box(w, h, false, view));
            assert_eq!(reserve_box(w, h, "image/gif", view), decibell_chat::media::reserve_box(w, h, true, view));
        }
        assert_eq!(grid_rows(7), decibell_chat::media::grid_rows(7));
        // A link image keeps no button's room: wider on a narrow panel.
        let narrow = space(500., 600.);
        assert!(reserve_box_for(1600, 900, narrow, MediaKind::Image, 0.).0 > reserve_box(1600, 900, "image/jpeg", narrow).0);
    }

    #[test]
    fn thumbnails_follow_the_device_pixels() {
        assert_eq!(preview_thumb(0b111, 1, "image/jpeg", 478., 269., 1.), Some(640));
        assert_eq!(preview_thumb(0b111, 1, "image/jpeg", 478., 269., 2.), Some(1280));
        assert_eq!(preview_thumb(0b001, 1, "image/jpeg", 478., 269., 2.), Some(320));
        assert_eq!(preview_thumb(0b111, 1, "image/gif", 478., 269., 1.), None);
        assert_eq!(preview_thumb(0b111, 0, "image/jpeg", 478., 269., 1.), None);
    }
}
