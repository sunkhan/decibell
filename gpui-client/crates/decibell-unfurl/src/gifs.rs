//! GIF search for the picker's GIFs tab (port of `electron/main/gifs.ts`). Two providers, the two
//! Discord moved to when Google shut the Tenor API down on 2026-06-30 (never Tenor):
//!
//! - KLIPY: `https://api.klipy.com/api/v1/{key}/gifs/{search|trending}`, page-numbered; items carry
//!   `file.{hd,md,sm,xs}.gif`.
//! - GIPHY: `https://api.giphy.com/v1/gifs/{search|trending}`, offset-paged; items carry
//!   `images.{original,fixed_width,…}`.
//!
//! The key stays in this process. Content filter: KLIPY `content_filter=low` / GIPHY `rating=r`
//! (owner's call: the widest catalogue short of unfiltered), `off` / no rating when the Privacy
//! tab's "Unfiltered GIF search" is on. Sending a GIF is sending its https URL as message text.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::http::HttpClient;
use crate::js;
use crate::Error;

const PAGE_SIZE: usize = 24;
const QUERY_MAX: usize = 200;
const TIMEOUT: Duration = Duration::from_secs(10);
const KLIPY_CONTENT_FILTER: &str = "low";
const GIPHY_RATING: &str = "r";
/// `fetch()` follows at most 20 redirects.
const MAX_REDIRECTS: usize = 20;
/// `res.text()` had no cap; a provider page is ~50–100 KB. A cap that no real answer reaches.
const BODY_MAX: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GifProvider {
    Klipy,
    Giphy,
}

impl GifProvider {
    /// The attribution name: the search placeholder reads "Search KLIPY", the footer
    /// "Powered by GIPHY" (both vendors' terms).
    pub fn label(self) -> &'static str {
        match self {
            GifProvider::Klipy => "KLIPY",
            GifProvider::Giphy => "GIPHY",
        }
    }

    fn parse(v: &str) -> Option<Self> {
        match v {
            "klipy" => Some(GifProvider::Klipy),
            "giphy" => Some(GifProvider::Giphy),
            _ => None,
        }
    }
}

/// Which provider, with which key. `Debug` never prints the key.
#[derive(Clone, PartialEq, Eq)]
pub struct GifConfig {
    provider: GifProvider,
    key: String,
}

impl std::fmt::Debug for GifConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GifConfig")
            .field("provider", &self.provider)
            .field("key", &"…")
            .finish()
    }
}

impl GifConfig {
    pub fn new(provider: GifProvider, key: impl Into<String>) -> Self {
        Self {
            provider,
            key: key.into(),
        }
    }

    pub fn provider(&self) -> GifProvider {
        self.provider
    }

    /// `loadConfig`: the `GIF_API_KEY` environment variable first (provider from
    /// `GIF_API_PROVIDER`, default KLIPY), else the first readable `gifs.json` among `candidates`
    /// (Electron looked in `process.resourcesPath` then `<app>/resources`). The app calls this
    /// once at startup; the result never changes for the process (Electron cached it too).
    pub fn load(candidates: &[PathBuf]) -> Option<GifConfig> {
        Self::load_from(
            std::env::var("GIF_API_KEY").ok(),
            std::env::var("GIF_API_PROVIDER").ok(),
            candidates,
        )
    }

    fn load_from(
        env_key: Option<String>,
        env_provider: Option<String>,
        candidates: &[PathBuf],
    ) -> Option<GifConfig> {
        if let Some(key) = env_key.as_deref().map(js::trim).filter(|k| !k.is_empty()) {
            let provider = env_provider
                .as_deref()
                .and_then(|p| GifProvider::parse(&js::trim(p).to_lowercase()))
                .unwrap_or(GifProvider::Klipy);
            return Some(GifConfig {
                provider,
                key: key.to_owned(),
            });
        }
        for p in candidates {
            match Self::from_file(p) {
                Ok(Some(c)) => return Some(c),
                Ok(None) => log::warn!(
                    "[gifs] {}: expected {{\"provider\":\"klipy\"|\"giphy\",\"key\":\"…\"}}",
                    p.display()
                ),
                Err(()) => {} // absent or unreadable → the next location
            }
        }
        None
    }

