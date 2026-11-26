//! Times as the message views show them (`MessageBubble.tsx`'s day and epoch helpers,
//! `lib/timeFormat.ts`): the local calendar day a message falls on, and the clock in the
//! user's 12 / 24-hour choice. Thin wrappers over [`crate::regional`], which owns every format:
//! the day of a message is taken once, when its row is built (the renderer cached it per message
//! too).

use crate::regional::{Regional, local};
use crate::stores::settings::TimeFormat;

/// The local calendar day of `secs` as yyyymmdd; 0 when it isn't a representable time.
pub fn local_day(secs: i64) -> i32 {
    local(secs).map_or(0, |t| t.date.day_number())
}

/// What `TimeFormat::Auto` stands for: the system's regional setting ([`Regional::hour12`]).
pub fn resolve_time_format(pref: TimeFormat) -> TimeFormat {
    if Regional::get().hour12(pref) { TimeFormat::H12 } else { TimeFormat::H24 }
}

/// "14:32" or "2:32 PM" (`formatClock`); "" for an unrepresentable time.
pub fn format_clock(secs: i64, pref: TimeFormat) -> String {
    Regional::get().clock(secs, pref)
}

/// The clock without AM / PM ("2:32"), for the hover time beside a grouped message
/// (`formatClockCompact`).
pub fn format_clock_compact(secs: i64, pref: TimeFormat) -> String {
    Regional::get().clock_compact(secs, pref)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_clocks() {
        // Whatever the machine's zone, a day is a plausible yyyymmdd and stable per time.
        let d = local_day(1_764_000_000);
        assert!(d > 2025_0000 && d < 2026_0000, "{d}");
        assert_eq!(local_day(1_764_000_000), d);
        assert_ne!(local_day(1_764_000_000), local_day(1_764_000_000 + 86_400));
        // Whatever the machine's regional settings: 24-hour is two digits, a separator, two.
        let clock = format_clock(1_764_000_000, TimeFormat::H24);
        assert_eq!(clock.chars().count(), 5, "{clock}");
        assert!(format_clock(1_764_000_000, TimeFormat::H12).ends_with('M'));
        assert!(!format_clock_compact(1_764_000_000, TimeFormat::H12).ends_with('M'));
        assert_eq!(resolve_time_format(TimeFormat::H12), TimeFormat::H12);
    }
}
