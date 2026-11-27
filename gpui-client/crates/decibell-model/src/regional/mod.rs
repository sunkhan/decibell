//! Dates, times of day and numbers as the user's system is set up to write them, in the app's
//! language (English). Owner instruction (2026-11-26): the formats follow the system's regional
//! settings, not the United States' and not the UI language (the Electron client formats through
//! `Intl`, i.e. by its UI language; Chromium ignores the OS's separate "formats" setting).
//!
//! **Never format a date, a time of day or a count by hand** (`gpui-client/CONVENTIONS.md`,
//! "Regional formats"): `Regional::get()` is read once at start-up and cached.
//!
//! What is taken from the system (`system.rs`): the numeric short-date pattern (field order,
//! separators, zero padding, two- or four-digit year), 12- or 24-hour time with the hour's padding,
//! the first day of the week, and the digit grouping and decimal mark. What is the app's: every
//! word — weekday and month names, "Yesterday, at", AM / PM — so a German-format user reads
//! "Saturday, 7 November 2026", never "Samstag". Long and medium dates are composed from English
//! names in the short pattern's field order ([`Order`]).
//!
//! The C / POSIX locale is "no regional preference" (its `%m/%d/%y` is a leftover, not a choice):
//! short date "7 Nov 2026", long "Saturday, 7 November 2026", 24-hour "14:32", Monday first, no
//! digit grouping.
//!
//! | Renderer (`Intl`, en-US) | Here |
//! |---|---|
//! | `formatTimestamp` (`MessageBubble.tsx`): "2:32 PM", "Yesterday, at 2:32 PM", "11/7/2026, at 2:32 PM" | [`Regional::message_stamp`] |
//! | `formatClock` / `formatClockCompact` (`lib/timeFormat.ts`) | [`Regional::clock`], [`Regional::clock_compact`], [`Regional::time`] |
//! | `formatFullDateTime`: "Saturday, November 7, 2026 at 2:32 PM" | [`Regional::full_date_time`] |
//! | `formatDateTime`: "Nov 7, 2026, 2:32 PM" | [`Regional::date_time`] |
//! | `new Intl.DateTimeFormat()` / `toLocaleDateString()`: "11/7/2026" | [`Regional::short_date`] |
//! | `{ day, month: "short", year }`: "Nov 7, 2026" | [`Regional::medium_date`] |
//! | `DAY_FMT` (the day divider): "Saturday, November 7, 2026" | [`Regional::long_date`] |
//! | `{ month: "short" \| "long", day }`: "Nov 7" | [`Regional::month_day`] |
//! | `{ weekday: "long", month: "long", day }`: "Saturday, November 7" | [`Regional::weekday_month_day`] |
//! | `{ month: "long", year }` (the date picker): "November 2026" | [`Regional::month_year`] |
//! | `Intl.Locale…getWeekInfo().firstDay` | [`Regional::first_weekday`] |
//! | `n.toLocaleString()`, `Intl.NumberFormat`: "1,234" | [`Regional::group`], [`Regional::decimal`] |
//! | `formatAgo` (`transfers/format.ts`, devices' "Last active") | [`Regional::relative`] |

mod pattern;
mod system;
#[cfg(test)]
mod tests;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};

use chrono::{Datelike as _, Local, TimeZone as _, Timelike as _};
pub use chrono::Weekday;
pub use pattern::Order;
use pattern::{Clock, DatePattern, Field, Month, Numbers};
pub use system::is_c_locale;

use crate::stores::settings::TimeFormat;

pub const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
pub const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
pub const MONTHS_SHORT: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// "Saturday".
pub fn weekday_name(weekday: Weekday) -> &'static str {
    WEEKDAYS[weekday.num_days_from_sunday() as usize]
}

/// "S" (`{ weekday: "narrow" }`, the date picker's column heads).
pub fn weekday_narrow(weekday: Weekday) -> &'static str {
    &weekday_name(weekday)[..1]
}

/// "November" for 11.
pub fn month_name(month: u8) -> &'static str {
    MONTHS[(month.clamp(1, 12) - 1) as usize]
}

/// "Nov" for 11.
pub fn month_short(month: u8) -> &'static str {
    MONTHS_SHORT[(month.clamp(1, 12) - 1) as usize]
}

