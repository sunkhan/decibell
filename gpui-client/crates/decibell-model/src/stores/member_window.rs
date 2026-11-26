//! The member sidebar's layout arithmetic (`features/members/memberWindow.ts`).
//!
//! The sidebar draws only the items near the viewport. Every item has a fixed
//! whole-pixel height, so the layout is arithmetic: the Online header, the online rows,
//! then, once the online part is complete, the Offline header and the offline rows.
//! Spacers above and below the drawn items keep the scroll height. The loading row ends
//! the list and is always drawn, so it stays out of the window. Pure.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberPart {
    Online,
    Offline,
}

/// Item heights in px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemberListMetrics {
    pub row: f32,
    pub online_header: f32,
    pub offline_header: f32,
}

/// What the list holds: rows drawn per part, and whether the offline part (header and
/// rows) is drawn at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberListShape {
    pub online: usize,
    pub offline: usize,
    pub show_offline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MemberListItem {
    Header { part: MemberPart, top: f32 },
    Member { part: MemberPart, index: usize, top: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    header: bool,
    part: MemberPart,
    top: f32,
    count: usize,
    height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemberListLayout {
    segments: Vec<Segment>,
    /// Height of everything the window covers; the loading row starts here.
    pub total: f32,
}

pub fn member_list_layout(shape: MemberListShape, m: MemberListMetrics) -> MemberListLayout {
    let mut segments = Vec::with_capacity(4);
    let mut top = 0.0;
    let mut add = |header: bool, part: MemberPart, count: usize, height: f32| {
        segments.push(Segment { header, part, top, count, height });
        top += count as f32 * height;
    };
    add(true, MemberPart::Online, 1, m.online_header);
    add(false, MemberPart::Online, shape.online, m.row);
    if shape.show_offline {
        add(true, MemberPart::Offline, 1, m.offline_header);
        add(false, MemberPart::Offline, shape.offline, m.row);
    }
    MemberListLayout { segments, total: top }
}

/// The items overlapping `[from, to)` (px from the list's top), in order, with the
/// spacer heights above and below them.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberListWindow {
    pub items: Vec<MemberListItem>,
    pub before: f32,
    pub after: f32,
}

pub fn member_list_window(layout: &MemberListLayout, from: f32, to: f32) -> MemberListWindow {
    let mut items = Vec::new();
    let mut first: Option<f32> = None;
    let mut end = 0.0;
    for s in &layout.segments {
        if s.count == 0 || s.height <= 0.0 {
            continue;
        }
        if s.top + s.count as f32 * s.height <= from || s.top >= to {
            continue;
        }
        let i0 = (((from - s.top) / s.height).floor().max(0.0)) as usize;
        let i1 = (((to - s.top) / s.height).ceil().max(0.0) as usize).min(s.count);
        for i in i0..i1 {
            let top = s.top + i as f32 * s.height;
            first.get_or_insert(top);
            end = top + s.height;
            items.push(if s.header {
                MemberListItem::Header { part: s.part, top }
            } else {
                MemberListItem::Member { part: s.part, index: i, top }
            });
        }
    }
    match first {
        None => MemberListWindow { items, before: layout.total, after: 0.0 },
        Some(first) => MemberListWindow { items, before: first, after: layout.total - end },
    }
}

/// Snapped to `step`, so the stretch worth drawing (and the list) changes once per
/// `step` px scrolled, with `overscan` px drawn either side of the viewport.
pub const WINDOW_STEP: f32 = 240.0;
pub const WINDOW_OVERSCAN: f32 = 480.0;

/// `scroll_top` is from the list's top. Returns `(from, to)`.
pub fn window_range(scroll_top: f32, viewport: f32, step: f32, overscan: f32) -> (f32, f32) {
    (
        ((scroll_top / step).floor() * step - overscan).max(0.0),
        ((scroll_top + viewport.max(0.0)) / step).ceil() * step + overscan,
    )
}

/// The next page loads once the loading row (at `total`) comes within `margin` px of the
/// viewport's bottom.
pub const PAGE_MARGIN: f32 = 200.0;

pub fn near_loaded_end(total: f32, scroll_top: f32, viewport: f32, margin: f32) -> bool {
    total < scroll_top + viewport + margin
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: MemberListMetrics = MemberListMetrics { row: 44.0, online_header: 40.0, offline_header: 36.0 };

    #[test]
    fn layout_stacks_headers_and_rows() {
        let layout = member_list_layout(MemberListShape { online: 3, offline: 2, show_offline: true }, M);
        assert_eq!(layout.total, 40.0 + 3.0 * 44.0 + 36.0 + 2.0 * 44.0);
        let online_only = member_list_layout(MemberListShape { online: 3, offline: 9, show_offline: false }, M);
        assert_eq!(online_only.total, 40.0 + 3.0 * 44.0);
    }

    #[test]
    fn the_window_draws_what_overlaps_and_spacers_keep_the_height() {
        let layout = member_list_layout(MemberListShape { online: 100, offline: 50, show_offline: true }, M);
        // The top of the list.
        let w = member_list_window(&layout, 0.0, 130.0);
        assert_eq!(
            w.items,
            [
                MemberListItem::Header { part: MemberPart::Online, top: 0.0 },
                MemberListItem::Member { part: MemberPart::Online, index: 0, top: 40.0 },
                MemberListItem::Member { part: MemberPart::Online, index: 1, top: 84.0 },
                MemberListItem::Member { part: MemberPart::Online, index: 2, top: 128.0 },
            ]
        );
        assert_eq!(w.before, 0.0);
        assert_eq!(w.after, layout.total - 172.0);

        // Across the online / offline boundary: rows 98, 99, the Offline header, row 0.
        let boundary = 40.0 + 100.0 * 44.0;
        let w = member_list_window(&layout, boundary - 60.0, boundary + 50.0);
        assert_eq!(
            w.items,
            [
                MemberListItem::Member { part: MemberPart::Online, index: 98, top: boundary - 88.0 },
                MemberListItem::Member { part: MemberPart::Online, index: 99, top: boundary - 44.0 },
                MemberListItem::Header { part: MemberPart::Offline, top: boundary },
                MemberListItem::Member { part: MemberPart::Offline, index: 0, top: boundary + 36.0 },
            ]
        );
        assert_eq!(w.before, boundary - 88.0);
        assert_eq!(w.before + 44.0 * 3.0 + 36.0 + w.after, layout.total);

        // Past the end: nothing drawn, the whole height above.
        let w = member_list_window(&layout, layout.total + 10.0, layout.total + 500.0);
        assert!(w.items.is_empty());
        assert_eq!((w.before, w.after), (layout.total, 0.0));
    }

    #[test]
    fn the_range_snaps_and_the_end_is_noticed_early() {
        assert_eq!(window_range(0.0, 600.0, WINDOW_STEP, WINDOW_OVERSCAN), (0.0, 720.0 + 480.0));
        assert_eq!(window_range(1000.0, 600.0, WINDOW_STEP, WINDOW_OVERSCAN), (960.0 - 480.0, 1680.0 + 480.0));
        // The same range for a small scroll: the list doesn't change every pixel.
        assert_eq!(
            window_range(1000.0, 600.0, WINDOW_STEP, WINDOW_OVERSCAN),
            window_range(1070.0, 600.0, WINDOW_STEP, WINDOW_OVERSCAN)
        );
        assert_eq!(window_range(5.0, -3.0, WINDOW_STEP, WINDOW_OVERSCAN).0, 0.0);
        assert!(near_loaded_end(2000.0, 1300.0, 600.0, PAGE_MARGIN));
        assert!(!near_loaded_end(2000.0, 1100.0, 600.0, PAGE_MARGIN));
    }
}
