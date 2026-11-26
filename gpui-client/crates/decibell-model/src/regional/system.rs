//! Reading the regional settings from the operating system, once.
//!
//! - Linux and the other Unixes (macOS included): the locale the environment names, per category
//!   as glibc resolves it (`LC_ALL`, else `LC_TIME` / `LC_NUMERIC`, else `LANG`), read through
//!   `newlocale` + `nl_langinfo_l` on a private locale object: never `setlocale`, which is
//!   process-wide and not thread-safe. A C / POSIX locale is "no preference" (see
//!   [`super::Regional::neutral`]); a locale that isn't installed is too.
//! - Windows: the user's format settings (`GetLocaleInfoEx(LOCALE_NAME_USER_DEFAULT, …)`), custom
//!   overrides included.

use super::Regional;

/// The locale name glibc resolves for `category`: `LC_ALL`, else the category, else `LANG`, else
/// C (an empty variable counts as unset).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn locale_name(category: &str, var: &dyn Fn(&str) -> Option<String>) -> String {
    ["LC_ALL", category, "LANG"]
        .iter()
        .find_map(|name| var(name).filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "C".into())
}

/// C, POSIX and their variants (`C.UTF-8`): no regional preference.
pub fn is_c_locale(name: &str) -> bool {
    name == "C" || name == "POSIX" || name.starts_with("C.") || name.starts_with("C@") || name.starts_with("POSIX.")
}

#[cfg(unix)]
pub(crate) fn read() -> Regional {
    let var = |name: &str| std::env::var(name).ok();
    let time_name = locale_name("LC_TIME", &var);
    let numeric_name = locale_name("LC_NUMERIC", &var);
    let time = (!is_c_locale(&time_name)).then(|| unix::Locale::open(libc::LC_TIME_MASK)).flatten();
    let numeric = (!is_c_locale(&numeric_name)).then(|| unix::Locale::open(libc::LC_NUMERIC_MASK)).flatten();
    if time.is_none() && !is_c_locale(&time_name) {
        log::info!("[regional] LC_TIME {time_name:?} is not installed: no regional preference for dates and times");
    }
    if numeric.is_none() && !is_c_locale(&numeric_name) {
        log::info!("[regional] LC_NUMERIC {numeric_name:?} is not installed: no regional preference for numbers");
    }
    let time = time.map(|locale| unix::TimeData {
        d_fmt: locale.info(libc::D_FMT),
        t_fmt: locale.info(libc::T_FMT),
        t_fmt_ampm: locale.info(libc::T_FMT_AMPM),
        first_weekday: locale.first_weekday(&time_name),
    });
    let numeric = numeric.map(|locale| unix::NumericData {
        thousep: locale.info(libc::THOUSEP),
        radix: locale.info(libc::RADIXCHAR),
        grouping: locale.grouping(),
    });
    Regional::from_posix(super::Posix {
        time_locale: &time_name,
        numeric_locale: &numeric_name,
        d_fmt: time.as_ref().map(|t| t.d_fmt.as_str()),
        t_fmt: time.as_ref().map_or("", |t| t.t_fmt.as_str()),
        t_fmt_ampm: time.as_ref().map_or("", |t| t.t_fmt_ampm.as_str()),
        first_weekday: time.as_ref().and_then(|t| t.first_weekday),
        thousep: numeric.as_ref().map_or("", |n| n.thousep.as_str()),
        radix: numeric.as_ref().map_or(".", |n| n.radix.as_str()),
        grouping: numeric.as_ref().map_or(&[][..], |n| n.grouping.as_slice()),
    })
}

#[cfg(unix)]
mod unix {
    use std::ffi::CStr;

    use chrono::Weekday;

    pub struct TimeData {
        pub d_fmt: String,
        pub t_fmt: String,
        pub t_fmt_ampm: String,
        pub first_weekday: Option<Weekday>,
    }

    pub struct NumericData {
        pub thousep: String,
        pub radix: String,
        pub grouping: Vec<u8>,
    }

    // macOS has `nl_langinfo_l` (xlocale) but the libc crate doesn't declare it there.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    unsafe extern "C" {
        fn nl_langinfo_l(item: libc::nl_item, locale: libc::locale_t) -> *mut libc::c_char;
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    use libc::nl_langinfo_l;

    /// A private locale object for one category, resolved from the environment as `setlocale(…,
    /// "")` would, without touching the process's locale.
    pub struct Locale(libc::locale_t);

    impl Drop for Locale {
        fn drop(&mut self) {
            // SAFETY: made by `newlocale`, freed once.
            unsafe { libc::freelocale(self.0) };
        }
    }

    impl Locale {
        pub fn open(mask: libc::c_int) -> Option<Locale> {
            // SAFETY: a NUL-terminated name; a null base asks for a new object. Null on failure
            // (the named locale isn't installed).
            let handle = unsafe { libc::newlocale(mask, c"".as_ptr(), std::ptr::null_mut()) };
            // Never a `Locale` around null: its drop would free it.
            if handle.is_null() { None } else { Some(Locale(handle)) }
        }

        fn raw(&self, item: libc::nl_item) -> *const libc::c_char {
            // SAFETY: a live locale object; the result points into its data (valid while it lives).
            unsafe { nl_langinfo_l(item, self.0) }
        }