/// A calendar day.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Date {
    pub const fn new(year: i32, month: u8, day: u8) -> Date {
        Date { year, month, day }
    }

    /// From the yyyymmdd day numbers the message views keep (`chat::time::local_day`); `None`
    /// for 0 (an unrepresentable time) or anything that isn't a date.
    pub fn from_day_number(day: i32) -> Option<Date> {
        let (y, m, d) = (day / 10000, day / 100 % 100, day % 100);
        ((1..=12).contains(&m) && (1..=31).contains(&d) && day > 0).then(|| Date::new(y, m as u8, d as u8))
    }

    /// yyyymmdd.
    pub fn day_number(self) -> i32 {
        self.year * 10000 + self.month as i32 * 100 + self.day as i32
    }

    pub fn weekday(self) -> Weekday {
        // Sakamoto's method; 0 = Sunday.
        const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let (m, d) = (self.month.clamp(1, 12) as i32, self.day as i32);
        let y = if m < 3 { self.year - 1 } else { self.year };
        let from_sunday = (y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400) + T[(m - 1) as usize] + d).rem_euclid(7);
        Weekday::try_from(((from_sunday + 6) % 7) as u8).expect("0..7")
    }

    /// The day before.
    pub fn previous(self) -> Date {
        if self.day > 1 {
            return Date::new(self.year, self.month, self.day - 1);
        }
        let (year, month) = if self.month > 1 { (self.year, self.month - 1) } else { (self.year - 1, 12) };
        Date::new(year, month, days_in_month(year, month))
    }
}

fn days_in_month(year: i32, month: u8) -> u8 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// A moment on the local wall clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub date: Date,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Unix seconds on the local wall clock (the system's zone through `chrono::Local`, which reads
/// it once and checks it again at most once a second; DST included). `None` when unrepresentable.
pub fn local(secs: i64) -> Option<LocalTime> {
    let t = Local.timestamp_opt(secs, 0).single()?;
    Some(LocalTime {
        date: Date::new(t.year(), t.month() as u8, t.day() as u8),
        hour: t.hour() as u8,
        minute: t.minute() as u8,
        second: t.second() as u8,
    })
}

/// The first second of the local day after `secs` (unix seconds; the system's zone and its DST
/// rules): when "today" changes for the chat's stamps and the custom status's "Today". `None`
/// when unrepresentable.
pub fn next_local_midnight(secs: i64) -> Option<i64> {
    next_midnight_in(&Local, secs)
}

/// [`next_local_midnight`] in `tz`. Where local midnight doesn't exist (the clocks jump over it: a
/// DST change at 00:00, as in Chile, Paraguay or Cuba) the day starts where the jump lands, as
/// JavaScript's `setHours(24, 0, 0, 0)` reads a missing local time with the offset in force
/// before it (`CustomStatusModal`'s `clearAfterSeconds`); a midnight that happens twice is the
/// first.
pub(crate) fn next_midnight_in<Tz: chrono::TimeZone>(tz: &Tz, secs: i64) -> Option<i64> {
    let t = tz.timestamp_opt(secs, 0).single()?;
    let next = t.date_naive().succ_opt()?.and_hms_opt(0, 0, 0)?;
    match tz.from_local_datetime(&next) {
        chrono::LocalResult::Single(m) | chrono::LocalResult::Ambiguous(m, _) => Some(m.timestamp()),
        chrono::LocalResult::None => {
            let before = tz.from_local_datetime(&(next - chrono::Duration::minutes(1))).earliest()?;
            Some(before.timestamp() + 60)
        }
    }
}

/// How a month is named in a composed date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonthName {
    /// "Nov"
    Short,
    /// "November"
    Long,
}

/// The regional formats. See the module doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Regional {
    date: DatePattern,
    clock: Clock,
    first_weekday: Weekday,
    numbers: Numbers,
    origin: Box<str>,
}

/// What glibc's locale data gives (`system.rs`): the test hook builds one by hand.
#[derive(Clone, Copy, Debug, Default)]
pub struct Posix<'a> {
    /// The locale names resolved for `LC_TIME` and `LC_NUMERIC` (C / POSIX: no preference).
    pub time_locale: &'a str,
    pub numeric_locale: &'a str,
    /// `D_FMT`; `None`: the time locale couldn't be read (not installed).
    pub d_fmt: Option<&'a str>,
    pub t_fmt: &'a str,
    pub t_fmt_ampm: &'a str,
    /// From `_NL_TIME_WEEK_1STDAY` / `_NL_TIME_FIRST_WEEKDAY` (see [`glibc_first_weekday`]).
    pub first_weekday: Option<Weekday>,
    pub thousep: &'a str,
    pub radix: &'a str,
    pub grouping: &'a [u8],
}

