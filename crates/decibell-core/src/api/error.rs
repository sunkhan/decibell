//! The one error type of `api`. Its message is the text a command has always
//! answered with (the Electron renderer shows some of these and matches a few),
//! so it is carried verbatim; the kind only names the distinctions the code
//! already makes, for a client that wants to branch without matching text.

use std::fmt;

/// What went wrong, where the code tells it apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// No connection to central or to the community the call needs, or it
    /// dropped ("Not connected to central server", "Central connection lost",
    /// "Not connected to community 3", "Community connection lost",
    /// "Connection closed").
    NotConnected,
    /// No signed-in account on this device ("Not authenticated",
    /// "Not signed in").
    NotSignedIn,
    /// The server answered and said no; the message is its own text (a
    /// rename, a password change, a device sign-out, an invite code).
    Refused,
    /// An argument the call can't use: an unknown name for an enum value, bad
    /// base64, a bad port, an empty invite code, a malformed invite link.
    InvalidArgument,
    /// Everything else: a send that timed out, an answer that never came,
    /// local state that rules the call out ("Already in a call"), local I/O,
    /// crypto.
    Other,
}

/// An `api` call's failure: today's message, verbatim, and its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreError {
    kind: ErrorKind,
    message: String,
}

impl CoreError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        CoreError { kind, message: message.into() }
    }

    /// A message from the core's own code: the kind follows from the
    /// messages that name a missing connection or sign-in, else `Other`.
    pub fn other(message: impl Into<String>) -> Self {
        let message = message.into();
        CoreError { kind: classify(&message), message }
    }

    /// The server refused; `message` is what it said.
    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Refused, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgument, message)
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn into_message(self) -> String {
        self.message
    }
}

/// The kind of a core message. Only exact texts (and the per-community
/// "Not connected to community …" family) the code uses for these cases.
fn classify(message: &str) -> ErrorKind {
    match message {
        "Not authenticated" | "Not signed in" => ErrorKind::NotSignedIn,
        "Not connected to central server"
        | "Not connected to the server"
        | "Central connection lost"
        | "Community connection lost"
        | "Connection closed" => ErrorKind::NotConnected,
        m if m.starts_with("Not connected to community ") => ErrorKind::NotConnected,
        _ => ErrorKind::Other,
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CoreError {}

impl From<String> for CoreError {
    fn from(message: String) -> Self {
        CoreError::other(message)
    }
}

impl From<&str> for CoreError {
    fn from(message: &str) -> Self {
        CoreError::other(message)
    }
}

/// What the addon's commands always answered with: `Error::from_reason`, i.e.
/// status `GenericFailure` and the message as the reason (JS sees it as
/// `error.message`, `error.code === "GenericFailure"`).
#[cfg(feature = "napi")]
impl From<CoreError> for napi::Error {
    fn from(e: CoreError) -> Self {
        napi::Error::from_reason(e.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_is_carried_verbatim() {
        let e = CoreError::other(format!("Not connected to community {}", 7));
        assert_eq!(e.to_string(), "Not connected to community 7");
        assert_eq!(e.message(), "Not connected to community 7");
        assert_eq!(e.into_message(), "Not connected to community 7");
    }

    #[test]
    fn kinds_follow_the_messages_that_name_them() {
        for m in ["Not authenticated", "Not signed in"] {
            assert_eq!(CoreError::other(m).kind(), ErrorKind::NotSignedIn, "{m}");
        }
        for m in [
            "Not connected to central server",
            "Not connected to the server",
            "Central connection lost",
            "Community connection lost",
            "Connection closed",
            "Not connected to community 3",
            "Not connected to community server 3",
        ] {
            assert_eq!(CoreError::from(m).kind(), ErrorKind::NotConnected, "{m}");
        }
        for m in ["Send timed out", "Already in a call", "Connection closed before the reply", ""] {
            assert_eq!(CoreError::from(m.to_string()).kind(), ErrorKind::Other, "{m}");
        }
        assert_eq!(CoreError::refused("Wrong password").kind(), ErrorKind::Refused);
        assert_eq!(CoreError::invalid("Invalid port").kind(), ErrorKind::InvalidArgument);
    }

    #[cfg(feature = "napi")]
    #[test]
    fn napi_gets_the_reason_with_generic_failure() {
        let e: napi::Error = CoreError::refused("That name is taken").into();
        assert_eq!(e.reason, "That name is taken");
        assert_eq!(e.status, napi::Status::GenericFailure);
    }
}