    /// `Err` = unreadable or not JSON (silently skipped); `Ok(None)` = JSON of the wrong shape
    /// (warned about).
    fn from_file(p: &Path) -> Result<Option<GifConfig>, ()> {
        let bytes = std::fs::read(p).map_err(|_| ())?;
        Self::from_json(&bytes)
    }

    fn from_json(bytes: &[u8]) -> Result<Option<GifConfig>, ()> {
        let raw = String::from_utf8_lossy(bytes);
        let raw = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
        let j: Value = serde_json::from_str(raw).map_err(|_| ())?;
        // `j.provider` on null throws inside the TS's try: skipped like an unreadable file.
        if j.is_null() {
            return Err(());
        }
        let provider = j
            .get("provider")
            .and_then(Value::as_str)
            .and_then(|p| GifProvider::parse(&p.to_lowercase()));
        let key = j
            .get("key")
            .and_then(Value::as_str)
            .map(js::trim)
            .filter(|k| !k.is_empty());
        Ok(match (provider, key) {
            (Some(provider), Some(key)) => Some(GifConfig {
                provider,
                key: key.to_owned(),
            }),
            _ => None,
        })
    }
}

/// `decibell:gifs:status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GifStatus {
    pub configured: bool,
    pub provider: Option<GifProvider>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GifResult {
    pub id: String,
    pub title: String,
    /// Full-size GIF: what gets sent as the message text.
    pub url: String,
    /// As the provider reports it: 0 when absent; a fraction or a huge value passes through, as
    /// in the TS (`num()`).
    pub width: f64,
    pub height: f64,
    /// Smaller GIF for the picker grid.
    pub preview: String,
    pub preview_width: f64,
    pub preview_height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GifPage {
    pub results: Vec<GifResult>,
    /// Opaque cursor for the next page (a page number or an offset); `None` when exhausted.
    pub next: Option<String>,
}

/// The GIF search service.
pub struct Gifs {
    config: Option<GifConfig>,
    http: OnceLock<Arc<HttpClient>>,
    timeout: Duration,
    #[cfg(test)]
    test: Option<(Arc<HttpClient>, String)>,
}

impl Gifs {
    /// `config` from [`GifConfig::load`]; `None` makes every search answer
    /// [`Error::NotConfigured`] and the picker explain how to set a key.
    pub fn new(config: Option<GifConfig>) -> Self {
        Self {
            config,
            http: OnceLock::new(),
            timeout: TIMEOUT,
            #[cfg(test)]
            test: None,
        }
    }

    /// Test builds only: send every request to `origin` (a loopback server) instead of the
    /// provider's host, through a client with the loopback relaxation.
    #[cfg(test)]
    pub(crate) fn for_tests(
        config: GifConfig,
        http: HttpClient,
        origin: &str,
        timeout: Duration,
    ) -> Self {
        Self {
            config: Some(config),
            http: OnceLock::new(),
            timeout,
            test: Some((Arc::new(http), origin.to_owned())),
        }
    }

    pub fn status(&self) -> GifStatus {
        GifStatus {
            configured: self.config.is_some(),
            provider: self.config.as_ref().map(|c| c.provider),
        }
    }

    /// Search (`query` non-empty) or the trending feed (empty). `pos` is the previous page's
    /// [`GifPage::next`]; `locale` the UI language (`navigator.language`, e.g. `en-US`);
    /// `unfiltered` the Privacy tab's switch. The query is trimmed and cut to 200 characters, as
    /// the IPC handler did. Errors display as the TS's messages (the picker shows them), except
    /// [`Error::NotConfigured`], which the picker answers with its set-up notice.
    pub async fn search(
        &self,
        query: &str,
        pos: Option<&str>,
        locale: Option<&str>,
        unfiltered: bool,
    ) -> Result<GifPage, Error> {
        let c = self.config.as_ref().ok_or(Error::NotConfigured)?;
        let (q, pos) = sanitize(query, pos);
        let req = build_request(c, &q, pos, locale, unfiltered);
        let (status, json) = self.get_json(&req.url).await?;
        match c.provider {
            GifProvider::Klipy => parse_klipy(status, &json, req.cursor),
            GifProvider::Giphy => parse_giphy(status, &json, req.cursor),
        }
    }

    fn http(&self) -> Arc<HttpClient> {
        #[cfg(test)]
        if let Some((h, _)) = &self.test {
            return Arc::clone(h);
        }
        Arc::clone(self.http.get_or_init(|| Arc::new(HttpClient::new())))
    }

    /// `getJson`: GET with fetch()'s redirect rules, 10 s for the whole exchange including the
    /// body, the body as UTF-8 text parsed as JSON (`Value::Null` when it isn't JSON).
    async fn get_json(&self, url: &Url) -> Result<(u16, Value), Error> {
        #[allow(unused_mut)]
        let mut url = url.clone();
        #[cfg(test)]
        if let Some((_, origin)) = &self.test {
            let target = &url[url::Position::BeforePath..];
            url = Url::parse(&format!("{origin}{target}")).expect("test origin");
        }
        let http = self.http();
        let work = async move {
            // What Electron's fetch() sent (undici, Node 20): these headers, lowercase names.
            let headers = [
                ("accept", "application/json"),
                ("accept-language", "*"),
                ("sec-fetch-mode", "cors"),
                ("user-agent", "node"),
                ("accept-encoding", "gzip, deflate"),
            ];
            let mut current = url;
            for _ in 0..=MAX_REDIRECTS {
                let res = http.get(&current, &headers, false).await?;
                if matches!(res.status, 301 | 302 | 303 | 307 | 308) {
                    if let Some(location) = res.header("location") {
                        current = current
                            .join(&location)
                            .map_err(|e| Error::Network(Box::new(e)))?;
                        continue;
                    }
                }
                let status = res.status;
                let body = res.read_body(BODY_MAX, None, |_| false).await?;
                let text = String::from_utf8_lossy(&body);
                let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
                let json = serde_json::from_str(text).unwrap_or(Value::Null);
                return Ok((status, json));
            }
            Err(Error::Network("redirect count exceeded".into()))
        };
        match tokio::time::timeout(self.timeout, work).await {
            Err(_) => Err(Error::Timeout),
            // fetch() reports a refused lookup or an unusable redirect as "fetch failed" too.
            Ok(Err(Error::Refused(r))) => Err(Error::Network(Box::new(r))),
            Ok(Err(Error::InvalidUrl)) => Err(Error::Network("invalid URL".into())),
            Ok(r) => r,
        }
    }
}

/// What the `decibell:gifs:search` handler did to its arguments: the query trimmed and cut to 200
/// UTF-16 units, an empty cursor treated as none.
pub(crate) fn sanitize<'a>(query: &str, pos: Option<&'a str>) -> (String, Option<&'a str>) {
    (
        js::slice16(js::trim(query), QUERY_MAX),
        pos.filter(|p| !p.is_empty()),
    )
}

