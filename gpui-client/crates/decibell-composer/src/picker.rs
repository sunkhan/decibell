//! The emoji picker and its GIFs tab (`EmojiPicker.tsx`, `GifPicker.tsx`): a 352 × 420 panel above
//! the composer's emoji button, right edges aligned.
//!
//! **Emoji.** The dataset is static (`emoji_data`); the body is a windowed list (only the rows near
//! the viewport are built: ~10 rows of 8 out of ~250), section headers stick to the top as CSS
//! `sticky` does, the category bar follows the scroll, search walks the dataset in its key order
//! (≤ 120 results), "Frequently used" is the last 24 picked (`ui.json` `emoji_recent`). Twemoji are
//! rasterised at the picker's sizes into the picker's own small cache, which is given back (atlas
//! tiles freed) when the picker closes. No skin tones and no keyboard grid navigation: the renderer
//! has neither (it inserts `skins[0]`).
//!
//! **GIFs.** Search or trending through [`crate::gifs::GifSource`] on the core's runtime, a 300 ms
//! debounce (a timer only while a query is pending), stale answers dropped by sequence, paging
//! near the bottom with results de-duplicated by id, the two-column masonry, attribution, the
//! not-configured notice. Previews are fetched (`decibell_unfurl::images`) only for tiles near the
//! viewport and decoded by GPUI, animated only while drawn; closing the picker frees them.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use decibell_model::Model;
use decibell_richtext::EmojiAsset;
use decibell_text::input::{InputEvent, InputState, TextInput};
use decibell_ui::scroll::ScrollKind;
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, RowHeights, ScrollArea, ScrollHandle, TextField, title_tooltip};
use decibell_unfurl::{GifProvider, GifResult};
use gpui::Focusable as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, Bounds, Context, Element, ElementId, Entity, GlobalElementId, Image,
    ImageFormat, InspectorElementId, IntoElement, LayoutId, ObjectFit, Pixels, Render, RenderImage, SharedString, Style, Subscription, Task,
    WeakEntity, Window, div, img, point, prelude::*, px,
};

use crate::Composer;
use crate::emoji_data::{self, CATEGORIES, EMOJI, RECENT_MAX};
use crate::gifs::{GifService, GifSource};

// ── Geometry (EmojiPicker.tsx) ─────────────────────────────────────────────────────────────────

/// `EMOJI_CELL_PX`, `EMOJI_GAP_PX`, `EMOJI_COLUMNS`.
const CELL: f32 = 38.;
const GAP: f32 = 2.;
const COLUMNS: usize = 8;
/// The sticky header: `pt-[7px] pb-[6px]`, 11 px text at line-height 1.5.
const HEADER: f32 = 7. + 16.5 + 6.;
/// A section's `mb-1`; the body's `pb-1`.
const SECTION_MARGIN: f32 = 4.;
const BODY_BOTTOM: f32 = 4.;
/// The body's `px-2` and Chromium's thin scrollbar (always there: the content overflows).
const BODY_PAD_X: f32 = 8.;
const BAR: f32 = 10.;
/// The panel is 352 wide with a 1 px border.
const INNER: f32 = crate::composer::PICKER_WIDTH - 2.;
/// The grid's width: `grid-cols-8 gap-[2px]` over the body's content box.
const GRID: f32 = INNER - 2. * BODY_PAD_X - BAR;
/// `scrollToCategory` scrolls to the section's `offsetTop − 4`, and `offsetTop` is measured from
/// the picker (the scroller is not positioned): it includes the tabs (`pt-3` + 24) and the search
/// row (`pt-2` + 36 + `pb-2`) above the scroller, so the target lands 84 px past the section's
/// start (the sticky header still names it). Mirrored as the renderer does it.
const SCROLLER_TOP_IN_PICKER: f32 = 12. + 24. + 8. + 36. + 8.;
/// Twemoji in a cell; in the category bar; in the preview bar.
const EMOJI_PX: f32 = 22.;
const NAV_EMOJI_PX: f32 = 18.;
const PREVIEW_EMOJI_PX: f32 = 28.;

// ── GifPicker.tsx ──────────────────────────────────────────────────────────────────────────────

/// `COLUMN_PX`: the renderer's column width for the cells' heights (its real columns are 160 px:
/// the constant ignores the border and the scrollbar; mirrored).
const GIF_COLUMN: f32 = 166.;
const GIF_CELL_MAX: f32 = 300.;
const GIF_CELL_MIN: f32 = 60.;
const GIF_DEBOUNCE: Duration = Duration::from_millis(300);
const GIF_LOAD_MORE: f32 = 300.;
/// Previews fetched at once.
const GIF_FETCHES: usize = 6;

/// The emoji images this picker drew, by (asset, device px): two generations, ~4 MB.
#[derive(Default)]
pub struct EmojiImages {
    young: HashMap<(u16, u16), Arc<RenderImage>>,
    old: HashMap<(u16, u16), Arc<RenderImage>>,
    young_bytes: usize,
    dropped: Vec<Arc<RenderImage>>,
}

const IMAGE_BUDGET: usize = 4 << 20;

impl EmojiImages {
    fn get(&mut self, asset: EmojiAsset, device_px: u32) -> Option<Arc<RenderImage>> {
        let key = (asset.index(), device_px.min(u16::MAX as u32) as u16);
        if let Some(image) = self.young.get(&key) {
            return Some(image.clone());
        }
        let image = match self.old.remove(&key) {
            Some(image) => image,
            None => decibell_text::rasterize_emoji(asset, device_px.max(1))?,
        };
        let bytes = (device_px * device_px * 4) as usize;
        if self.young_bytes + bytes > IMAGE_BUDGET / 2 && !self.young.is_empty() {
            let young = std::mem::take(&mut self.young);
            let old = std::mem::replace(&mut self.old, young);
            self.dropped.extend(old.into_values());
            self.young_bytes = 0;
        }
        self.young_bytes += bytes;
        self.young.insert(key, image.clone());
        Some(image)
    }

