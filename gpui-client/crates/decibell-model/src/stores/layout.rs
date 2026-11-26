//! Client-only preferences: what the renderer keeps in `localStorage` and nowhere else.
//! Per-install view state (the sidebar's width, the mini player's corner, which panels
//! are open, recent emoji), so it is not in the core's `config.json`: it is not a choice
//! made in Settings and must not roam to a machine with a different window size.
//!
//! One small JSON file, `<user data dir>/ui.json`, read once when the model is built
//! (synchronously: the first frame already has the widths and the theme) and written
//! atomically 200 ms after the last change. The write timer exists only while there is
//! something to write.
//!
//! docs/model.md §7 lists every `localStorage` key of the renderer and its home here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{AppContext as _, Context, EventEmitter, Task};
use serde::{Deserialize, Serialize};

pub const SIDEBAR_WIDTH_MIN: u32 = 180;
pub const SIDEBAR_WIDTH_MAX: u32 = 480;
pub const SIDEBAR_WIDTH_DEFAULT: u32 = 240;
pub const PIP_WIDTH_MIN: u32 = 240;
pub const PIP_WIDTH_MAX: u32 = 640;
pub const PIP_WIDTH_DEFAULT: u32 = 320;
/// P2P call stage (top of the DM): one height for the voice tiles, another for a
/// focused stream.
pub const CALL_STAGE_MIN: u32 = 200;
pub const CALL_STAGE_MAX: u32 = 1200;
pub const CALL_STAGE_VOICE_DEFAULT: u32 = 300;
pub const CALL_STAGE_STREAM_DEFAULT: u32 = 470;

const WRITE_DEBOUNCE: Duration = Duration::from_millis(200);
const FILE_NAME: &str = "ui.json";

/// Which corner the floating pop-out stream player snaps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PipCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallStageMode {
    Voice,
    Stream,
}

/// The file. Unknown fields are ignored and missing ones take their defaults, so an
/// older or newer build's file always loads; a value out of range is clamped on read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    /// Mirrors of `config.json`'s appearance (the renderer's `decibell.theme`,
    /// `decibell.textSizePx`, `decibell.rowScale`): the real values arrive only once the
    /// core has started, after the first frame, so the window opens with these.
    pub theme: String,
    pub text_size_px: f64,
    pub row_scale: f64,
    pub sidebar_width: u32,
    pub pip_width: u32,
    pub pip_corner: PipCorner,
    pub call_stage_voice_height: u32,
    pub call_stage_stream_height: u32,
    pub members_panel: bool,
    pub dm_friends_panel: bool,
    /// The emoji picker's recently used ids, newest first (`decibell.emoji.recent`).
    pub emoji_recent: Vec<String>,
    /// The picker's last tab, "emoji" or "gifs" (`decibell.picker.tab`).
    pub picker_tab: String,
    /// Anything else a feature keeps per install (codec-capability caches, …), under
    /// its own key: [`Layout::extra`] / [`Layout::set_extra`].
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            theme: "nocturne".into(),
            text_size_px: 14.5,
            row_scale: 1.0,
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            pip_width: PIP_WIDTH_DEFAULT,
            pip_corner: PipCorner::default(),
            call_stage_voice_height: CALL_STAGE_VOICE_DEFAULT,
            call_stage_stream_height: CALL_STAGE_STREAM_DEFAULT,
            members_panel: true,
            dm_friends_panel: true,
            emoji_recent: Vec::new(),
            picker_tab: "emoji".into(),
            extra: BTreeMap::new(),
        }
    }
}

impl UiPrefs {
    fn clamped(mut self) -> UiPrefs {
        let d = UiPrefs::default();
        let px = |v: u32, min: u32, max: u32, fallback: u32| if v == 0 { fallback } else { v.clamp(min, max) };
        self.sidebar_width = px(self.sidebar_width, SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX, d.sidebar_width);
        self.pip_width = px(self.pip_width, PIP_WIDTH_MIN, PIP_WIDTH_MAX, d.pip_width);
        self.call_stage_voice_height =
            px(self.call_stage_voice_height, CALL_STAGE_MIN, CALL_STAGE_MAX, d.call_stage_voice_height);
        self.call_stage_stream_height =
            px(self.call_stage_stream_height, CALL_STAGE_MIN, CALL_STAGE_MAX, d.call_stage_stream_height);
        self.text_size_px = super::settings::clamp_text_size_px(self.text_size_px);
        self.row_scale = super::settings::clamp_scale(
            if self.row_scale == 0.0 { 1.0 } else { self.row_scale },
            super::settings::ROW_SCALE_MIN,
            super::settings::ROW_SCALE_MAX,
            1.0,
        );
        if self.picker_tab != "gifs" {
            self.picker_tab = "emoji".into();
        }
        self
    }

