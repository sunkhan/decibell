//! The stage's layout maths (`features/voice/stage/stageLayout.ts`), ported number for number:
//! every tile is 16:9; the grid picks the column count that gives the biggest tile for the space;
//! once tiles would shrink below a floor the tail collapses into a "+N more" tile; a hidden
//! speaker swaps into view after talking a moment ([`Visible`]).
//!
//! Pure: no GPUI, no clock of its own (the caller passes `now`), so it is unit-tested here.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use decibell_model::Uid;

pub const TILE_ASPECT: f32 = 16. / 9.;
pub const TILE_GAP: f32 = 12.;
/// Smallest grid tile before people start collapsing into "+N more".
pub const MIN_TILE_W: f32 = 200.;
/// Biggest a participant tile grows (two people in a wide window).
pub const MAX_TILE_W: f32 = 560.;
/// The compact people row under live streams.
pub const ROW_TILE_W: f32 = 176.;
/// Filmstrip tiles under a focused stream.
pub const STRIP_TILE_W: f32 = 128.;
pub const STRIP_GAP: f32 = 8.;
/// Gap between the streams block and the people row under it (`VoicePanel.tsx`).
pub const SECTION_GAP: f32 = 16.;
/// Live streams can grow bigger than people: they're the content.
pub const MAX_STREAM_W: f32 = 960.;
/// Below this the streams block stops shrinking and the stage scrolls.
pub const MIN_STREAMS_H: f32 = 180.;

/// A hidden speaker must talk this long before swapping into view (a cough doesn't reshuffle).
pub const SPEAK_MIN: Duration = Duration::from_millis(600);
/// The grid swaps at most once per this window.
pub const SWAP_COOLDOWN: Duration = Duration::from_millis(3000);

/// A tile's height for its width (`aspect-video`), on whole pixels: Chromium lays the fraction out
/// but paints edges at the nearest pixel, so rounding here lands every row where it paints.
pub fn tile_height(width: f32) -> f32 {
    (width / TILE_ASPECT).round()
}

/// Best-fit tile width for `n` tiles in a `width` × `height` box (`fitGrid`): every column count
/// is tried, the widest tile wins; `cols` caps the row width so a height-bound grid doesn't pack
/// an extra tile per row.
pub fn fit_grid(n: usize, width: f32, height: f32, gap: f32, max_w: f32) -> Grid {
    if n == 0 || width <= 0. || height <= 0. {
        return Grid { cols: 1, tile_w: 0. };
    }
    let mut best = 0f32;
    let mut best_cols = 1;
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let by_w = (width - gap * (cols as f32 - 1.)) / cols as f32;
        let by_h = ((height - gap * (rows as f32 - 1.)) / rows as f32) * TILE_ASPECT;
        let w = by_w.min(by_h).min(max_w);
        if w > best {
            best = w;
            best_cols = cols;
        }
    }
    Grid { cols: best_cols, tile_w: best.floor().max(0.) }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub cols: usize,
    pub tile_w: f32,
}

/// How many tiles of at least `min_w` fit in the box (`gridCapacity`, ≥ 1).
pub fn grid_capacity(width: f32, height: f32, gap: f32, min_w: f32) -> usize {
    let min_h = min_w / TILE_ASPECT;
    let cols = (((width + gap) / (min_w + gap)).floor() as usize).max(1);
    let rows = (((height + gap) / (min_h + gap)).floor() as usize).max(1);
    cols * rows
}

/// How many `tile_w` tiles fit side by side (`rowCapacity`, ≥ 1).
pub fn row_capacity(width: f32, tile_w: f32, gap: f32) -> usize {
    (((width + gap) / (tile_w + gap)).floor() as usize).max(1)
}

/// The width a row of `cols` tiles may take (the flex-wrap container's `maxWidth`).
pub fn row_max(cols: usize, w: f32, gap: f32) -> f32 {
    cols as f32 * w + (cols.max(1) as f32 - 1.) * gap
}

/// Avatar size for a tile height (`avatarFor`): ~38 % of it, on an 8 px step, 32–96.
pub fn avatar_for(height: f32) -> f32 {
    ((height * 0.38 / 8.).round() * 8.).clamp(32., 96.)
}

/// The alone stage's tile (`AloneStage`): room for the copy and buttons under it.
pub fn alone_tile_w(width: f32, height: f32) -> f32 {
    160f32.max(480f32.min(width).min(((height - 140.) * TILE_ASPECT).floor()))
}

/// What the stage shows (`VoiceStage`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// Not measured yet: nothing.
    Unmeasured,
    /// Only us (or nobody listed yet): our tile, large, and the invite / share prompt.
    Alone { tile_w: f32 },
    /// Everyone as tiles, best-fit.
    Grid,
    /// Live streams large on top, people in one compact row below.
    Streams { cols: usize, tile_w: f32 },
}