    /// (images, bytes of pixels).
    pub fn usage(&self) -> (usize, usize) {
        let bytes = |map: &HashMap<(u16, u16), Arc<RenderImage>>| map.keys().map(|(_, px)| *px as usize * *px as usize * 4).sum::<usize>();
        (self.young.len() + self.old.len(), bytes(&self.young) + bytes(&self.old))
    }

    fn release(&mut self, window: Option<&mut Window>, cx: &mut App) {
        let all: Vec<Arc<RenderImage>> = self.young.drain().chain(self.old.drain()).map(|(_, image)| image).chain(self.dropped.drain(..)).collect();
        self.young_bytes = 0;
        match window {
            Some(window) => {
                for image in all {
                    cx.drop_image(image, Some(window));
                }
            }
            None => {
                for image in all {
                    cx.drop_image(image, None);
                }
            }
        }
    }

    fn release_dropped(&mut self, window: &mut Window, cx: &mut App) {
        for image in self.dropped.drain(..) {
            cx.drop_image(image, Some(window));
        }
    }
}

/// A Twemoji at `size` px (or the characters in the system font when Twemoji has no drawing).
fn twemoji(native: &str, size: f32, images: &RefCell<EmojiImages>, window: &Window) -> AnyElement {
    match decibell_richtext::emoji::twemoji_asset(native) {
        Some(asset) => {
            let device = (size * window.scale_factor()).round() as u32;
            match images.borrow_mut().get(asset, device) {
                Some(image) => img(image).size(px(size)).flex_none().into_any_element(),
                None => div().size(px(size)).into_any_element(),
            }
        }
        None => Label::new(native.to_string()).size(size * 0.8).nowrap().into_any_element(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Emoji,
    Gifs,
}

/// One row of the emoji body.
#[derive(Clone, Copy, Debug)]
enum Row {
    /// A section's sticky header (`section` indexes [`Sections`]).
    Header(usize),
    /// Emoji `from..to` of a section's list.
    Grid { section: usize, from: usize, to: usize },
    /// "No emoji match …".
    Empty,
    /// The body's bottom padding.
    End,
}

/// The sections of the body: "frequent" (when anything was picked) then the dataset's categories.
struct Sections {
    /// (id, emoji indices, top, bottom of the section's box) in content px.
    list: Vec<(&'static str, Rc<[u16]>, f32, f32)>,
}

pub struct Picker {
    composer: WeakEntity<Composer>,
    anchor: Rc<Cell<Bounds<Pixels>>>,
    tab: Tab,
    /// When the picker was asked for (the open time is measured to its first frame).
    pub opened_at: Option<Instant>,
    pub open_time: Option<Duration>,
    // Emoji.
    search: Entity<InputState>,
    query: String,
    results: Rc<[u16]>,
    recent: Vec<u16>,
    /// `recent.length`: the frequent section shows when it isn't 0 (unknown ids included).
    recent_len: usize,
    sections: Rc<Sections>,
    rows: Rc<Vec<Row>>,
    offsets: Rc<[Pixels]>,
    scroll: ScrollHandle,
    hovered: Option<u16>,
    images: Rc<RefCell<EmojiImages>>,
    // GIFs.
    gif: Option<GifTab>,
    _subscriptions: Vec<Subscription>,
}

impl Picker {
    pub fn new(composer: WeakEntity<Composer>, anchor: Rc<Cell<Bounds<Pixels>>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = cx.try_global::<Model>().cloned();
        let (tab, recent_ids) = match &model {
            Some(model) => {
                let prefs = model.layout.read(cx).prefs();
                (if prefs.picker_tab == "gifs" { Tab::Gifs } else { Tab::Emoji }, prefs.emoji_recent.clone())
            }
            None => (Tab::Emoji, Vec::new()),
        };
        let recent: Vec<u16> = recent_ids.iter().filter_map(|id| emoji_data::index_of(id)).collect();
        let search = cx.new(|cx| InputState::single_line(window, cx).placeholder("Search emoji..."));
        let subscriptions = vec![cx.subscribe(&search, |this, search, event: &InputEvent, cx| {
            if *event == InputEvent::Change {
                let query = search.read(cx).value().trim().to_lowercase();
                if query != this.query {
                    this.results = emoji_data::search(&query).into();
                    this.query = query;
                    this.rebuild_rows();
                    cx.notify();
                }
            }
        })];
        let mut this = Self {
            composer,
            anchor,
            tab,
            opened_at: None,
            open_time: None,
            search,
            query: String::new(),
            results: Rc::from(Vec::new()),
            recent_len: recent_ids.len(),
            recent,
            sections: Rc::new(Sections { list: Vec::new() }),
            rows: Rc::new(Vec::new()),
            offsets: Rc::from(Vec::new()),
            scroll: ScrollHandle::new(),
            hovered: None,
            images: Rc::default(),
            gif: None,
            _subscriptions: subscriptions,
        };
        this.rebuild_rows();
        match tab {
            Tab::Emoji => this.focus_search(window, cx),
            Tab::Gifs => this.open_gifs(window, cx),
        }
        this
    }

    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let search = self.search.clone();
        window.defer(cx, move |window, cx| search.update(cx, |s, cx| s.focus(window, cx)));
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn search_input(&self) -> &Entity<InputState> {
        &self.search
    }

    pub fn gif_search_input(&self) -> Option<&Entity<InputState>> {
        self.gif.as_ref().map(|g| &g.search)
    }

    pub fn results(&self) -> &[u16] {
        &self.results
    }

    pub fn gif_results(&self) -> &[GifResult] {
        self.gif.as_ref().map_or(&[], |g| &g.results)
    }

    pub fn gif_loading(&self) -> bool {
        self.gif.as_ref().is_some_and(|g| g.loading)
    }

    /// Picks the `n`-th emoji the body shows (search results, or the first category): what a
    /// click on its cell does.
    pub fn pick_shown(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) -> Option<&'static str> {
        let list: Vec<u16> = if self.query.is_empty() { CATEGORIES.first().map(|(_, m)| m.to_vec()).unwrap_or_default() } else { self.results.to_vec() };
        let index = *list.get(n)?;
        self.pick(index, window, cx);
        Some(emoji_data::get(index).native)
    }

    /// Sends the `n`-th GIF of the results: what a click on its tile does.
    pub fn send_gif_shown(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        let url = self.gif.as_ref()?.results.get(n)?.url.clone();
        self.send_gif(url.clone(), window, cx);
        Some(url)
    }

    pub fn image_usage(&self) -> (usize, usize) {
        self.images.borrow().usage()
    }

    /// The emoji body's rows and their tops (frequent first when anything was picked).
    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        let mut offsets = vec![px(0.)];
        let mut y = 0f32;
        fn push(rows: &mut Vec<Row>, offsets: &mut Vec<Pixels>, y: &mut f32, row: Row, height: f32) {
            rows.push(row);
            *y += height;
            offsets.push(px(*y));
        }
        let mut sections = Vec::new();
        if self.query.is_empty() {
            let mut lists: Vec<(&'static str, Rc<[u16]>)> = Vec::new();
            if self.recent_len > 0 {
                lists.push(("frequent", Rc::from(self.recent.clone())));
            }
            for (id, members) in CATEGORIES {
                lists.push((id, Rc::from(members.to_vec())));
            }
            for (index, (id, list)) in lists.into_iter().enumerate() {
                let top = y;
                push(&mut rows, &mut offsets, &mut y, Row::Header(index), HEADER);
                let grid_rows = list.len().div_ceil(COLUMNS);
                for r in 0..grid_rows {
                    let last = r + 1 == grid_rows;
                    let height = CELL + if last { 0. } else { GAP };
                    push(&mut rows, &mut offsets, &mut y, Row::Grid { section: index, from: r * COLUMNS, to: ((r + 1) * COLUMNS).min(list.len()) }, height);
                }
                let bottom = y;
                // The section's `mb-1` after its box.
                if let Some(last) = offsets.last_mut() {
                    *last += px(SECTION_MARGIN);
                }
                y += SECTION_MARGIN;
                sections.push((id, list, top, bottom));
            }
        } else if self.results.is_empty() {
            push(&mut rows, &mut offsets, &mut y, Row::Empty, 48. + 40. + 10. + 18. + 48.);
        } else {
            let list = self.results.clone();
            let grid_rows = list.len().div_ceil(COLUMNS);
            for r in 0..grid_rows {
                let last = r + 1 == grid_rows;
                push(&mut rows, &mut offsets, &mut y, Row::Grid { section: 0, from: r * COLUMNS, to: ((r + 1) * COLUMNS).min(list.len()) }, CELL + if last { 0. } else { GAP });
            }
            sections.push(("search", list, 0., y));
        }
        push(&mut rows, &mut offsets, &mut y, Row::End, BODY_BOTTOM);
        self.sections = Rc::new(Sections { list: sections });
        self.rows = Rc::new(rows);
        self.offsets = Rc::from(offsets);
    }

    /// The section the band 10–20 % down the viewport is in (`IntersectionObserver` with
    /// `rootMargin: -10% 0px -80% 0px`: the last section to enter it).
    fn active_category(&self) -> Option<&'static str> {
        if !self.query.is_empty() {
            return None;
        }
        let top = f32::from(self.scroll.offset());
        let height = f32::from(self.scroll.viewport_height()).max(1.);
        let (band_top, band_bottom) = (top + 0.1 * height, top + 0.2 * height);
        self.sections
            .list
            .iter()
            .filter(|(_, _, t, b)| *t < band_bottom && *b > band_top)
            .last()
            .or_else(|| self.sections.list.first())
            .map(|(id, ..)| *id)
    }

    fn scroll_to_category(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some((_, _, top, _)) = self.sections.list.iter().find(|(s, ..)| *s == id) else { return };
        let target = (top + SCROLLER_TOP_IN_PICKER - 4.).min(f32::from(self.scroll.max_offset()));
        let current = f32::from(self.scroll.offset());
        self.scroll.scroll_by(px(target - current), ScrollKind::Key, cx);
        cx.notify();
    }

    fn pick(&mut self, index: u16, window: &mut Window, cx: &mut Context<Self>) {
        let emoji = emoji_data::get(index);
        if let Some(composer) = self.composer.upgrade() {
            composer.update(cx, |composer, cx| composer.insert_emoji(emoji.native, window, cx));
        }
        // `[id, ...prev.filter(x => x !== id)].slice(0, RECENT_MAX)`.
        let had = self.recent_len;
        self.recent.retain(|r| *r != index);
        self.recent.insert(0, index);
        self.recent.truncate(RECENT_MAX);
        if let Some(model) = cx.try_global::<Model>().cloned() {
            let mut ids: Vec<String> = model.layout.read(cx).prefs().emoji_recent.clone();
            ids.retain(|id| id != emoji.id);
            ids.insert(0, emoji.id.to_string());
            ids.truncate(RECENT_MAX);
            self.recent_len = ids.len();
            model.layout.update(cx, |layout, cx| layout.set_emoji_recent(ids, cx));
        } else {
            self.recent_len = self.recent.len();
        }
        if had == 0 || self.query.is_empty() {
            self.rebuild_rows();
        }
        cx.notify();
    }

    fn set_hovered(&mut self, index: Option<u16>, cx: &mut Context<Self>) {
        if self.hovered != index {
            self.hovered = index;
            cx.notify();
        }
    }

    pub fn switch_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        if let Some(model) = cx.try_global::<Model>().cloned() {
            model.layout.update(cx, |layout, cx| layout.set_picker_tab(tab == Tab::Gifs, cx));
        }
        match tab {
            Tab::Emoji => self.focus_search(window, cx),
            Tab::Gifs => self.open_gifs(window, cx),
        }
        cx.notify();
    }

    /// Gives back what the picker alone held: its emoji images (their atlas tiles) and the GIF
    /// previews it fetched and GPUI decoded.
    pub fn release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.images.borrow_mut().release(Some(window), cx);
        if let Some(gif) = self.gif.as_mut() {
            gif.release(window, cx);
        }
    }

    /// Asks the composer to close the picker: after this update (closing releases what the
    /// picker holds, which updates the picker).
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let composer = self.composer.clone();
        window.defer(cx, move |window, cx| {
            let _ = composer.update(cx, |composer, cx| composer.close_picker(window, cx));
        });
    }

    // ── GIFs ────────────────────────────────────────────────────────────────────────────────

    fn open_gifs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.gif.is_none() {
            let source = GifService::source(cx);
            let status = source.status();
            let label = status.provider.map_or("GIFs", GifProvider::label);
            let search = cx.new(|cx| {
                let mut state = InputState::single_line(window, cx).placeholder(format!("Search {label}"));
                if !status.configured {
                    state.set_disabled(true, cx);
                }
                state
            });
            let subscription = cx.subscribe(&search, |this, search, event: &InputEvent, cx| {
                if *event == InputEvent::Change {
                    let query = search.read(cx).value().to_string();
                    this.gif_query_changed(query, cx);
                }
            });
            let unfiltered = cx.try_global::<Model>().is_some_and(|m| m.settings.read(cx).get().gif_unfiltered);
            let settings = cx.try_global::<Model>().cloned().map(|m| {
                // Part of the request: flipping the Privacy switch while open runs the query again.
                cx.observe(&m.settings, |this, settings, cx| {
                    let unfiltered = settings.read(cx).get().gif_unfiltered;
                    if let Some(gif) = this.gif.as_mut()
                        && gif.unfiltered != unfiltered
                    {
                        gif.unfiltered = unfiltered;
                        this.gif_load(None, cx);
                    }
                })
            });
            self.gif = Some(GifTab {
                source,
                search,
                configured: status.configured,
                provider: status.provider,
                query: String::new(),
                debounced: String::new(),
                debounce: None,
                results: Vec::new(),
                next: None,
                loading: false,
                error: None,
                seq: 0,
                request: None,
                scroll: ScrollHandle::new(),
                last_offset: None,
                images: HashMap::new(),
                fetching: 0,
                unfiltered,
                _subscriptions: [Some(subscription), settings].into_iter().flatten().collect(),
            });
            if status.configured {
                self.gif_load(None, cx);
            }
        }
        if let Some(gif) = &self.gif {
            let search = gif.search.clone();
            window.defer(cx, move |window, cx| search.update(cx, |s, cx| s.focus(window, cx)));
        }
    }

    fn gif_query_changed(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(gif) = self.gif.as_mut() else { return };
        gif.query = query;
        // The debounce timer exists only while a query waits.
        gif.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(GIF_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                let Some(gif) = this.gif.as_mut() else { return };
                gif.debounce = None;
                let debounced = gif.query.trim().to_string();
                if debounced != gif.debounced {
                    gif.debounced = debounced;
                    gif.scroll.scroll_to_top();
                    this.gif_load(None, cx);
                }
            });
        }));
        cx.notify();
    }

    /// `load`: a page of the debounced query (or trending); a newer request makes this one's
    /// answer stale.
    fn gif_load(&mut self, pos: Option<String>, cx: &mut Context<Self>) {
        let Some(gif) = self.gif.as_mut() else { return };
        if !gif.configured {
            return;
        }
        gif.seq += 1;
        let seq = gif.seq;
        gif.loading = true;
        let task = gif.source.search(gif.debounced.clone(), pos.clone(), crate::gifs::locale(), gif.unfiltered, cx);
        gif.request = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(gif) = this.gif.as_mut() else { return };
                if seq != gif.seq {
                    return;
                }
                gif.loading = false;
                gif.request = None;
                match result {
                    Err(error) => {
                        gif.error = (error != "not-configured").then_some(error);
                        if pos.is_none() {
                            gif.results.clear();
                        }
                        gif.next = None;
                    }
                    Ok(page) => {
                        gif.error = None;
                        if pos.is_some() {
                            for result in page.results {
                                if !gif.results.iter().any(|r| r.id == result.id) {
                                    gif.results.push(result);
                                }
                            }
                        } else {
                            gif.results = page.results;
                            // A new list: what the old one fetched goes.
                            let keep: Vec<String> = gif.results.iter().map(|r| r.preview.clone()).collect();
                            gif.images.retain(|url, _| keep.contains(url));
                        }
                        gif.next = page.next;
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Starts fetching the previews of `urls` (tiles near the viewport), at most
    /// [`GIF_FETCHES`] at a time, on the core's runtime.
    fn gif_fetch(&mut self, urls: Vec<String>, cx: &mut Context<Self>) {
        let Some(gif) = self.gif.as_mut() else { return };
        for url in urls {
            if gif.fetching >= GIF_FETCHES || gif.images.contains_key(&url) || url.is_empty() {
                continue;
            }
            if !decibell_model::core::is_running() {
                gif.images.insert(url, Tile::Failed);
                continue;
            }
            gif.images.insert(url.clone(), Tile::Loading);
            gif.fetching += 1;
            let (tx, rx) = futures::channel::oneshot::channel();
            let fetch_url = url.clone();
            decibell_model::core::spawn_detached(async move {
                let _ = tx.send(decibell_unfurl::images::fetch(&fetch_url).await);
            });
            cx.spawn(async move |this, cx| {
                let bytes = rx.await.ok().and_then(Result::ok);
                let _ = this.update(cx, |this, cx| {
                    let Some(gif) = this.gif.as_mut() else { return };
                    gif.fetching = gif.fetching.saturating_sub(1);
                    if let Some(tile) = gif.images.get_mut(&url) {
                        *tile = match bytes {
                            Some(bytes) => Tile::Ready(Arc::new(Image::from_bytes(sniff_format(&bytes), bytes))),
                            None => Tile::Failed,
                        };
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// A GIF tile's click: sent by the composer, which closes the picker (after this update).
    fn send_gif(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        let composer = self.composer.clone();
        window.defer(cx, move |window, cx| {
            let _ = composer.update(cx, |composer, cx| composer.send_gif(&url, window, cx));
        });
    }
}

/// What the bytes are (a provider's "GIF" is sometimes a WebP or an MP4 poster).
fn sniff_format(bytes: &[u8]) -> ImageFormat {
    if bytes.starts_with(b"GIF8") {
        ImageFormat::Gif
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        ImageFormat::Png
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        ImageFormat::Webp
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        ImageFormat::Jpeg
    } else {
        ImageFormat::Gif
    }
}

enum Tile {
    Loading,
    Ready(Arc<Image>),
    Failed,
}

struct GifTab {
    source: Rc<dyn GifSource>,
    search: Entity<InputState>,
    configured: bool,
    provider: Option<GifProvider>,
    /// What is typed; what was searched (after the debounce).
    query: String,
    debounced: String,
    debounce: Option<Task<()>>,
    results: Vec<GifResult>,
    next: Option<String>,
    loading: bool,
    error: Option<String>,
    seq: u64,
    request: Option<Task<()>>,
    scroll: ScrollHandle,
    last_offset: Option<f32>,
    /// Previews by URL.
    images: HashMap<String, Tile>,
    fetching: usize,
    unfiltered: bool,
    _subscriptions: Vec<Subscription>,
}

impl GifTab {
    fn release(&mut self, window: &mut Window, cx: &mut App) {
        for (_, tile) in self.images.drain() {
            if let Tile::Ready(image) = tile {
                if let Some(render) = image.clone().get_render_image(window, cx) {
                    cx.drop_image(render, Some(window));
                }
                image.remove_asset(cx);
            }
        }
    }
}

/// `cellHeight`.
fn cell_height(g: &GifResult) -> f32 {
    let w = if g.preview_width > 0. { g.preview_width } else { g.width };
    let h = if g.preview_height > 0. { g.preview_height } else { g.height };
    if w <= 0. || h <= 0. {
        return GIF_COLUMN;
    }
    ((GIF_COLUMN as f64 * h / w).round() as f32).clamp(GIF_CELL_MIN, GIF_CELL_MAX)
}

/// `intoColumns`: shortest column first.
fn into_columns(results: &[GifResult]) -> [Vec<usize>; 2] {
    let mut columns: [Vec<usize>; 2] = [Vec::new(), Vec::new()];
    let mut heights = [0f32; 2];
    for (index, g) in results.iter().enumerate() {
        let target = if heights[1] < heights[0] { 1 } else { 0 };
        columns[target].push(index);
        heights[target] += cell_height(g) + 4.;
    }
    columns
}

// ── Drawing ────────────────────────────────────────────────────────────────────────────────────

/// The search box both tabs share (`h-[36px] rounded-md border bg-bg-lighter px-3 gap-2`,
/// `focus-within:border-accent focus-within:shadow-ring`).
fn search_box(input: &Entity<InputState>, on_clear: Option<Box<dyn Fn(&mut Window, &mut App)>>, window: &Window, cx: &mut App) -> AnyElement {
    let style = TextField::input_style(FontRole::Sans, 13., 1.21, cx);
    let theme = cx.theme();
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    let muted = theme.color_text_muted;
    div()
        .flex()
        .items_center()
        .gap_2()
        .h(px(36.))
        .px_3()
        .rounded(theme.radius_md)
        .border_1()
        .border_color(if focused { theme.color_accent } else { theme.color_border })
        .bg(theme.color_bg_lighter)
        .when(focused, |this| this.shadow(theme.shadow_ring.clone()))
        .child(Icon::new(IconName::ComposerSearch).size(px(14.)).color(muted))
        .child(div().flex_1().min_w_0().child(TextInput::new(input, style)))
        .children(on_clear.map(|clear| {
            div()
                .id("search-clear")
                .group("search-clear")
                .cursor_pointer()
                .tooltip(title_tooltip("Clear"))
                .on_click(move |_, window, cx| clear(window, cx))
                .child(Icon::new(IconName::ComposerX).size(px(14.)).stroke_width(2.5).color(muted).group_hover("search-clear", theme.color_text_secondary))
        }))
        .into_any_element()
}

impl Render for Picker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.open_time.is_none()
            && let Some(at) = self.opened_at
        {
            // Measured to the frame that shows it (the next frame callback after this render).
            let weak = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |this, _| this.open_time = Some(at.elapsed()));
            });
        }
        self.images.borrow_mut().release_dropped(window, cx);
        let theme = cx.theme();
        let anchor = self.anchor.clone();
        let tab = self.tab;
        let tabs = div().flex().flex_none().gap_1().px_3().pt_3().children([Tab::Emoji, Tab::Gifs].map(|t| {
            let active = t == tab;
            let (hover_bg, hover_fg) = (theme.color_surface_hover, theme.color_text_secondary);
            div()
                .id(if t == Tab::Emoji { "tab-emoji" } else { "tab-gifs" })
                .px_3()
                .py_1()
                .rounded(theme.radius_sm)
                .cursor_pointer()
                .when(active, |this| this.bg(theme.color_accent_soft))
                .when(!active, |this| this.hover(move |s| s.bg(hover_bg)))
                .on_click(cx.listener(move |this, _, window, cx| this.switch_tab(t, window, cx)))
                .child(
                    Label::new(if t == Tab::Emoji { "Emoji" } else { "GIFs" })
                        .size(12.)
                        .weight(600.)
                        .color(if active { theme.color_text_primary } else { theme.color_text_muted })
                        .nowrap(),
                )
                .when(!active, |this| this.group(if t == Tab::Emoji { "tab-emoji" } else { "tab-gifs" }))
                .map(|this| {
                    let _ = hover_fg;
                    this
                })
        }));
        let body = match tab {
            Tab::Emoji => self.render_emoji(window, cx),
            Tab::Gifs => self.render_gifs(window, cx),
        };
        let theme = cx.theme();
        div()
            .id("emoji-picker")
            .occlude()
            .w(px(crate::composer::PICKER_WIDTH))
            .h(px(crate::composer::PICKER_HEIGHT))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_light)
            .shadow(theme.shadow_modal.clone())
            .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                // A press outside, except on the button that opened it (its click toggles).
                if !anchor.get().contains(&event.position) {
                    this.close(window, cx);
                }
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(window, cx);
                }
            }))
            .child(tabs)
            .children(body)
            .with_animation(
                ("picker-in", cx.entity_id().as_u64() as usize),
                Animation::new(Duration::from_millis(200)).with_easing(decibell_ui::motion::EASE.easing()),
                |this, t| this.opacity(t).top(px(8. * (1. - t))),
            )
    }
}

