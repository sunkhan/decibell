//! GIF search for the picker's GIFs tab: `decibell-unfurl`'s [`Gifs`] (KLIPY or GIPHY, never
//! Tenor) run on the core's runtime, never on the UI thread, behind [`GifSource`] so the screens
//! and the self-tests can use a canned provider.
//!
//! **Where the key comes from** (HANDOFF §5.9a, mirrored for this client): the `GIF_API_KEY`
//! environment variable (provider from `GIF_API_PROVIDER`, default KLIPY) first; else
//! `resources/gifs.json` beside the executable — what a packaged install ships, written by the
//! release workflow from the same repository secret as Electron's; else, in a dev checkout,
//! `electron-client/resources/gifs.json` found by walking up from the executable (the file the
//! Electron client's dev setup already uses, gitignored). The key never leaves the process except
//! in the provider's URL, and is never printed (`GifConfig`'s `Debug` hides it).

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use decibell_unfurl::{GifConfig, GifPage, GifStatus, Gifs};
use gpui::{App, Global, Task};

/// One search (or the trending feed for an empty query), as the picker asks for it.
pub trait GifSource: 'static {
    fn status(&self) -> GifStatus;
    /// `Err` carries the text the picker shows under "GIF search failed" ("not-configured" when
    /// there is no key).
    fn search(&self, query: String, pos: Option<String>, locale: String, unfiltered: bool, cx: &mut App) -> Task<Result<GifPage, String>>;
}

/// The places a `gifs.json` is looked for, in order.
pub fn key_file_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(dir) = exe.parent() {
            out.push(dir.join("resources").join("gifs.json"));
            // A dev checkout: target/<profile>/decibell → gpui-client → the repository.
            for ancestor in dir.ancestors().skip(1).take(5) {
                let candidate = ancestor.join("electron-client").join("resources").join("gifs.json");
                if candidate.parent().is_some_and(|p| p.is_dir()) {
                    out.push(candidate);
                    break;
                }
            }
        }
    }
    out
}

/// The app's provider: the key loaded once per process (Electron cached it too).
pub struct CoreGifs;

fn gifs() -> Arc<Gifs> {
    static GIFS: OnceLock<Arc<Gifs>> = OnceLock::new();
    GIFS.get_or_init(|| Arc::new(Gifs::new(GifConfig::load(&key_file_candidates())))).clone()
}

impl GifSource for CoreGifs {
    fn status(&self) -> GifStatus {
        gifs().status()
    }

    fn search(&self, query: String, pos: Option<String>, locale: String, unfiltered: bool, cx: &mut App) -> Task<Result<GifPage, String>> {
        if !decibell_model::core::is_running() {
            return Task::ready(Err("Not connected".into()));
        }
        let (tx, rx) = futures::channel::oneshot::channel();
        let gifs = gifs();
        decibell_model::core::spawn_detached(async move {
            let result = gifs.search(&query, pos.as_deref(), Some(&locale), unfiltered).await.map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        cx.spawn(async move |_| rx.await.unwrap_or_else(|_| Err("Not connected".into())))
    }
}

/// Which [`GifSource`] the pickers use (the app's by default).
pub struct GifService(pub Rc<dyn GifSource>);

impl Global for GifService {}

impl GifService {
    pub fn source(cx: &App) -> Rc<dyn GifSource> {
        cx.try_global::<GifService>().map_or_else(|| Rc::new(CoreGifs) as Rc<dyn GifSource>, |s| s.0.clone())
    }

    /// Replace the provider (screens, self-tests).
    pub fn set(cx: &mut App, source: Rc<dyn GifSource>) {
        cx.set_global(GifService(source));
    }
}

/// `navigator.language` from the environment (`tr_TR.UTF-8` → `tr-TR`), `en-US` when unset.
pub fn locale() -> String {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.split(['.', '@']).next().unwrap_or_default().replace('_', "-");
            if !value.is_empty() && value != "C" && value != "POSIX" {
                return value;
            }
        }
    }
    "en-US".into()
}

/// A canned provider: a page of results, or the state a test wants (not configured, an error).
pub struct FixtureGifs {
    pub configured: bool,
    pub pages: Vec<GifPage>,
    pub error: Option<String>,
    /// What was asked for, for the self-tests.
    pub asked: std::cell::RefCell<Vec<(String, Option<String>, bool)>>,
}

impl FixtureGifs {
    /// `count` results per page, `pages` pages; ids `g<page>-<n>`, previews from `preview` (the
    /// same picture for every tile, or none: tiles then stay empty boxes).
    pub fn pages(count: usize, pages: usize, preview: Option<&str>) -> FixtureGifs {
        let pages = (0..pages)
            .map(|page| GifPage {
                results: (0..count)
                    .map(|n| {
                        // Heights vary like a real feed: the masonry places each in the shorter column.
                        let height = [120., 200., 160., 90., 240., 140.][(n + page) % 6];
                        decibell_unfurl::GifResult {
                            id: format!("g{page}-{n}"),
                            title: format!("Fixture GIF {page}-{n}"),
                            url: format!("https://static.klipy.com/fixture/{page}-{n}.gif"),
                            width: 200.,
                            height,
                            preview: preview.map_or_else(String::new, str::to_string),
                            preview_width: 200.,
                            preview_height: height,
                        }
                    })
                    .collect(),
                next: (page + 1 < pages).then(|| (page + 1).to_string()),
            })
            .collect();
        FixtureGifs { configured: true, pages, error: None, asked: Default::default() }
    }
}

impl GifSource for FixtureGifs {
    fn status(&self) -> GifStatus {
        GifStatus { configured: self.configured, provider: self.configured.then_some(decibell_unfurl::GifProvider::Klipy) }
    }

    fn search(&self, query: String, pos: Option<String>, _: String, unfiltered: bool, _: &mut App) -> Task<Result<GifPage, String>> {
        self.asked.borrow_mut().push((query, pos.clone(), unfiltered));
        if !self.configured {
            return Task::ready(Err("not-configured".into()));
        }
        if let Some(error) = &self.error {
            return Task::ready(Err(error.clone()));
        }
        let page = pos.and_then(|p| p.parse::<usize>().ok()).unwrap_or(0);
        Task::ready(Ok(self.pages.get(page).cloned().unwrap_or(GifPage { results: Vec::new(), next: None })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_file_is_looked_for_beside_the_binary_first() {
        let candidates = key_file_candidates();
        assert!(candidates.first().is_some_and(|c| c.ends_with("resources/gifs.json")));
    }
}
