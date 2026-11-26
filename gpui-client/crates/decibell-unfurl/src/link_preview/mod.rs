//! The link unfurler (port of `electron/main/linkPreview.ts`): a message's URL in, a small metadata
//! record out, or nothing.
//!
//! - A page: OpenGraph, then Twitter-card tags, then `<title>` / `<meta name=description>`; an oEmbed
//!   discovery link fills what the page's own tags left out (YouTube, Vimeo, SoundCloud…); an image
//!   without declared dimensions is probed (header bytes only).
//! - A direct image link: its dimensions and whether it's a GIF (content type or magic bytes).
//! - Guards: [`crate::public_net`] on every hop (page, redirects, `og:image` probe, oEmbed endpoint);
//!   redirects followed by hand (≤ 5); reads capped (a page up to its `</head>` or 3 MiB, 64 KiB of
//!   an image or an oEmbed answer); 8 s per request; at most 4 unfurls at a time; results cached
//!   (30 min hits, 5 min misses, 500 entries) and de-duplicated while in flight.

mod extract;
mod probe;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{watch, Semaphore};
use url::Url;

use crate::http::HttpClient;
use crate::js;
use crate::Error;
use extract::{
    clean, decode_html, is_html, parse_page, pick, resolve_image, to_int, to_int_number,
    valid_color, DESCRIPTION_MAX, SITE_NAME_MAX, TITLE_MAX, URL_MAX,
};
use probe::probe_image_dims;

