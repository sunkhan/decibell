//! Chromium's form validation of the sign-in and sign-up form (`LoginPage.tsx`'s three
//! `required` inputs, the email one `type="email"`): a submit with an empty field, or an email
//! Chromium doesn't take, is refused before anything is sent; the first such field (in the
//! form's order) takes the focus and a bubble under it says why ([`crate::auth`] draws it).
//!
//! The rules and the words are Blink's (`EmailInputType`, `TextFieldInputType`,
//! `ValidationMessageClientImpl`), checked against what Chromium answers in the Electron
//! client's own build: `tools/electron-captures/pages/auth-register-refused.tsx` records
//! `validationMessage` and `value` for a table of typed emails (the vectors in this module's
//! tests come from its output).

use std::time::Duration;

/// `valueMissing` (`IDS_FORM_VALIDATION_VALUE_MISSING`).
pub const VALUE_MISSING: &str = "Please fill out this field.";

/// Which input a value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `type="text"` / `type="password"`: only an empty value is missing ("   " is not).
    Text,
    /// `type="email"`: the value is sanitised (line breaks out, the ends trimmed) and must be
    /// an email address.
    Email,
}

/// What an email `<input>` holds for what was typed into it (`HTMLInputElement.value`): line
/// breaks removed, leading and trailing HTML white space trimmed, and an internationalised domain
/// in its ASCII form (`EmailInputType::ConvertFromVisibleValue`: `a@bä.com` → `a@xn--b-0fa.com`).
/// This is what the renderer sends to central.
pub fn email_value(visible: &str) -> String {
    to_ascii(&sanitize(visible))
}

/// Why `typed` (what the field shows) doesn't validate, as Chromium words it: `None` when it does.
pub fn message(kind: Kind, typed: &str) -> Option<String> {
    match kind {
        Kind::Text => typed.is_empty().then(|| VALUE_MISSING.to_string()),
        Kind::Email => {
            let visible = sanitize(typed);
            if visible.is_empty() {
                return Some(VALUE_MISSING.to_string());
            }
            email_mismatch(&visible, &to_ascii(&visible))
        }
    }
}

/// How long Chromium shows the bubble (`ValidationMessageClientImpl::ShowValidationMessage`): at
/// least 5 s, else 50 ms a character.
pub fn shown_for(message: &str) -> Duration {
    Duration::from_millis(5000.max(message.encode_utf16().count() as u64 * 50))
}

/// `EmailInputType::SanitizeValue` (one address): `StripLeadingAndTrailingHTMLSpaces(value
/// .RemoveCharacters(IsHTMLLineBreak))`.
fn sanitize(value: &str) -> String {
    let without_breaks: String = value.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
    without_breaks.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\u{c}' | '\r')).to_string()
}

/// `ConvertEmailAddressToASCII`: only a non-ASCII address with an `@`; the part after the first
/// `@` through IDNA (UTS #46, as the URL standard runs it); a domain that fails stays as typed.
fn to_ascii(address: &str) -> String {
    if address.is_ascii() {
        return address.to_string();
    }
    let Some(at) = address.find('@') else { return address.to_string() };
    match idna::domain_to_ascii(&address[at + 1..]) {
        Ok(domain) => format!("{}@{domain}", &address[..at]),
        Err(_) => address.to_string(),
    }
}

/// Blink's `kLocalPartCharacters`.
fn local_char_ok(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~.-".contains(c)
}

fn domain_char_ok(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '.'
}