    /// Read `dir`'s file; defaults when it is missing or doesn't parse.
    pub fn load(dir: &Path) -> UiPrefs {
        let path = dir.join(FILE_NAME);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<UiPrefs>(&bytes) {
                Ok(prefs) => prefs.clamped(),
                Err(e) => {
                    log::warn!("[prefs] {} doesn't parse ({e}); using defaults", path.display());
                    UiPrefs::default()
                }
            },
            Err(_) => UiPrefs::default(),
        }
    }

    /// Write `dir`'s file atomically: a temporary file beside it, then a rename, so a
    /// crash mid-write leaves the old file.
    pub fn store(&self, dir: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE_NAME}.tmp"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, dir.join(FILE_NAME))
    }
}

/// Which part changed, for a view that reads one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutEvent {
    Sidebar,
    Pip,
    CallStage,
    Panels,
    Picker,
    Extra,
}

impl EventEmitter<LayoutEvent> for Layout {}

pub struct Layout {
    prefs: UiPrefs,
    /// `None`: nothing is persisted (unit tests, screens).
    dir: Option<PathBuf>,
    /// The debounce timer of a pending write.
    write: Option<Task<()>>,
}

impl Layout {
    /// `dir`: the user data directory, or `None` to keep everything in memory.
    pub fn new(dir: Option<PathBuf>) -> Layout {
        Layout {
            prefs: dir.as_deref().map(UiPrefs::load).unwrap_or_default(),
            dir,
            write: None,
        }
    }

    pub fn prefs(&self) -> &UiPrefs {
        &self.prefs
    }