/// An unfurled link: the shape the Electron renderer's `linkPreviewStore` receives (serde gives the
/// same JSON: `{"kind":"site","siteName":…}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LinkPreview {
    /// A page: OpenGraph / Twitter-card / `<title>` metadata.
    Site(SitePreview),
    /// The link is an image file.
    Image(ImagePreview),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SitePreview {
    /// The page's final URL (after redirects).
    pub url: String,
    /// `og:site_name`, oEmbed's `provider_name`, else the host without `www.`; ≤ 80 characters.
    pub site_name: Option<String>,
    /// ≤ 200 characters. `Some("")` is possible (a blank oEmbed title), as in the TS.
    pub title: Option<String>,
    /// ≤ 350 characters.
    pub description: Option<String>,
    pub image: Option<LinkPreviewImage>,
    /// Render the image full-width under the text (video pages, `summary_large_image` cards)
    /// instead of as a side thumbnail.
    pub large_image: bool,
    /// The page's `theme-color` as a lowercase `#hex` string, for the card's edge.
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPreviewImage {
    /// https only (`http:` references are upgraded).
    pub url: String,
    /// 0 when neither the page nor the byte probe could tell.
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePreview {
    /// The image's final URL, https only.
    pub url: String,
    /// 0 when the probe couldn't tell.
    pub width: u32,
    pub height: u32,
    /// An animated-format file (content type `image/gif` or `GIF` magic): the UI gives those a
    /// tighter box than a still.
    pub gif: bool,
}

/// The TS's constants. Production uses [`Limits::ELECTRON`]; only this crate's tests change them.
#[derive(Debug, Clone)]
pub(crate) struct Limits {
    pub html: usize,
    pub probe: usize,
    pub oembed: usize,
    pub timeout: Duration,
    pub max_redirects: usize,
    pub max_concurrent: usize,
    pub ok_ttl: Duration,
    pub fail_ttl: Duration,
    pub cache_max: usize,
}

impl Limits {
    pub(crate) const ELECTRON: Limits = Limits {
        html: 3 * 1024 * 1024,
        probe: 64 * 1024,
        oembed: 64 * 1024,
        timeout: Duration::from_secs(8),
        max_redirects: 5,
        max_concurrent: 4,
        ok_ttl: Duration::from_secs(30 * 60),
        fail_ttl: Duration::from_secs(5 * 60),
        cache_max: 500,
    };
}

const ACCEPT_PAGE: &str = "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5";
/// Everything a page preview needs is in `<head>`; reading stops there.
const HEAD_END: &[u8] = b"</head>";

/// The link-preview service. Create one per app (it holds the cache) and share it; cloning is
/// cheap and shares the cache.
#[derive(Clone)]
pub struct LinkPreviews {
    inner: Arc<Inner>,
}

struct Inner {
    http: HttpClient,
    user_agent: String,
    limits: Limits,
    slots: Semaphore,
    state: Mutex<State>,
}

type Outcome = Option<LinkPreview>;

#[derive(Default)]
struct State {
    cache: HashMap<String, Cached>,
    /// Cache keys in insertion order (a JS `Map`'s order: replacing a value keeps its place).
    order: VecDeque<String>,
    /// Unfurls under way: later callers for the same URL wait on the first one's result.
    inflight: HashMap<String, watch::Receiver<Option<Outcome>>>,
}

struct Cached {
    expires: Instant,
    value: Outcome,
}

impl LinkPreviews {
    /// `app_version` goes into the User-Agent exactly as Electron sends it:
    /// `Mozilla/5.0 (compatible; Decibell/<version>; +https://github.com/sunkhan/decibell)`.
    /// The production guard (public addresses only) is the only one this can build.
    pub fn new(app_version: &str) -> Self {
        Self::with(HttpClient::new(), app_version, Limits::ELECTRON)
    }

    fn with(http: HttpClient, app_version: &str, limits: Limits) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                user_agent: format!(
                    "Mozilla/5.0 (compatible; Decibell/{app_version}; +https://github.com/sunkhan/decibell)"
                ),
                slots: Semaphore::new(limits.max_concurrent),
                limits,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// Test builds only: a client with the loopback relaxation / scripted DNS / test CA, and
    /// shorter limits.
    #[cfg(test)]
    pub(crate) fn for_tests(http: HttpClient, limits: Limits) -> Self {
        Self::with(http, "0.0.0-test", limits)
    }

    /// Unfurl `url` (as typed in the message; it is also the cache key). `None` = nothing to
    /// show: unreachable, refused by the guard, no usable metadata, or a URL over 2048 characters.
    /// Never fails otherwise; failures are cached for 5 minutes like empty results.
    ///
    /// Must be polled on a tokio runtime: the unfurl itself runs as its own task, so it finishes
    /// (and fills the cache) even if this caller goes away, and concurrent callers share it.
    pub async fn fetch(&self, url: &str) -> Option<LinkPreview> {
        if js::len16(url) > URL_MAX {
            return None;
        }
        let mut rx = {
            let mut st = self.inner.lock();
            if let Some(hit) = st.cache.get(url) {
                if hit.expires > Instant::now() {
                    return hit.value.clone();
                }
            }
            match st.inflight.get(url) {
                Some(rx) => rx.clone(),
                None => {
                    let (tx, rx) = watch::channel(None);
                    st.inflight.insert(url.to_owned(), rx.clone());
                    let inner = Arc::clone(&self.inner);
                    let url = url.to_owned();
                    tokio::spawn(async move {
                        let guard = InflightGuard {
                            inner: &inner,
                            url: &url,
                        };
                        let value = {
                            let _slot =
                                inner.slots.acquire().await.expect("semaphore never closed");
                            inner.unfurl(&url).await.unwrap_or_else(|e| {
                                log::debug!("link preview: {e}");
                                None
                            })
                        };
                        guard.settle(value.clone());
                        let _ = tx.send(Some(value));
                    });
                    rx
                }
            }
        };
        let out = match rx.wait_for(Option::is_some).await {
            Ok(v) => v.clone().flatten(),
            Err(_) => None,
        };
        out
    }

    /// Cached entries (expired ones included until evicted): for tests and diagnostics.
    pub fn cached_len(&self) -> usize {
        self.inner.lock().cache.len()
    }
}

/// Removes the in-flight entry however the unfurl task ends, so a panic can't leave later callers
/// waiting on a result that never comes.
struct InflightGuard<'a> {
    inner: &'a Inner,
    url: &'a str,
}

impl InflightGuard<'_> {
    /// `inflight.delete(url); remember(url, value)`.
    fn settle(self, value: Outcome) {
        let mut st = self.inner.lock();
        st.inflight.remove(self.url);
        self.inner.remember(&mut st, self.url, value);
        drop(st);
        std::mem::forget(self);
    }
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.inner.lock().inflight.remove(self.url);
    }
}

/// Which request of an unfurl this is: decides the cap, the Accept header, and which bodies are
/// worth reading at all.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Page,
    Probe,
    OEmbed,
}

struct Fetched {
    url: String,
    content_type: String,
    bytes: Vec<u8>,
}

