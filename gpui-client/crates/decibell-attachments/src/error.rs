use std::io;

/// Everything that can go wrong moving an attachment. Server refusals the UI words differently
/// have their own variants; the rest of the HTTP statuses are [`Error::Http`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No attachment target registered for that server (not connected, or it has no
    /// attachment port).
    #[error("not connected to that server")]
    NotConnected,
    /// An encrypted-channel attachment whose key isn't known (the message wasn't opened).
    #[error("no key for this encrypted attachment")]
    NoKey,
    /// Bigger than the server's per-file cap: refused before sending, or `413` from the server.
    #[error("the file is larger than this server allows")]
    TooLarge,
    /// `507`: the upload would eat the server's free-space headroom.
    #[error("the server is out of space")]
    InsufficientStorage,
    /// `404` on a PATCH: the server swept the upload (idle an hour) or it was deleted.
    #[error("Upload expired on the server")]
    UploadExpired,
    /// Any other unexpected status.
    #[error("HTTP {0}")]
    Http(u16),
    /// The attachment host presented another certificate than the community's pinned one
    /// (the core verifier's `CERT_MISMATCH:host:port:fingerprint` contract).
    #[error("certificate mismatch: {0}")]
    CertMismatch(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("timed out")]
    Timeout,
    /// A sealed chunk or thumbnail failed authentication.
    #[error("the file failed to decrypt")]
    Decrypt,
    /// The server sent fewer bytes than the range it promised.
    #[error("the server sent fewer bytes than expected")]
    ShortRead,
    #[error("cancelled")]
    Cancelled,
    /// A local file error (reading an upload, writing a download).
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The picture couldn't be decoded or encoded.
    #[error("image: {0}")]
    Image(String),
    /// Bad input from the caller or a malformed server answer.
    #[error("{0}")]
    Invalid(&'static str),
    /// A user-facing condition with its own wording (download manager).
    #[error("{0}")]
    Message(String),
}

impl Error {
    /// The HTTP status behind this error, when there is one.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Http(s) => Some(*s),
            Error::TooLarge => Some(413),
            Error::InsufficientStorage => Some(507),
            _ => None,
        }
    }

    /// The download manager's wording (`downloads.ts` `httpError` + `describe`), for the
    /// Transfers panel's "Failed — …" line.
    pub fn download_message(&self) -> String {
        match self {
            Error::Http(404) | Error::Http(410) => "No longer on the server".into(),
            Error::Http(401) | Error::Http(403) => "Not allowed by the server".into(),
            Error::Decrypt | Error::Http(502) => "Couldn't decrypt the file".into(),
            Error::Http(s) => format!("Server error {s}"),
            Error::Message(m) => m.clone(),
            Error::NoKey => "Open the file's channel again, then retry".into(),
            Error::Io(e) => io_message(e).unwrap_or("Network error").into(),
            _ => "Network error".into(),
        }
    }

    /// The renderer's wording of a failed upload (`startQueuedUpload` →
    /// `markFailed(pendingId, err.message)`): a refusal of the init request reads
    /// `init failed: HTTP <status> <reason>`. The server refuses a file over its cap (413) or one
    /// that would eat its free-space headroom (507) at init — this client refuses the first
    /// before sending anything, with the same words. Other statuses keep `HTTP <status>
    /// <reason>`; everything else its own message.
    pub fn upload_message(&self) -> String {
        let reason = |status: u16| http::StatusCode::from_u16(status).ok().and_then(|s| s.canonical_reason()).unwrap_or("");
        match self {
            Error::TooLarge => format!("init failed: HTTP 413 {}", reason(413)),
            Error::InsufficientStorage => format!("init failed: HTTP 507 {}", reason(507)),
            Error::Http(status) => format!("HTTP {status} {}", reason(*status)).trim_end().to_string(),
            Error::Cancelled => "Cancelled".into(),
            other => other.to_string(),
        }
    }

    pub(crate) fn from_status(status: u16) -> Error {
        match status {
            413 => Error::TooLarge,
            507 => Error::InsufficientStorage,
            s => Error::Http(s),
        }
    }

    pub(crate) fn network(e: impl std::fmt::Display) -> Error {
        let s = e.to_string();
        // The pinning verifier's contract (core `net/tls.rs`): the handshake fails with a
        // rustls General error "CERT_MISMATCH:host:port:presented".
        if let Some(i) = s.find("CERT_MISMATCH:") {
            return Error::CertMismatch(s[i + "CERT_MISMATCH:".len()..].trim().to_string());
        }
        Error::Network(s)
    }
}

/// `describe()` for local file errors.
fn io_message(e: &io::Error) -> Option<&'static str> {
    #[cfg(unix)]
    const ENOSPC: i32 = 28;
    #[cfg(windows)]
    const ENOSPC: i32 = 112; // ERROR_DISK_FULL
    if e.raw_os_error() == Some(ENOSPC) || e.kind() == io::ErrorKind::StorageFull {
        return Some("Disk full");
    }
    match e.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => Some("Can't write to the folder"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn upload_refusals_read_as_the_renderer_writes_them() {
        assert_eq!(Error::TooLarge.upload_message(), "init failed: HTTP 413 Payload Too Large");
        assert_eq!(Error::InsufficientStorage.upload_message(), "init failed: HTTP 507 Insufficient Storage");
        assert_eq!(Error::Http(403).upload_message(), "HTTP 403 Forbidden");
        assert_eq!(Error::UploadExpired.upload_message(), "Upload expired on the server");
    }
}
