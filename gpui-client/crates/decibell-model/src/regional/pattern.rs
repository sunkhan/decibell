//! The system's numeric short-date pattern and clock, parsed from either dialect into one small
//! form that [`super::Regional`] renders itself: glibc's `strftime` patterns (`D_FMT`, `T_FMT`:
//! `%d.%m.%Y`, `%m/%d/%Y`, `%T`, `%r`) and Windows' picture strings (`LOCALE_SSHORTDATE`,
//! `LOCALE_STIMEFORMAT`: `dd.MM.yyyy`, `M/d/yyyy`, `HH:mm:ss`, `h:mm:ss tt`).
//!
//! Only the shape is taken: the order of day, month and year, the separators, zero padding, the
//! year's digits, 12 or 24 hours. Words are the app's (English): a weekday or era field is dropped,
//! and separators made of letters (`%Y年%m月%d日`, `%Y년 %m월 %d일`) become plain ones.

/// A field of a short date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Day { pad: bool },
    Month(Month),
    /// `full`: every digit (2026); else the last two, zero-padded (26).
    Year { full: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Month {
    Number { pad: bool },
    /// "Nov"
    Short,
    /// "November"
    Long,
}

/// Which field leads: what the long and medium forms follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// 7 November 2026
    DayFirst,
    /// November 7, 2026
    MonthFirst,
    /// 2026 November 7
    YearFirst,
}

/// A short date: `lead`, then each field followed by its literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DatePattern {
    pub lead: Box<str>,
    pub parts: [(Field, Box<str>); 3],
}

impl DatePattern {
    /// `d MMM yyyy`: what "no regional preference" (the C / POSIX locale) shows.
    pub fn neutral() -> DatePattern {
        DatePattern {
            lead: "".into(),
            parts: [(Field::Day { pad: false }, " ".into()), (Field::Month(Month::Short), " ".into()), (Field::Year { full: true }, "".into())],
        }
    }

    pub fn order(&self) -> Order {
        match self.parts[0].0 {
            Field::Day { .. } => Order::DayFirst,
            Field::Month(_) => Order::MonthFirst,
            Field::Year { .. } => Order::YearFirst,
        }
    }

    /// A glibc `strftime` date pattern (`D_FMT`). `None` when it isn't one numeric date.
    pub fn parse_posix(pattern: &str) -> Option<DatePattern> {
        let mut tokens = Vec::new();
        posix_tokens(pattern, &mut tokens, 0)?;
        DatePattern::assemble(tokens)
    }

