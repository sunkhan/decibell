//! Parse Chromium desktopCapturer source ids into native handles.
//!
//! The renderer's CaptureSourcePicker passes the source id through
//! to `start_screen_share`. We parse it here so the WGC layer
//! receives a typed `CaptureTarget` instead of a string.
//!
//! Chromium source ids look like `screen:<N>:0` and `window:<HWND>:0`
//! (HWND in decimal). For screens, N is WebRTC's screen id: the
//! `EnumDisplayDevicesW(NULL, N, ..)` device index of an active display
//! (modules/desktop_capture/win/screen_capture_utils.cc GetScreenList) —
//! not a position in EnumDisplayMonitors order. capture_wgc resolves it
//! to that device's name (`\\.\DISPLAYn`) and from there to the
//! HMONITOR / DXGI output. The trailing `:0` is a Chromium
//! implementation detail (capture plane index) that we ignore.

#[derive(Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    /// WebRTC screen id (EnumDisplayDevicesW device index). Resolved to
    /// an HMONITOR / DXGI output at capture-open time by device name.
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

/// Compare two fixed-size, NUL-terminated UTF-16 device names
/// (`DISPLAY_DEVICEW.DeviceName`, `MONITORINFOEXW.szDevice`,
/// `DXGI_OUTPUT_DESC.DeviceName`) up to their terminators. GDI device
/// names are compared case-insensitively (ASCII), as Windows does.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn wide_name_eq(a: &[u16], b: &[u16]) -> bool {
    let a = &a[..a.iter().position(|&c| c == 0).unwrap_or(a.len())];
    let b = &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())];
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    let fold = |c: u16| {
        if (b'a' as u16..=b'z' as u16).contains(&c) {
            c - 32
        } else {
            c
        }
    };
    a.iter().zip(b).all(|(&x, &y)| fold(x) == fold(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide<const N: usize>(s: &str) -> [u16; N] {
        let mut out = [0u16; N];
        for (i, c) in s.encode_utf16().take(N).enumerate() {
            out[i] = c;
        }
        out
    }

    #[test]
    fn wide_names_compare_up_to_nul() {
        let a: [u16; 32] = wide(r"\\.\DISPLAY1");
        let mut b: [u16; 32] = wide(r"\\.\DISPLAY1");
        // Garbage after the terminator is ignored.
        b[20] = 0x41;
        assert!(wide_name_eq(&a, &b));
        assert!(wide_name_eq(&a, &wide::<32>(r"\\.\display1")));
        assert!(!wide_name_eq(&a, &wide::<32>(r"\\.\DISPLAY10")));
        assert!(!wide_name_eq(&a, &wide::<32>(r"\\.\DISPLAY2")));
        // Two empty names are not a match (failed lookups stay apart).
        assert!(!wide_name_eq(&[0u16; 32], &[0u16; 32]));
        // Unterminated full-width buffers still compare.
        assert!(wide_name_eq(&wide::<4>("ABCD"), &wide::<4>("abcd")));
    }

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
