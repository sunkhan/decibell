//! Every date, time of day and count Settings shows, in one place (owner, 2026-11-26: the native
//! client's dates, times and counts follow the system's regional format; the UI language stays
//! English). For now the output is en-US, as the renderer's `toLocale*` gave it in an English
//! locale; `decibell_model::regional` replaces the bodies here (short / medium dates, the
//! 12 / 24-hour clock, digit grouping, `relative`) without touching the tabs. Durations ("100 ms",
//! "After 10 minutes") and sizes are not formats of this kind and stay where they are.

use chrono::{Local, TimeZone as _};
use decibell_model::stores::settings::TimeFormat;

/// `formatAgo` (`features/transfers/format.ts`): "just now", "5 min ago", "3 h ago", "yesterday",
/// else the short date ("Nov 20"). Devices' "Last active …".
pub fn ago(at: i64, now: i64) -> String {
    let s = (now - at).max(0);
    if s < 60 {
        return "just now".into();
    }
    if s < 3600 {
        return format!("{} min ago", s / 60);
    }
    if s < 86_400 {
        return format!("{} h ago", s / 3600);
    }
    if s < 2 * 86_400 {
        return "yesterday".into();
    }
    short_date(at)
}

/// `toLocaleDateString(undefined, { month: "short", day: "numeric" })`: "Nov 20".
pub fn short_date(at: i64) -> String {
    Local.timestamp_opt(at, 0).single().map_or(String::new(), |d| d.format("%b %-d").to_string())
}

/// `toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" })`:
/// "Nov 20, 2026". Devices' "Signed in …".
pub fn medium_date(at: i64) -> String {
    Local.timestamp_opt(at, 0).single().map_or(String::new(), |d| d.format("%b %-d, %Y").to_string())
}

/// A time of day in the chosen clock (`formatClock`): "14:32" or "2:32 PM". `Auto` is the
/// system's regional setting.
pub fn clock(at: i64, format: TimeFormat) -> String {
    decibell_model::chat::time::format_clock(at, format)
}

/// Appearance's sample time (`SAMPLE_TIME`, 2026-01-01 14:32 local): an afternoon, so the 12- and
/// 24-hour samples differ.
pub fn sample_time() -> i64 {
    Local.with_ymd_and_hms(2026, 1, 1, 14, 32, 0).single().map_or(0, |t| t.timestamp())
}

/// A count with the digit grouping ("1,234"): Devices' "Every device but this one (3)".
pub fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_the_renderer_wrote_them_in_english() {
        let now = 1_800_000_000;
        assert_eq!(ago(now - 10, now), "just now");
        assert_eq!(ago(now - 5 * 60, now), "5 min ago");
        assert_eq!(ago(now - 3 * 3600, now), "3 h ago");
        assert_eq!(ago(now - 30 * 3600, now), "yesterday");
        assert_eq!(ago(now + 50, now), "just now", "a clock ahead of ours");
        assert_eq!(ago(now - 9 * 86_400, now), short_date(now - 9 * 86_400));
        assert!(medium_date(now).ends_with(", 2027"), "{}", medium_date(now));
        assert_eq!(clock(sample_time(), TimeFormat::H24), "14:32");
        assert_eq!(clock(sample_time(), TimeFormat::H12), "2:32 PM");
        assert_eq!(count(3), "3");
        assert_eq!(count(1234), "1,234");
        assert_eq!(count(1_234_567), "1,234,567");
    }
}
