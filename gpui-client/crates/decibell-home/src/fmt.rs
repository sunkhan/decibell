//! Every date, time of day and count this crate shows, in one place: the owner's decision of
//! 2026-11-26 is that they follow the system's regional format settings (the UI language stays
//! English). `decibell_model::regional` provides that on main; until it is merged here these give
//! the en-US forms the renderer showed in an en-US build. Durations ("5m", "3h" in the DM list,
//! "10 min" timeouts) and badge counts stay as the renderer writes them, elsewhere.

use chrono::{Datelike, Local, TimeZone};
use gpui::{App, SharedString};

/// A count with digit grouping ("1,234"): tab and section counts, the status message's length.
pub fn count(n: usize) -> SharedString {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out.into()
}

/// A short date (`toLocaleDateString()`: "10/9/2025"); "—" for none (`formatJoined`).
pub fn short_date(epoch: i64) -> SharedString {
    if epoch == 0 {
        return "—".into();
    }
    match Local.timestamp_opt(epoch, 0).single() {
        Some(t) => format!("{}/{}/{}", t.month(), t.day(), t.year()).into(),
        None => "—".into(),
    }
}

/// A date with its time of day (`formatDateTime`: `dateStyle: "medium", timeStyle: "short"`,
/// "Nov 6, 2026, 14:32" or "…, 2:32 PM" by the clock setting).
pub fn date_time(epoch: i64, cx: &App) -> SharedString {
    let Some(t) = Local.timestamp_opt(epoch, 0).single() else { return SharedString::default() };
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let format = decibell_model::Model::global(cx).settings.read(cx).time_format();
    let clock = decibell_model::chat::time::format_clock(epoch, format);
    format!("{} {}, {}, {clock}", MONTHS[t.month0() as usize], t.day(), t.year()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_group_their_digits() {
        assert_eq!(count(0).as_ref(), "0");
        assert_eq!(count(999).as_ref(), "999");
        assert_eq!(count(1000).as_ref(), "1,000");
        assert_eq!(count(1_234_567).as_ref(), "1,234,567");
        assert_eq!(short_date(0).as_ref(), "—");
    }
}
