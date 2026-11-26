//! Remote pictures for views that show a public-web image directly (the renderer's
//! `<img src="https://…">`: the GIF picker's previews, an embed's picture), independent of
//! [`crate::LinkPreviews`]: one GET through the same guard as the unfurler (public addresses only,
//! every hop checked), https only (`http:` is upgraded, as the renderer upgrades image refs),
//! a few redirects, a deadline for the whole exchange and a size cap on the decoded body.
//!
//! The caller runs it on the app's tokio runtime, decodes the bytes and keeps them for as long as
//! the picture is on screen; nothing here caches (Electron's renderer kept them in Chromium's
//! memory cache; this client's views own theirs).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use url::Url;

use crate::http::HttpClient;
use crate::Error;

/// The whole exchange, redirects included.
pub const TIMEOUT: Duration = Duration::from_secs(15);
/// The largest picture accepted (decoded bytes): a GIF preview is ~0.1–2 MB.
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

fn client() -> Arc<HttpClient> {
    static CLIENT: OnceLock<Arc<HttpClient>> = OnceLock::new();
    CLIENT.get_or_init(|| Arc::new(HttpClient::new())).clone()
}

/// The URL as an `<img>` would load it: absolute, https (`http:` upgraded), anything else refused.
pub fn normalize(url: &str) -> Result<Url, Error> {
    let mut parsed = Url::parse(url.trim()).map_err(|_| Error::InvalidUrl)?;
    match parsed.scheme() {
        "https" => {}
        "http" => parsed.set_scheme("https").map_err(|_| Error::InvalidUrl)?,
        _ => return Err(Error::InvalidUrl),
    }
    Ok(parsed)
}

/// Fetches a public picture's bytes. `Err` for a refused destination, a non-2xx answer
/// (`Error::Provider("HTTP <status>")`), a body over [`MAX_BYTES`], or the deadline.
pub async fn fetch(url: &str) -> Result<Vec<u8>, Error> {
    let start = normalize(url)?;
    let http = client();
    let work = async move {
        // What Chromium sends for an <img> with `referrerPolicy="no-referrer"`.
        let headers = [
            ("accept", "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8"),
            ("accept-encoding", "gzip, deflate, br"),
            ("user-agent", "Mozilla/5.0 (compatible; Decibell)"),
        ];
        let mut current = start;
        for _ in 0..=MAX_REDIRECTS {
            let res = http.get(&current, &headers, false).await?;
            if (300..400).contains(&res.status) {
                let Some(location) = res.header("location").filter(|l| !l.is_empty()) else {
                    return Err(Error::Provider(format!("HTTP {}", res.status)));
                };
                current = normalize(current.join(&location).map_err(|_| Error::InvalidUrl)?.as_str())?;
                continue;
            }
            if !(200..=299).contains(&res.status) {
                return Err(Error::Provider(format!("HTTP {}", res.status)));
            }
            let mut over = false;
            let body = res
                .read_body(MAX_BYTES + 1, None, |bytes| {
                    over = bytes.len() > MAX_BYTES;
                    over
                })
                .await?;
            if over || body.len() > MAX_BYTES {
                return Err(Error::Provider("too large".into()));
            }
            return Ok(body);
        }
        Err(Error::Network("redirect count exceeded".into()))
    };
    match tokio::time::timeout(TIMEOUT, work).await {
        Err(_) => Err(Error::Timeout),
        Ok(result) => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_pictures_and_http_is_upgraded() {
        assert_eq!(normalize("http://media.klipy.com/a.gif").unwrap().as_str(), "https://media.klipy.com/a.gif");
        assert_eq!(normalize(" https://x.test/b.gif ").unwrap().as_str(), "https://x.test/b.gif");
        assert!(normalize("file:///etc/passwd").is_err());
        assert!(normalize("decibell://invite/X").is_err());
        assert!(normalize("not a url").is_err());
    }

    #[tokio::test]
    async fn private_destinations_are_refused() {
        // The guard answers before any connection: a loopback or private address is refused.
        for url in ["https://127.0.0.1/x.gif", "https://10.0.0.1/x.gif", "https://[::1]/x.gif"] {
            assert!(matches!(fetch(url).await, Err(Error::Refused(_))), "{url}");
        }
    }
}
