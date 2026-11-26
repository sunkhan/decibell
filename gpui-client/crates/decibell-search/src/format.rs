//! The words, dates and counts the search UI draws, each through `decibell_model::regional`
//! (CONVENTIONS "Regional formats"): the chips' and the menu's days (`formatDay`), the results'
//! times (`when`), the panel's count.

use chrono::Datelike as _;
use decibell_model::chat::search::Day;
use decibell_model::regional::{Date, Regional, local};
use decibell_model::stores::settings::TimeFormat;
use gpui::{App, Global};

/// "Today" pinned for screens and tests (the clock must not show in a capture). Unset, the
/// system's local day.
pub struct PinnedToday(pub Day);

impl Global for PinnedToday {}

/// The local calendar day the box and the results take as today.
pub fn today(cx: &App) -> Day {
    cx.try_global::<PinnedToday>().map_or_else(decibell_model::chat::search::today, |pinned| pinned.0)
}

pub fn to_date(day: Day) -> Date {
    Date::new(day.year(), day.month() as u8, day.day() as u8)
}

/// `formatDay` (`{ day: "numeric", month: "short", year: "numeric" }`): "Oct 3, 2026".
pub fn format_day(day: Day) -> String {
    Regional::get().medium_date(to_date(day))
}

/// A result's time (`when` in `SearchResultsPanel.tsx`): "Today at 2:32 PM", "Yesterday at …",
/// else "Oct 3, 2026 at 2:32 PM". "" when unrepresentable.
pub fn result_stamp(secs: i64, today: Day, pref: TimeFormat) -> String {
    let Some(at) = local(secs) else { return String::new() };
    let regional = Regional::get();
    let time = regional.time(at.hour, at.minute, pref);
    let today = to_date(today);
    if at.date == today {
        format!("Today at {time}")
    } else if at.date == today.previous() {
        format!("Yesterday at {time}")
    } else {
        format!("{} at {time}", regional.medium_date(at.date))
    }
}

/// The panel's title (`SearchResultsPanel`'s `title`).
pub fn results_title(loading: bool, error: bool, total: i64, capped: bool, results: usize) -> String {
    if loading {
        "Searching…".into()
    } else if error {
        "Search".into()
    } else if total == 0 && results == 0 {
        "No results".into()
    } else if total < 0 {
        "Results".into()
    } else {
        let noun = if total == 1 && !capped { "result" } else { "results" };
        format!("{}{} {noun}", Regional::get().group(total.max(0) as u64), if capped { "+" } else { "" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_titles() {
        let before = Regional::pin(Some(Regional::en_us()));
        assert_eq!(results_title(false, false, 10_000, true, 25), "10,000+ results");
        assert_eq!(results_title(false, false, 1, false, 1), "1 result");
        assert_eq!(results_title(false, false, 0, false, 0), "No results");
        assert_eq!(results_title(false, false, -1, false, 3), "Results");
        assert_eq!(results_title(true, false, 5, false, 0), "Searching…");
        assert_eq!(format_day(Day::from_ymd_opt(2026, 10, 3).unwrap()), "Oct 3, 2026");
        Regional::pin(before);
    }
}
