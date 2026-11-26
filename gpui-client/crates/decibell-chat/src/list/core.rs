//! The list's arithmetic, apart from GPUI: rows with their heights, the content coordinates,
//! the scroll position, and how a change to the slice is applied without moving what is read.
//!
//! **Coordinates.** Every row has a top in *content coordinates*, an f64 that never gets
//! renormalised: a row keeps its number for as long as nothing changes between it and the
//! anchor. The scroll position is a number in the same space. A row is drawn at
//! `round(top × scale) − round(scroll × scale)` device pixels.
//!
//! **Anchor.** Whenever heights or rows change, tops are recomputed *outward from the anchor
//! row* (the first row in the viewport), whose own top is kept. A history page spliced in above
//! therefore gets negative-going tops and changes neither the anchor's top nor the scroll
//! position: no pixel of the viewport can move, there is nothing to correct afterwards. (The
//! renderer gets the same effect from Chromium's scroll anchoring plus its residual; here it is
//! the coordinate system.) Row heights are multiples of 1/64 px (Blink's layout unit), so the
//! sums are exact in an f64.
//!
//! **Estimates.** A row not yet measured carries an estimate (the mean of the measured ones).
//! Rows in and around the viewport are always measured before a frame; the others are measured
//! in slices on idle turns. A late exact height moves only rows on the far side of it from the
//! anchor, i.e. outside the viewport: the scroll range (and the scrollbar thumb) settles, the
//! view does not.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use decibell_model::chat::{Message, RowFacts, RowKey, RowOp};
use decibell_ui::scroll::{ScrollAnimator, ScrollKind};

use crate::row::RowLayout;

/// Row count past which the far end of the slice is trimmed (`MAX_ROWS`).
pub const MAX_ROWS: usize = 150;
/// Distance from a scroll edge at which the next page is requested (`NEAR_PX`).
pub const NEAR_PX: f64 = 800.;
/// A trim never cuts closer than this to the viewport (`KEEP_PX`).
pub const KEEP_PX: f64 = 2. * NEAR_PX;
/// Slack under which the viewport counts as at the bottom (`AT_BOTTOM_PX`).
pub const AT_BOTTOM_PX: f64 = 4.;
/// Rows painted beyond each edge of the viewport are also kept measured: one wheel notch.
pub const OVERDRAW_PX: f64 = 240.;
/// Layouts farther than this from the viewport are dropped (their heights stay).
pub const RETAIN_PX: f64 = 4. * NEAR_PX;
/// The height assumed for a row before any was measured.
const FIRST_ESTIMATE: f32 = 44.;

pub struct Entry {
    pub key: RowKey,
    pub message: Arc<Message>,
    pub facts: RowFacts,
    pub height: f32,
    /// `height` is the layout's, not an estimate.
    pub exact: bool,
    pub layout: Option<Rc<RowLayout>>,
    /// Bit `n`: the row's `n`-th code block is expanded.
    pub expanded: u64,
}

impl Entry {
    fn new(message: &Arc<Message>, facts: RowFacts, estimate: f32) -> Entry {
        Entry { key: message.key(), message: message.clone(), facts, height: estimate, exact: false, layout: None, expanded: 0 }
    }

    /// The content changed: measure again (the old height is the estimate meanwhile).
    pub fn invalidate(&mut self) {
        self.exact = false;
        self.layout = None;
    }
}

/// The scroll position: Chromium's animator over a range that can extend upwards.
#[derive(Default)]
pub struct Scroller {
    animator: ScrollAnimator,
    /// The top of the first row and the position that shows the last row's bottom at the
    /// viewport's bottom. `max < min` while the content is shorter than the viewport: the rows
    /// then sit against the bottom (`justify-end`) and nothing scrolls.
    min: f64,
    max: f64,
}

impl Scroller {
    pub fn position(&mut self, now: Instant) -> f64 {
        if self.max <= self.min { self.max } else { self.min + self.animator.position(now) }
    }

    pub fn target(&self) -> f64 {
        if self.max <= self.min { self.max } else { self.min + self.animator.target() }
    }

