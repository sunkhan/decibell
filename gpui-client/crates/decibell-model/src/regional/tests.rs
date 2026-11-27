use chrono::Datelike as _;

use super::*;

const NOV_7: Date = Date::new(2026, 11, 7);

/// glibc's data for a locale, as `/usr/share/i18n/locales/<name>` defines it (only C, de_DE and
/// en_US are installed on the development machine: the rest are fed by hand).
#[allow(clippy::too_many_arguments)]
fn glibc(name: &str, d_fmt: &str, t_fmt: &str, t_fmt_ampm: &str, first_weekday: u8, thousep: &str, radix: &str, grouping: &[u8]) -> Regional {
    Regional::from_posix(Posix {
        time_locale: name,
        numeric_locale: name,
        d_fmt: Some(d_fmt),
        t_fmt,
        t_fmt_ampm,
        first_weekday: glibc_first_weekday(19971130, first_weekday),
        thousep,
        radix,
        grouping,
    })
}

fn en_us_glibc() -> Regional {
    glibc("en_US.UTF-8", "%m/%d/%Y", "%r", "%I:%M:%S %p", 1, ",", ".", &[3])
}

fn de_de_glibc() -> Regional {
    glibc("de_DE.UTF-8", "%d.%m.%Y", "%T", "", 2, ".", ",", &[3])
}

/// (short, medium, long, time at 14:32 with Auto, first weekday, 1234567, 1234567.5)
fn summary(r: &Regional) -> (String, String, String, String, Weekday, String, String) {
    (r.short_date(NOV_7), r.medium_date(NOV_7), r.long_date(NOV_7), r.time(14, 32, TimeFormat::Auto), r.first_weekday(), r.group(1_234_567), r.decimal(1_234_567.5, 1))
}

fn s(text: &str) -> String {
    text.to_string()
}

#[test]
fn glibc_locales() {
    use Weekday::*;
    assert_eq!(
        summary(&en_us_glibc()),
        (s("11/07/2026"), s("Nov 7, 2026"), s("Saturday, November 7, 2026"), s("2:32 PM"), Sun, s("1,234,567"), s("1,234,567.5"))
    );
    let en_gb = glibc("en_GB.UTF-8", "%d/%m/%y", "%T", "%l:%M:%S %P %Z", 2, ",", ".", &[3]);
    assert_eq!(
        summary(&en_gb),
        (s("07/11/26"), s("7 Nov 2026"), s("Saturday, 7 November 2026"), s("14:32"), Mon, s("1,234,567"), s("1,234,567.5"))
    );
    assert_eq!(
        summary(&de_de_glibc()),
        (s("07.11.2026"), s("7 Nov 2026"), s("Saturday, 7 November 2026"), s("14:32"), Mon, s("1.234.567"), s("1.234.567,5"))
    );
    // tr_TR writes dashes in glibc (ICU: dots); its AM / PM are Turkish, the app's are English.
    let tr = glibc("tr_TR.UTF-8", "%d-%m-%Y", "%T", "%I:%M:%S %p", 2, ".", ",", &[3]);
    assert_eq!(summary(&tr), (s("07-11-2026"), s("7 Nov 2026"), s("Saturday, 7 November 2026"), s("14:32"), Mon, s("1.234.567"), s("1.234.567,5")));
    assert_eq!(tr.time(14, 32, TimeFormat::H12), "2:32 PM");
    // fr_FR groups with a narrow no-break space (U+202F).
    let fr = glibc("fr_FR.UTF-8", "%d/%m/%Y", "%T", "", 2, "\u{202f}", ",", &[3]);
    assert_eq!(
        summary(&fr),
        (s("07/11/2026"), s("7 Nov 2026"), s("Saturday, 7 November 2026"), s("14:32"), Mon, s("1\u{202f}234\u{202f}567"), s("1\u{202f}234\u{202f}567,5"))
    );
    // ja_JP: year first; its 年 月 日 are words (the app's language is English): plain separators.
    let ja = glibc("ja_JP.UTF-8", "%Y年%m月%d日", "%H時%M分%S秒", "%p%I時%M分%S秒", 1, ",", ".", &[3]);
    assert_eq!(
        summary(&ja),
        (s("2026/11/07"), s("2026 Nov 7"), s("Saturday, 2026 November 7"), s("14:32"), Sun, s("1,234,567"), s("1,234,567.5"))
    );
    let hu = glibc("hu_HU.UTF-8", "%Y-%m-%d", "%H:%M:%S", "%H:%M:%S", 2, ".", ",", &[3]);
    assert_eq!(summary(&hu), (s("2026-11-07"), s("2026 Nov 7"), s("Saturday, 2026 November 7"), s("14:32"), Mon, s("1.234.567"), s("1.234.567,5")));
    let sv = glibc("sv_SE.UTF-8", "%Y-%m-%d", "%H:%M:%S", "", 2, "\u{202f}", ",", &[3]);
    assert_eq!(
        summary(&sv),
        (s("2026-11-07"), s("2026 Nov 7"), s("Saturday, 2026 November 7"), s("14:32"), Mon, s("1\u{202f}234\u{202f}567"), s("1\u{202f}234\u{202f}567,5"))
    );
    // fi_FI: the time's separator is a dot.
    let fi = glibc("fi_FI.UTF-8", "%d.%m.%Y", "%H.%M.%S", "", 2, "\u{202f}", ",", &[3]);
    assert_eq!((fi.short_date(NOV_7), fi.time(9, 5, TimeFormat::Auto), fi.time_with_seconds(9, 5, 7, TimeFormat::H12)), (s("07.11.2026"), s("09.05"), s("9.05.07 AM")));
    // de_CH: a right single quotation mark (U+2019) groups.
    let de_ch = glibc("de_CH.UTF-8", "%d.%m.%Y", "%T", "", 2, "’", ".", &[3]);
    assert_eq!(de_ch.decimal(1_234_567.25, 2), "1’234’567.25");
    // hi_IN: unpadded fields (%-d / %-m), a two-digit year, 12-hour.
    let hi = glibc("hi_IN.UTF-8", "%-d/%-m/%y", "%I:%M:%S %p %Z", "%I:%M:%S %p %Z", 1, ",", ".", &[3]);
    assert_eq!((hi.short_date(NOV_7), hi.time(14, 32, TimeFormat::Auto), hi.first_weekday()), (s("7/11/26"), s("2:32 PM"), Sun));
    // en_IN: lakh grouping (3, then 2s).
    let en_in = glibc("en_IN.UTF-8", "%d/%m/%y", "%I:%M:%S %p %Z", "%I:%M:%S %p %Z", 1, ",", ".", &[3, 2]);
    assert_eq!((en_in.group(1_234_567), en_in.group(123), en_in.group(12_345)), (s("12,34,567"), s("123"), s("12,345")));
    let ko = glibc("ko_KR.UTF-8", "%Y년 %m월 %d일", "%H시 %M분 %S초", "%p %I시 %M분 %S초", 1, ",", ".", &[3]);
    assert_eq!((ko.short_date(NOV_7), ko.time(14, 32, TimeFormat::Auto)), (s("2026/11/07"), s("14:32")));
}