/// The stage for `people` participants and `streams` live streams in a `width` × `height` content
/// box: the mode, the people slots (fewer than `people` → the "+N more" tile), their tile width
/// and the row's column cap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stage {
    pub mode: Mode,
    pub slots: usize,
    pub people_w: f32,
    pub people_cols: usize,
}

pub fn stage(people: usize, streams: usize, width: f32, height: f32) -> Stage {
    let measured = width > 0. && height > 0.;
    let mut stage = Stage { mode: Mode::Unmeasured, slots: people, people_w: MIN_TILE_W, people_cols: 1 };
    if !measured {
        return stage;
    }
    if streams > 0 {
        let row_cap = row_capacity(width, ROW_TILE_W, TILE_GAP);
        if people > row_cap {
            stage.slots = row_cap - 1;
        }
        stage.people_w = ROW_TILE_W;
        stage.people_cols = row_cap;
        let streams_h = MIN_STREAMS_H.max(height - ROW_TILE_W / TILE_ASPECT - SECTION_GAP);
        let grid = fit_grid(streams, width, streams_h, TILE_GAP, MAX_STREAM_W);
        stage.mode = Mode::Streams { cols: grid.cols, tile_w: grid.tile_w };
    } else if people > 1 {
        let cap = grid_capacity(width, height, TILE_GAP, MIN_TILE_W);
        if people > cap {
            stage.slots = cap - 1;
        }
        let fit = fit_grid(people.min(cap), width, height, TILE_GAP, MAX_TILE_W);
        stage.people_w = fit.tile_w;
        stage.people_cols = fit.cols;
        stage.mode = Mode::Grid;
    } else {
        stage.mode = Mode::Alone { tile_w: alone_tile_w(width, height) };
    }
    stage
}

/// The filmstrip under a focused stream (`Filmstrip`): the other streams first, then people,
/// capped to one row with the same "+N more".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Strip {
    pub stream_slots: usize,
    pub slots: usize,
    pub overflow: bool,
}

pub fn strip(width: f32, other_streams: usize, people: usize) -> Strip {
    let cap = if width > 0. { row_capacity(width, STRIP_TILE_W, STRIP_GAP) } else { other_streams + people };
    let stream_slots = other_streams.min(cap);
    let people_cap = cap - stream_slots;
    let slots = if people > people_cap { people_cap.saturating_sub(1) } else { people };
    Strip { stream_slots, slots, overflow: slots < people && people_cap > 0 }
}

/// Which participants get a tile when there are more people than slots
/// (`useVisibleParticipants`).
///
/// Pinned users (us, anyone streaming) always have one. The rest fill in roster order; when a
/// hidden user talks for [`SPEAK_MIN`] they take the slot of the visible, unpinned, silent user
/// who has been quiet longest, in place, at most once per [`SWAP_COOLDOWN`]. The speaking signal
/// is fed in by the owner ([`Visible::speaking`]); only a swap changes what is shown.
#[derive(Debug, Default)]
pub struct Visible {
    shown: Vec<Uid>,
    /// When each user last spoke (started or stopped). Never spoken = oldest.
    last_spoke: HashMap<Uid, Instant>,
    /// When each current speaker started.
    speak_start: HashMap<Uid, Instant>,
    /// Current speakers, in the order they started (the renderer's `Set` order).
    speakers: Vec<Uid>,
    last_swap: Option<Instant>,
}

/// What [`Visible::check`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Nothing to do (no hidden speaker).
    Idle,
    /// A hidden speaker swapped in: redraw. Check again after the cooldown (another may queue).
    Swapped,
    /// Check again after this long (a hidden speaker hasn't talked long enough, or the cooldown).
    Wait(Duration),
}

impl Visible {
    pub fn shown(&self) -> &[Uid] {
        &self.shown
    }

    /// Reconcile the kept set with the roster (the `useMemo`): below the cap it is simply `all`;
    /// above it the kept users stay in place, pinned ones are added, the rest fill in roster
    /// order, and when there are too many the unpinned user quiet longest goes, one at a time.
    pub fn reconcile(&mut self, all: &[Uid], slots: usize, pinned: &HashSet<Uid>) -> &[Uid] {
        if all.len() <= slots {
            self.shown = all.to_vec();
            return &self.shown;
        }
        let present: HashSet<Uid> = all.iter().copied().collect();
        let mut next: Vec<Uid> = self.shown.iter().copied().filter(|u| present.contains(u)).collect();
        for u in all {
            if pinned.contains(u) && !next.contains(u) {
                next.push(*u);
            }
        }
        for u in all {
            if next.len() >= slots {
                break;
            }
            if !next.contains(u) {
                next.push(*u);
            }
        }
        while next.len() > slots {
            let victim = self.quietest(&next, |u| pinned.contains(&u));
            let Some(victim) = victim else { break };
            next.remove(victim);
        }
        next.truncate(slots);
        self.shown = next;
        &self.shown
    }