    fn changed(&mut self, event: Option<LayoutEvent>, cx: &mut Context<Self>) {
        if let Some(event) = event {
            cx.emit(event);
            cx.notify();
        }
        if self.dir.is_none() {
            return;
        }
        // Trailing: a resize drag is one write.
        self.write = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WRITE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                // Dropping `write` cancels this very task at its next await: everything
                // it still has to do is started here.
                this.write = None;
                let Some(dir) = this.dir.clone() else { return };
                let prefs = this.prefs.clone();
                // Off the UI thread: the file is small, the disk may not be fast.
                cx.background_spawn(async move {
                    if let Err(e) = prefs.store(&dir) {
                        log::warn!("[prefs] couldn't write {}: {e}", dir.join(FILE_NAME).display());
                    }
                })
                .detach();
            });
        }));
    }

    /// Write a pending change now, on this thread (quit). No-op when nothing is pending.
    pub fn flush(&mut self) {
        if self.write.take().is_some()
            && let Some(dir) = &self.dir
            && let Err(e) = self.prefs.store(dir)
        {
            log::warn!("[prefs] couldn't write {}: {e}", dir.join(FILE_NAME).display());
        }
    }

    pub fn write_pending(&self) -> bool {
        self.write.is_some()
    }

    pub fn set_sidebar_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = clamp_px(width, SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_DEFAULT);
        if self.prefs.sidebar_width != width {
            self.prefs.sidebar_width = width;
            self.changed(Some(LayoutEvent::Sidebar), cx);
        }
    }

    pub fn set_pip_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = clamp_px(width, PIP_WIDTH_MIN, PIP_WIDTH_MAX, PIP_WIDTH_DEFAULT);
        if self.prefs.pip_width != width {
            self.prefs.pip_width = width;
            self.changed(Some(LayoutEvent::Pip), cx);
        }
    }

    pub fn set_pip_corner(&mut self, corner: PipCorner, cx: &mut Context<Self>) {
        if self.prefs.pip_corner != corner {
            self.prefs.pip_corner = corner;
            self.changed(Some(LayoutEvent::Pip), cx);
        }
    }

    pub fn set_call_stage_height(&mut self, mode: CallStageMode, height: f32, cx: &mut Context<Self>) {
        let (slot, fallback) = match mode {
            CallStageMode::Voice => (&mut self.prefs.call_stage_voice_height, CALL_STAGE_VOICE_DEFAULT),
            CallStageMode::Stream => (&mut self.prefs.call_stage_stream_height, CALL_STAGE_STREAM_DEFAULT),
        };
        let height = clamp_px(height, CALL_STAGE_MIN, CALL_STAGE_MAX, fallback);
        if *slot != height {
            *slot = height;
            self.changed(Some(LayoutEvent::CallStage), cx);
        }
    }

    pub fn toggle_members_panel(&mut self, cx: &mut Context<Self>) {
        self.prefs.members_panel = !self.prefs.members_panel;
        self.changed(Some(LayoutEvent::Panels), cx);
    }

    pub fn toggle_dm_friends_panel(&mut self, cx: &mut Context<Self>) {
        self.prefs.dm_friends_panel = !self.prefs.dm_friends_panel;
        self.changed(Some(LayoutEvent::Panels), cx);
    }

    pub fn set_picker_tab(&mut self, gifs: bool, cx: &mut Context<Self>) {
        let tab = if gifs { "gifs" } else { "emoji" };
        if self.prefs.picker_tab != tab {
            self.prefs.picker_tab = tab.into();
            self.changed(Some(LayoutEvent::Picker), cx);
        }
    }

    pub fn set_emoji_recent(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        if self.prefs.emoji_recent != ids {
            self.prefs.emoji_recent = ids;
            self.changed(Some(LayoutEvent::Picker), cx);
        }
    }

    /// The appearance mirror (see [`UiPrefs::theme`]). Nobody reads it while the app
    /// runs, so no notification: only the file changes.
    pub fn mirror_appearance(&mut self, theme: &str, text_size_px: f64, row_scale: f64, cx: &mut Context<Self>) {
        if self.prefs.theme != theme || self.prefs.text_size_px != text_size_px || self.prefs.row_scale != row_scale {
            self.prefs.theme = theme.into();
            self.prefs.text_size_px = text_size_px;
            self.prefs.row_scale = row_scale;
            self.changed(None, cx);
        }
    }

    /// A feature's own per-install value (see [`UiPrefs::extra`]).
    pub fn extra<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        serde_json::from_value(self.prefs.extra.get(key)?.clone()).ok()
    }

    /// Set (or with `None` remove) a feature's own per-install value.
    pub fn set_extra<T: Serialize>(&mut self, key: &str, value: Option<&T>, cx: &mut Context<Self>) {
        let value = value.and_then(|v| serde_json::to_value(v).ok());
        if self.prefs.extra.get(key) == value.as_ref() {
            return;
        }
        match value {
            Some(value) => self.prefs.extra.insert(key.to_string(), value),
            None => self.prefs.extra.remove(key),
        };
        self.changed(Some(LayoutEvent::Extra), cx);
    }
}

