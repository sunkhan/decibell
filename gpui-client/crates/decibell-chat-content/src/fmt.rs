//! Every date, time of day and count this crate shows, in one place: they follow the system's
//! regional format settings with the app's English words (owner, 2026-11-26), through
//! `decibell_model::regional` (CONVENTIONS "Regional formats"). File sizes, speeds and durations
//! are not regional: they keep the renderer's own formatting where they are written.

use decibell_model::chat::time::local_day;
use decibell_model::regional::{Date, Regional};
use decibell_model::stores::settings::TimeFormat;

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The viewer's "posted" line (`postedAt`): "Today, at 2:32 PM", "Yesterday, at …", else the short
/// date, at the time (`formatTimestamp`'s older form), in the system's formats.
pub(crate) fn posted_at(secs: Option<i64>, format: TimeFormat) -> String {
    let Some(secs) = secs else { return String::new() };
    posted_at_on(secs, local_day(now_secs()), format, Regional::get())
}

fn posted_at_on(secs: i64, today: i32, format: TimeFormat, regional: &Regional) -> String {
    if local_day(secs) == today {
        return format!("Today, at {}", regional.clock(secs, format));
    }
    regional.message_stamp(secs, today, format)
}

/// `formatAgo` (the Transfers panel): "just now", "N min ago", "N h ago", "yesterday", else the
/// month and day ("Nov 5" / "5 Nov").
pub(crate) fn ago(epoch_ms: i64, now_ms: i64) -> String {
    Regional::get().relative(epoch_ms.div_euclid(1000), now_ms.div_euclid(1000))
}

/// A count with the system's digit grouping (`n.toLocaleString()`).
pub(crate) fn count(n: i64) -> String {
    Regional::get().group_signed(n)
}

/// A local day (`yyyymmdd`) as the system writes a short date (`Intl.DateTimeFormat()`).
#[allow(dead_code)]
pub(crate) fn short_date(day: i32) -> String {
    Date::from_day_number(day).map_or_else(String::new, |date| Regional::get().short_date(date))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_the_regional_module() {
        let before = Regional::pin(Some(Regional::en_us()));
        let now = 1_800_000_000_000;
        assert_eq!(ago(now - 30_000, now), "just now");
        assert_eq!(ago(now - 5 * 60_000, now), "5 min ago");
        assert_eq!(ago(now - 3 * 3_600_000, now), "3 h ago");
        assert_eq!(ago(now - 30 * 3_600_000, now), "yesterday");
        assert_eq!(count(1234), "1,234");
        assert_eq!(short_date(20261105), "11/5/2026");
        Regional::pin(before);
        // C / POSIX: no grouping, day-first dates with the month as a word.
        let neutral = Regional::neutral();
        assert_eq!(neutral.group(1234), "1234");
        let secs = 1_800_000_000; // 2027-01-15 08:00 UTC
        let today = local_day(secs);
        assert!(posted_at_on(secs, today, TimeFormat::H24, &neutral).starts_with("Today, at "));
        let older = posted_at_on(secs - 10 * 86_400, today, TimeFormat::H24, &neutral);
        assert!(older.contains(", at ") && !older.contains('/'), "{older}");
    }
}