    pub fn range(&self) -> (f64, f64) {
        (self.min, self.max)
    }

    pub fn is_animating(&self) -> bool {
        self.animator.is_animating()
    }

    /// The content now spans `[min, max + viewport]`. The position keeps its content
    /// coordinate; a running animation is translated with it and keeps its timing (cc's
    /// `ApplyAdjustment`, docs/wheel-scroll.md §5.5).
    pub fn set_range(&mut self, min: f64, max: f64, now: Instant) {
        if min == self.min && max == self.max {
            return;
        }
        let was = self.position(now);
        let collapsed = self.max <= self.min;
        let shift = self.min - min;
        self.min = min;
        self.max = max;
        let range = (max - min).max(0.);
        if self.animator.is_animating() && !collapsed {
            // Widen first so the translation isn't clamped, then set the real range.
            self.animator.set_max_offset(f64::MAX / 4.);
            self.animator.shift(shift);
            self.animator.set_max_offset(range);
        } else {
            self.animator.set_max_offset(range);
            self.animator.jump_to(was - min);
        }
    }

    pub fn input(&mut self, now: Instant, delta: f64, kind: ScrollKind) {
        self.animator.input(now, delta, kind);
    }

    pub fn precise(&mut self, now: Instant, delta: f64) {
        self.animator.precise(now, delta);
    }

    pub fn jump_to(&mut self, position: f64) {
        self.animator.jump_to(position - self.min);
    }

    pub fn can_scroll(&self, delta: f64) -> bool {
        let target = self.target();
        self.max > self.min && ((delta > 0. && target < self.max - 0.5) || (delta < 0. && target > self.min + 0.5))
    }
}

/// Where the list should put the viewport after a change.
#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    /// Keep what is on screen where it is (the anchor rule).
    Hold,
    /// Show the newest row.
    Bottom,
    /// Put `id`'s row top `offset` px from the viewport's top (a restored position:
    /// `offset` ≤ 0 means the row started above the edge).
    Restore { id: i64, offset: f32 },
    /// Centre row `id` (a jump).
    Center { id: i64 },
}

#[derive(Default)]
pub struct ListCore {
    pub entries: Vec<Entry>,
    /// `tops[i]`: the top of row `i`; `tops[len]`: the bottom of the last row.
    tops: Vec<f64>,
    pub scroller: Scroller,
    pub viewport_height: f64,
    /// The slice revision the entries are aligned with.
    pub seen: u64,
    /// The viewport was at the bottom when last looked at (`atBottomRef`).
    pub at_bottom: bool,
    /// Rows still carrying an estimate.
    pub inexact: usize,
    /// The head grew in the last change (an older page): trim the tail first.
    pub grew_head: bool,
}

/// What [`ListCore::edges`] found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub near_top: bool,
    pub near_bottom: bool,
}

