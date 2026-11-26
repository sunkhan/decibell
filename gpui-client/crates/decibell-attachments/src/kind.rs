//! Attachment kinds and MIME guessing: `classify` of `uploadAttachment.ts` (the server's
//! `kind_from_mime` is the same rule) and `guessMime` of `electron/main/fs.ts`, which named
//! every picked or dropped file.

use serde::{Deserialize, Serialize};

/// `Attachment.Kind` (proto): IMAGE = 0, VIDEO = 1, DOCUMENT = 2, AUDIO = 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    Image,
    Video,
    Document,
    Audio,
}

impl AttachmentKind {
    /// By MIME prefix; everything else is a document.
    pub fn from_mime(mime: &str) -> Self {
        let m = mime.to_ascii_lowercase();
        if m.starts_with("image/") {
            Self::Image
        } else if m.starts_with("video/") {
            Self::Video
        } else if m.starts_with("audio/") {
            Self::Audio
        } else {
            Self::Document
        }
    }

    /// The wire number.
    pub fn index(self) -> i32 {
        match self {
            Self::Image => 0,
            Self::Video => 1,
            Self::Document => 2,
            Self::Audio => 3,
        }
    }

    pub fn from_index(i: i64) -> Option<Self> {
        Some(match i {
            0 => Self::Image,
            1 => Self::Video,
            2 => Self::Document,
            3 => Self::Audio,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Document => "document",
            Self::Audio => "audio",
        }
    }
}

const EXT_TO_MIME: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
    ("tiff", "image/tiff"),
    ("avif", "image/avif"),
    ("mp4", "video/mp4"),
    ("webm", "video/webm"),
    ("mov", "video/quicktime"),
    ("mkv", "video/x-matroska"),
    ("avi", "video/x-msvideo"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("m4a", "audio/mp4"),
    ("flac", "audio/flac"),
    ("opus", "audio/opus"),
    ("pdf", "application/pdf"),
    ("txt", "text/plain"),
    ("json", "application/json"),
];

/// MIME type from a file name's extension (`guessMime`), `application/octet-stream` otherwise.
pub fn guess_mime(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    EXT_TO_MIME.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m).unwrap_or("application/octet-stream")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_and_kind() {
        assert_eq!(guess_mime("Holiday.JPG"), "image/jpeg");
        assert_eq!(guess_mime("a.tar.gz"), "application/octet-stream");
        assert_eq!(guess_mime("noext"), "application/octet-stream");
        assert_eq!(AttachmentKind::from_mime("Video/MP4"), AttachmentKind::Video);
        assert_eq!(AttachmentKind::from_mime("application/pdf"), AttachmentKind::Document);
        assert_eq!(AttachmentKind::from_index(3), Some(AttachmentKind::Audio));
    }
}