impl Picker {
    fn render_emoji(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = crate::ink::Ink::of(cx);
        let has_query = !self.search.read(cx).value().is_empty();
        let clear = has_query.then(|| {
            let search = self.search.clone();
            Box::new(move |_: &mut Window, cx: &mut App| search.update(cx, |s, cx| s.clear(cx))) as Box<dyn Fn(&mut Window, &mut App)>
        });
        let search = div().flex_none().px_3().pb_2().pt_2().child(search_box(&self.search, clear, window, cx));

        // The body: a windowed list over the rows.
        let rows = self.rows.clone();
        let sections = self.sections.clone();
        let images = self.images.clone();
        let weak = cx.weak_entity();
        let query = self.query.clone();
        let typed = self.search.read(cx).value().to_string();
        let muted = theme.color_text_muted;
        let body = ScrollArea::new("emoji-body")
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .windowed(rows.len(), RowHeights::Offsets(self.offsets.clone()), move |range, window, cx| {
                range
                    .map(|index| match rows[index] {
                        Row::Header(section) => header(sections.list[section].0, cx).into_any_element(),
                        Row::Grid { section, from, to } => {
                            let list = sections.list[section].1.clone();
                            grid_row(&list[from..to], &images, &weak, window, cx)
                        }
                        Row::Empty => empty_search(&typed, muted, cx),
                        Row::End => div().into_any_element(),
                    })
                    .collect()
            });
        let _ = query;

        // The sticky header over the body, placed with this frame's scroll position.
        let sticky = (self.query.is_empty()).then(|| {
            let offset = f32::from(self.scroll.offset());
            let near: Vec<(f32, f32, AnyElement)> = self
                .sections
                .list
                .iter()
                .filter(|(_, _, top, bottom)| *bottom > offset - 400. && *top < offset + 400.)
                .map(|(id, _, top, bottom)| (*top, *bottom, header(id, cx).into_any_element()))
                .collect();
            StickyHeader { handle: self.scroll.clone(), sections: near }
        });

        // The category bar.
        let active = self.active_category();
        let mut nav_ids: Vec<&'static str> = Vec::new();
        if self.recent_len > 0 {
            nav_ids.push("frequent");
        }
        nav_ids.extend(CATEGORIES.iter().map(|(id, _)| *id));
        let images = self.images.clone();
        let nav = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .border_t_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_mid)
            .px_1p5()
            .py_1()
            .children(nav_ids.into_iter().map(|id| {
                let is_active = active == Some(id) && !has_query;
                let (accent_soft, hover, accent) = (theme.color_accent_soft, theme.color_surface_hover, theme.color_accent);
                div()
                    .id(ElementId::Name(format!("nav-{id}").into()))
                    .relative()
                    .flex_1()
                    .h(px(34.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_md)
                    .cursor_pointer()
                    .when(is_active, |this| this.bg(accent_soft))
                    .when(!is_active, |this| this.hover(move |s| s.bg(hover)))
                    .tooltip(title_tooltip(emoji_data::category_label(id)))
                    .on_click(cx.listener(move |this, _, _, cx| this.scroll_to_category(id, cx)))
                    .child(twemoji(emoji_data::category_icon(id), NAV_EMOJI_PX, &images, window))
                    .when(is_active, |this| {
                        // `absolute bottom-[2px] left-1/2 h-[2px] w-4 -translate-x-1/2 rounded-full bg-accent`.
                        this.child(div().absolute().bottom(px(2.)).left(gpui::relative(0.5)).ml(px(-8.)).h(px(2.)).w_4().rounded_full().bg(accent))
                    })
            }));

        // The preview bar.
        let preview = div()
            .flex()
            .flex_none()
            .h(px(46.))
            .items_center()
            .gap(px(10.))
            .border_t_1()
            .border_color(theme.color_border_divider)
            .px(px(14.))
            .map(|this| match self.hovered {
                Some(index) => {
                    let emoji = emoji_data::get(index);
                    this.child(twemoji(emoji.native, PREVIEW_EMOJI_PX, &self.images, window))
                        .child(Label::new(format!(":{}:", emoji.id)).font(FontRole::Meta).size(12.).weight(500.).color(theme.color_text_secondary).nowrap())
                }
                None => this.child(Label::new("Hover to preview").size(12.).color(theme.color_text_muted).nowrap()),
            });

        let body = div().relative().flex_1().min_h_0().flex().flex_col().child(body).children(sticky);
        vec![search.into_any_element(), body.into_any_element(), nav.into_any_element(), preview.into_any_element()]
    }

