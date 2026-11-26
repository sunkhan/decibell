//! Times as the message views show them (`MessageBubble.tsx`'s day and epoch helpers,
//! `lib/timeFormat.ts`): the local calendar day a message falls on, and the clock in the
//! user's 12 / 24-hour choice. Pure functions over unix seconds; the day of a message is
//! taken once, when its row is built (the renderer cached it per message too).

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

use crate::stores::settings::TimeFormat;

/// The local calendar day of `secs` as yyyymmdd; 0 when it isn't a representable time.
pub fn local_day(secs: i64) -> i32 {
    match Local.timestamp_opt(secs, 0).single() {
        Some(t) => t.year() * 10000 + t.month() as i32 * 100 + t.day() as i32,
        None => 0,
    }
}

/// What `TimeFormat::Auto` stands for: the renderer asked Intl for the app language's hour
/// cycle. Here the locale variables decide (`LC_ALL`, `LC_TIME`, `LANG`): the regions that
/// write a 12-hour clock, else 24-hour.
pub fn resolve_time_format(pref: TimeFormat) -> TimeFormat {
    match pref {
        TimeFormat::Auto => {
            let locale = ["LC_ALL", "LC_TIME", "LANG"]
                .iter()
                .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
                .unwrap_or_default();
            // `en_US.UTF-8` → "US".
            let region = locale.split(['.', '@']).next().unwrap_or("").rsplit(['_', '-']).next().unwrap_or("");
            const TWELVE_HOUR: [&str; 12] = ["US", "CA", "AU", "NZ", "IN", "PH", "PK", "EG", "SA", "BD", "MY", "CO"];
            if locale.contains('_') && TWELVE_HOUR.contains(&region) { TimeFormat::H12 } else { TimeFormat::H24 }
        }
        explicit => explicit,
    }
}

fn local(secs: i64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(secs, 0).single()
}

/// "14:32" or "2:32 PM" (`formatClock`); "" for an unrepresentable time.
pub fn format_clock(secs: i64, pref: TimeFormat) -> String {
    let Some(t) = local(secs) else { return String::new() };
    match resolve_time_format(pref) {
        TimeFormat::H12 => {
            let (pm, hour) = t.hour12();
            format!("{hour}:{:02} {}", t.minute(), if pm { "PM" } else { "AM" })
        }
        _ => format!("{:02}:{:02}", t.hour(), t.minute()),
    }
}

/// The clock without AM / PM ("2:32"), for the hover time beside a grouped message
/// (`formatClockCompact`).
pub fn format_clock_compact(secs: i64, pref: TimeFormat) -> String {
    let Some(t) = local(secs) else { return String::new() };
    match resolve_time_format(pref) {
        TimeFormat::H12 => format!("{}:{:02}", t.hour12().1, t.minute()),
        _ => format!("{:02}:{:02}", t.hour(), t.minute()),
    }
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
        let clock = format_clock(1_764_000_000, TimeFormat::H24);
        assert_eq!(clock.len(), 5);
        assert!(format_clock(1_764_000_000, TimeFormat::H12).ends_with('M'));
        assert!(!format_clock_compact(1_764_000_000, TimeFormat::H12).ends_with('M'));
        assert_eq!(resolve_time_format(TimeFormat::H12), TimeFormat::H12);
    }
}