/// The request for one search, and the cursor it was built from (the parser needs it).
pub(crate) struct Request {
    pub(crate) url: Url,
    pub(crate) cursor: f64,
}

/// `parseInt(pos, 10)` when it's a non-negative integer, else `fallback`.
pub(crate) fn cursor_int(pos: Option<&str>, fallback: f64) -> f64 {
    match pos.and_then(js::parse_int) {
        Some(n) if n.is_finite() && n.fract() == 0.0 && n >= 0.0 => n + 0.0, // -0 → 0
        _ => fallback,
    }
}

/// `/^([A-Za-z]{2,3})(?:[-_]([A-Za-z]{2}))?/`: "en-US" → ("en", "us"). Note "zh-Hans-CN" → region
/// "ha", as in the TS.
pub(crate) fn locale_parts(locale: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(l) = locale else { return (None, None) };
    let b = l.as_bytes();
    let lang_len = b
        .iter()
        .take(3)
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    if lang_len < 2 {
        return (None, None);
    }
    let lang = l[..lang_len].to_ascii_lowercase();
    let rest = &b[lang_len..];
    let region = (rest.len() >= 3
        && (rest[0] == b'-' || rest[0] == b'_')
        && rest[1].is_ascii_alphabetic()
        && rest[2].is_ascii_alphabetic())
    .then(|| l[lang_len + 1..lang_len + 3].to_ascii_lowercase());
    (Some(lang), region)
}