    fn render_gifs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(gif) = self.gif.as_mut() else { return Vec::new() };
        let has_query = !gif.query.is_empty();
        let clear = has_query.then(|| {
            let search = gif.search.clone();
            Box::new(move |_: &mut Window, cx: &mut App| search.update(cx, |s, cx| s.clear(cx))) as Box<dyn Fn(&mut Window, &mut App)>
        });
        let search = div().flex_none().px_3().pb_2().pt_2().child(search_box(&gif.search, clear, window, cx));
        let theme = crate::ink::Ink::of(cx);
        let label = gif.provider.map_or("GIFs", GifProvider::label);

        // Paging: near the bottom after a scroll (`onScroll`).
        let offset = f32::from(gif.scroll.offset());
        let scrolled = gif.last_offset.is_some_and(|last| last != offset);
        gif.last_offset = Some(offset);
        if scrolled && !gif.loading && gif.next.is_some() && f32::from(gif.scroll.max_offset()) - offset < GIF_LOAD_MORE {
            let next = gif.next.clone();
            cx.defer_in(window, move |this, _, cx| this.gif_load(next, cx));
        }

        let notice = |title: String, body: Option<String>| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1p5()
                .px_4()
                .py_12()
                .child(Label::new(title).size(12.).weight(500.).color(theme.color_text_secondary))
                .children(body.map(|body| Label::new(body).size(12.).line_height(18.).color(theme.color_text_muted)))
                .into_any_element()
        };
        let content: AnyElement = if !gif.configured {
            notice(
                "GIF search isn't set up for this build".into(),
                Some("Put a KLIPY or GIPHY key in resources/gifs.json or set GIF_API_KEY — see HANDOFF.md.".into()),
            )
        } else if let Some(error) = &gif.error {
            notice("GIF search failed".into(), Some(error.clone()))
        } else if gif.results.is_empty() {
            notice(if gif.loading { "Loading…".into() } else { format!("No GIFs match \"{}\"", gif.debounced) }, None)
        } else {
            // Tiles near the viewport get their previews fetched.
            let viewport = f32::from(gif.scroll.viewport_height()).max(400.);
            let columns = into_columns(&gif.results);
            let mut wanted = Vec::new();
            let mut columns_el = Vec::new();
            for column in &columns {
                let mut y = 0.;
                let mut cells = Vec::new();
                for index in column {
                    let g = &gif.results[*index];
                    let h = cell_height(g);
                    let near = y + h > offset - viewport && y < offset + 2. * viewport;
                    y += h + 4.;
                    let image = match gif.images.get(&g.preview) {
                        Some(Tile::Ready(image)) if near => Some(image.clone()),
                        None if near => {
                            wanted.push(g.preview.clone());
                            None
                        }
                        _ => None,
                    };
                    let url = g.url.clone();
                    let group: SharedString = format!("gif-{}", g.id).into();
                    cells.push(
                        div()
                            .id(ElementId::Name(group.clone()))
                            .group(group.clone())
                            .relative()
                            .w_full()
                            .h(px(h))
                            .flex_none()
                            .overflow_hidden()
                            .rounded(theme.radius_md)
                            .bg(theme.color_bg_lighter)
                            .cursor_pointer()
                            .tooltip(title_tooltip(g.title.clone()))
                            .on_click(cx.listener(move |this, _, window, cx| this.send_gif(url.clone(), window, cx)))
                            .children(image.map(|image| img(image).size_full().object_fit(ObjectFit::Cover)))
                            // `outline-accent -outline-offset-2 hover:outline-2`.
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .rounded(theme.radius_md)
                                    .border_2()
                                    .border_color(theme.color_accent)
                                    .opacity(0.)
                                    .group_hover(group, |s| s.opacity(1.)),
                            ),
                    );
                }
                columns_el.push(div().flex().flex_col().flex_1().min_w_0().gap_1().children(cells));
            }
            if !wanted.is_empty() {
                cx.defer_in(window, move |this, _, cx| this.gif_fetch(wanted, cx));
            }
            div().flex().gap_1().children(columns_el).into_any_element()
        };
        let body = ScrollArea::new("gif-body").track_scroll(&gif.scroll).flex_1().min_h_0().child(div().px_2().pb_2().child(content));
        let footer = div()
            .flex()
            .flex_none()
            .h(px(34.))
            .items_center()
            .justify_end()
            .border_t_1()
            .border_color(theme.color_border_divider)
            .px(px(14.))
            .when(gif.loading && !gif.results.is_empty(), |this| {
                this.child(div().mr_auto().child(Label::new("Loading…").font(FontRole::Meta).size(11.).color(theme.color_text_muted).nowrap()))
            })
            .when(gif.provider.is_some(), |this| {
                this.child(Label::new(format!("Powered by {label}")).font(FontRole::Meta).size(11.).color(theme.color_text_muted).nowrap())
            });
        vec![search.into_any_element(), body.into_any_element(), footer.into_any_element()]
    }
}

