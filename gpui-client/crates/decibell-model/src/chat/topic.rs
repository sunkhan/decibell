//! A channel's topic as typed (`features/servers/channelTopic.ts`, `lib/utf8.ts`'s
//! `clampLine`): one line, cut to the server's byte cap on a character boundary while
//! typing, so the save comes back exactly as the user saw it (settings saves settle on
//! the echo).

/// The server's cap on a channel topic, in UTF-8 bytes.
pub const TOPIC_MAX_BYTES: usize = 1024;

/// A one-line field as the community stores it (`clamp_utf8`): runs of line breaks and tabs
/// become one space (the server drops control characters outright, which would glue the
/// words together), other control characters go, and the text is cut to `max_bytes`.
pub fn clamp_line(input: &str, max_bytes: usize) -> String {
    let mut line = String::with_capacity(input.len().min(max_bytes + 4));
    let mut in_break = false;
    for c in input.chars() {
        if matches!(c, '\r' | '\n' | '\t') {
            if !in_break {
                line.push(' ');
            }
            in_break = true;
            continue;
        }
        in_break = false;
        if (c as u32) < 0x20 || c == '\u{7f}' {
            continue;
        }
        if line.len() + c.len_utf8() > max_bytes {
            break;
        }
        line.push(c);
    }
    if line.len() > max_bytes {
        // Only a trailing space from a break can push past it.
        line.truncate(max_bytes);
    }
    line
}

/// `normalizeTopicInput`.
pub fn normalize_topic_input(input: &str) -> String {
    clamp_line(input, TOPIC_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_cut_on_a_character() {
        assert_eq!(clamp_line("a\r\n\tb\u{1}c\u{7f}", 100), "a bc");
        assert_eq!(clamp_line("ğğğ", 5), "ğğ", "2-byte letters, cut before the third");
        assert_eq!(normalize_topic_input(&"x".repeat(2000)).len(), TOPIC_MAX_BYTES);
        assert_eq!(clamp_line("ab\n", 2), "ab");
    }
}