pub(crate) fn build_request(
    c: &GifConfig,
    q: &str,
    pos: Option<&str>,
    locale: Option<&str>,
    unfiltered: bool,
) -> Request {
    let kind = if q.is_empty() { "trending" } else { "search" };
    match c.provider {
        GifProvider::Klipy => {
            let page = cursor_int(pos, 1.0).max(1.0);
            let mut u = Url::parse(&format!(
                "https://api.klipy.com/api/v1/{}/gifs/{kind}",
                js::encode_uri_component(&c.key)
            ))
            .expect("static URL");
            {
                let mut qp = u.query_pairs_mut();
                qp.append_pair("page", &js::number_to_string(page));
                qp.append_pair("per_page", &PAGE_SIZE.to_string());
                qp.append_pair(
                    "content_filter",
                    if unfiltered {
                        "off"
                    } else {
                        KLIPY_CONTENT_FILTER
                    },
                );
                qp.append_pair("format_filter", "gif");
                if !q.is_empty() {
                    qp.append_pair("q", q);
                }
                if let (_, Some(region)) = locale_parts(locale) {
                    qp.append_pair("locale", &region);
                }
            }
            Request {
                url: u,
                cursor: page,
            }
        }
        GifProvider::Giphy => {
            let offset = cursor_int(pos, 0.0);
            let mut u =
                Url::parse(&format!("https://api.giphy.com/v1/gifs/{kind}")).expect("static URL");
            {
                let mut qp = u.query_pairs_mut();
                qp.append_pair("api_key", &c.key);
                qp.append_pair("limit", &PAGE_SIZE.to_string());
                qp.append_pair("offset", &js::number_to_string(offset));
                if !unfiltered {
                    qp.append_pair("rating", GIPHY_RATING);
                }
                if !q.is_empty() {
                    qp.append_pair("q", &js::slice16(q, 50)); // GIPHY caps q at 50 characters
                }
                if let (Some(lang), _) = locale_parts(locale) {
                    if !q.is_empty() {
                        qp.append_pair("lang", &lang);
                    }
                }
            }
            Request {
                url: u,
                cursor: offset,
            }
        }
    }
}

/// `httpsUrl`: a string starting with `https://` (case-sensitive), else nothing.
fn https_url(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| s.starts_with("https://"))
        .map(str::to_owned)
}

/// `num`: a positive finite number (a numeric string through `parseInt`), else 0.
fn num(v: Option<&Value>) -> f64 {
    let n = match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => js::parse_int(s),
        _ => None,
    };
    n.filter(|n| n.is_finite() && *n > 0.0).unwrap_or(0.0)
}

/// `{url, width, height}` of one rendition, when its URL is https.
fn rendition(f: Option<&Value>) -> Option<(String, f64, f64)> {
    // `f ? httpsUrl(f.url) : null`: a non-object rendition has no `.url`.
    let f = f?;
    let url = https_url(f.get("url"))?;
    Some((url, num(f.get("width")), num(f.get("height"))))
}

/// `a?.b`: a property of an object, or nothing (null, a string, a number…).
fn field<'a>(v: Option<&'a Value>, k: &str) -> Option<&'a Value> {
    v.and_then(|v| v.get(k))
}

fn result(
    id: String,
    title: String,
    full: (String, f64, f64),
    preview: (String, f64, f64),
) -> GifResult {
    GifResult {
        id,
        title,
        url: full.0,
        width: full.1,
        height: full.2,
        preview: preview.0,
        preview_width: preview.1,
        preview_height: preview.2,
    }
}

/// `normalizeKlipy`. A `null` item throws in the TS (`item.file`), failing the whole search with
/// the TypeError's message; that is kept.
fn normalize_klipy(item: &Value) -> Result<Option<GifResult>, Error> {
    if item.is_null() {
        return Err(Error::Provider(
            "Cannot read properties of null (reading 'file')".into(),
        ));
    }
    let file = item.get("file");
    let gif = |size: &str| field(field(file, size), "gif");
    let full = rendition(gif("hd")).or_else(|| rendition(gif("md")));
    let preview = rendition(gif("sm"))
        .or_else(|| rendition(gif("xs")))
        .or_else(|| full.clone());
    let id = match item.get("id") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => n.as_f64().map(js::number_to_string),
        _ => match item.get("slug") {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        },
    };
    let (Some(id), Some(full), Some(preview)) = (id.filter(|i| !i.is_empty()), full, preview)
    else {
        return Ok(None);
    };
    let title = item
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Ok(Some(result(id, title, full, preview)))
}