#[test]
fn glibc_date_patterns() {
    let parse = |pattern: &str| DatePattern::parse_posix(pattern).map(|p| Regional { date: p, ..Regional::neutral() }.short_date(NOV_7));
    let cases: [(&str, Option<&str>); 18] = [
        ("%d.%m.%Y", Some("07.11.2026")),
        ("%m/%d/%Y", Some("11/07/2026")),
        ("%d/%m/%y", Some("07/11/26")),
        ("%Y-%m-%d", Some("2026-11-07")),
        ("%F", Some("2026-11-07")),
        ("%D", Some("11/07/26")),
        ("%e.%m.%Y", Some("7.11.2026")),
        ("%d.%m.%Y.", Some("07.11.2026.")),
        ("%d. %m. %y", Some("07. 11. 26")),
        // A weekday is dropped with the literal that joined it.
        ("%A %d %b %Y", Some("07 Nov 2026")),
        ("%Y年%m月%d日 %A", Some("2026/11/07")),
        ("%d %b, %Y", Some("07 Nov, 2026")),
        ("%OC%Oy %b %Od %A", Some("2026 Nov 07")),
        ("西元%Y年%m月%d日", Some("2026/11/07")),
        ("%d/%m/%Ey", Some("07/11/26")),
        // Not one numeric date: the neutral pattern stands in.
        ("%H:%M", None),
        ("%x", None),
        ("%d.%m.", None),
    ];
    for (pattern, expected) in cases {
        assert_eq!(parse(pattern).as_deref(), expected, "{pattern}");
    }
    let broken = Regional::from_posix(Posix { time_locale: "xx_XX.UTF-8", numeric_locale: "C", d_fmt: Some("%x"), ..Posix::default() });
    assert_eq!(broken.short_date(NOV_7), "7 Nov 2026");
}

