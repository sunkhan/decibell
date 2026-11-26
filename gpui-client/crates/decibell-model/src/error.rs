//! The one place a `CoreError` becomes what the UI shows.

use decibell_core::api::{CoreError, ErrorKind};
use gpui::SharedString;

/// A failed core call, ready to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiError {
    pub kind: ErrorKind,
    /// The core's message, verbatim (some are the server's own words).
    pub text: SharedString,
    /// Set when the call failed because a server's certificate no longer matches its
    /// pin: the caller offers the re-trust prompt (`Overlays::set_cert_mismatch`)
    /// instead of showing `text`. [`crate::call`]'s helpers do that themselves.
    pub cert: Option<CertMismatch>,
}

/// `CERT_MISMATCH:<host>:<port>:<sha256hex>` taken apart (`lib/certMismatch.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertMismatch {
    pub host: SharedString,
    pub port: u16,
    /// sha256-hex of the certificate the server presented.
    pub fingerprint: SharedString,
}

impl UiError {
    /// The core is not running (a call made before `start` finished or after shutdown).
    pub fn not_running() -> UiError {
        UiError {
            kind: ErrorKind::NotConnected,
            text: SharedString::new_static("Not connected to central server"),
            cert: None,
        }
    }

    /// The text as Electron's `String(err)` gives it: `"Error: <message>"`. The sign-in
    /// page shows exactly that (`LoginPage.tsx`: `setLoginError(String(err))`), so the
    /// sign-in and registration errors keep the prefix here too (docs/model.md §11).
    pub fn js_string(&self) -> SharedString {
        format!("Error: {}", self.text).into()
    }

    /// For toasts: `String(e).replace(/^Error:\s*/, "")`, i.e. the plain message.
    pub fn plain(&self) -> SharedString {
        self.text.clone()
    }
}

impl From<CoreError> for UiError {
    fn from(e: CoreError) -> UiError {
        let cert = parse_cert_mismatch(e.message());
        UiError {
            kind: e.kind(),
            text: e.into_message().into(),
            cert,
        }
    }
}

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// A TLS handshake the core refused because the certificate doesn't match the pin
/// answers `…CERT_MISMATCH:<host>:<port>:<sha256hex>`. The host may contain `:` (IPv6):
/// the port and the fingerprint are the last two fields.
pub fn parse_cert_mismatch(text: &str) -> Option<CertMismatch> {
    const MARK: &str = "CERT_MISMATCH:";
    let rest = &text[text.find(MARK)? + MARK.len()..];
    let (rest, fingerprint) = rest.rsplit_once(':')?;
    let (host, port) = rest.rsplit_once(':')?;
    // JS `parseInt`: leading digits count ("8082abc" is 8082).
    let digits: String = port.trim_start().chars().take_while(char::is_ascii_digit).collect();
    let port = digits.parse::<u16>().ok()?;
    if host.is_empty() {
        return None;
    }
    Some(CertMismatch {
        host: host.to_string().into(),
        port,
        fingerprint: fingerprint.trim().to_string().into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_mismatch_is_taken_apart() {
        let m = parse_cert_mismatch("TLS: CERT_MISMATCH:127.0.0.11:8082:ab12cd").unwrap();
        assert_eq!((&*m.host, m.port, &*m.fingerprint), ("127.0.0.11", 8082, "ab12cd"));
        let v6 = parse_cert_mismatch("CERT_MISMATCH:::1:8082:ff00 ").unwrap();
        assert_eq!((&*v6.host, v6.port, &*v6.fingerprint), ("::1", 8082, "ff00"));
        assert_eq!(parse_cert_mismatch("Connection refused"), None);
        assert_eq!(parse_cert_mismatch("CERT_MISMATCH:host:8082"), None);
        assert_eq!(parse_cert_mismatch("CERT_MISMATCH:host:port:ff"), None);
    }

    #[test]
    fn core_errors_keep_their_text_and_kind() {
        let e: UiError = CoreError::other("Not connected to community 3").into();
        assert_eq!(e.kind, ErrorKind::NotConnected);
        assert_eq!(&*e.text, "Not connected to community 3");
        assert_eq!(&*e.js_string(), "Error: Not connected to community 3");
        assert!(e.cert.is_none());
        let e: UiError = CoreError::other("connect: CERT_MISMATCH:10.0.0.1:8080:aa").into();
        assert_eq!(e.cert.unwrap().port, 8080);
    }
}