fn clamp_px(value: f32, min: u32, max: u32, fallback: u32) -> u32 {
    if !value.is_finite() {
        return fallback;
    }
    (value.round().max(0.0) as u32).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn read(dir: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join(FILE_NAME)).unwrap()).unwrap()
    }

    #[test]
    fn a_stored_value_is_clamped_on_read_and_garbage_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(UiPrefs::load(dir.path()), UiPrefs::default());
        std::fs::write(
            dir.path().join(FILE_NAME),
            br#"{"sidebar_width": 9000, "pip_width": 10, "pip_corner": "top-left", "members_panel": false,
                "picker_tab": "weird", "text_size_px": 99, "from_a_newer_build": [1, 2]}"#,
        )
        .unwrap();
        let prefs = UiPrefs::load(dir.path());
        assert_eq!(prefs.sidebar_width, SIDEBAR_WIDTH_MAX);
        assert_eq!(prefs.pip_width, PIP_WIDTH_MIN);
        assert_eq!(prefs.pip_corner, PipCorner::TopLeft);
        assert!(!prefs.members_panel);
        assert!(prefs.dm_friends_panel, "a missing field takes its default");
        assert_eq!(prefs.picker_tab, "emoji");
        assert_eq!(prefs.text_size_px, 17.0);
        assert_eq!(prefs.call_stage_stream_height, CALL_STAGE_STREAM_DEFAULT);

        std::fs::write(dir.path().join(FILE_NAME), b"{ not json").unwrap();
        assert_eq!(UiPrefs::load(dir.path()), UiPrefs::default());
    }

    #[test]
    fn store_is_atomic_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut prefs = UiPrefs { sidebar_width: 300, ..UiPrefs::default() };
        prefs.extra.insert("decoder_caps.v6".into(), serde_json::json!({"av1": true}));
        prefs.store(dir.path()).unwrap();
        assert!(!dir.path().join("ui.json.tmp").exists(), "the temporary file was renamed away");
        assert_eq!(UiPrefs::load(dir.path()), prefs);
    }

    #[gpui::test]
    fn a_drag_is_one_write_and_nothing_is_written_at_rest(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let layout = cx.new(|_| Layout::new(Some(path.clone())));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _sub = cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(&layout, move |_, e: &LayoutEvent, _| events.borrow_mut().push(*e))
        });
        cx.executor().allow_parking();

        // A sidebar drag: a setter call per pointer move.
        for w in 241..300 {
            layout.update(cx, |l, cx| l.set_sidebar_width(w as f32, cx));
            cx.executor().advance_clock(Duration::from_millis(16));
            cx.run_until_parked();
        }
        assert!(!path.join(FILE_NAME).exists(), "nothing is written during the drag");
        cx.executor().advance_clock(WRITE_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(read(&path)["sidebar_width"], 299);
        layout.read_with(cx, |l, _| assert!(!l.write_pending(), "no timer at rest"));
        assert_eq!(events.borrow().len(), 59);
        assert!(events.borrow().iter().all(|e| *e == LayoutEvent::Sidebar));

        // An unchanged value notifies nobody and writes nothing.
        events.borrow_mut().clear();
        layout.update(cx, |l, cx| {
            l.set_sidebar_width(299.2, cx);
            l.set_pip_corner(PipCorner::BottomRight, cx);
        });
        assert!(events.borrow().is_empty());
        layout.read_with(cx, |l, _| assert!(!l.write_pending()));

        // Out-of-range input is clamped; flush writes at once.
        layout.update(cx, |l, cx| {
            l.set_pip_width(5000.0, cx);
            l.toggle_members_panel(cx);
            l.set_call_stage_height(CallStageMode::Stream, f32::NAN, cx);
            l.set_extra("native_encoder_caps.v2", Some(&vec![1u32, 3]), cx);
            l.mirror_appearance("matinee", 15.0, 1.1, cx);
            assert!(l.write_pending());
            l.flush();
            assert!(!l.write_pending());
        });
        let file = read(&path);
        assert_eq!(file["pip_width"], PIP_WIDTH_MAX);
        assert_eq!(file["members_panel"], false);
        assert_eq!(file["call_stage_stream_height"], CALL_STAGE_STREAM_DEFAULT);
        assert_eq!(file["extra"]["native_encoder_caps.v2"], serde_json::json!([1, 3]));
        assert_eq!(file["theme"], "matinee");
        layout.read_with(cx, |l, _| assert_eq!(l.extra::<Vec<u32>>("native_encoder_caps.v2"), Some(vec![1, 3])));

        // The next start reads it back.
        let again = cx.new(|_| Layout::new(Some(path.clone())));
        again.read_with(cx, |l, _| {
            assert_eq!(l.prefs().sidebar_width, 299);
            assert_eq!(l.prefs().theme, "matinee");
            assert!(!l.prefs().members_panel);
        });
    }

    #[gpui::test]
    fn without_a_directory_nothing_is_persisted(cx: &mut TestAppContext) {
        let layout = cx.new(|_| Layout::new(None));
        layout.update(cx, |l, cx| {
            l.set_sidebar_width(400.0, cx);
            assert!(!l.write_pending());
            assert_eq!(l.prefs().sidebar_width, 400);
        });
    }
}