#[test]
fn the_c_locale_is_no_preference() {
    // glibc's C locale says %m/%d/%y, 24-hour, Sunday first (week 7;19971130;4, first_weekday 1):
    // a leftover, not a choice. Qt's C locale: "dddd, d MMMM yyyy", "HH:mm", Monday.
    for name in ["C", "POSIX", "C.UTF-8", "C.utf8"] {
        let c = Regional::from_posix(Posix {
            time_locale: name,
            numeric_locale: name,
            d_fmt: Some("%m/%d/%y"),
            t_fmt: "%H:%M:%S",
            t_fmt_ampm: "%I:%M:%S %p",
            first_weekday: glibc_first_weekday(19971130, 1),
            thousep: "",
            radix: ".",
            grouping: &[],
        });
        assert_eq!(
            summary(&c),
            (s("7 Nov 2026"), s("7 Nov 2026"), s("Saturday, 7 November 2026"), s("14:32"), Weekday::Mon, s("1234567"), s("1234567.5")),
            "{name}"
        );
        assert_eq!(c, Regional { origin: c.origin.clone(), ..Regional::neutral() });
    }
    // The owner's machine: LANG=en_US.UTF-8, LC_TIME=C, LC_NUMERIC=de_DE.UTF-8.
    let owner = Regional::from_posix(Posix {
        time_locale: "C",
        numeric_locale: "de_DE.UTF-8",
        d_fmt: Some("%m/%d/%y"),
        t_fmt: "%H:%M:%S",
        t_fmt_ampm: "%I:%M:%S %p",
        first_weekday: None,
        thousep: ".",
        radix: ",",
        grouping: &[3],
    });
    assert_eq!(
        (owner.short_date(NOV_7), owner.time(14, 32, TimeFormat::Auto), owner.group(1234), owner.first_weekday()),
        (s("7 Nov 2026"), s("14:32"), s("1.234"), Weekday::Mon)
    );
}

#[test]
fn windows_settings() {
    let windows = |short: &str, time: &str, first: &str, thousand: &str, decimal: &str, grouping: &str| {
        Regional::from_windows(WindowsSettings {
            short_date: Some(short),
            time_format: Some(time),
            itime: None,
            first_day_of_week: Some(first),
            thousand,
            decimal,
            grouping,
        })
    };
    let en_us = windows("M/d/yyyy", "h:mm:ss tt", "6", ",", ".", "3;0");
    assert_eq!(
        summary(&en_us),
        (s("11/7/2026"), s("Nov 7, 2026"), s("Saturday, November 7, 2026"), s("2:32 PM"), Weekday::Sun, s("1,234,567"), s("1,234,567.5"))
    );
    assert_eq!(&en_us.date, &Regional::en_us().date);
    let de = windows("dd.MM.yyyy", "HH:mm:ss", "0", ".", ",", "3;0");
    assert_eq!(summary(&de), (s("07.11.2026"), s("7 Nov 2026"), s("Saturday, 7 November 2026"), s("14:32"), Weekday::Mon, s("1.234.567"), s("1.234.567,5")));
    let ja = windows("yyyy/MM/dd", "H:mm", "6", ",", ".", "3;0");
    assert_eq!((ja.short_date(NOV_7), ja.time(9, 5, TimeFormat::Auto), ja.long_date(NOV_7)), (s("2026/11/07"), s("9:05"), s("Saturday, 2026 November 7")));
    let unpadded = windows("d.M.yyyy", "H.mm", "0", "\u{a0}", ",", "3;0");
    assert_eq!((unpadded.short_date(Date::new(2026, 3, 4)), unpadded.time(9, 5, TimeFormat::Auto), unpadded.group(12_345)), (s("4.3.2026"), s("9.05"), s("12\u{a0}345")));
    // Weekdays and eras are dropped; quoted words become plain separators.
    assert_eq!(windows("ddd d MMM yyyy", "HH:mm", "0", ",", ".", "3;0").short_date(NOV_7), "7 Nov 2026");
    assert_eq!(windows("d 'de' MMMM 'de' yyyy", "HH:mm", "0", ",", ".", "3;0").short_date(NOV_7), "7 November 2026");
    assert_eq!(windows("gg yyyy/M/d", "tt h:mm", "6", ",", ".", "3;0").short_date(NOV_7), "2026/11/7");
    assert_eq!(windows("yy-MM-dd", "hh:mm tt", "6", ",", ".", "3;0").time(9, 5, TimeFormat::Auto), "09:05 AM");
    // de-CH groups with an apostrophe; India 3 then 2s; "3" alone groups only once; none.
    assert_eq!(windows("dd.MM.yyyy", "HH:mm", "0", "'", ".", "3;0").decimal(1_234_567.5, 1), "1'234'567.5");
    assert_eq!(windows("dd-MM-yyyy", "HH:mm", "6", ",", ".", "3;2;0").group(1_234_567), "12,34,567");
    assert_eq!(windows("dd-MM-yyyy", "HH:mm", "6", ",", ".", "3").group(1_234_567), "1234,567");
    assert_eq!(windows("dd-MM-yyyy", "HH:mm", "6", "", ".", "3;0").group(1_234_567), "1234567");
    // LOCALE_ITIME decides when the picture has no hour; a broken short date falls back.
    let itime = Regional::from_windows(WindowsSettings { short_date: Some("??"), time_format: Some(""), itime: Some("0"), ..WindowsSettings::default() });
    assert_eq!((itime.time(14, 32, TimeFormat::Auto), itime.short_date(NOV_7)), (s("2:32 PM"), s("7 Nov 2026")));
    assert_eq!(Regional::from_windows(WindowsSettings { itime: Some("1"), ..WindowsSettings::default() }).time(14, 32, TimeFormat::Auto), "14:32");
}