/// What `GetLocaleInfoEx(LOCALE_NAME_USER_DEFAULT, …)` gives.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsSettings<'a> {
    /// `LOCALE_SSHORTDATE`: "M/d/yyyy", "dd.MM.yyyy".
    pub short_date: Option<&'a str>,
    /// `LOCALE_SSHORTTIME` (else `LOCALE_STIMEFORMAT`): "h:mm tt", "HH:mm".
    pub time_format: Option<&'a str>,
    /// `LOCALE_ITIME`: "0" 12-hour, "1" 24-hour.
    pub itime: Option<&'a str>,
    /// `LOCALE_IFIRSTDAYOFWEEK`: "0" Monday … "6" Sunday.
    pub first_day_of_week: Option<&'a str>,
    /// `LOCALE_STHOUSAND`, `LOCALE_SDECIMAL`, `LOCALE_SGROUPING` ("3;0").
    pub thousand: &'a str,
    pub decimal: &'a str,
    pub grouping: &'a str,
}

static CURRENT: AtomicPtr<Regional> = AtomicPtr::new(std::ptr::null_mut());
static SYSTEM: OnceLock<Regional> = OnceLock::new();
static EN_US: OnceLock<Regional> = OnceLock::new();

impl Regional {
    /// The formats in use: the system's (read on first use, then cached), unless a test or the
    /// capture tool pinned others ([`Regional::pin`]). An atomic load per call.
    pub fn get() -> &'static Regional {
        let pinned = CURRENT.load(Ordering::Acquire);
        if pinned.is_null() {
            Regional::system()
        } else {
            // SAFETY: only `&'static Regional`s are stored (see `pin`).
            unsafe { &*pinned }
        }
    }

    /// The system's settings, read once (`system.rs`).
    pub fn system() -> &'static Regional {
        SYSTEM.get_or_init(|| {
            let regional = system::read();
            log::info!(
                "[regional] {}: {} · {} · {} · first day {:?} · {}",
                regional.origin,
                regional.short_date(Date::new(2026, 11, 7)),
                regional.long_date(Date::new(2026, 11, 7)),
                regional.time(14, 32, TimeFormat::Auto),
                regional.first_weekday,
                regional.decimal(1234567.5, 1),
            );
            regional
        })
    }

    /// Test and capture hook: `Some` makes [`Regional::get`] answer `regional` from now on
    /// (`None`: the system's again). Returns what was pinned before, to restore it. Views keep
    /// what they built: pin before they build (a test opens its window after pinning).
    pub fn pin(regional: Option<&'static Regional>) -> Option<&'static Regional> {
        let new = regional.map_or(std::ptr::null_mut(), |r| r as *const Regional as *mut Regional);
        let old = CURRENT.swap(new, Ordering::AcqRel);
        // SAFETY: as in `get`.
        (!old.is_null()).then(|| unsafe { &*old })
    }

    /// Leaks it, for [`Regional::pin`] (tests and tools: a few hundred bytes each).
    pub fn leak(self) -> &'static Regional {
        Box::leak(Box::new(self))
    }

    /// What `Intl` gives the Electron renderer on an en-US machine (Windows' en-US settings:
    /// `M/d/yyyy`, `h:mm:ss tt`, Sunday first, 1,234.5; equal to the renderer's strings, see the
    /// tests): what captures and the row comparison with Electron pin, so both clients draw the
    /// same strings.
    pub fn en_us() -> &'static Regional {
        EN_US.get_or_init(|| {
            let mut regional = Regional::from_windows(WindowsSettings {
                short_date: Some("M/d/yyyy"),
                time_format: Some("h:mm:ss tt"),
                itime: Some("0"),
                first_day_of_week: Some("6"),
                thousand: ",",
                decimal: ".",
                grouping: "3;0",
            });
            regional.origin = "en-US (pinned: the Electron renderer's Intl on an en-US machine)".into();
            regional
        })
    }

    /// No regional preference (the C / POSIX locale): "7 Nov 2026", "Saturday, 7 November 2026",
    /// "14:32", Monday first, "1234.5".
    pub fn neutral() -> Regional {
        Regional {
            date: DatePattern::neutral(),
            clock: Clock::neutral(),
            first_weekday: Weekday::Mon,
            numbers: Numbers::neutral(),
            origin: "C (no regional preference)".into(),
        }
    }

    /// From glibc's locale data (also the test hook for the Unix path).
    pub fn from_posix(posix: Posix<'_>) -> Regional {
        let mut regional = Regional::neutral();
        let neutral_time = is_c_locale(posix.time_locale) || posix.d_fmt.is_none();
        if !neutral_time {
            let d_fmt = posix.d_fmt.unwrap_or_default();
            match DatePattern::parse_posix(d_fmt) {
                Some(date) => regional.date = date,
                None => log::warn!("[regional] D_FMT {d_fmt:?} of {} is not a numeric date: using d MMM yyyy", posix.time_locale),
            }
            regional.clock = Clock::parse_posix(posix.t_fmt, posix.t_fmt_ampm);
            regional.first_weekday = posix.first_weekday.unwrap_or_else(|| region_first_weekday(posix.time_locale));
        }
        regional.numbers = if is_c_locale(posix.numeric_locale) {
            Numbers::neutral()
        } else {
            Numbers::from_posix(posix.thousep, posix.radix, posix.grouping)
        };
        let describe = |name: &str, neutral: bool| if neutral { format!("{name} (no preference)") } else { name.to_string() };
        regional.origin = format!(
            "LC_TIME {} · LC_NUMERIC {}",
            describe(posix.time_locale, neutral_time),
            describe(posix.numeric_locale, is_c_locale(posix.numeric_locale))
        )
        .into();
        regional
    }

    /// From Windows' user format settings (also the test hook for that path).
    pub fn from_windows(settings: WindowsSettings<'_>) -> Regional {
        let date = settings.short_date.and_then(DatePattern::parse_windows).unwrap_or_else(|| {
            log::warn!("[regional] short date {:?} is not a numeric date: using d MMM yyyy", settings.short_date);
            DatePattern::neutral()
        });
        let clock = Clock::parse_windows(settings.time_format.unwrap_or(""), settings.itime);
        // 0 = Monday … 6 = Sunday.
        let first_weekday = settings
            .first_day_of_week
            .and_then(|v| v.trim().parse::<u8>().ok())
            .and_then(|v| Weekday::try_from(v).ok())
            .unwrap_or(Weekday::Mon);
        Regional {
            date,
            clock,
            first_weekday,
            numbers: Numbers::from_windows(settings.thousand, settings.decimal, settings.grouping),
            origin: format!("Windows user formats ({})", settings.short_date.unwrap_or("?")).into(),
        }
    }

    /// Where these settings came from, for logs.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The short date's leading field: what the composed forms follow.
    pub fn order(&self) -> Order {
        self.date.order()
    }

    /// The first column of a calendar.
    pub fn first_weekday(&self) -> Weekday {
        self.first_weekday
    }

    /// Whether `pref` shows a 12-hour clock: the user's choice, or for `Auto` the system's.
    pub fn hour12(&self, pref: TimeFormat) -> bool {
        match pref {
            TimeFormat::Auto => self.clock.h12,
            TimeFormat::H12 => true,
            TimeFormat::H24 => false,
        }
    }

    // ── Dates ──

    /// The numeric short date: "11/7/2026", "07.11.2026", "2026-11-07"; "7 Nov 2026" with no
    /// preference.
    pub fn short_date(&self, date: Date) -> String {
        let mut out = String::with_capacity(16);
        self.write_short_date(&mut out, date);
        out
    }

    pub fn write_short_date(&self, out: &mut String, date: Date) {
        out.push_str(&self.date.lead);
        for (field, after) in &self.date.parts {
            match *field {
                Field::Day { pad } => push_num(out, date.day as u32, if pad { 2 } else { 1 }),
                Field::Month(Month::Number { pad }) => push_num(out, date.month as u32, if pad { 2 } else { 1 }),
                Field::Month(Month::Short) => out.push_str(month_short(date.month)),
                Field::Month(Month::Long) => out.push_str(month_name(date.month)),
                Field::Year { full: true } => push_year(out, date.year),
                Field::Year { full: false } => push_num(out, date.year.rem_euclid(100) as u32, 2),
            }
            out.push_str(after);
        }
    }

    /// "7 Nov 2026" / "Nov 7, 2026" / "2026 Nov 7".
    pub fn medium_date(&self, date: Date) -> String {
        let mut out = String::with_capacity(16);
        self.write_composed(&mut out, date, MonthName::Short, true);
        out
    }

    /// "Saturday, 7 November 2026" / "Saturday, November 7, 2026" / "Saturday, 2026 November 7".
    pub fn long_date(&self, date: Date) -> String {
        let mut out = String::with_capacity(32);
        out.push_str(weekday_name(date.weekday()));
        out.push_str(", ");
        self.write_composed(&mut out, date, MonthName::Long, true);
        out
    }

    /// "7 Nov" / "Nov 7" (a year-first order writes the month first too).
    pub fn month_day(&self, date: Date, month: MonthName) -> String {
        let mut out = String::with_capacity(16);
        self.write_composed(&mut out, date, month, false);
        out
    }

    /// "7 November 2026" / "November 7, 2026" / "2026 November 7" (`{ month: "long", day,
    /// year }`: the audit log's day heading in another year).
    pub fn month_day_year(&self, date: Date, month: MonthName) -> String {
        let mut out = String::with_capacity(24);
        self.write_composed(&mut out, date, month, true);
        out
    }

    /// "Saturday, 7 November" / "Saturday, November 7".
    pub fn weekday_month_day(&self, date: Date) -> String {
        let mut out = String::with_capacity(24);
        out.push_str(weekday_name(date.weekday()));
        out.push_str(", ");
        self.write_composed(&mut out, date, MonthName::Long, false);
        out
    }

    /// "November 2026" / "2026 November".
    pub fn month_year(&self, year: i32, month: u8) -> String {
        let mut out = String::with_capacity(16);
        if self.order() == Order::YearFirst {
            push_year(&mut out, year);
            out.push(' ');
            out.push_str(month_name(month));
        } else {
            out.push_str(month_name(month));
            out.push(' ');
            push_year(&mut out, year);
        }
        out
    }

    fn write_composed(&self, out: &mut String, date: Date, month: MonthName, year: bool) {
        let name = match month {
            MonthName::Short => month_short(date.month),
            MonthName::Long => month_name(date.month),
        };
        match (self.order(), year) {
            (Order::DayFirst, _) => {
                push_num(out, date.day as u32, 1);
                out.push(' ');
                out.push_str(name);
                if year {
                    out.push(' ');
                    push_year(out, date.year);
                }
            }
            (Order::MonthFirst, _) | (Order::YearFirst, false) => {
                out.push_str(name);
                out.push(' ');
                push_num(out, date.day as u32, 1);
                if year {
                    out.push_str(", ");
                    push_year(out, date.year);
                }
            }
            (Order::YearFirst, true) => {
                push_year(out, date.year);
                out.push(' ');
                out.push_str(name);
                out.push(' ');
                push_num(out, date.day as u32, 1);
            }
        }
    }

    // ── Times of day ──

    /// "14:32" / "2:32 PM" (`formatClock`).
    pub fn time(&self, hour: u8, minute: u8, pref: TimeFormat) -> String {
        let mut out = String::with_capacity(8);
        self.write_time(&mut out, hour, minute, None, pref, false);
        out
    }

    /// "14:32:05" / "2:32:05 PM".
    pub fn time_with_seconds(&self, hour: u8, minute: u8, second: u8, pref: TimeFormat) -> String {
        let mut out = String::with_capacity(12);
        self.write_time(&mut out, hour, minute, Some(second), pref, false);
        out
    }

    /// The clock without AM / PM ("2:32"), for the hover time beside a grouped message
    /// (`formatClockCompact`).
    pub fn time_compact(&self, hour: u8, minute: u8, pref: TimeFormat) -> String {
        let mut out = String::with_capacity(8);
        self.write_time(&mut out, hour, minute, None, pref, true);
        out
    }

    pub fn write_time(&self, out: &mut String, hour: u8, minute: u8, second: Option<u8>, pref: TimeFormat, compact: bool) {
        let h12 = self.hour12(pref);
        if h12 {
            let shown = if hour.is_multiple_of(12) { 12 } else { hour % 12 };
            push_num(out, shown as u32, if self.clock.pad12 { 2 } else { 1 });
        } else {
            push_num(out, hour as u32, if self.clock.pad24 { 2 } else { 1 });
        }
        out.push(self.clock.separator);
        push_num(out, minute as u32, 2);
        if let Some(second) = second {
            out.push(self.clock.separator);
            push_num(out, second as u32, 2);
        }
        if h12 && !compact {
            out.push_str(if hour < 12 { " AM" } else { " PM" });
        }
    }

    /// The local time of unix seconds `secs` (`formatClock`); "" when unrepresentable.
    pub fn clock(&self, secs: i64, pref: TimeFormat) -> String {
        local(secs).map_or_else(String::new, |t| self.time(t.hour, t.minute, pref))
    }

    /// [`Regional::clock`] without AM / PM (`formatClockCompact`).
    pub fn clock_compact(&self, secs: i64, pref: TimeFormat) -> String {
        local(secs).map_or_else(String::new, |t| self.time_compact(t.hour, t.minute, pref))
    }

    // ── Moments ──

    /// The header time of a message (`formatTimestamp` in `MessageBubble.tsx`): the time today,
    /// "Yesterday, at 2:32 PM", else "11/7/2026, at 2:32 PM" (the short date). `today` is the
    /// local day as yyyymmdd (the list's). "" when `secs` is unrepresentable.
    pub fn message_stamp(&self, secs: i64, today: i32, pref: TimeFormat) -> String {
        let (Some(time), Some(today)) = (local(secs), Date::from_day_number(today)) else { return String::new() };
        self.message_stamp_at(time, today, pref)
    }

    /// [`Regional::message_stamp`] on the local wall clock.
    pub fn message_stamp_at(&self, time: LocalTime, today: Date, pref: TimeFormat) -> String {
        let mut out = String::with_capacity(32);
        if time.date == today.previous() {
            out.push_str("Yesterday, at ");
        } else if time.date != today {
            self.write_short_date(&mut out, time.date);
            out.push_str(", at ");
        }
        self.write_time(&mut out, time.hour, time.minute, None, pref, false);
        out
    }

    /// "Nov 7, 2026, 2:32 PM" / "7 Nov 2026, 14:32" (`formatDateTime`: an expiry, a join date);
    /// "" when unrepresentable.
    pub fn date_time(&self, secs: i64, pref: TimeFormat) -> String {
        let Some(t) = local(secs) else { return String::new() };
        let mut out = String::with_capacity(28);
        self.write_composed(&mut out, t.date, MonthName::Short, true);
        out.push_str(", ");
        self.write_time(&mut out, t.hour, t.minute, None, pref, false);
        out
    }

    /// "Saturday, November 7, 2026 at 2:32 PM" (`formatFullDateTime`: tooltips).
    pub fn full_date_time(&self, secs: i64, pref: TimeFormat) -> String {
        local(secs).map_or_else(String::new, |t| self.full_date_time_at(t, pref, false))
    }

    /// With seconds (the audit log's tooltip: `dateStyle: "full", timeStyle: "medium"`).
    pub fn full_date_time_with_seconds(&self, secs: i64, pref: TimeFormat) -> String {
        local(secs).map_or_else(String::new, |t| self.full_date_time_at(t, pref, true))
    }

    pub fn full_date_time_at(&self, t: LocalTime, pref: TimeFormat, seconds: bool) -> String {
        let mut out = self.long_date(t.date);
        out.push_str(" at ");
        self.write_time(&mut out, t.hour, t.minute, seconds.then_some(t.second), pref, false);
        out
    }

    /// "just now", "5 min ago", "3 h ago", "yesterday", else "Nov 7" (`formatAgo`: the devices'
    /// "Last active", the transfers' times). Unix seconds.
    pub fn relative(&self, then: i64, now: i64) -> String {
        let s = (now - then).max(0);
        match s {
            0..60 => "just now".into(),
            60..3_600 => format!("{} min ago", s / 60),
            3_600..86_400 => format!("{} h ago", s / 3_600),
            86_400..172_800 => "yesterday".into(),
            _ => local(then).map_or_else(String::new, |t| self.month_day(t.date, MonthName::Short)),
        }
    }

    // ── Numbers ──

    /// A count: "1,234" / "1.234" / "1 234" / "1234" (`n.toLocaleString()`).
    pub fn group(&self, n: u64) -> String {
        let mut digits = String::with_capacity(20);
        push_u64(&mut digits, n);
        let mut out = String::with_capacity(digits.len() + digits.len() / 2 * self.numbers.group.len());
        self.numbers.write_grouped(&mut out, &digits);
        out
    }

    /// A signed count.
    pub fn group_signed(&self, n: i64) -> String {
        let grouped = self.group(n.unsigned_abs());
        if n < 0 { format!("-{grouped}") } else { grouped }
    }

    /// A decimal number with `digits` decimals: "1,234.5" / "1.234,5".
    pub fn decimal(&self, value: f64, digits: usize) -> String {
        if !value.is_finite() {
            return value.to_string();
        }
        let text = format!("{:.*}", digits, value.abs());
        let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
        let mut out = String::with_capacity(text.len() + 8);
        if value.is_sign_negative() && text.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
            out.push('-');
        }
        self.numbers.write_grouped(&mut out, int);
        if !frac.is_empty() {
            out.push_str(&self.numbers.decimal);
            out.push_str(frac);
        }
        out
    }

    /// The decimal mark ("." or ",").
    pub fn decimal_mark(&self) -> &str {
        &self.numbers.decimal
    }
}

