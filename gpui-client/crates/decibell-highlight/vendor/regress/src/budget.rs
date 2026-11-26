//! Decibell addition: a budget for the backtracking matcher.
//!
//! A backtracking regex can take time exponential in its input. This module
//! lets a caller bound the work of every match made on the current thread:
//! each attempt to match at a position and each backtrack spends one step,
//! and once the steps (or the optional deadline) run out every match in
//! progress fails at once and later ones fail immediately, until the budget
//! is set again. A failed match is then indistinguishable from "no match",
//! so the caller must check [`exhausted`] before trusting a `None`.

use core::cell::Cell;
use std::time::Instant;

/// No limit.
pub const UNLIMITED: u64 = u64::MAX;

/// How often (in steps) the deadline is compared with the clock.
const CLOCK_INTERVAL: u64 = 4096;

std::thread_local! {
    static STEPS: Cell<u64> = const { Cell::new(UNLIMITED) };
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    static EXHAUSTED: Cell<bool> = const { Cell::new(false) };
}

/// Sets this thread's budget and clears the exhausted flag.
pub fn set(steps: u64, deadline: Option<Instant>) {
    STEPS.with(|s| s.set(steps));
    DEADLINE.with(|d| d.set(deadline));
    EXHAUSTED.with(|e| e.set(false));
}

/// Removes any limit.
pub fn clear() {
    set(UNLIMITED, None);
}

/// Steps left (`UNLIMITED` when there is no limit).
pub fn remaining() -> u64 {
    STEPS.with(|s| s.get())
}

/// Whether a match was cut short since the budget was last set.
pub fn exhausted() -> bool {
    EXHAUSTED.with(|e| e.get())
}

/// Spends one step. Returns false once the budget is gone.
#[inline]
pub(crate) fn step() -> bool {
    STEPS.with(|s| {
        let n = s.get();
        if n == UNLIMITED {
            return true;
        }
        if n == 0 {
            EXHAUSTED.with(|e| e.set(true));
            return false;
        }
        s.set(n - 1);
        if n % CLOCK_INTERVAL == 0 {
            let late = DEADLINE.with(|d| d.get()).is_some_and(|d| Instant::now() >= d);
            if late {
                s.set(0);
                EXHAUSTED.with(|e| e.set(true));
                return false;
            }
        }
        true
    })
}