#[test]
fn twelve_and_twenty_four_hours() {
    let en = Regional::en_us();
    let de = de_de_glibc();
    use TimeFormat::*;
    // Midnight and noon on a 12-hour clock.
    assert_eq!(en.time(0, 5, Auto), "12:05 AM");
    assert_eq!(en.time(12, 0, Auto), "12:00 PM");
    assert_eq!(en.time(23, 59, Auto), "11:59 PM");
    assert_eq!(en.time(11, 59, Auto), "11:59 AM");
    assert_eq!(en.time_compact(0, 5, Auto), "12:05");
    assert_eq!(en.time_with_seconds(14, 32, 5, Auto), "2:32:05 PM");
    // The user's choice wins over the system's.
    assert_eq!((en.time(0, 5, H24), en.time(9, 5, H24), en.time_compact(9, 5, H24)), (s("00:05"), s("09:05"), s("09:05")));
    assert_eq!((de.time(0, 5, Auto), de.time(9, 5, H12), de.time(12, 30, H12), de.time_compact(21, 5, H12)), (s("00:05"), s("9:05 AM"), s("12:30 PM"), s("9:05")));
    assert!(en.hour12(Auto) && !de.hour12(Auto) && de.hour12(H12) && !en.hour12(H24));
    assert_eq!(de.time_with_seconds(14, 32, 5, Auto), "14:32:05");
}

#[test]
fn first_day_of_the_week() {
    // glibc: `week 7;19971130;…` (a Sunday) and `first_weekday` counting from it.
    assert_eq!(glibc_first_weekday(19971130, 1), Some(Weekday::Sun));
    assert_eq!(glibc_first_weekday(19971130, 2), Some(Weekday::Mon));
    assert_eq!(glibc_first_weekday(19971130, 7), Some(Weekday::Sat));
    assert_eq!(glibc_first_weekday(19971130, 0), Some(Weekday::Sun));
    assert_eq!(glibc_first_weekday(19971201, 1), Some(Weekday::Mon));
    assert_eq!(glibc_first_weekday(0, 1), None);
    assert_eq!(region_first_weekday("en_US.UTF-8"), Weekday::Sun);
    assert_eq!(region_first_weekday("de_DE.UTF-8@euro"), Weekday::Mon);
    assert_eq!(region_first_weekday("ar_EG"), Weekday::Sat);
    assert_eq!(region_first_weekday("C"), Weekday::Mon);
    // A locale whose week data couldn't be read: its region decides.
    let no_week = Regional::from_posix(Posix { time_locale: "pt_BR.UTF-8", numeric_locale: "C", d_fmt: Some("%d/%m/%Y"), t_fmt: "%T", ..Posix::default() });
    assert_eq!(no_week.first_weekday(), Weekday::Sun);
}

#[test]
fn grouping() {
    let with = |group: &str, decimal: &str| Regional { numbers: Numbers::from_posix(group, decimal, &[3]), ..Regional::neutral() };
    for (regional, expected) in [
        (with(".", ","), ["0", "999", "1.000", "1.234", "1.234.567", "18.446.744.073.709.551.615"]),
        (with(",", "."), ["0", "999", "1,000", "1,234", "1,234,567", "18,446,744,073,709,551,615"]),
        (with("\u{202f}", ","), ["0", "999", "1\u{202f}000", "1\u{202f}234", "1\u{202f}234\u{202f}567", "18\u{202f}446\u{202f}744\u{202f}073\u{202f}709\u{202f}551\u{202f}615"]),
        (with("'", "."), ["0", "999", "1'000", "1'234", "1'234'567", "18'446'744'073'709'551'615"]),
        (Regional::neutral(), ["0", "999", "1000", "1234", "1234567", "18446744073709551615"]),
    ] {
        let got: Vec<String> = [0, 999, 1000, 1234, 1_234_567, u64::MAX].iter().map(|n| regional.group(*n)).collect();
        assert_eq!(got, expected);
    }
    let de = de_de_glibc();
    assert_eq!((de.group_signed(-1234), de.decimal(-0.04, 1), de.decimal(-1234.56, 1), de.decimal(0.5, 0)), (s("-1.234"), s("0,0"), s("-1.234,6"), s("0")));
    // CHAR_MAX stops grouping after the first group.
    let once = Regional { numbers: Numbers::from_posix(",", ".", &[3, 127]), ..Regional::neutral() };
    assert_eq!(once.group(1_234_567), "1234,567");
    assert_eq!(Regional { numbers: Numbers::from_posix(",", ".", &[3, 0]), ..Regional::neutral() }.group(1_234_567), "1,234,567");
}