    /// A Windows date picture (`LOCALE_SSHORTDATE`). `None` when it isn't one numeric date.
    pub fn parse_windows(pattern: &str) -> Option<DatePattern> {
        let mut tokens = Vec::new();
        let chars: Vec<char> = pattern.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let run = chars[i..].iter().take_while(|&&x| x == c).count();
            match c {
                'd' => {
                    tokens.push(match run {
                        1 => Token::Field(Field::Day { pad: false }),
                        2 => Token::Field(Field::Day { pad: true }),
                        _ => Token::Word,
                    });
                    i += run;
                }
                'M' => {
                    tokens.push(Token::Field(Field::Month(match run {
                        1 => Month::Number { pad: false },
                        2 => Month::Number { pad: true },
                        3 => Month::Short,
                        _ => Month::Long,
                    })));
                    i += run;
                }
                'y' => {
                    tokens.push(Token::Field(Field::Year { full: run > 2 }));
                    i += run;
                }
                // The era ("A.D."): a word, dropped like a weekday.
                'g' => {
                    tokens.push(Token::Word);
                    i += run;
                }
                '\'' => {
                    // A quoted literal; '' inside it is a quote.
                    let mut text = String::new();
                    i += 1;
                    while i < chars.len() {
                        if chars[i] == '\'' {
                            if chars.get(i + 1) == Some(&'\'') {
                                text.push('\'');
                                i += 2;
                                continue;
                            }
                            i += 1;
                            break;
                        }
                        text.push(chars[i]);
                        i += 1;
                    }
                    if text.is_empty() {
                        text.push('\'');
                    }
                    tokens.push(Token::Text(text));
                }
                _ => {
                    tokens.push(Token::Text(c.to_string()));
                    i += 1;
                }
            }
        }
        DatePattern::assemble(tokens)
    }

    /// Tokens → exactly one day, one month and one year with the literals between them.
    fn assemble(tokens: Vec<Token>) -> Option<DatePattern> {
        // Adjacent literals as one.
        let mut merged: Vec<Token> = Vec::with_capacity(tokens.len());
        for token in tokens {
            match (merged.last_mut(), token) {
                (Some(Token::Text(previous)), Token::Text(text)) => previous.push_str(&text),
                (_, token) => merged.push(token),
            }
        }
        // Words (a weekday, an era) go, each with the literal that joined it to the date: the one
        // after it when it leads ("%A, %d…"), else the one before it ("…%d日 %A").
        let mut cleaned: Vec<Token> = Vec::with_capacity(merged.len());
        let mut drop_next_text = false;
        for token in merged {
            match token {
                Token::Word => {
                    if !cleaned.iter().any(|t| matches!(t, Token::Field(_))) {
                        drop_next_text = true;
                    } else if matches!(cleaned.last(), Some(Token::Text(_))) {
                        cleaned.pop();
                    }
                }
                Token::Text(_) if drop_next_text => drop_next_text = false,
                other => {
                    drop_next_text = false;
                    match (cleaned.last_mut(), other) {
                        (Some(Token::Text(previous)), Token::Text(text)) => previous.push_str(&text),
                        (_, other) => cleaned.push(other),
                    }
                }
            }
        }
        let mut lead = String::new();
        let mut parts: Vec<(Field, String)> = Vec::with_capacity(3);
        for token in cleaned {
            match token {
                Token::Field(field) => parts.push((field, String::new())),
                Token::Text(text) => match parts.last_mut() {
                    Some((_, after)) => after.push_str(&text),
                    None => lead.push_str(&text),
                },
                Token::Word => unreachable!("dropped above"),
            }
        }
        let count = |pick: fn(&Field) -> bool| parts.iter().filter(|(f, _)| pick(f)).count();
        if parts.len() != 3
            || count(|f| matches!(f, Field::Day { .. })) != 1
            || count(|f| matches!(f, Field::Month(_))) != 1
            || count(|f| matches!(f, Field::Year { .. })) != 1
        {
            return None;
        }
        // Separators that are words (年 月 日, 년 월 일, "西元") are language: the app's is English,
        // so they become plain separators ('/' between numbers, a space next to a month name),
        // and words before the first or after the last field go.
        let wordy = |text: &str| text.chars().any(char::is_alphabetic);
        if wordy(&lead) || parts.iter().any(|(_, after)| wordy(after)) {
            lead.clear();
            let names = |field: &Field| matches!(field, Field::Month(Month::Short | Month::Long));
            for i in 0..3 {
                if i == 2 {
                    parts[i].1.clear();
                    continue;
                }
                let kept: String = parts[i].1.chars().filter(|c| !c.is_alphabetic()).collect();
                let kept = kept.trim().to_string();
                parts[i].1 = if !kept.is_empty() {
                    kept
                } else if names(&parts[i].0) || names(&parts[i + 1].0) {
                    " ".into()
                } else {
                    "/".into()
                };
            }
        }
        let mut parts = parts.into_iter().map(|(field, after)| (field, after.into_boxed_str()));
        Some(DatePattern {
            lead: lead.into_boxed_str(),
            parts: [parts.next()?, parts.next()?, parts.next()?],
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Field(Field),
    Text(String),
    /// A weekday or an era: dropped.
    Word,
}

/// glibc's conversions, expanded (`%F`, `%D`) and with their flags (`%-d`, `%_m`, `%0e`) and
/// modifiers (`%Ey`, `%Od`: eras and alternative digits are read as the plain field).
fn posix_tokens(pattern: &str, out: &mut Vec<Token>, depth: u8) -> Option<()> {
    let mut chars = pattern.chars().peekable();
    let mut century = false;
    while let Some(c) = chars.next() {
        if c != '%' {
            push_text(out, c);
            continue;
        }
        let mut flag = None;
        while let Some(&f) = chars.peek() {
            if matches!(f, '-' | '_' | '0' | '^' | '#') {
                flag = Some(f);
                chars.next();
            } else {
                break;
            }
        }
        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
            chars.next();
        }
        if matches!(chars.peek(), Some('E' | 'O')) {
            chars.next();
        }
        let conversion = chars.next()?;
        // `%C%y` (century, then the year in it) is the full year.
        if century && conversion != 'y' {
            return None;
        }
        let unpadded = matches!(flag, Some('-' | '_'));
        match conversion {
            'd' => out.push(Token::Field(Field::Day { pad: !unpadded })),
            // Space-padded unless `%0e`.
            'e' => out.push(Token::Field(Field::Day { pad: flag == Some('0') })),
            'm' => out.push(Token::Field(Field::Month(Month::Number { pad: !unpadded }))),
            'b' | 'h' => out.push(Token::Field(Field::Month(Month::Short))),
            'B' => out.push(Token::Field(Field::Month(Month::Long))),
            'Y' | 'G' => out.push(Token::Field(Field::Year { full: true })),
            'y' | 'g' => {
                out.push(Token::Field(Field::Year { full: century }));
                century = false;
            }
            'C' => century = true,
            'a' | 'A' | 'u' | 'w' => out.push(Token::Word),
            'F' if depth == 0 => posix_tokens("%Y-%m-%d", out, 1)?,
            'D' if depth == 0 => posix_tokens("%m/%d/%y", out, 1)?,
            'n' | 't' => push_text(out, ' '),
            '%' => push_text(out, '%'),
            _ => return None,
        }
    }
    (!century).then_some(())
}

fn push_text(out: &mut Vec<Token>, c: char) {
    match out.last_mut() {
        Some(Token::Text(text)) => text.push(c),
        _ => out.push(Token::Text(c.to_string())),
    }
}

/// The clock: 12 or 24 hours, whether the hour is zero-padded, what separates hours, minutes and
/// seconds. AM / PM are the app's ("PM", after the time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Clock {
    pub h12: bool,
    /// The hour's zero padding in a 24-hour clock ("09:05") and in a 12-hour one ("09:05 AM").
    pub pad24: bool,
    pub pad12: bool,
    pub separator: char,
}