/// A section's header (`px-1.5 pb-[6px] pt-[7px] text-[11px] font-semibold uppercase
/// tracking-[0.07em] text-text-muted`, `bg-bg-light`).
fn header(id: &str, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    div()
        .w_full()
        .px_2()
        .child(
            div()
                .w_full()
                .px_1p5()
                .pt(px(7.))
                .pb(px(6.))
                .bg(theme.color_bg_light)
                .child(Label::new(emoji_data::category_label(id).to_uppercase()).size(11.).weight(600.).tracking(0.07).color(theme.color_text_muted).nowrap()),
        )
}

/// A grid row: up to 8 cells, `grid-cols-8 gap-[2px]` over the body's content box.
fn grid_row(list: &[u16], images: &RefCell<EmojiImages>, picker: &WeakEntity<Picker>, window: &Window, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let column = (GRID - GAP * (COLUMNS as f32 - 1.)) / COLUMNS as f32;
    let active_bg = theme.color_surface_active;
    let mut row = div().relative().w_full().h(px(CELL)).px_2();
    for (n, index) in list.iter().copied().enumerate() {
        let emoji = &EMOJI[index as usize];
        let (pick, hover) = (picker.clone(), picker.clone());
        row = row.child(
            div()
                .id(("emoji", index as usize))
                .absolute()
                .left(px(BODY_PAD_X + n as f32 * (column + GAP)))
                .top_0()
                .size(px(CELL))
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .cursor_pointer()
                .hover(move |s| s.bg(active_bg))
                .tooltip(title_tooltip(emoji.name))
                .on_hover(move |hovered, _, cx| {
                    let _ = hover.update(cx, |this, cx| this.set_hovered(hovered.then_some(index).or(this.hovered.filter(|h| *h != index)), cx));
                })
                .on_click(move |_, window, cx| {
                    let _ = pick.update(cx, |this, cx| this.pick(index, window, cx));
                })
                .child(twemoji(emoji.native, EMOJI_PX, images, window)),
        );
    }
    row.into_any_element()
}