#[test]
fn composed_dates_follow_the_short_patterns_order() {
    let de = de_de_glibc();
    let en = Regional::en_us();
    let ja = glibc("ja_JP.UTF-8", "%Y年%m月%d日", "%H時%M分%S秒", "", 1, ",", ".", &[3]);
    assert_eq!((de.month_day(NOV_7, MonthName::Short), en.month_day(NOV_7, MonthName::Long), ja.month_day(NOV_7, MonthName::Short)), (s("7 Nov"), s("November 7"), s("Nov 7")));
    assert_eq!((de.weekday_month_day(NOV_7), en.weekday_month_day(NOV_7)), (s("Saturday, 7 November"), s("Saturday, November 7")));
    assert_eq!((de.month_year(2026, 11), en.month_year(2026, 11), ja.month_year(2026, 11)), (s("November 2026"), s("November 2026"), s("2026 November")));
    assert_eq!((de.order(), en.order(), ja.order()), (Order::DayFirst, Order::MonthFirst, Order::YearFirst));
    assert_eq!((en.long_date(Date::new(2024, 2, 29)), de.long_date(Date::new(2026, 1, 1))), (s("Thursday, February 29, 2024"), s("Thursday, 1 January 2026")));
    assert_eq!(weekday_narrow(Weekday::Thu), "T");
    let t = LocalTime { date: NOV_7, hour: 14, minute: 32, second: 5 };
    assert_eq!(en.full_date_time_at(t, TimeFormat::Auto, false), "Saturday, November 7, 2026 at 2:32 PM");
    assert_eq!(de.full_date_time_at(t, TimeFormat::Auto, true), "Saturday, 7 November 2026 at 14:32:05");
}

#[test]
fn dates() {
    assert_eq!(Date::new(2026, 3, 1).previous(), Date::new(2026, 2, 28));
    assert_eq!(Date::new(2024, 3, 1).previous(), Date::new(2024, 2, 29));
    assert_eq!(Date::new(2026, 1, 1).previous(), Date::new(2025, 12, 31));
    assert_eq!(Date::new(2026, 11, 1).previous(), Date::new(2026, 10, 31));
    assert_eq!(NOV_7.weekday(), Weekday::Sat);
    assert_eq!(Date::new(2024, 2, 29).weekday(), Weekday::Thu);
    assert_eq!(Date::new(1997, 11, 30).weekday(), Weekday::Sun);
    assert_eq!(Date::from_day_number(20_261_107), Some(NOV_7));
    assert_eq!(Date::from_day_number(0), None);
    assert_eq!(NOV_7.day_number(), 20_261_107);
    // Against chrono over a few thousand days.
    let mut day = chrono::NaiveDate::from_ymd_opt(1999, 12, 25).unwrap();
    for _ in 0..4000 {
        let ours = Date::new(day.year(), day.month() as u8, day.day() as u8);
        assert_eq!(ours.weekday(), day.weekday(), "{day}");
        let next = day.succ_opt().unwrap();
        assert_eq!(Date::new(next.year(), next.month() as u8, next.day() as u8).previous(), ours);
        day = next;
    }
}

#[test]
fn message_stamps_in_other_formats() {
    let de = de_de_glibc();
    let at = |date: Date, hour, minute| LocalTime { date, hour, minute, second: 0 };
    let today = NOV_7;
    assert_eq!(de.message_stamp_at(at(today, 14, 32), today, TimeFormat::Auto), "14:32");
    assert_eq!(de.message_stamp_at(at(Date::new(2026, 11, 6), 9, 5), today, TimeFormat::Auto), "Yesterday, at 09:05");
    assert_eq!(de.message_stamp_at(at(Date::new(2026, 10, 7), 9, 5), today, TimeFormat::Auto), "07.10.2026, at 09:05");
    assert_eq!(Regional::neutral().message_stamp_at(at(Date::new(2026, 10, 7), 9, 5), today, TimeFormat::H12), "7 Oct 2026, at 9:05 AM");
    // From unix seconds through the local zone: an unrepresentable time is empty.
    assert_eq!(de.message_stamp(i64::MAX, 20_261_107, TimeFormat::Auto), "");
    let now = 1_794_072_600;
    let local_now = local(now).unwrap();
    assert_eq!(de.message_stamp(now, local_now.date.day_number(), TimeFormat::Auto), de.time(local_now.hour, local_now.minute, TimeFormat::Auto));
}

