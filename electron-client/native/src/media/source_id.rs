//! Parse Chromium desktopCapturer source ids into native handles.
//!
//! The renderer's CaptureSourcePicker passes the source id through
//! to `start_screen_share`. We parse it here so the WGC layer
//! receives a typed `CaptureTarget` instead of a string.
//!
//! Chromium source ids look like `screen:<N>:0` (where N is the
//! display index in left-to-right top-to-bottom order, matching
//! EnumDisplayMonitors) and `window:<HWND>:0` (HWND in decimal).
//! The trailing `:0` is a Chromium implementation detail (capture
//! plane index) that we ignore.

#[derive(Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    /// Index into Chromium's enumerated monitors. Resolved to an
    /// HMONITOR at WGC-open time via EnumDisplayMonitors.
    Monitor(u32),
    /// Decimal HWND value as a u64 — cast to HWND inside WGC.
    Window(u64),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    EmptyId,
    UnknownKind,
    BadIndex,
}

pub fn parse(id: &str) -> Result<CaptureTarget, ParseError> {
    if id.is_empty() {
        return Err(ParseError::EmptyId);
    }
    let mut parts = id.split(':');
    let kind = parts.next().ok_or(ParseError::UnknownKind)?;
    let payload = parts.next().ok_or(ParseError::BadIndex)?;
    match kind {
        "screen" => Ok(CaptureTarget::Monitor(
            payload.parse().map_err(|_| ParseError::BadIndex)?,
        )),
        "window" => Ok(CaptureTarget::Window(
            payload.parse().map_err(|_| ParseError::BadIndex)?,
        )),
        _ => Err(ParseError::UnknownKind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_screen_id() {
        assert_eq!(parse("screen:0:0").unwrap(), CaptureTarget::Monitor(0));
        assert_eq!(parse("screen:2:0").unwrap(), CaptureTarget::Monitor(2));
    }

    #[test]
    fn parses_window_id() {
        assert_eq!(parse("window:65998:0").unwrap(), CaptureTarget::Window(65998));
        assert_eq!(parse("window:1:0").unwrap(), CaptureTarget::Window(1));
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(parse(""), Err(ParseError::EmptyId));
    }

    #[test]
    fn rejects_unknown_kind() {
        assert_eq!(parse("tab:1:0"), Err(ParseError::UnknownKind));
    }

    #[test]
    fn rejects_non_numeric_index() {
        assert_eq!(parse("screen:abc:0"), Err(ParseError::BadIndex));
    }
}