pub(crate) fn parse_klipy(status: u16, json: &Value, page: f64) -> Result<GifPage, Error> {
    if json.get("result") != Some(&Value::Bool(true)) {
        let message = match field(json.get("errors"), "message") {
            Some(Value::Array(msgs)) => msgs
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; "),
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        return Err(Error::Provider(if message.is_empty() {
            format!("KLIPY returned HTTP {status}")
        } else {
            message
        }));
    }
    let data = json.get("data");
    let items = match field(data, "data") {
        Some(Value::Array(a)) => a.as_slice(),
        _ => &[],
    };
    let mut results = Vec::with_capacity(items.len());
    for item in items {
        if let Some(r) = normalize_klipy(item)? {
            results.push(r);
        }
    }
    let next = (field(data, "has_next") == Some(&Value::Bool(true)))
        .then(|| js::number_to_string(page + 1.0));
    Ok(GifPage { results, next })
}

/// `normalizeGiphy`. A `null` item throws in the TS (`item.images`); kept.
fn normalize_giphy(item: &Value) -> Result<Option<GifResult>, Error> {
    if item.is_null() {
        return Err(Error::Provider(
            "Cannot read properties of null (reading 'images')".into(),
        ));
    }
    // `item.images ?? {}`
    let images = item.get("images").filter(|v| !v.is_null());
    let im = |k: &str| field(images, k);
    let full = rendition(im("original")).or_else(|| rendition(im("downsized")));
    let preview = rendition(im("fixed_width"))
        .or_else(|| rendition(im("downsized")))
        .or_else(|| rendition(im("preview_gif")))
        .or_else(|| full.clone());
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|i| !i.is_empty())
        .map(str::to_owned);
    let (Some(id), Some(full), Some(preview)) = (id, full, preview) else {
        return Ok(None);
    };
    let nonempty = |k: &str| {
        item.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let title = nonempty("title")
        .or_else(|| nonempty("alt_text"))
        .unwrap_or("")
        .to_owned();
    Ok(Some(result(id, title, full, preview)))
}

pub(crate) fn parse_giphy(status: u16, json: &Value, offset: f64) -> Result<GifPage, Error> {
    let meta = json.get("meta");
    let meta_status = match num(field(meta, "status")) {
        0.0 => f64::from(status),
        n => n,
    };
    let data = match json.get("data") {
        Some(Value::Array(a)) if (200.0..300.0).contains(&meta_status) => a,
        _ => {
            return Err(Error::Provider(
                match field(meta, "msg").and_then(Value::as_str) {
                    Some(msg) if !msg.is_empty() => format!("GIPHY: {msg}"),
                    _ => format!("GIPHY returned HTTP {status}"),
                },
            ))
        }
    };
    let mut results = Vec::with_capacity(data.len());
    for item in data {
        if let Some(r) = normalize_giphy(item)? {
            results.push(r);
        }
    }
    let total = num(field(json.get("pagination"), "total_count"));
    let next_offset = offset + data.len() as f64;
    let exhausted = data.len() < PAGE_SIZE || (total > 0.0 && next_offset >= total);
    Ok(GifPage {
        results,
        next: (!exhausted).then(|| js::number_to_string(next_offset)),
    })
}

#[cfg(test)]
pub(crate) mod test_access {
    //! What the vector tests drive directly.
    use super::GifConfig;
    pub(crate) use super::{
        build_request, cursor_int, locale_parts, parse_giphy, parse_klipy, sanitize,
    };

    pub(crate) fn config_from(
        env_key: Option<&str>,
        env_provider: Option<&str>,
        files: &[std::path::PathBuf],
    ) -> Option<GifConfig> {
        GifConfig::load_from(
            env_key.map(str::to_owned),
            env_provider.map(str::to_owned),
            files,
        )
    }

    pub(crate) fn key(c: &GifConfig) -> &str {
        &c.key
    }
}