#[test]
fn relative_times() {
    let en = Regional::en_us();
    let now = 1_794_072_600;
    assert_eq!(en.relative(now - 5, now), "just now");
    assert_eq!(en.relative(now + 30, now), "just now");
    assert_eq!(en.relative(now - 5 * 60 - 59, now), "5 min ago");
    assert_eq!(en.relative(now - 3 * 3600, now), "3 h ago");
    assert_eq!(en.relative(now - 86_400 - 5, now), "yesterday");
    let then = now - 10 * 86_400;
    assert_eq!(en.relative(then, now), en.month_day(local(then).unwrap().date, MonthName::Short));
}

/// One renderer expectation: `tools/electron-captures/pages/regional-stamps.tsx` renders the real
/// `MessageBubble` / `DayDivider` under a fixed "now" in Electron (en-US: run.sh pins it) and writes
/// `renderer_stamps.json` (see that page for how to regenerate it).
struct RendererCase {
    message: LocalTime,
    now: Date,
    format: TimeFormat,
    stamp: String,
    title: String,
    compact: String,
    divider: String,
}

fn renderer_cases() -> Vec<RendererCase> {
    let json: serde_json::Value = serde_json::from_str(include_str!("renderer_stamps.json")).expect("renderer_stamps.json");
    assert_eq!(json["locale"], "en-US", "the page ran with another locale");
    let moment = |value: &serde_json::Value| -> LocalTime {
        let n = |i: usize| value[i].as_i64().expect("a number");
        LocalTime { date: Date::new(n(0) as i32, n(1) as u8, n(2) as u8), hour: n(3) as u8, minute: n(4) as u8, second: n(5) as u8 }
    };
    json["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|case| RendererCase {
            message: moment(&case["message"]),
            now: moment(&case["now"]).date,
            format: TimeFormat::from_setting(case["format"].as_str().expect("format")),
            stamp: case["stamp"].as_str().expect("stamp").to_string(),
            title: case["title"].as_str().expect("title").to_string(),
            compact: case["compact"].as_str().expect("compact").to_string(),
            divider: case["divider"].as_str().expect("divider").to_string(),
        })
        .collect()
}

#[test]
fn message_stamps_equal_the_renderers_in_en_us() {
    let en = Regional::en_us();
    let cases = renderer_cases();
    assert!(cases.len() >= 30, "{} cases", cases.len());
    for case in &cases {
        let at = format!("{:?} now {:?} {:?}", case.message, case.now, case.format);
        assert_eq!(en.message_stamp_at(case.message, case.now, case.format), case.stamp, "stamp {at}");
        assert_eq!(en.full_date_time_at(case.message, case.format, false), case.title, "title {at}");
        assert_eq!(en.time_compact(case.message.hour, case.message.minute, case.format), case.compact, "hover time {at}");
        assert_eq!(en.long_date(case.message.date), case.divider, "divider {at}");
    }
}

#[test]
fn other_formats_change_only_the_date_and_time() {
    for regional in [de_de_glibc(), Regional::neutral(), en_us_glibc()] {
        for case in renderer_cases() {
            let time = regional.time(case.message.hour, case.message.minute, case.format);
            let expected = if case.stamp.starts_with("Yesterday, at ") {
                format!("Yesterday, at {time}")
            } else if case.stamp.contains(", at ") {
                format!("{}, at {time}", regional.short_date(case.message.date))
            } else {
                time
            };
            assert_eq!(regional.message_stamp_at(case.message, case.now, case.format), expected, "{}", regional.origin());
        }
    }
}

#[test]
fn locale_names_resolve_as_glibc_does() {
    let env = |pairs: &'static [(&'static str, &'static str)]| move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string());
    assert_eq!(system::locale_name("LC_TIME", &env(&[("LANG", "en_US.UTF-8"), ("LC_TIME", "C")])), "C");
    assert_eq!(system::locale_name("LC_NUMERIC", &env(&[("LANG", "en_US.UTF-8"), ("LC_TIME", "C")])), "en_US.UTF-8");
    assert_eq!(system::locale_name("LC_TIME", &env(&[("LC_ALL", "de_DE.UTF-8"), ("LC_TIME", "C")])), "de_DE.UTF-8");
    assert_eq!(system::locale_name("LC_TIME", &env(&[("LC_ALL", ""), ("LC_TIME", "")])), "C");
    assert!(is_c_locale("C.UTF-8") && is_c_locale("POSIX") && !is_c_locale("ca_ES.UTF-8"));
}

