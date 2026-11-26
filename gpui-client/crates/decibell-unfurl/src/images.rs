//! Pictures from the public web: a link preview's image, a GIF link, a pasted image URL. What
//! the renderer's `<img src=https:…>` fetched (Blink, under the page's CSP `img-src https:`), done
//! here with the unfurler's guard so a message can't make the client reach a private address.
//!
//! Rules: https only (the unfurler already upgraded `http:` image references; anything else is
//! refused, as the CSP refused it); every hop through [`crate::public_net`]; redirects followed by
//! hand (≤ 5); the response head within 8 s per hop, the whole body within [`BODY_TIMEOUT`]; at
//! most [`MAX_BYTES`] decoded bytes (Blink has no cap; a picture larger than this is not shown);
//! `Referer` never sent (`referrerPolicy="no-referrer"`). No cache here: the caller keeps what it
//! decoded.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use url::Url;

use crate::http::HttpClient;
use crate::Error;

/// Largest picture fetched (decoded bytes).
pub const MAX_BYTES: usize = 32 << 20;
/// Per hop: until the response head.
const HOP_TIMEOUT: Duration = Duration::from_secs(8);
/// The body of one picture.
pub const BODY_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: usize = 5;
/// What Chromium asks for an `<img>`.
const ACCEPT_IMAGE: &str = "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8";

/// See the module doc. Cheap to clone.
#[derive(Clone)]
pub struct RemoteImages {
    inner: Arc<Inner>,
}

struct Inner {
    http: HttpClient,
    user_agent: String,
    max_bytes: usize,
}

impl RemoteImages {
    /// `app_version` goes into the User-Agent the unfurler sends.
    pub fn new(app_version: &str) -> Self {
        Self::with(HttpClient::new(), app_version, MAX_BYTES)
    }

    /// The `loopback` feature only (development builds' self-tests): may also reach 127.0.0.0/8.
    #[cfg(feature = "loopback")]
    pub fn loopback(app_version: &str) -> Self {
        Self::with(HttpClient::loopback(), app_version, MAX_BYTES)
    }

    fn with(http: HttpClient, app_version: &str, max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                user_agent: format!("Mozilla/5.0 (compatible; Decibell/{app_version}; +https://github.com/sunkhan/decibell)"),
                max_bytes,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(http: HttpClient, max_bytes: usize) -> Self {
        Self::with(http, "0.0.0-test", max_bytes)
    }

    /// The picture at `url`: its bytes, or why not. Must be polled on a tokio runtime.
    pub async fn fetch(&self, url: &str) -> Result<Bytes, Error> {
        let mut current = Url::parse(url).map_err(|_| Error::InvalidUrl)?;
        for _ in 0..=MAX_REDIRECTS {
            if current.scheme() != "https" {
                return Err(Error::InvalidUrl);
            }
            let headers = [
                ("User-Agent", self.inner.user_agent.as_str()),
                ("Accept", ACCEPT_IMAGE),
                ("Accept-Encoding", "gzip, deflate, br"),
            ];
            let res = match tokio::time::timeout(HOP_TIMEOUT, self.inner.http.get(&current, &headers, true)).await {
                Err(_) => return Err(Error::Timeout),
                Ok(r) => r?,
            };
            if (300..400).contains(&res.status) {
                let Some(location) = res.header("location").filter(|l| !l.is_empty()) else {
                    return Err(Error::Provider(format!("HTTP {}", res.status)));
                };
                current = current.join(&location).map_err(|_| Error::InvalidUrl)?;
                continue;
            }
            if !(200..=299).contains(&res.status) {
                return Err(Error::Provider(format!("HTTP {}", res.status)));
            }
            let max = self.inner.max_bytes;
            let body = match tokio::time::timeout(BODY_TIMEOUT, res.read_body(max + 1, None, |_| false)).await {
                Err(_) => return Err(Error::Timeout),
                Ok(r) => r?,
            };
            if body.len() > max {
                return Err(Error::Provider("the picture is too large".into()));
            }
            return Ok(Bytes::from(body));
        }
        Err(Error::Provider("too many redirects".into()))
    }
}

/// The URL as an `<img>` would load it: absolute, https (`http:` upgraded, as the renderer
/// upgrades image references), anything else refused.
pub fn normalize(url: &str) -> Result<Url, Error> {
    let mut parsed = Url::parse(url.trim()).map_err(|_| Error::InvalidUrl)?;
    match parsed.scheme() {
        "https" => {}
        "http" => parsed.set_scheme("https").map_err(|_| Error::InvalidUrl)?,
        _ => return Err(Error::InvalidUrl),
    }
    Ok(parsed)
}

/// A public picture's bytes for a view that holds no [`RemoteImages`] of its own (the GIF picker's
/// previews): the URL through [`normalize`], then [`RemoteImages::fetch`] on one shared instance.
/// Must be polled on a tokio runtime.
pub async fn fetch(url: &str) -> Result<Vec<u8>, Error> {
    static SHARED: OnceLock<RemoteImages> = OnceLock::new();
    let url = normalize(url)?;
    let images = SHARED.get_or_init(|| RemoteImages::new(env!("CARGO_PKG_VERSION")));
    Ok(images.fetch(url.as_str()).await?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_https_is_fetched() {
        let images = RemoteImages::new("0.0.0-test");
        assert!(matches!(images.fetch("http://example.com/a.png").await, Err(Error::InvalidUrl)));
        assert!(matches!(images.fetch("ftp://example.com/a.png").await, Err(Error::InvalidUrl)));
        assert!(matches!(images.fetch("not a url").await, Err(Error::InvalidUrl)));
    }

    #[tokio::test]
    async fn private_addresses_are_refused() {
        let images = RemoteImages::new("0.0.0-test");
        for url in ["https://127.0.0.1/a.png", "https://10.0.0.1/a.png", "https://[::1]/a.png", "https://192.168.1.1/x.gif"] {
            assert!(matches!(images.fetch(url).await, Err(Error::Refused(_))), "{url}");
        }
    }

    #[test]
    fn only_web_pictures_and_http_is_upgraded() {
        assert_eq!(normalize("http://media.klipy.com/a.gif").unwrap().as_str(), "https://media.klipy.com/a.gif");
        assert_eq!(normalize(" https://x.test/b.gif ").unwrap().as_str(), "https://x.test/b.gif");
        assert!(normalize("file:///etc/passwd").is_err());
        assert!(normalize("decibell://invite/X").is_err());
        assert!(normalize("not a url").is_err());
    }

    #[tokio::test]
    async fn the_shared_fetch_refuses_private_destinations() {
        // The guard answers before any connection: a loopback or private address is refused.
        for url in ["https://127.0.0.1/x.gif", "https://10.0.0.1/x.gif", "https://[::1]/x.gif"] {
            assert!(matches!(fetch(url).await, Err(Error::Refused(_))), "{url}");
        }
    }
}