fn empty_search(typed: &str, muted: gpui::Hsla, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.))
        .py_12()
        .child(
            div()
                .size_10()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(gpui::Hsla { a: muted.a * 0.1, ..muted })
                .child(Icon::new(IconName::ComposerSearch).size(px(18.)).color(theme.color_text_muted)),
        )
        .child(Label::new(format!("No emoji match \"{typed}\"")).size(12.).color(theme.color_text_muted).nowrap())
        .into_any_element()
}

/// The sticky section header: drawn over the body at the position CSS `position: sticky;
/// top: -1px` gives it, from the scroll position of **this** frame (read in prepaint, after the
/// scroll area placed its content), so it never lags the rows it covers.
struct StickyHeader {
    handle: ScrollHandle,
    /// (top, bottom of the section's box, its header) for the sections near the viewport.
    sections: Vec<(f32, f32, AnyElement)>,
}

impl Element for StickyHeader {
    type RequestLayoutState = Vec<LayoutId>;
    type PrepaintState = Option<usize>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Vec<LayoutId>) {
        let children: Vec<LayoutId> = self.sections.iter_mut().map(|(_, _, header)| header.request_layout(window, cx)).collect();
        let mut style = Style { position: gpui::Position::Absolute, ..Style::default() };
        style.inset.top = px(0.).into();
        style.inset.left = px(0.).into();
        style.size.width = px(INNER - BAR).into();
        (window.request_layout(style, children.iter().copied(), cx), children)
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut Vec<LayoutId>, window: &mut Window, cx: &mut App) -> Option<usize> {
        let offset = f32::from(self.handle.offset());
        // The section whose box holds the viewport's top edge.
        let index = self.sections.iter().position(|(top, bottom, _)| *top <= offset + 1. && *bottom > offset + 1.)?;
        let (top, bottom, header) = &mut self.sections[index];
        // In flow it sits at the section's top; stuck it sits at −1 px; it never leaves its box.
        let y = (*top - offset).max(-1.).min(*bottom - HEADER - offset);
        if y >= 0. {
            // In flow, where the row draws it already.
            return None;
        }
        header.prepaint_at(point(bounds.origin.x, bounds.origin.y + px(y)), window, cx);
        Some(index)
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut Vec<LayoutId>, shown: &mut Option<usize>, window: &mut Window, cx: &mut App) {
        let Some(index) = *shown else { return };
        let mask = Bounds::new(bounds.origin, gpui::size(px(INNER - BAR), px(HEADER)));
        window.with_content_mask(Some(gpui::ContentMask { bounds: mask }), |window| self.sections[index].2.paint(window, cx));
    }
}
