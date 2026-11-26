//! The memo of link unfurls (`stores/linkPreviewStore.ts`), keyed by the URL as typed, over
//! `decibell-unfurl` (which holds the real cache, with its TTLs, and the private-network
//! guard). A row that re-renders or remounts never asks twice for a URL it has seen, and
//! every bubble citing one URL shares one entry. At most [`MAX_ENTRIES`], the oldest
//! forgotten first; entries are only added or replaced.
//!
//! Views call [`LinkPreviewMemo::request`] where they build a row with a link (never from
//! `render`), and only when `Settings::get().link_previews_enabled` is on (the Privacy
//! toggle; invite cards ignore it and resolve against central instead). They subscribe and
//! filter [`PreviewChanged`] by their URL.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use decibell_unfurl::LinkPreview;
use gpui::{App, Context, EventEmitter, SharedString, Task};

pub const MAX_ENTRIES: usize = 400;

#[derive(Debug, Clone)]
pub enum PreviewEntry {
    Loading,
    /// `None`: nothing to show (unreachable, no metadata, refused by the guard).
    Done(Option<Arc<LinkPreview>>),
}

/// The entry of one URL changed.
pub struct PreviewChanged(pub SharedString);

impl EventEmitter<PreviewChanged> for LinkPreviewMemo {}

/// How an unfurl runs: on the core's runtime in the app, a stub in tests.
pub type Unfurler = Rc<dyn Fn(SharedString, &mut App) -> Task<Option<LinkPreview>>>;

pub struct LinkPreviewMemo {
    entries: HashMap<SharedString, PreviewEntry>,
    /// Insertion order (a replaced entry keeps its place, as a JS object key does).
    order: VecDeque<SharedString>,
    unfurler: Unfurler,
    /// The fetches in flight: each lives until its answer.
    inflight: HashMap<SharedString, Task<()>>,
}

impl LinkPreviewMemo {
    pub fn new(unfurler: Unfurler) -> LinkPreviewMemo {
        LinkPreviewMemo { entries: HashMap::new(), order: VecDeque::new(), unfurler, inflight: HashMap::new() }
    }

    pub fn get(&self, url: &str) -> Option<&PreviewEntry> {
        self.entries.get(url)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn put(&mut self, url: SharedString, entry: PreviewEntry) {
        if self.entries.insert(url.clone(), entry).is_none() {
            self.order.push_back(url);
            while self.order.len() > MAX_ENTRIES {
                if let Some(old) = self.order.pop_front() {
                    self.entries.remove(&old);
                    self.inflight.remove(&old);
                }
            }
        }
    }

    /// Unfurl `url` once: a URL loading or done is left alone.
    pub fn request(&mut self, url: SharedString, cx: &mut Context<Self>) {
        if self.entries.contains_key(&url) {
            return;
        }
        self.put(url.clone(), PreviewEntry::Loading);
        let fetch = (self.unfurler)(url.clone(), cx);
        let task = cx.spawn({
            let url = url.clone();
            async move |this, cx| {
                let preview = fetch.await;
                let _ = this.update(cx, |this, cx| {
                    this.inflight.remove(&url);
                    // Forgotten meanwhile (the memo overflowed): nothing to fill.
                    if this.entries.contains_key(&url) {
                        this.put(url.clone(), PreviewEntry::Done(preview.map(Arc::new)));
                        cx.emit(PreviewChanged(url));
                        cx.notify();
                    }
                });
            }
        });
        self.inflight.insert(url.clone(), task);
        cx.emit(PreviewChanged(url));
        cx.notify();
    }

    pub fn reset_for_logout(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.inflight.clear();
    }
}

/// The app's unfurler: `decibell_unfurl::LinkPreviews` (one per process, created on first
/// use) on the core's runtime; the answer comes back to the UI thread. With the core not
/// running the answer is "nothing to show".
pub fn core_unfurler(app_version: &'static str) -> Unfurler {
    let previews: Rc<std::cell::OnceCell<Arc<decibell_unfurl::LinkPreviews>>> = Rc::default();
    Rc::new(move |url, cx| {
        let previews = previews.get_or_init(|| Arc::new(decibell_unfurl::LinkPreviews::new(app_version))).clone();
        let (tx, rx) = futures::channel::oneshot::channel();
        crate::core::spawn_detached(async move {
            let _ = tx.send(previews.fetch(&url).await);
        });
        cx.spawn(async move |_| rx.await.ok().flatten())
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    #[gpui::test]
    fn one_fetch_per_url_and_a_bounded_memo(cx: &mut TestAppContext) {
        let asked: Rc<RefCell<Vec<String>>> = Rc::default();
        let memo = cx.new(|_| {
            let asked = asked.clone();
            LinkPreviewMemo::new(Rc::new(move |url, _| {
                asked.borrow_mut().push(url.to_string());
                Task::ready(None)
            }))
        });
        memo.update(cx, |m, cx| {
            m.request("https://a.test/".into(), cx);
            m.request("https://a.test/".into(), cx);
        });
        assert!(matches!(memo.read_with(cx, |m, _| m.get("https://a.test/").cloned()), Some(PreviewEntry::Loading)));
        cx.run_until_parked();
        assert!(matches!(memo.read_with(cx, |m, _| m.get("https://a.test/").cloned()), Some(PreviewEntry::Done(None))));
        assert_eq!(asked.borrow().len(), 1);
        memo.update(cx, |m, cx| {
            for i in 0..MAX_ENTRIES {
                m.request(format!("https://b.test/{i}").into(), cx);
            }
        });
        cx.run_until_parked();
        memo.read_with(cx, |m, _| {
            assert_eq!(m.len(), MAX_ENTRIES);
            assert!(m.get("https://a.test/").is_none(), "the oldest went first");
            assert!(m.get("https://b.test/0").is_some());
        });
    }
}
