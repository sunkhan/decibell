//! Every date, time of day and count this crate shows, in one place (owner decision 2026-11-26:
//! the native client follows the system's regional format settings, the UI language stays
//! English). For now the output is the renderer's en-US; switching to `decibell_model::regional`
//! happens here and nowhere else. File sizes, speeds and durations are not regional: they keep
//! the renderer's own formatting where they are written.

use decibell_model::chat::time::{format_clock, local_day};
use decibell_model::stores::settings::TimeFormat;

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// A local day (`yyyymmdd`) as `Intl.DateTimeFormat()` writes it in en-US: `M/D/YYYY`.
pub(crate) fn short_date(day: i32) -> String {
    format!("{}/{}/{}", day / 100 % 100, day % 100, day / 10000)
}

/// The viewer's "posted" line (`postedAt`): "Today, at 14:32", "Yesterday, at …", else the date.
pub(crate) fn posted_at(secs: Option<i64>, format: TimeFormat) -> String {
    let Some(secs) = secs else { return String::new() };
    let now = now_secs();
    let (day, today) = (local_day(secs), local_day(now));
    let clock = format_clock(secs, format);
    if day == today {
        return format!("Today, at {clock}");
    }
    if day == local_day(now - 86_400) {
        return format!("Yesterday, at {clock}");
    }
    format!("{}, at {clock}", short_date(day))
}

/// `formatAgo` (the Transfers panel): "just now", "N min ago", "N h ago", "yesterday", else the
/// date as `toLocaleDateString(undefined, { month: "short", day: "numeric" })` ("Nov 5").
pub(crate) fn ago(epoch_ms: i64, now_ms: i64) -> String {
    let s = ((now_ms - epoch_ms) as f64 / 1000.).max(0.);
    if s < 60. {
        return "just now".into();
    }
    if s < 3600. {
        return format!("{} min ago", (s / 60.).floor());
    }
    if s < 86_400. {
        return format!("{} h ago", (s / 3600.).floor());
    }
    if s < 2. * 86_400. {
        return "yesterday".into();
    }
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let day = local_day(epoch_ms / 1000);
    format!("{} {}", MONTHS[((day / 100 % 100) as usize).clamp(1, 12) - 1], day % 100)
}

/// A count as the renderer writes it (`${n}`: no digit grouping today).
pub(crate) fn count(n: i64) -> String {
    n.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_reads_like_the_panel() {
        let now = 1_800_000_000_000;
        assert_eq!(ago(now - 30_000, now), "just now");
        assert_eq!(ago(now - 5 * 60_000, now), "5 min ago");
        assert_eq!(ago(now - 3 * 3_600_000, now), "3 h ago");
        assert_eq!(ago(now - 30 * 3_600_000, now), "yesterday");
        assert_eq!(count(1234), "1234");
        assert_eq!(short_date(20261105), "11/5/2026");
    }
}