impl Clock {
    /// "14:32": no regional preference.
    pub fn neutral() -> Clock {
        Clock { h12: false, pad24: true, pad12: false, separator: ':' }
    }

    /// glibc's `T_FMT`, with `T_FMT_AMPM` for what `%r` stands for.
    ///
    /// The 12-hour hour is never padded from here: glibc's 12-hour patterns write `%I` (the
    /// zero-padded hour, its only zero-padded 12-hour conversion) in every locale, C included, so it
    /// says nothing about the region; ICU, Windows and macOS write "2:32 PM" for the 12-hour
    /// regions.
    pub fn parse_posix(t_fmt: &str, t_fmt_ampm: &str) -> Clock {
        let expanded = t_fmt.replace("%T", "%H:%M:%S").replace("%R", "%H:%M").replace("%r", t_fmt_ampm);
        let mut clock = Clock::neutral();
        let mut hour_seen = false;
        let mut chars = expanded.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                continue;
            }
            let mut flag = None;
            while let Some(&f) = chars.peek() {
                if matches!(f, '-' | '_' | '0' | '^' | '#') {
                    flag = Some(f);
                    chars.next();
                } else {
                    break;
                }
            }
            if matches!(chars.peek(), Some('E' | 'O')) {
                chars.next();
            }
            let Some(conversion) = chars.next() else { break };
            match conversion {
                'I' | 'l' | 'p' | 'P' => clock.h12 = true,
                'H' if !hour_seen => clock.pad24 = !matches!(flag, Some('-' | '_')),
                'k' if !hour_seen => clock.pad24 = flag == Some('0'),
                _ => {}
            }
            // What separates the first hour from the minutes.
            if matches!(conversion, 'H' | 'k' | 'I' | 'l') && !hour_seen {
                hour_seen = true;
                if let Some(&next) = chars.peek()
                    && is_separator(next)
                {
                    clock.separator = next;
                }
            }
        }
        clock
    }

    /// A Windows time picture (`LOCALE_STIMEFORMAT` / `LOCALE_SSHORTTIME`); `itime` is
    /// `LOCALE_ITIME` ("1" = 24-hour), used when the picture has no hour.
    pub fn parse_windows(picture: &str, itime: Option<&str>) -> Clock {
        let chars: Vec<char> = picture.chars().collect();
        let mut clock = Clock::neutral();
        let mut found = false;
        let mut i = 0;
        let mut quoted = false;
        while i < chars.len() {
            let c = chars[i];
            if c == '\'' {
                quoted = !quoted;
                i += 1;
                continue;
            }
            if !quoted && (c == 'h' || c == 'H') {
                let run = chars[i..].iter().take_while(|&&x| x == c).count();
                clock.h12 = c == 'h';
                if clock.h12 {
                    clock.pad12 = run >= 2;
                } else {
                    clock.pad24 = run >= 2;
                }
                if let Some(&next) = chars.get(i + run)
                    && is_separator(next)
                {
                    clock.separator = next;
                }
                found = true;
                break;
            }
            i += 1;
        }
        if !found {
            clock.h12 = itime.is_some_and(|v| v.trim() == "0");
        }
        clock
    }
}