/// The real `newlocale` path, in child processes of this test binary with chosen environments
/// (the process's environment is read once, so each combination needs its own process). Only
/// C, de_DE.UTF-8 and en_US.UTF-8 need to be installed.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn the_system_path_in_child_processes() {
    if std::env::var_os("DECIBELL_REGIONAL_CHILD").is_some() {
        return;
    }
    let installed = std::process::Command::new("locale").arg("-a").output().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
    if !(installed.contains("de_de.utf8") && installed.contains("en_us.utf8")) {
        eprintln!("SKIP: de_DE.UTF-8 and en_US.UTF-8 must be installed (locale -a)");
        return;
    }
    let exe = std::env::current_exe().expect("the test binary");
    let run = |vars: &[(&str, &str)]| -> String {
        let mut command = std::process::Command::new(&exe);
        command.args(["--exact", "regional::tests::system_child", "--nocapture", "--test-threads=1"]).env_clear().env("DECIBELL_REGIONAL_CHILD", "1");
        for (key, value) in vars {
            command.env(key, value);
        }
        let output = command.output().expect("running the child");
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines().find_map(|line| line.split_once("REGIONAL ")).map(|(_, answer)| answer.trim().to_string()).unwrap_or_else(|| panic!("no answer from {vars:?}: {text}"))
    };
    // short | long | time (Auto) | first weekday | 1234 | 1234567.5
    let cases: [(&[(&str, &str)], &str); 9] = [
        // The owner's machine.
        (&[("LANG", "en_US.UTF-8"), ("LC_TIME", "C"), ("LC_NUMERIC", "de_DE.UTF-8")], "7 Nov 2026|Saturday, 7 November 2026|14:32|Mon|1.234|1.234.567,5"),
        (&[("LC_ALL", "de_DE.UTF-8")], "07.11.2026|Saturday, 7 November 2026|14:32|Mon|1.234|1.234.567,5"),
        (&[("LC_ALL", "en_US.UTF-8")], "11/07/2026|Saturday, November 7, 2026|2:32 PM|Sun|1,234|1,234,567.5"),
        (&[("LANG", "de_DE.UTF-8"), ("LC_TIME", "en_US.UTF-8")], "11/07/2026|Saturday, November 7, 2026|2:32 PM|Sun|1.234|1.234.567,5"),
        (&[("LANG", "en_US.UTF-8"), ("LC_TIME", "de_DE.UTF-8")], "07.11.2026|Saturday, 7 November 2026|14:32|Mon|1,234|1,234,567.5"),
        // LC_ALL wins over the categories.
        (&[("LC_ALL", "de_DE.UTF-8"), ("LC_TIME", "C"), ("LC_NUMERIC", "C")], "07.11.2026|Saturday, 7 November 2026|14:32|Mon|1.234|1.234.567,5"),
        (&[("LC_ALL", "C")], "7 Nov 2026|Saturday, 7 November 2026|14:32|Mon|1234|1234567.5"),
        (&[("LANG", "POSIX")], "7 Nov 2026|Saturday, 7 November 2026|14:32|Mon|1234|1234567.5"),
        // A locale that isn't installed: no preference for its category.
        (&[("LANG", "en_US.UTF-8"), ("LC_TIME", "xx_YY.UTF-8")], "7 Nov 2026|Saturday, 7 November 2026|14:32|Mon|1,234|1,234,567.5"),
    ];
    for (vars, expected) in cases {
        assert_eq!(run(vars), expected, "{vars:?}");
    }
    assert_eq!(run(&[]), "7 Nov 2026|Saturday, 7 November 2026|14:32|Mon|1234|1234567.5", "nothing set: C");
}

/// The child of [`the_system_path_in_child_processes`]: prints what the system path read.
#[test]
fn system_child() {
    if std::env::var_os("DECIBELL_REGIONAL_CHILD").is_none() {
        return;
    }
    let r = Regional::system();
    println!(
        "REGIONAL {}|{}|{}|{:?}|{}|{}",
        r.short_date(NOV_7),
        r.long_date(NOV_7),
        r.time(14, 32, TimeFormat::Auto),
        r.first_weekday(),
        r.group(1234),
        r.decimal(1_234_567.5, 1)
    );
}

#[test]
fn pinning() {
    // Restores whatever was pinned (tests run in parallel threads of one process: only this test
    // pins).
    let before = Regional::pin(Some(Regional::en_us()));
    assert_eq!(Regional::get().short_date(NOV_7), "11/7/2026");
    let neutral = Regional::neutral().leak();
    assert_eq!(Regional::pin(Some(neutral)), Some(Regional::en_us()));
    assert_eq!(Regional::get().short_date(NOV_7), "7 Nov 2026");
    Regional::pin(before);
}