impl ListCore {
    /// An empty list that will open at the bottom (or at a restored position).
    pub fn starting(at_bottom: bool) -> ListCore {
        ListCore { at_bottom, ..ListCore::default() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn top(&self, index: usize) -> f64 {
        self.tops[index]
    }

    pub fn bottom(&self) -> f64 {
        self.tops.last().copied().unwrap_or(0.)
    }

    pub fn content_height(&self) -> f64 {
        self.bottom() - self.tops.first().copied().unwrap_or(0.)
    }

    /// The first row whose bottom edge is below `y` (`firstRowBelow`).
    pub fn row_at(&self, y: f64) -> usize {
        // tops[i + 1] is row i's bottom.
        self.tops[1..].partition_point(|bottom| *bottom <= y).min(self.entries.len().saturating_sub(1))
    }

    /// The first row whose top is at or below `y` (`firstRowAtOrBelow`); `len` if none.
    fn first_top_at_or_below(&self, y: f64) -> usize {
        self.tops[..self.entries.len()].partition_point(|top| *top < y)
    }

    pub fn index_of(&self, key: &RowKey) -> Option<usize> {
        self.entries.iter().position(|entry| &entry.key == key)
    }

    pub fn index_of_id(&self, id: i64) -> Option<usize> {
        self.entries.iter().position(|entry| entry.message.id == id && id > 0)
    }

    fn estimate(&self) -> f32 {
        let (sum, count) = self.entries.iter().filter(|entry| entry.exact).fold((0f64, 0usize), |(sum, count), entry| (sum + entry.height as f64, count + 1));
        if count == 0 { FIRST_ESTIMATE } else { ((sum / count as f64) * 64.).round() as f32 / 64. }
    }

    /// Recomputes every top outward from row `anchor`, whose top stays `anchor_top`.
    fn relayout(&mut self, anchor: usize, anchor_top: f64) {
        let len = self.entries.len();
        self.tops.resize(len + 1, 0.);
        if len == 0 {
            self.tops[0] = anchor_top;
            return;
        }
        let anchor = anchor.min(len - 1);
        self.tops[anchor] = anchor_top;
        for index in anchor..len {
            self.tops[index + 1] = self.tops[index] + self.entries[index].height as f64;
        }
        for index in (0..anchor).rev() {
            self.tops[index] = self.tops[index + 1] - self.entries[index].height as f64;
        }
    }

    fn sync_range(&mut self, now: Instant) {
        let min = self.tops.first().copied().unwrap_or(0.);
        let max = self.bottom() - self.viewport_height;
        self.scroller.set_range(min, max, now);
    }

    /// The row the viewport's top edge is in and that row's top: the anchor.
    pub fn anchor(&mut self, now: Instant) -> Option<(usize, f64)> {
        if self.entries.is_empty() {
            return None;
        }
        let position = self.scroller.position(now);
        let index = self.row_at(position);
        Some((index, self.tops[index]))
    }

    /// Heights changed (measurements, a width change): tops move away from the anchor, the
    /// range follows, and a view at the bottom stays there.
    pub fn heights_changed(&mut self, pinned: bool, now: Instant) {
        self.inexact = self.entries.iter().filter(|entry| !entry.exact).count();
        let Some((anchor, top)) = self.anchor(now) else { return };
        let was_bottom = self.at_bottom;
        self.relayout(anchor, top);
        self.sync_range(now);
        if was_bottom && pinned {
            let (_, max) = self.scroller.range();
            self.scroller.jump_to(max);
        }
    }

    /// The viewport's height changed (the window, the composer growing).
    pub fn set_viewport(&mut self, height: f64, pinned: bool, now: Instant) {
        if height == self.viewport_height {
            return;
        }
        let was_bottom = self.at_bottom;
        self.viewport_height = height;
        self.sync_range(now);
        if was_bottom && pinned {
            let (_, max) = self.scroller.range();
            self.scroller.jump_to(max);
        }
    }

    /// Brings the entries in step with the slice (`rows` / `facts` at its current revision,
    /// `ops` what changed since `seen`, `None` for a rebuild) and places the viewport.
    ///
    /// `pinned`: new rows at the end keep a view that is at the bottom there (not while the
    /// slice is windowed: nothing appended is the present then).
    pub fn apply(&mut self, rows: &[Arc<Message>], facts: &[RowFacts], ops: Option<&[RowOp]>, placement: &Placement, pinned: bool, now: Instant) {
        let estimate = self.estimate();
        let position = self.scroller.position(now);
        let was_bottom = self.at_bottom;
        let first_key = self.entries.first().map(|entry| entry.key.clone());

        // The rows on screen, top first: the first that survives is the anchor (deleting the
        // topmost visible row, or the echo replacing a bubble there, doesn't lose the place).
        let mut candidates: Vec<(RowKey, f64)> = Vec::new();
        if !self.entries.is_empty() {
            let first = self.row_at(position);
            let limit = position + self.viewport_height.max(1.);
            for index in first..self.entries.len() {
                if self.tops[index] >= limit || candidates.len() >= 40 {
                    break;
                }
                candidates.push((self.entries[index].key.clone(), self.tops[index]));
            }
        }

        match ops {
            Some(ops) => {
                // Replay over slots: a kept entry, one to refresh from the slice, or a new row.
                enum Slot {
                    Keep(Entry),
                    Refresh(Entry),
                    New,
                }
                let mut slots: Vec<Slot> = self.entries.drain(..).map(Slot::Keep).collect();
                for op in ops {
                    match *op {
                        RowOp::Splice { at, removed, inserted } => {
                            let at = (at as usize).min(slots.len());
                            let end = (at + removed as usize).min(slots.len());
                            slots.splice(at..end, (0..inserted).map(|_| Slot::New));
                        }
                        RowOp::Update { at } => {
                            if let Some(slot) = slots.get_mut(at as usize) {
                                *slot = match std::mem::replace(slot, Slot::New) {
                                    Slot::Keep(entry) | Slot::Refresh(entry) => Slot::Refresh(entry),
                                    Slot::New => Slot::New,
                                };
                            }
                        }
                    }
                }
                debug_assert_eq!(slots.len(), rows.len(), "the change log replays to the slice's rows");
                if slots.len() != rows.len() {
                    // Never trust a misaligned array: rebuild from what survived.
                    self.entries = slots
                        .into_iter()
                        .filter_map(|slot| match slot {
                            Slot::Keep(entry) => Some(entry),
                            _ => None,
                        })
                        .collect();
                    return self.apply(rows, facts, None, placement, pinned, now);
                }
                self.entries.extend(slots.into_iter().enumerate().map(|(index, slot)| match slot {
                    Slot::Keep(entry) => entry,
                    // Same identity, new content: the old height is the estimate until measured.
                    Slot::Refresh(entry) => Entry { expanded: entry.expanded, ..Entry::new(&rows[index], facts[index], entry.height) },
                    Slot::New => Entry::new(&rows[index], facts[index], estimate),
                }));
            }
            None => {
                let mut old: std::collections::HashMap<RowKey, Entry> = self.entries.drain(..).map(|entry| (entry.key.clone(), entry)).collect();
                for (row, facts) in rows.iter().zip(facts) {
                    let entry = match old.remove(&row.key()) {
                        // Same message object and facts: its layout still holds.
                        Some(entry) if Arc::ptr_eq(&entry.message, row) && entry.facts == *facts => entry,
                        Some(entry) => Entry { expanded: entry.expanded, ..Entry::new(row, *facts, entry.height) },
                        None => Entry::new(row, *facts, estimate),
                    };
                    self.entries.push(entry);
                }
            }
        }

        self.grew_head = match (&first_key, self.entries.first()) {
            (Some(was), Some(now_first)) => *was != now_first.key && self.index_of(was).is_some(),
            _ => false,
        };
        self.inexact = self.entries.iter().filter(|entry| !entry.exact).count();

        // Tops: outward from the first surviving on-screen row.
        let survivor = candidates.iter().find_map(|(key, top)| self.index_of(key).map(|index| (index, *top)));
        match survivor {
            Some((index, top)) => self.relayout(index, top),
            None => self.relayout(0, 0.),
        }
        self.sync_range(now);

        let (_, max) = self.scroller.range();
        match placement {
            Placement::Center { id } if self.index_of_id(*id).is_some() => self.center(*id),
            Placement::Restore { id, offset } if self.index_of_id(*id).is_some() => self.restore(*id, *offset, now),
            Placement::Bottom | Placement::Restore { .. } => self.scroller.jump_to(max),
            // Bottom-follow is instant, never smooth: a smooth scroll aims at an offset taken at
            // its start, and a second append in flight would land short (the renderer's rule).
            _ if was_bottom && pinned => self.scroller.jump_to(max),
            // The slice was replaced with nothing of the old view in it: land at the bottom.
            _ if survivor.is_none() => self.scroller.jump_to(max),
            _ => {}
        }
    }

    /// Puts row `id`'s top `offset` px from the viewport's top (a restored position).
    ///
    /// The saved offset was taken against a position on a whole device pixel, so its fraction
    /// is the row top's sub-pixel phase. Rebuilding the coordinates with that row at `offset`
    /// and the position at 0 reproduces every row's device pixel, not just the anchor's.
    pub fn restore(&mut self, id: i64, offset: f32, now: Instant) {
        if let Some(index) = self.index_of_id(id) {
            self.relayout(index, offset as f64);
            self.sync_range(now);
            self.scroller.jump_to(0.);
        }
    }

    /// Centres row `id` in the viewport (exact once that row and those around it are measured).
    pub fn center(&mut self, id: i64) {
        let Some(index) = self.index_of_id(id) else { return };
        let height = self.entries[index].height as f64;
        self.scroller.jump_to(self.tops[index] - (self.viewport_height - height) / 2.);
    }

    /// Looks at the position: whether the view is at the bottom, and which edges are near.
    pub fn edges(&mut self, now: Instant) -> Edges {
        let position = self.scroller.position(now);
        let (min, max) = self.scroller.range();
        self.at_bottom = max - position <= AT_BOTTOM_PX;
        Edges { near_top: position - min < NEAR_PX, near_bottom: max - position < NEAR_PX }
    }

    /// The slice is over [`MAX_ROWS`]: which end to drop and how many rows to keep, by the
    /// pixel rule (the cut stays [`KEEP_PX`] from the viewport, so it can't land in the paging
    /// zone and ping-pong). `None` while a cut would be too close.
    pub fn overflow(&mut self, now: Instant) -> Option<(Side, usize)> {
        let len = self.entries.len();
        if len <= MAX_ROWS || self.inexact > 0 {
            return None;
        }
        let position = self.scroller.position(now);
        let tail_cut = self.first_top_at_or_below(position + self.viewport_height + KEEP_PX);
        let head_end = self.row_at(position - KEEP_PX);
        // `firstRowBelow` is `len` when every row ends above the line.
        let head_end = if self.bottom() <= position - KEEP_PX { len } else { head_end };
        let tail = (tail_cut < len).then_some((Side::Tail, tail_cut));
        let head = (head_end > 0).then_some((Side::Head, len - head_end));
        // The side opposite the growth first; either is safe by the pixel rule.
        if self.grew_head { tail.or(head) } else { head.or(tail) }
    }

    /// The saved position: the first row in the viewport and where its top sits relative to
    /// the viewport's top (`ListPosition`).
    ///
    /// The position is taken on a whole device pixel (`scale` device px per px), as it is
    /// drawn: see [`Self::restore`].
    pub fn position(&mut self, now: Instant, scale: f64) -> (bool, i64, f32) {
        let position = (self.scroller.position(now) * scale).round() / scale;
        let (_, max) = self.scroller.range();
        let at_bottom = max - position <= AT_BOTTOM_PX;
        if self.entries.is_empty() {
            return (true, 0, 0.);
        }
        let index = self.row_at(position);
        (at_bottom, self.entries[index].message.id.max(0), (self.tops[index] - position) as f32)
    }

    /// The rows whose boxes meet `[from, to)`: an index range.
    pub fn rows_between(&self, from: f64, to: f64) -> std::ops::Range<usize> {
        if self.entries.is_empty() {
            return 0..0;
        }
        let first = self.row_at(from);
        let last = self.first_top_at_or_below(to);
        first..last.max(first)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Head,
    Tail,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn message(id: i64) -> Arc<Message> {
        crate::testing::message(id, 7, "text", 1_764_000_000 + id)
    }

    fn rows(ids: std::ops::Range<i64>) -> Vec<Arc<Message>> {
        ids.map(message).collect()
    }

    fn facts(rows: &[Arc<Message>]) -> Vec<RowFacts> {
        vec![RowFacts::default(); rows.len()]
    }

    /// Gives every row its "exact" height: 20 px + a part that depends on the id, in 1/64 px.
    fn measure(core: &mut ListCore, now: Instant) {
        for entry in &mut core.entries {
            entry.height = 20. + (entry.message.id % 7) as f32 * 21.015625;
            entry.exact = true;
        }
        core.heights_changed(true, now);
    }

    fn screen_y(core: &mut ListCore, id: i64, now: Instant) -> i64 {
        let index = core.index_of_id(id).unwrap();
        (core.top(index)).round() as i64 - core.scroller.position(now).round() as i64
    }

    fn open(ids: std::ops::Range<i64>, now: Instant) -> (ListCore, Vec<Arc<Message>>) {
        let mut core = ListCore { viewport_height: 600., at_bottom: true, ..ListCore::default() };
        let rows = rows(ids);
        core.apply(&rows, &facts(&rows), None, &Placement::Bottom, true, now);
        measure(&mut core, now);
        core.edges(now);
        (core, rows)
    }

    #[test]
    fn opens_at_the_bottom_and_follows_new_rows() {
        let now = Instant::now();
        let (mut core, mut rows) = open(100..150, now);
        assert!(core.at_bottom);
        let (_, max) = core.scroller.range();
        assert_eq!(core.scroller.position(now), max);
        // A live message: one splice at the end; the view stays flush with the bottom.
        rows.push(message(150));
        core.apply(&rows, &facts(&rows), Some(&[RowOp::Splice { at: 50, removed: 0, inserted: 1 }]), &Placement::Hold, true, now);
        measure(&mut core, now);
        let (_, max) = core.scroller.range();
        assert_eq!(core.scroller.position(now), max);
        assert_eq!(core.bottom() - core.scroller.position(now), 600.);
    }

    #[test]
    fn a_page_above_moves_nothing_on_screen() {
        let now = Instant::now();
        let (mut core, rows) = open(100..150, now);
        // Read 800 px above the bottom.
        let (_, max) = core.scroller.range();
        core.scroller.jump_to(max - 800.);
        core.edges(now);
        assert!(!core.at_bottom);
        let at = core.scroller.position(now);
        let reading = core.entries[core.row_at(at)].message.id;
        let before: Vec<i64> = (reading..reading + 8).map(|id| screen_y(&mut core, id, now)).collect();
        let position = core.scroller.position(now);
        let (min_before, max_before) = core.scroller.range();

        // An older page of 50 rows, unmeasured at first, then measured in two slices.
        let mut all = super::tests::rows(50..100);
        all.extend(rows.iter().cloned());
        core.apply(&all, &facts(&all), Some(&[RowOp::Splice { at: 0, removed: 0, inserted: 50 }]), &Placement::Hold, true, now);
        assert_eq!(core.scroller.position(now), position, "the position is a content coordinate: a prepend doesn't touch it");
        assert_eq!((reading..reading + 8).map(|id| screen_y(&mut core, id, now)).collect::<Vec<_>>(), before);
        assert!(core.grew_head);
        for chunk in [0..25usize, 25..50] {
            for entry in &mut core.entries[chunk] {
                entry.height = 20. + (entry.message.id % 7) as f32 * 21.015625;
                entry.exact = true;
            }
            core.heights_changed(true, now);
            assert_eq!((reading..reading + 8).map(|id| screen_y(&mut core, id, now)).collect::<Vec<_>>(), before);
        }
        // The range grew upwards by exactly the page; its lower end did not move.
        let (min, max) = core.scroller.range();
        let page: f64 = core.entries[..50].iter().map(|entry| entry.height as f64).sum();
        assert_eq!(min, min_before - page);
        assert_eq!(max, max_before);
    }

    #[test]
    fn a_new_row_while_reading_history_moves_nothing() {
        let now = Instant::now();
        let (mut core, mut rows) = open(100..150, now);
        let (_, max) = core.scroller.range();
        core.scroller.jump_to(max - 800.);
        core.edges(now);
        let at = core.scroller.position(now);
        let reading = core.entries[core.row_at(at)].message.id;
        let before = screen_y(&mut core, reading, now);
        rows.push(message(150));
        core.apply(&rows, &facts(&rows), Some(&[RowOp::Splice { at: 50, removed: 0, inserted: 1 }]), &Placement::Hold, true, now);
        measure(&mut core, now);
        assert_eq!(screen_y(&mut core, reading, now), before);
        assert!(!core.edges(now).near_top || core.scroller.position(now) - core.scroller.range().0 < NEAR_PX);
    }

    #[test]
    fn an_edit_above_and_a_removed_anchor() {
        let now = Instant::now();
        let (mut core, mut rows) = open(100..150, now);
        let (_, max) = core.scroller.range();
        core.scroller.jump_to(max - 800.);
        core.edges(now);
        let at = core.scroller.position(now);
        let first = core.row_at(at);
        let reading = core.entries[first + 1].message.id;
        let before = screen_y(&mut core, reading, now);
        // A row above the viewport changes height.
        core.apply(&rows, &facts(&rows), Some(&[RowOp::Update { at: 3 }]), &Placement::Hold, true, now);
        core.entries[3].height = 300.;
        core.entries[3].exact = true;
        core.heights_changed(true, now);
        assert_eq!(screen_y(&mut core, reading, now), before);
        // The topmost visible row is deleted: the next one keeps its place.
        rows.remove(first);
        core.apply(&rows, &facts(&rows), Some(&[RowOp::Splice { at: first as u32, removed: 1, inserted: 0 }]), &Placement::Hold, true, now);
        assert_eq!(screen_y(&mut core, reading, now), before);
    }

    #[test]
    fn a_page_during_a_wheel_animation_keeps_the_content_continuous() {
        let start = Instant::now();
        let (mut core, rows) = open(100..150, start);
        let (_, max) = core.scroller.range();
        core.scroller.jump_to(max - 300.);
        core.edges(start);
        // Three notches up.
        for notch in 0..3 {
            core.scroller.input(start + Duration::from_millis(30 * notch), -120., ScrollKind::Wheel);
        }
        let mid = start + Duration::from_millis(100);
        let position = core.scroller.position(mid);
        let target = core.scroller.target();
        let mut all = super::tests::rows(50..100);
        all.extend(rows.iter().cloned());
        core.apply(&all, &facts(&all), Some(&[RowOp::Splice { at: 0, removed: 0, inserted: 50 }]), &Placement::Hold, true, mid);
        measure(&mut core, mid);
        // Same content coordinate at the same instant, same target, still animating.
        assert!((core.scroller.position(mid) - position).abs() < 1e-6);
        assert!((core.scroller.target() - target).abs() < 1e-6);
        assert!(core.scroller.is_animating());
        let end = start + Duration::from_millis(600);
        assert!((core.scroller.position(end) - target).abs() < 1e-6);
    }

    #[test]
    fn trims_by_pixels_on_the_side_opposite_the_growth() {
        let now = Instant::now();
        let (mut core, _) = open(0..400, now);
        // At the bottom, nothing grew at the head: the head goes, cut 1600 px above the viewport.
        let (side, keep) = core.overflow(now).unwrap();
        assert_eq!(side, Side::Head);
        let first_kept = 400 - keep;
        let position = core.scroller.position(now);
        assert!(core.top(first_kept + 1) > position - KEEP_PX, "the cut row ends within KEEP_PX");
        assert!(core.top(first_kept) <= position - KEEP_PX + 200.);
        // Short of the limit, or with estimates outstanding: no trim.
        let (mut small, _) = open(0..100, now);
        assert_eq!(small.overflow(now), None);
        core.entries[10].exact = false;
        core.inexact = 1;
        assert_eq!(core.overflow(now), None);
    }

    #[test]
    fn restore_and_centre() {
        let now = Instant::now();
        let mut core = ListCore { viewport_height: 600., ..ListCore::default() };
        let rows = rows(0..120);
        core.apply(&rows, &facts(&rows), None, &Placement::Restore { id: 60, offset: -12.5 }, true, now);
        measure(&mut core, now);
        // Measuring keeps the anchor row where it was placed.
        assert_eq!(core.top(60) - core.scroller.position(now), -12.5);
        assert_eq!(core.position(now, 1.), (false, 60, -12.5));
        core.center(30);
        let top = core.top(30) - core.scroller.position(now);
        assert_eq!(top, (600. - core.entries[30].height as f64) / 2.);
        // A short list sits against the bottom.
        let (mut short, _) = open(0..3, now);
        assert!(short.bottom() - short.scroller.position(now) == 600.);
        assert!(short.edges(now).near_top && short.at_bottom);
    }
}
