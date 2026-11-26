//! The date picker's arithmetic (`features/search/DatePicker.tsx` and the box's `moveDay`): the
//! month grid from the system's first day of the week, the column heads, moving the highlighted
//! day. Days after today are off: nothing was posted then.

use chrono::{Datelike as _, Days, Months, Weekday};
use decibell_model::chat::search::Day;
use decibell_model::regional::weekday_narrow;

/// One day of the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub day: Day,
    pub in_month: bool,
}

/// The grid of `active`'s month: whole weeks from the one holding the 1st, 6 of them, or 5 when
/// the sixth is entirely next month's.
pub fn cells(active: Day, first: Weekday) -> Vec<Cell> {
    let month_start = active.with_day(1).unwrap_or(active);
    // getDay(): 0 = Sunday; shift to the locale's first weekday.
    let lead = (month_start.weekday().num_days_from_sunday() + 7 - first.num_days_from_sunday()) % 7;
    let grid_start = month_start - Days::new(lead as u64);
    let mut out: Vec<Cell> = (0..42u64)
        .map(|i| {
            let day = grid_start + Days::new(i);
            Cell { day, in_month: day.month() == month_start.month() && day.year() == month_start.year() }
        })
        .collect();
    if !out[35].in_month {
        out.truncate(35);
    }
    out
}

/// The column heads, from the first weekday (`{ weekday: "narrow" }`).
pub fn weekday_heads(first: Weekday) -> [&'static str; 7] {
    let mut day = first;
    std::array::from_fn(|_| {
        let head = weekday_narrow(day);
        day = day.succ();
        head
    })
}

/// `shiftMonth(n)`: the same day of the month `n` months on (at most the 28th), never after today.
pub fn shift_month(active: Day, n: i32, today: Day) -> Day {
    let day = active.day().min(28);
    let base = active.with_day(day).unwrap_or(active);
    let moved = if n >= 0 { base.checked_add_months(Months::new(n as u32)) } else { base.checked_sub_months(Months::new(n.unsigned_abs())) };
    moved.unwrap_or(base).min(today)
}

/// `moveDay(days)`: the arrow keys (±1, ±7), never after today.
pub fn move_day(active: Day, days: i64, today: Day) -> Day {
    let moved = if days >= 0 { active.checked_add_days(Days::new(days as u64)) } else { active.checked_sub_days(Days::new(days.unsigned_abs())) };
    moved.unwrap_or(active).min(today)
}

/// "Next month" is off while next month's 1st is after today.
pub fn next_month_disabled(active: Day, today: Day) -> bool {
    let next = active.with_day(1).and_then(|d| d.checked_add_months(Months::new(1)));
    next.is_none_or(|next| next > today)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> Day {
        Day::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn grids() {
        // November 2026 starts on a Sunday.
        let sunday = cells(day("2026-11-15"), Weekday::Sun);
        assert_eq!((sunday[0].day, sunday.len()), (day("2026-11-01"), 35));
        let monday = cells(day("2026-11-15"), Weekday::Mon);
        assert_eq!((monday[0].day, monday.len()), (day("2026-10-26"), 42));
        assert!(!monday[0].in_month && monday[6].in_month);
        // February 2026: Sunday 1st, 28 days: four whole weeks, then a week of March.
        let feb = cells(day("2026-02-10"), Weekday::Sun);
        assert_eq!(feb.len(), 35);
        assert!(!feb[28].in_month);
        assert_eq!(weekday_heads(Weekday::Mon), ["M", "T", "W", "T", "F", "S", "S"]);
        assert_eq!(weekday_heads(Weekday::Sun)[0], "S");
    }

    #[test]
    fn moving() {
        let today = day("2026-11-26");
        assert_eq!(shift_month(day("2026-10-31"), 1, today), day("2026-11-26"), "clamped to today");
        assert_eq!(shift_month(day("2026-03-31"), -1, today), day("2026-02-28"));
        assert_eq!(shift_month(day("2026-01-15"), -1, today), day("2025-12-15"));
        assert_eq!(move_day(day("2026-11-25"), 7, today), today);
        assert_eq!(move_day(day("2026-11-01"), -1, today), day("2026-10-31"));
        assert!(next_month_disabled(day("2026-11-03"), today));
        assert!(!next_month_disabled(day("2026-10-31"), today));
    }
}
