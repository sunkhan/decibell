//! The high-rate voice signals, each its own tiny entity (docs/model.md §4): who is
//! speaking (`voiceStore.speakingUsers`) and our mic level (Settings → Audio's meter).
//!
//! **Speaking** changes whenever anyone in the session starts or stops talking, several
//! times a second in a lively channel. One entity with a keyed event: a tile or roster
//! row shows one person, so it subscribes and notifies itself only for its uid
//! (`SpeakingChanged { uid, .. }`); nothing that shows the stage as a whole may observe
//! this entity, or a speaking ring re-renders the stage. The notify exists for the few
//! views that show "is anyone speaking" and for the auto-away driver.
//!
//! **Input level** arrives ~16 times a second, only while a meter asked for it
//! ([`InputLevel::retain_meter`]: the core's reporting is switched on for the first
//! meter and off with the last). Only the meter observes it.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{Context, EventEmitter};

use crate::ids::Uid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeakingChanged {
    pub uid: Uid,
    pub speaking: bool,
}

#[derive(Default)]
pub struct Speaking {
    speaking: HashSet<Uid>,
}

impl EventEmitter<SpeakingChanged> for Speaking {}

impl Speaking {
    pub fn is_speaking(&self, uid: Uid) -> bool {
        self.speaking.contains(&uid)
    }

    pub fn speakers(&self) -> impl Iterator<Item = Uid> + '_ {
        self.speaking.iter().copied()
    }

    /// One engine report. A repeat of what we have changes nothing (the wire repeats
    /// start / stop often).
    pub fn set(&mut self, uid: Uid, speaking: bool, cx: &mut Context<Self>) {
        let changed = if speaking { self.speaking.insert(uid) } else { self.speaking.remove(&uid) };
        if changed {
            cx.emit(SpeakingChanged { uid, speaking });
            cx.notify();
        }
    }

    /// A new session starts silent; one that carries on through a moderator's move keeps
    /// `keep` (ourselves, talking: the engine doesn't announce us again).
    pub fn reset(&mut self, keep: Option<Uid>, cx: &mut Context<Self>) {
        let kept = keep.filter(|uid| self.speaking.contains(uid));
        let gone: Vec<Uid> = self.speaking.iter().copied().filter(|u| Some(*u) != kept).collect();
        for uid in gone {
            self.speaking.remove(&uid);
            cx.emit(SpeakingChanged { uid, speaking: false });
        }
        cx.notify();
    }
}

/// Switches the core's `voice_input_level` reporting on or off.
pub type LevelReporting = Rc<dyn Fn(bool)>;

pub struct InputLevel {
    /// The last level, dB (−96 = silence).
    pub db: f32,
    meters: usize,
    reporting: LevelReporting,
}

impl InputLevel {
    pub fn new(reporting: LevelReporting) -> Self {
        InputLevel { db: -96.0, meters: 0, reporting }
    }

    pub fn set(&mut self, db: f32, cx: &mut Context<Self>) {
        if self.db != db {
            self.db = db;
            cx.notify();
        }
    }

    /// A meter appeared (the Audio tab mounted): reporting on with the first.
    pub fn retain_meter(&mut self) {
        self.meters += 1;
        if self.meters == 1 {
            (self.reporting)(true);
        }
    }

    /// A meter went: reporting off with the last.
    pub fn release_meter(&mut self, cx: &mut Context<Self>) {
        if self.meters == 0 {
            return;
        }
        self.meters -= 1;
        if self.meters == 0 {
            (self.reporting)(false);
            self.set(-96.0, cx);
        }
    }

    pub fn meters(&self) -> usize {
        self.meters
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    #[gpui::test]
    fn speaking_notifies_only_on_change_with_the_uid(cx: &mut TestAppContext) {
        let s = cx.new(|_| Speaking::default());
        let seen = Rc::new(RefCell::new(Vec::new()));
        let notifies = Rc::new(RefCell::new(0));
        let _subs = cx.update(|cx| {
            let (seen, notifies) = (seen.clone(), notifies.clone());
            (
                cx.subscribe(&s, move |_, e: &SpeakingChanged, _| seen.borrow_mut().push(*e)),
                cx.observe(&s, move |_, _| *notifies.borrow_mut() += 1),
            )
        });
        s.update(cx, |s, cx| {
            s.set(2, true, cx);
            s.set(2, true, cx);
            s.set(3, false, cx);
        });
        cx.run_until_parked();
        assert_eq!(*seen.borrow(), vec![SpeakingChanged { uid: 2, speaking: true }]);
        assert_eq!(*notifies.borrow(), 1, "one notify for the one change");
        s.update(cx, |s, cx| {
            s.set(7, true, cx);
            s.reset(Some(7), cx);
        });
        s.read_with(cx, |s, _| assert!(s.is_speaking(7) && !s.is_speaking(2)));
    }

    #[gpui::test]
    fn reporting_follows_the_meters(cx: &mut TestAppContext) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let level = cx.new(|_| {
            let calls = calls.clone();
            InputLevel::new(Rc::new(move |on| calls.borrow_mut().push(on)))
        });
        level.update(cx, |l, cx| {
            l.retain_meter();
            l.retain_meter();
            l.set(-20.0, cx);
            l.release_meter(cx);
            l.release_meter(cx);
            l.release_meter(cx);
        });
        assert_eq!(*calls.borrow(), vec![true, false]);
        level.read_with(cx, |l, _| assert_eq!(l.db, -96.0));
    }
}