/// glibc's first day of the week: the weekday of `reference` (yyyymmdd, `_NL_TIME_WEEK_1STDAY`)
/// moved on by `first_weekday − 1` (`_NL_TIME_FIRST_WEEKDAY`, 1-based; 0 is read as 1).
pub fn glibc_first_weekday(reference: u32, first_weekday: u8) -> Option<Weekday> {
    let date = Date::from_day_number(reference as i32)?;
    let offset = first_weekday.max(1) as u32 - 1;
    let from_sunday = (date.weekday().num_days_from_sunday() + offset) % 7;
    Weekday::try_from(((from_sunday + 6) % 7) as u8).ok()
}

/// The first day of the week by the locale name's region (CLDR's week data), where the system
/// doesn't say (macOS's C library): Sunday, Saturday or Monday.
pub fn region_first_weekday(locale: &str) -> Weekday {
    const SUNDAY: [&str; 49] = [
        "AG", "AS", "BD", "BR", "BS", "BT", "BW", "BZ", "CA", "CN", "CO", "DM", "DO", "ET", "GT", "GU", "HK", "HN", "ID", "IL", "IN",
        "JM", "JP", "KE", "KH", "KR", "LA", "MH", "MM", "MO", "MT", "MX", "MZ", "NI", "NP", "PA", "PE", "PH", "PK", "PR", "PT", "PY",
        "SA", "SG", "SV", "TH", "TT", "TW", "US",
    ];
    const SUNDAY_MORE: [&str; 6] = ["UM", "VE", "VI", "WS", "ZA", "ZW"];
    const SATURDAY: [&str; 15] = ["AE", "AF", "BH", "DJ", "DZ", "EG", "IQ", "IR", "JO", "KW", "LY", "OM", "QA", "SD", "SY"];
    // `en_US.UTF-8@euro` → "US".
    let base = locale.split(['.', '@']).next().unwrap_or("");
    let region = base.split_once(['_', '-']).map_or("", |(_, region)| region);
    if SUNDAY.contains(&region) || SUNDAY_MORE.contains(&region) {
        Weekday::Sun
    } else if SATURDAY.contains(&region) {
        Weekday::Sat
    } else {
        Weekday::Mon
    }
}

/// `n` in decimal, at least `width` digits (zero-padded).
fn push_num(out: &mut String, n: u32, width: usize) {
    let mut buffer = [b'0'; 10];
    let mut start = buffer.len();
    let mut n = n;
    loop {
        start -= 1;
        buffer[start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    start = start.min(buffer.len().saturating_sub(width));
    for &digit in &buffer[start..] {
        out.push(digit as char);
    }
}

fn push_u64(out: &mut String, n: u64) {
    let mut buffer = [0u8; 20];
    let mut start = buffer.len();
    let mut n = n;
    loop {
        start -= 1;
        buffer[start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for &digit in &buffer[start..] {
        out.push(digit as char);
    }
}

fn push_year(out: &mut String, year: i32) {
    if year < 0 {
        out.push('-');
    }
    push_num(out, year.unsigned_abs(), 1);
}