/// `kEmailPattern`, case-insensitive: `[local chars]+ @ label (. label)*`, a label being 1–63
/// letters, digits and hyphens that neither starts nor ends with a hyphen.
fn valid_email(address: &str) -> bool {
    let Some((local, domain)) = address.split_once('@') else { return false };
    !local.is_empty()
        && local.chars().all(local_char_ok)
        && domain.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

/// `CheckValidDotUsage`: no dot at either end and never two in a row.
fn dots_ok(domain: &str) -> bool {
    domain.is_empty() || (!domain.starts_with('.') && !domain.ends_with('.') && !domain.contains(".."))
}

/// `EmailInputType::ValidationMessage` for a type mismatch. `ascii` is the value Chromium checks
/// (the domain converted), `visible` the address as typed, which the messages quote.
fn email_mismatch(visible: &str, ascii: &str) -> Option<String> {
    if valid_email(ascii) {
        return None;
    }
    let Some(at) = ascii.find('@') else {
        return Some(format!("Please include an '@' in the email address. '{visible}' is missing an '@'."));
    };
    let (local, domain) = (&ascii[..at], &ascii[at + 1..]);
    if local.is_empty() {
        return Some(format!("Please enter a part followed by '@'. '{visible}' is incomplete."));
    }
    if domain.is_empty() {
        return Some(format!("Please enter a part following '@'. '{visible}' is incomplete."));
    }
    if let Some(c) = local.chars().find(|c| !local_char_ok(*c)) {
        return Some(format!("A part followed by '@' should not contain the symbol '{c}'."));
    }
    if let Some(c) = domain.chars().find(|c| !domain_char_ok(*c)) {
        return Some(format!("A part following '@' should not contain the symbol '{c}'."));
    }
    if !dots_ok(domain) {
        let shown = visible.split_once('@').map_or(domain, |(_, d)| d);
        return Some(format!("'.' is used at a wrong position in '{shown}'."));
    }
    Some("Please enter an email address.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (typed, `value` as Chromium holds it, `validationMessage`), from the Electron client's
    /// Chromium (`pages/auth-register-refused.tsx`, inserted as typed text).
    const CHROMIUM: &[(&str, &str, &str)] = &[
        ("", "", "Please fill out this field."),
        (" ", "", "Please fill out this field."),
        ("abc", "abc", "Please include an '@' in the email address. 'abc' is missing an '@'."),
        ("abc@", "abc@", "Please enter a part following '@'. 'abc@' is incomplete."),
        ("@abc", "@abc", "Please enter a part followed by '@'. '@abc' is incomplete."),
        ("@", "@", "Please enter a part followed by '@'. '@' is incomplete."),
        ("a@b", "a@b", ""),
        ("a@b.c", "a@b.c", ""),
        ("a b@c", "a b@c", "A part followed by '@' should not contain the symbol ' '."),
        ("a@b c", "a@b c", "A part following '@' should not contain the symbol ' '."),
        ("a@b..c", "a@b..c", "'.' is used at a wrong position in 'b..c'."),
        ("a@.b", "a@.b", "'.' is used at a wrong position in '.b'."),
        ("a@b.", "a@b.", "'.' is used at a wrong position in 'b.'."),
        ("a@-b", "a@-b", "Please enter an email address."),
        ("a@b-", "a@b-", "Please enter an email address."),
        ("a@b-.c", "a@b-.c", "Please enter an email address."),
        ("a@-b.c", "a@-b.c", "Please enter an email address."),
        ("ä@b", "ä@b", "A part followed by '@' should not contain the symbol 'ä'."),
        ("a@bä.com", "a@xn--b-0fa.com", ""),
        ("a@é.com", "a@xn--9ca.com", ""),
        ("a@b_c", "a@b_c", "A part following '@' should not contain the symbol '_'."),
        ("a(b)@c", "a(b)@c", "A part followed by '@' should not contain the symbol '('."),
        ("a@b@c", "a@b@c", "A part following '@' should not contain the symbol '@'."),
        (" a@b ", "a@b", ""),
        ("a..b@c", "a..b@c", ""),
        (".a@b", ".a@b", ""),
        ("a.@b", "a.@b", ""),
        ("a+tag@example.test", "a+tag@example.test", ""),
        ("x@123", "x@123", ""),
        ("a@[1.2.3.4]", "a@[1.2.3.4]", "A part following '@' should not contain the symbol '['."),
        ("a\"b@c", "a\"b@c", "A part followed by '@' should not contain the symbol '\"'."),
        ("a@b,c", "a@b,c", "A part following '@' should not contain the symbol ','."),
        ("a@b;c", "a@b;c", "A part following '@' should not contain the symbol ';'."),
        ("a@b\\c", "a@b\\c", "A part following '@' should not contain the symbol '\\'."),
        ("a@xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx.com", "a@xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx.com", ""),
        ("a@xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx.com", "a@xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx.com", "Please enter an email address."),
        ("😀@b", "😀@b", "A part followed by '@' should not contain the symbol '😀'."),
        ("a@😀.com", "a@xn--e28h.com", ""),
        ("a@b.😀", "a@b.xn--e28h", ""),
        ("a@b:c", "a@b:c", "A part following '@' should not contain the symbol ':'."),
        ("a@b..", "a@b..", "'.' is used at a wrong position in 'b..'."),
        ("a@.", "a@.", "'.' is used at a wrong position in '.'."),
        ("a@..b", "a@..b", "'.' is used at a wrong position in '..b'."),
        ("A@B.COM", "A@B.COM", ""),
        ("a@b.c.", "a@b.c.", "'.' is used at a wrong position in 'b.c.'."),
        ("a@ b", "a@ b", "A part following '@' should not contain the symbol ' '."),
    ];

    #[test]
    fn chromium_email_messages() {
        for (typed, value, expected) in CHROMIUM {
            assert_eq!(email_value(typed), *value, "the value of {typed:?}");
            let got = message(Kind::Email, typed).unwrap_or_default();
            assert_eq!(got, *expected, "the message for {typed:?}");
        }
    }

    #[test]
    fn required_text_fields() {
        // `pages/auth-register-refused.tsx`: an empty `required` text input is missing, spaces are not.
        assert_eq!(message(Kind::Text, "").as_deref(), Some(VALUE_MISSING));
        assert_eq!(message(Kind::Text, "   "), None);
        assert_eq!(message(Kind::Text, "perf1"), None);
    }

    #[test]
    fn the_bubble_stays_5_s_or_50_ms_a_character() {
        assert_eq!(shown_for(VALUE_MISSING), Duration::from_secs(5));
        assert_eq!(shown_for(&"x".repeat(200)), Duration::from_secs(10));
    }
}