enum Hop {
    Done(Option<Fetched>),
    Redirect(String),
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `remember`: at 500 entries, drop the oldest quarter, then store (30 min for a preview, 5 for
    /// nothing).
    fn remember(&self, st: &mut State, url: &str, value: Outcome) {
        if st.cache.len() >= self.limits.cache_max {
            for _ in 0..(self.limits.cache_max / 4).max(1) {
                let Some(k) = st.order.pop_front() else { break };
                st.cache.remove(&k);
            }
        }
        let ttl = if value.is_some() {
            self.limits.ok_ttl
        } else {
            self.limits.fail_ttl
        };
        let entry = Cached {
            expires: Instant::now() + ttl,
            value,
        };
        if let Some(slot) = st.cache.get_mut(url) {
            *slot = entry;
        } else {
            st.cache.insert(url.to_owned(), entry);
            st.order.push_back(url.to_owned());
        }
    }

    /// `fetchBounded`: GET with manual redirects (each hop re-checked by the guard inside the
    /// client), 8 s per hop, a 2xx body read within the cap. `None` = a non-2xx answer, a redirect
    /// without a Location, or too many redirects.
    async fn fetch_bounded(&self, start: Url, want: Want) -> Result<Option<Fetched>, Error> {
        let mut current = start;
        for _ in 0..=self.limits.max_redirects {
            match tokio::time::timeout(self.limits.timeout, self.hop(&current, want)).await {
                Err(_) => return Err(Error::Timeout),
                Ok(r) => match r? {
                    Hop::Done(f) => return Ok(f),
                    Hop::Redirect(location) => {
                        current = current.join(&location).map_err(|_| Error::InvalidUrl)?;
                    }
                },
            }
        }
        Ok(None)
    }

    async fn hop(&self, url: &Url, want: Want) -> Result<Hop, Error> {
        let (limit, accept, stop_at) = match want {
            Want::Page => (self.limits.html, ACCEPT_PAGE, Some(HEAD_END)),
            Want::Probe => (self.limits.probe, "image/*", None),
            Want::OEmbed => (self.limits.oembed, "application/json", None),
        };
        let headers = [
            ("User-Agent", self.user_agent.as_str()),
            ("Accept", accept),
            ("Accept-Language", "en,*;q=0.5"),
            ("Accept-Encoding", "gzip, deflate, br"),
        ];
        let res = self.http.get(url, &headers, true).await?;
        if (300..400).contains(&res.status) {
            return Ok(match res.header("location").filter(|l| !l.is_empty()) {
                Some(location) => Hop::Redirect(location),
                None => Hop::Done(None),
            });
        }
        if !(200..=299).contains(&res.status) {
            return Ok(Hop::Done(None));
        }
        let content_type = res.content_type();
        let image = content_type.starts_with("image/");
        // The TS reads every 2xx body up to its cap, then decides. A body that can't change the
        // result isn't read here (a video or PDF link costs 3 MiB there), and an image stops once
        // its header has given its size: the same answer, as the probe of a prefix is the probe of
        // the whole once it succeeds.
        let useful = match want {
            Want::Page => image || is_html(&content_type),
            Want::Probe => image,
            Want::OEmbed => content_type.contains("json"),
        };
        let bytes = if useful {
            let probing = image && want != Want::OEmbed;
            res.read_body(limit, stop_at, |b| probing && probe_image_dims(b).is_some())
                .await?
        } else {
            Vec::new()
        };
        Ok(Hop::Done(Some(Fetched {
            url: url.to_string(),
            content_type,
            bytes,
        })))
    }

    /// `probeImage`: the declared-size fallback; 0 × 0 when anything fails.
    async fn probe_image(&self, url: &str) -> (u32, u32) {
        let Ok(u) = Url::parse(url) else {
            return (0, 0);
        };
        match self.fetch_bounded(u, Want::Probe).await {
            Ok(Some(got)) if got.content_type.starts_with("image/") => {
                probe_image_dims(&got.bytes).unwrap_or((0, 0))
            }
            _ => (0, 0),
        }
    }

    /// `unfurl`.
    async fn unfurl(&self, url: &str) -> Result<Outcome, Error> {
        let start = Url::parse(url).map_err(|_| Error::InvalidUrl)?;
        let Some(got) = self.fetch_bounded(start, Want::Page).await? else {
            return Ok(None);
        };

        if got.content_type.starts_with("image/") {
            let Some(image_url) = resolve_image(Some(&got.url), &got.url) else {
                return Ok(None);
            };
            let (width, height) = probe_image_dims(&got.bytes).unwrap_or((0, 0));
            let gif = got.content_type.starts_with("image/gif") || got.bytes.starts_with(b"GIF");
            return Ok(Some(LinkPreview::Image(ImagePreview {
                url: image_url,
                width,
                height,
                gif,
            })));
        }
        if !is_html(&got.content_type) {
            return Ok(None);
        }

        let page = parse_page(&decode_html(&got.bytes, &got.content_type));
        let final_url = got.url;
        let meta = &page.meta;

        let og_title = pick(meta, &["og:title", "twitter:title"]).map(str::to_owned);
        let mut title = og_title.clone().or_else(|| page.title.clone());
        let mut description = pick(
            meta,
            &["og:description", "twitter:description", "description"],
        )
        .map(str::to_owned);
        let mut site_name = pick(meta, &["og:site_name"]).map(str::to_owned);
        let mut image_url = resolve_image(
            pick(
                meta,
                &[
                    "og:image:secure_url",
                    "og:image",
                    "og:image:url",
                    "twitter:image",
                    "twitter:image:src",
                ],
            ),
            &final_url,
        );
        let mut image_w = to_int(meta.get("og:image:width").map(String::as_str));
        let mut image_h = to_int(meta.get("og:image:height").map(String::as_str));
        let card = pick(meta, &["twitter:card"]).unwrap_or("").to_lowercase();
        let og_type = pick(meta, &["og:type"]).unwrap_or("").to_lowercase();
        let mut large_image = card == "summary_large_image"
            || card == "player"
            || og_type.starts_with("video")
            || meta.contains_key("og:video")
            || meta.contains_key("og:video:url");

        // oEmbed fills what the page's own tags left out.
        if let Some(oembed) = page.oembed.as_deref() {
            if og_title.is_none() || image_url.is_none() {
                if let Some(j) = self.oembed(&final_url, oembed).await {
                    let s = |k: &str| j.get(k).and_then(Value::as_str);
                    if og_title.is_none() {
                        if let Some(t) = s("title").filter(|t| !t.is_empty()) {
                            title = Some(t.to_owned());
                        }
                    }
                    if site_name.is_none() {
                        site_name = s("provider_name").map(str::to_owned);
                    }
                    if description.is_none() {
                        if let Some(a) = s("author_name").filter(|a| !a.is_empty()) {
                            description = Some(format!("by {a}"));
                        }
                    }
                    if image_url.is_none() {
                        image_url = resolve_image(s("thumbnail_url"), &final_url);
                        image_w = j
                            .get("thumbnail_width")
                            .and_then(Value::as_f64)
                            .map_or(0, to_int_number);
                        image_h = j
                            .get("thumbnail_height")
                            .and_then(Value::as_f64)
                            .map_or(0, to_int_number);
                    }
                    if matches!(s("type"), Some("video" | "rich")) {
                        large_image = true;
                    }
                }
            }
        }

        if let Some(u) = &image_url {
            if image_w == 0 || image_h == 0 {
                (image_w, image_h) = self.probe_image(u).await;
            }
        }

        if site_name.as_deref().is_none_or(str::is_empty) {
            site_name = Url::parse(&final_url).ok().and_then(|u| {
                u.host_str()
                    .map(|h| h.strip_prefix("www.").unwrap_or(h).to_owned())
            });
        }

        let title = title
            .filter(|t| !t.is_empty())
            .map(|t| clean(&t, TITLE_MAX));
        let description = description
            .filter(|d| !d.is_empty())
            .map(|d| clean(&d, DESCRIPTION_MAX));
        let site_name = site_name
            .filter(|s| !s.is_empty())
            .map(|s| clean(&s, SITE_NAME_MAX));
        let blank = |v: &Option<String>| v.as_deref().is_none_or(str::is_empty);
        if blank(&title) && blank(&description) && image_url.is_none() {
            return Ok(None);
        }

        Ok(Some(LinkPreview::Site(SitePreview {
            url: final_url,
            site_name,
            title,
            description,
            image: image_url.map(|url| LinkPreviewImage {
                url,
                width: image_w,
                height: image_h,
            }),
            large_image,
            color: valid_color(pick(meta, &["theme-color"])),
        })))
    }

    /// The oEmbed answer as a JSON object, or `None` (bad URL, failed fetch, not JSON). Resolved
    /// against the page's URL.
    async fn oembed(&self, page_url: &str, href: &str) -> Option<serde_json::Map<String, Value>> {
        let u = Url::parse(page_url).ok()?.join(href).ok()?;
        let o = self.fetch_bounded(u, Want::OEmbed).await.ok()??;
        if !o.content_type.contains("json") {
            return None;
        }
        // TextDecoder("utf-8"): malformed bytes → U+FFFD, a leading BOM dropped.
        let text = String::from_utf8_lossy(&o.bytes);
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
        match serde_json::from_str(text) {
            Ok(Value::Object(j)) => Some(j),
            _ => None,
        }
    }
}

#[cfg(test)]
pub(crate) mod test_access {
    //! Internals the vector and loopback tests compare against the TypeScript.
    pub(crate) use super::extract::{
        clean, decode_entities, decode_html, parse_attrs, parse_page, resolve_image, to_int,
        valid_color, PageMeta,
    };
    pub(crate) use super::probe::probe_image_dims;
    pub(crate) use super::Limits;
}