    /// The index of the user in `list` quiet longest that `skip` doesn't exclude; on a tie (nobody
    /// has spoken yet) the latest in the list (`<=`).
    fn quietest(&self, list: &[Uid], skip: impl Fn(Uid) -> bool) -> Option<usize> {
        let mut victim = None;
        let mut oldest: Option<Option<Instant>> = None;
        for (i, u) in list.iter().enumerate() {
            if skip(*u) {
                continue;
            }
            let t = self.last_spoke.get(u).copied();
            // None (never spoke) sorts before every instant.
            if oldest.is_none_or(|o| t <= o) {
                oldest = Some(t);
                victim = Some(i);
            }
        }
        victim
    }

    /// A speaking flip (the store's `SpeakingChanged`).
    pub fn speaking(&mut self, uid: Uid, speaking: bool, now: Instant) {
        if speaking {
            if !self.speakers.contains(&uid) {
                self.speakers.push(uid);
                self.speak_start.insert(uid, now);
            }
        } else {
            self.speakers.retain(|u| *u != uid);
            self.speak_start.remove(&uid);
        }
        self.last_spoke.insert(uid, now);
    }

    pub fn is_speaking(&self, uid: Uid) -> bool {
        self.speakers.contains(&uid)
    }

    /// The renderer's `check()`: swap a hidden speaker in if one has talked long enough and the
    /// cooldown passed.
    pub fn check(&mut self, all: &[Uid], slots: usize, pinned: &HashSet<Uid>, now: Instant) -> Check {
        if all.len() <= slots {
            return Check::Idle;
        }
        let shown: HashSet<Uid> = self.shown.iter().copied().collect();
        let mut candidate = None;
        let mut wait: Option<Duration> = None;
        for u in &self.speakers {
            if shown.contains(u) || !all.contains(u) {
                continue;
            }
            let started = self.speak_start.get(u).copied().unwrap_or(now);
            let talk_left = SPEAK_MIN.saturating_sub(now.saturating_duration_since(started));
            let cool_left = match self.last_swap {
                Some(at) => SWAP_COOLDOWN.saturating_sub(now.saturating_duration_since(at)),
                None => Duration::ZERO,
            };
            let remaining = talk_left.max(cool_left);
            if remaining.is_zero() {
                candidate = Some(*u);
                break;
            }
            wait = Some(wait.map_or(remaining, |w| w.min(remaining)));
        }
        let Some(candidate) = candidate else {
            return wait.map_or(Check::Idle, Check::Wait);
        };
        let speakers = self.speakers.clone();
        let Some(victim) = self.quietest(&self.shown.clone(), |u| pinned.contains(&u) || speakers.contains(&u)) else {
            // Every visible tile is pinned or talking: try again later.
            return Check::Wait(SWAP_COOLDOWN);
        };
        self.shown[victim] = candidate;
        self.last_swap = Some(now);
        Check::Swapped
    }