        /// A string item, as UTF-8 (lossily for a legacy single-byte locale's separators).
        pub fn info(&self, item: libc::nl_item) -> String {
            let raw = self.raw(item);
            if raw.is_null() {
                return String::new();
            }
            // SAFETY: nl_langinfo_l returns a NUL-terminated string.
            unsafe { CStr::from_ptr(raw) }.to_string_lossy().into_owned()
        }

        /// `GROUPING`'s bytes (glibc: `_NL_ITEM(LC_NUMERIC, 2)`).
        pub fn grouping(&self) -> Vec<u8> {
            #[cfg(all(target_os = "linux", target_env = "gnu"))]
            {
                const GROUPING: libc::nl_item = 0x10002;
                let raw = self.raw(GROUPING);
                if raw.is_null() {
                    return Vec::new();
                }
                // SAFETY: a NUL-terminated byte string.
                unsafe { CStr::from_ptr(raw) }.to_bytes().to_vec()
            }
            #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
            {
                vec![3]
            }
        }

        /// glibc's first day of the week: `week` gives a reference date (`_NL_TIME_WEEK_1STDAY`,
        /// e.g. 19971130, a Sunday) and `first_weekday` (`_NL_TIME_FIRST_WEEKDAY`) counts from it,
        /// 1 being that date's weekday. en_US: `week 7;19971130;1`, no `first_weekday` (1) →
        /// Sunday; de_DE: `first_weekday 2` → Monday. Elsewhere (macOS has no such item): the
        /// region's custom.
        pub fn first_weekday(&self, name: &str) -> Option<Weekday> {
            #[cfg(all(target_os = "linux", target_env = "gnu"))]
            {
                let _ = name;
                const WEEK_1STDAY: libc::nl_item = 0x20066;
                const FIRST_WEEKDAY: libc::nl_item = 0x20068;
                // A word item: the value is in the pointer's bits (the union glibc returns it
                // in), its low 32 bits on a little-endian machine.
                let word = self.raw(WEEK_1STDAY) as usize as u64;
                #[cfg(target_endian = "little")]
                let reference = word as u32;
                #[cfg(target_endian = "big")]
                let reference = (word >> 32) as u32;
                let first = self.raw(FIRST_WEEKDAY);
                let offset = if first.is_null() {
                    1
                } else {
                    // SAFETY: a byte item points at its byte.
                    unsafe { *first as u8 }
                };
                super::super::glibc_first_weekday(reference, offset)
            }
            #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
            {
                Some(super::super::region_first_weekday(name))
            }
        }
    }
}

#[cfg(windows)]
pub(crate) fn read() -> Regional {
    use self::win32::*;
    let short_date = info(LOCALE_SSHORTDATE);
    let time = info(LOCALE_SSHORTTIME).filter(|v| v.contains(['h', 'H'])).or_else(|| info(LOCALE_STIMEFORMAT));
    let itime = info(LOCALE_ITIME);
    let first_day = info(LOCALE_IFIRSTDAYOFWEEK);
    let thousand = info(LOCALE_STHOUSAND).unwrap_or_default();
    let decimal = info(LOCALE_SDECIMAL).unwrap_or_else(|| ".".into());
    let grouping = info(LOCALE_SGROUPING).unwrap_or_else(|| "3;0".into());
    Regional::from_windows(super::WindowsSettings {
        short_date: short_date.as_deref(),
        time_format: time.as_deref(),
        itime: itime.as_deref(),
        first_day_of_week: first_day.as_deref(),
        thousand: &thousand,
        decimal: &decimal,
        grouping: &grouping,
    })
}

#[cfg(windows)]
mod win32 {
    // winnls.h. `LOCALE_NAME_USER_DEFAULT` is a null name: the user's settings, overrides included
    // (never `LOCALE_NOUSEROVERRIDE`).
    pub const LOCALE_SDECIMAL: u32 = 0x0000_000E;
    pub const LOCALE_STHOUSAND: u32 = 0x0000_000F;
    pub const LOCALE_SGROUPING: u32 = 0x0000_0010;
    pub const LOCALE_SSHORTDATE: u32 = 0x0000_001F;
    pub const LOCALE_ITIME: u32 = 0x0000_0023;
    pub const LOCALE_SSHORTTIME: u32 = 0x0000_0079;
    pub const LOCALE_STIMEFORMAT: u32 = 0x0000_1003;
    pub const LOCALE_IFIRSTDAYOFWEEK: u32 = 0x0000_100C;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocaleInfoEx(locale_name: *const u16, lc_type: u32, data: *mut u16, cch_data: i32) -> i32;
    }

    pub fn info(lc_type: u32) -> Option<String> {
        let mut buffer = [0u16; 128];
        // SAFETY: a null name is LOCALE_NAME_USER_DEFAULT; the buffer's length is passed in
        // characters. The result counts the terminating NUL; 0 is a failure.
        let written = unsafe { GetLocaleInfoEx(std::ptr::null(), lc_type, buffer.as_mut_ptr(), buffer.len() as i32) };
        if written <= 0 {
            return None;
        }
        let len = (written as usize).saturating_sub(1).min(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..len]))
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn read() -> Regional {
    Regional::neutral()
}
