//! Every date, time of day and count this crate shows, in one place: they follow the system's
//! regional format settings with the app's English words (owner, 2026-11-26), through
//! `decibell_model::regional` (CONVENTIONS "Regional formats"). Durations ("5m", "3h" in the DM
//! list, "10 min" timeouts) and badge counts stay as the renderer writes them, elsewhere.

use decibell_model::regional::{Regional, local};
use gpui::{App, SharedString};

/// A count with the system's digit grouping (`n.toLocaleString()`): tab and section counts, the
/// status message's length.
pub fn count(n: usize) -> SharedString {
    Regional::get().group(n as u64).into()
}

/// A short date (`toLocaleDateString()`); "—" for none (`formatJoined`).
pub fn short_date(epoch: i64) -> SharedString {
    if epoch == 0 {
        return "—".into();
    }
    match local(epoch) {
        Some(t) => Regional::get().short_date(t.date).into(),
        None => "—".into(),
    }
}

/// A date with its time of day (`formatDateTime`: `dateStyle: "medium", timeStyle: "short"`), the
/// clock in the user's 12 / 24-hour choice.
pub fn date_time(epoch: i64, cx: &App) -> SharedString {
    let format = decibell_model::Model::global(cx).settings.read(cx).time_format();
    Regional::get().date_time(epoch, format).into()
}

#[cfg(test)]
mod tests {
    use decibell_model::regional::Regional;

    #[test]
    fn counts_and_dates_follow_the_regional_module() {
        // The global is the system's; en-US pinned the way the captures pin it.
        let before = Regional::pin(Some(Regional::en_us()));
        assert_eq!(super::count(0).as_ref(), "0");
        assert_eq!(super::count(999).as_ref(), "999");
        assert_eq!(super::count(1000).as_ref(), "1,000");
        assert_eq!(super::count(1_234_567).as_ref(), "1,234,567");
        assert_eq!(super::short_date(0).as_ref(), "—");
        assert!(!super::short_date(1_764_000_000).is_empty());
        Regional::pin(before);
    }
}