    /// The hidden users, in roster order (the "+N more" tile's list).
    pub fn hidden(&self, all: &[Uid]) -> Vec<Uid> {
        let shown: HashSet<Uid> = self.shown.iter().copied().collect();
        all.iter().copied().filter(|u| !shown.contains(u)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_grid_prefers_the_widest_tile() {
        // Four tiles in a wide box: 2 × 2, not 3 + 1.
        let g = fit_grid(4, 1200., 700., TILE_GAP, MAX_TILE_W);
        assert_eq!(g.cols, 2);
        assert_eq!(g.tile_w, 560., "capped at MAX_TILE_W");
        let g = fit_grid(9, 1100., 600., TILE_GAP, MAX_TILE_W);
        assert_eq!(g.cols, 3);
        assert_eq!(g.tile_w, ((600. - 24.) / 3. * TILE_ASPECT).floor());
        assert_eq!(fit_grid(0, 100., 100., 12., 500.), Grid { cols: 1, tile_w: 0. });
    }

    #[test]
    fn capacities() {
        // 4 columns of 200 (+ gaps) in 1000; rows of 112.5 (+ gaps) in 600: 4.
        assert_eq!(grid_capacity(1000., 600., 12., 200.), 4 * 4);
        assert_eq!(grid_capacity(10., 10., 12., 200.), 1);
        assert_eq!(row_capacity(1000., 176., 12.), 5);
        assert_eq!(row_capacity(100., 176., 12.), 1);
        assert_eq!(row_max(3, 176., 12.), 552.);
    }

    #[test]
    fn avatar_sizes() {
        assert_eq!(avatar_for(99.), 40.);
        assert_eq!(avatar_for(315.), 96.);
        assert_eq!(avatar_for(40.), 32.);
        assert_eq!(alone_tile_w(1000., 700.), 480.);
        assert_eq!(alone_tile_w(1000., 300.), 284.);
        assert_eq!(alone_tile_w(1000., 100.), 160.);
    }

    #[test]
    fn stage_modes() {
        assert_eq!(stage(1, 0, 0., 0.).mode, Mode::Unmeasured);
        assert!(matches!(stage(1, 0, 900., 500.).mode, Mode::Alone { .. }));
        let s = stage(4, 0, 900., 500.);
        assert_eq!((s.mode, s.slots), (Mode::Grid, 4));
        // 25 people in a box that holds 4 × 3 at the floor: 11 tiles + "+14 more".
        let s = stage(25, 0, 900., 400.);
        assert_eq!(grid_capacity(900., 400., 12., 200.), 12);
        assert_eq!(s.slots, 11);
        let s = stage(9, 1, 900., 500.);
        assert_eq!(s.people_w, ROW_TILE_W);
        assert_eq!(s.people_cols, 4);
        assert_eq!(s.slots, 3, "one row: 3 people + the overflow tile");
        assert!(matches!(s.mode, Mode::Streams { cols: 1, .. }));
    }

    #[test]
    fn the_strip_fills_one_row() {
        let s = strip(700., 1, 9);
        assert_eq!(row_capacity(700., STRIP_TILE_W, STRIP_GAP), 5);
        assert_eq!(s, Strip { stream_slots: 1, slots: 3, overflow: true });
        assert_eq!(strip(700., 0, 3), Strip { stream_slots: 0, slots: 3, overflow: false });
    }

    fn pins(list: &[Uid]) -> HashSet<Uid> {
        list.iter().copied().collect()
    }

    #[test]
    fn reconcile_keeps_pinned_and_drops_the_latest_joiner() {
        let mut v = Visible::default();
        let all: Vec<Uid> = (1..=6).collect();
        // Pinned users are added first, then the roster fills in (the renderer's order, kept as is).
        assert_eq!(v.reconcile(&all, 4, &pins(&[6])), &[6, 1, 2, 3]);
        // Shrink: the latest unpinned joiner (nobody has spoken) goes; the rest keep their places.
        assert_eq!(v.reconcile(&all, 3, &pins(&[6])), &[6, 1, 2]);
        // Below the cap: everyone, roster order.
        assert_eq!(v.reconcile(&all[..3], 4, &pins(&[6])), &[1, 2, 3]);
    }

    #[test]
    fn a_hidden_speaker_swaps_in_after_talking_and_the_cooldown() {
        let t0 = Instant::now();
        let mut v = Visible::default();
        let all: Vec<Uid> = (1..=6).collect();
        let pinned = pins(&[1]);
        v.reconcile(&all, 4, &pinned);
        assert_eq!(v.shown(), &[1, 2, 3, 4]);
        v.speaking(2, true, t0);
        v.speaking(2, false, t0 + Duration::from_millis(100));
        v.speaking(6, true, t0 + Duration::from_millis(200));
        assert_eq!(v.check(&all, 4, &pinned, t0 + Duration::from_millis(300)), Check::Wait(Duration::from_millis(500)));
        // 600 ms of talking: 6 takes the place of the quietest unpinned, silent visible user (3 or
        // 4 never spoke; the later one, 4).
        assert_eq!(v.check(&all, 4, &pinned, t0 + Duration::from_millis(800)), Check::Swapped);
        assert_eq!(v.shown(), &[1, 2, 3, 6]);
        assert_eq!(v.hidden(&all), vec![4, 5]);
        // Another hidden speaker waits out the cooldown.
        v.speaking(5, true, t0 + Duration::from_millis(900));
        assert_eq!(v.check(&all, 4, &pinned, t0 + Duration::from_millis(1600)), Check::Wait(Duration::from_millis(2200)));
        assert_eq!(v.check(&all, 4, &pinned, t0 + Duration::from_millis(3800)), Check::Swapped);
        assert_eq!(v.shown(), &[1, 2, 5, 6], "3 had never spoken: quieter than 2");
    }

    #[test]
    fn no_victim_when_everyone_visible_is_pinned_or_talking() {
        let t0 = Instant::now();
        let mut v = Visible::default();
        let all: Vec<Uid> = vec![1, 2, 3];
        v.reconcile(&all, 2, &pins(&[1]));
        v.speaking(2, true, t0);
        v.speaking(3, true, t0);
        assert_eq!(v.check(&all, 2, &pins(&[1]), t0 + Duration::from_secs(1)), Check::Wait(SWAP_COOLDOWN));
    }
}