/// One punctuation character between hours and minutes (':' or '.'); a letter ("14h32") or a
/// space keeps ':'.
fn is_separator(c: char) -> bool {
    !c.is_alphanumeric() && !c.is_whitespace() && c != '%' && c != '\''
}

/// Digit grouping: the separator, the decimal mark, and the group sizes from the right
/// (`[3]` repeated: 1,234,567; `[3, 2]` repeated: 12,34,567).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Numbers {
    pub group: Box<str>,
    pub decimal: Box<str>,
    /// Empty: no grouping.
    pub sizes: Box<[u8]>,
    /// The last size repeats; otherwise the digits left after the sizes stay together.
    pub repeat: bool,
}

impl Numbers {
    /// "1234.5": no grouping (C / POSIX).
    pub fn neutral() -> Numbers {
        Numbers { group: "".into(), decimal: ".".into(), sizes: Box::new([]), repeat: false }
    }

    /// glibc's `THOUSEP`, `RADIXCHAR` and `GROUPING` (bytes: each a group size, the last repeating;
    /// `CHAR_MAX` or a negative byte stops grouping there; empty: none).
    pub fn from_posix(thousep: &str, radix: &str, grouping: &[u8]) -> Numbers {
        let mut sizes = Vec::new();
        let mut repeat = false;
        for &byte in grouping {
            match byte {
                0 => {
                    repeat = !sizes.is_empty();
                    break;
                }
                1..=126 => sizes.push(byte),
                _ => break,
            }
        }
        // The bytes were all read without a terminator (a caller's slice): the last repeats.
        if !sizes.is_empty() && !grouping.iter().any(|&b| b == 0 || b >= 127) {
            repeat = true;
        }
        if thousep.is_empty() {
            sizes.clear();
        }
        Numbers {
            group: thousep.into(),
            decimal: if radix.is_empty() { ".".into() } else { radix.into() },
            sizes: sizes.into_boxed_slice(),
            repeat,
        }
    }

    /// Windows' `LOCALE_STHOUSAND`, `LOCALE_SDECIMAL` and `LOCALE_SGROUPING` ("3;0": threes;
    /// "3;2;0": three then twos; "3": only the first group).
    pub fn from_windows(thousand: &str, decimal: &str, grouping: &str) -> Numbers {
        let values: Vec<u8> = grouping.split(';').filter_map(|v| v.trim().parse::<u8>().ok()).collect();
        let repeat = values.last() == Some(&0);
        let sizes: Vec<u8> = values.into_iter().take_while(|&v| v > 0).collect();
        let repeat = repeat && !sizes.is_empty();
        Numbers {
            group: thousand.into(),
            decimal: if decimal.is_empty() { ".".into() } else { decimal.into() },
            sizes: if thousand.is_empty() { Box::new([]) } else { sizes.into_boxed_slice() },
            repeat,
        }
    }

    /// Appends `digits` (ASCII, most significant first) with the group separators.
    pub fn write_grouped(&self, out: &mut String, digits: &str) {
        if self.sizes.is_empty() || self.group.is_empty() {
            out.push_str(digits);
            return;
        }
        // Group boundaries from the right, as lengths from the left.
        let len = digits.len();
        let mut cuts: [usize; 24] = [0; 24];
        let mut count = 0;
        let mut taken = 0;
        let mut index = 0;
        loop {
            let size = match self.sizes.get(index) {
                Some(&size) => size as usize,
                None if self.repeat => *self.sizes.last().expect("not empty") as usize,
                None => break,
            };
            index += 1;
            if taken + size >= len || count == cuts.len() {
                break;
            }
            taken += size;
            cuts[count] = len - taken;
            count += 1;
        }
        let mut start = 0;
        for &cut in cuts[..count].iter().rev() {
            out.push_str(&digits[start..cut]);
            out.push_str(&self.group);
            start = cut;
        }
        out.push_str(&digits[start..]);
    }
}