/// The hot call, per message row while a list lays out: `cargo test --release -p decibell-model
/// regional::tests::cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn cost() {
    use std::hint::black_box;
    use std::time::Instant;
    let regional = Regional::get();
    let base = 1_794_072_600i64;
    let today = local(base).unwrap().date;
    let n = 2_000_000;
    let started = Instant::now();
    let mut total = 0usize;
    for i in 0..n {
        total += black_box(regional.message_stamp(black_box(base - (i % 500_000) * 37), today.day_number(), TimeFormat::Auto)).len();
    }
    let stamp = started.elapsed().as_nanos() as f64 / n as f64;
    let started = Instant::now();
    for i in 0..n {
        total += black_box(local(black_box(base - (i % 500_000) * 37))).map_or(0, |t| t.minute as usize);
    }
    let zone = started.elapsed().as_nanos() as f64 / n as f64;
    let t = local(base - 5 * 86_400).unwrap();
    let started = Instant::now();
    for _ in 0..n {
        total += black_box(regional.message_stamp_at(black_box(t), today, TimeFormat::Auto)).len();
    }
    let format = started.elapsed().as_nanos() as f64 / n as f64;
    let started = Instant::now();
    for i in 0..n as u64 {
        total += black_box(regional.group(black_box(1_000 + i))).len();
    }
    let group = started.elapsed().as_nanos() as f64 / n as f64;
    println!("message_stamp {stamp:.1} ns (local zone {zone:.1} ns + formatting {format:.1} ns); group {group:.1} ns [{total}]");
}

/// A zone whose clocks jump from 00:00 to 01:00 on 2026-09-06 (UTC−4 → UTC−3, as Chile's do):
/// midnight that night doesn't exist.
#[derive(Clone, Copy, Debug)]
struct JumpsAtMidnight;

impl JumpsAtMidnight {
    const WINTER: i32 = -4 * 3600;
    const SUMMER: i32 = -3 * 3600;
    fn jump_local() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap().and_hms_opt(0, 0, 0).unwrap()
    }
    /// The instant of the jump: 00:00 at UTC−4.
    fn jump_utc() -> chrono::NaiveDateTime {
        Self::jump_local() - chrono::Duration::seconds(i64::from(Self::WINTER))
    }
}

impl chrono::TimeZone for JumpsAtMidnight {
    type Offset = chrono::FixedOffset;
    fn from_offset(_: &chrono::FixedOffset) -> Self {
        JumpsAtMidnight
    }
    fn offset_from_local_date(&self, local: &chrono::NaiveDate) -> chrono::LocalResult<chrono::FixedOffset> {
        self.offset_from_local_datetime(&local.and_hms_opt(12, 0, 0).unwrap())
    }
    fn offset_from_local_datetime(&self, local: &chrono::NaiveDateTime) -> chrono::LocalResult<chrono::FixedOffset> {
        let jump = Self::jump_local();
        if *local < jump {
            chrono::LocalResult::Single(chrono::FixedOffset::east_opt(Self::WINTER).unwrap())
        } else if *local < jump + chrono::Duration::hours(1) {
            chrono::LocalResult::None
        } else {
            chrono::LocalResult::Single(chrono::FixedOffset::east_opt(Self::SUMMER).unwrap())
        }
    }
    fn offset_from_utc_date(&self, utc: &chrono::NaiveDate) -> chrono::FixedOffset {
        self.offset_from_utc_datetime(&utc.and_hms_opt(12, 0, 0).unwrap())
    }
    fn offset_from_utc_datetime(&self, utc: &chrono::NaiveDateTime) -> chrono::FixedOffset {
        chrono::FixedOffset::east_opt(if *utc < Self::jump_utc() { Self::WINTER } else { Self::SUMMER }).unwrap()
    }
}

/// "Today" on the evening before a jump over midnight ends where the jump lands (01:00 new time,
/// the same instant as 00:00 old time), not a day later; an ordinary night ends at its midnight.
#[test]
fn midnight_in_a_dst_gap_is_where_the_jump_lands() {
    use chrono::TimeZone as _;
    let jump = JumpsAtMidnight::jump_utc().and_utc().timestamp();
    let evening = jump - 3 * 3600;
    assert_eq!(next_midnight_in(&JumpsAtMidnight, evening), Some(jump), "the day ends at the jump");
    let local = JumpsAtMidnight.timestamp_opt(jump, 0).single().unwrap().naive_local();
    assert_eq!(local, JumpsAtMidnight::jump_local() + chrono::Duration::hours(1), "01:00 on the clock");
    // The next night has its midnight (UTC−3).
    let next = next_midnight_in(&JumpsAtMidnight, jump + 3600).unwrap();
    assert_eq!(next, jump + 23 * 3600, "a 23-hour day");
    // A night a week before: 24 hours after the previous midnight.
    let week_before = jump - 7 * 86_400 - 3 * 3600;
    assert_eq!(next_midnight_in(&JumpsAtMidnight, week_before), Some(jump - 7 * 86_400));
}
