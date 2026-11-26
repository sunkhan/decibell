//! Link previews and GIF search for the native client: what the Electron client's main process does
//! in `electron/main/linkPreview.ts`, `publicNet.ts` and `gifs.ts`, ported rule for rule. Plain
//! Rust + tokio, no UI types.
//!
//! - [`LinkPreviews`]: unfurl a URL from a message into a [`LinkPreview`] (OpenGraph / Twitter card /
//!   `<title>`, oEmbed, a header-only image size probe), behind the private-network guard
//!   ([`public_net`]), with Electron's caps, 30-minute cache, in-flight de-duplication and 4-request
//!   concurrency limit.
//! - [`Gifs`]: KLIPY or GIPHY search and trending feed ([`GifConfig`] from `gifs.json` or the
//!   `GIF_API_KEY` / `GIF_API_PROVIDER` environment variables). Never Tenor: its API is gone.
//! - [`images`]: a public-web picture's bytes for a view that shows it directly (the GIF picker's
//!   previews), through the same guard and caps, independent of `LinkPreviews`.
//! - [`links`]: the `decibell://invite/…` grammar and the small pure URL rules the message view
//!   needs (GIF / image labels, which links may open in the browser).
//!
//! Everything is `Send + Sync`. The async methods use tokio (sockets, timers, one task per
//! unfurl), so run them on the app's runtime, e.g. `handle.spawn(async move { previews.fetch(&url).await })`.
//! Nothing runs while idle: no pool, no timer, cache expiry is checked on access.

mod http;
mod js;

pub mod gifs;
pub mod images;
pub mod link_preview;
pub mod links;
pub mod public_net;

use std::fmt;

pub use gifs::{GifConfig, GifPage, GifProvider, GifResult, GifStatus, Gifs};
pub use link_preview::{ImagePreview, LinkPreview, LinkPreviewImage, LinkPreviews, SitePreview};
pub use links::{MediaLabel, PendingInvite};
pub use public_net::Refusal;

/// Everything that can go wrong. `Display` is the message the Electron client shows or logs for the
/// same failure (the GIF picker shows it verbatim under "GIF search failed").
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The URL doesn't parse (`new URL` threw "Invalid URL").
    InvalidUrl,
    /// The destination guard refused the URL or what its host resolves to.
    Refused(Refusal),
    /// DNS, connect, TLS or HTTP failure before a response (undici's "fetch failed").
    Network(Box<dyn std::error::Error + Send + Sync>),
    /// The deadline passed (`AbortSignal.timeout`'s "The operation was aborted due to timeout").
    Timeout,
    /// The response body broke off or didn't decode (undici's "terminated").
    Terminated(Box<dyn std::error::Error + Send + Sync>),
    /// No GIF key configured; the picker explains itself instead of showing an error.
    NotConfigured,
    /// The GIF provider's own error text, or the TS's fallback (`KLIPY returned HTTP 403`).
    Provider(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidUrl => f.write_str("Invalid URL"),
            Error::Refused(r) => r.fmt(f),
            Error::Network(_) => f.write_str("fetch failed"),
            Error::Timeout => f.write_str("The operation was aborted due to timeout"),
            Error::Terminated(_) => f.write_str("terminated"),
            Error::NotConfigured => f.write_str("not-configured"),
            Error::Provider(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Refused(r) => Some(r),
            Error::Network(e) | Error::Terminated(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod api_tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn public_types_are_send_sync() {
        assert_send_sync::<LinkPreviews>();
        assert_send_sync::<Gifs>();
        assert_send_sync::<Error>();
        assert_send_sync::<LinkPreview>();
        assert_send_sync::<GifPage>();
    }
}
