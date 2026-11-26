//! The settings that live in the core's `config.json` (`decibell_core::config::
//! AppSettings`): one copy here, the source of truth while the app runs, saved back
//! whole through `api::settings::save_settings` (`features/settings/loadSettings.ts`,
//! `saveSettings.ts`).
//!
//! The renderer spreads these over `uiStore`, `dmStore`, `voiceStore`, `transfersStore`
//! and `hotkeysStore` and rebuilds the blob on every save; here every feature reads and
//! writes its fields of the one struct with [`Settings::change`].
//!
//! Saves are debounced: 250 ms after the last change (a slider drag is one write). The
//! timer exists only while a save is pending.

use std::time::Duration;

use decibell_core::config::AppSettings;
use decibell_theme::ThemeId;
use gpui::{Context, EventEmitter, Task};

pub const TEXT_SIZE_MIN_PX: f64 = 11.0;
pub const TEXT_SIZE_MAX_PX: f64 = 17.0;
pub const TEXT_SIZE_STEP_PX: f64 = 0.5;
pub const DEFAULT_TEXT_SIZE_PX: f64 = 14.5;
pub const ROW_SCALE_MIN: f64 = 0.85;
pub const ROW_SCALE_MAX: f64 = 1.3;
pub const DEFAULT_ROW_SCALE: f64 = 1.0;

const SAVE_DEBOUNCE: Duration = Duration::from_millis(250);

/// Clock style for times of day (Settings → Appearance → Time format).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeFormat {
    /// Follows the system's regional setting (`regional`).
    #[default]
    Auto,
    H12,
    H24,
}

impl TimeFormat {
    pub fn from_setting(value: &str) -> TimeFormat {
        match value {
            "12h" => TimeFormat::H12,
            "24h" => TimeFormat::H24,
            _ => TimeFormat::Auto,
        }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            TimeFormat::Auto => "auto",
            TimeFormat::H12 => "12h",
            TimeFormat::H24 => "24h",
        }
    }
}

/// 0 (and anything else out of range) falls back to the default: it's what serde writes
/// for a config predating the field. Snapped to the half-pixel grid.
pub fn clamp_text_size_px(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return DEFAULT_TEXT_SIZE_PX;
    }
    let snapped = (value / TEXT_SIZE_STEP_PX).round() * TEXT_SIZE_STEP_PX;
    snapped.clamp(TEXT_SIZE_MIN_PX, TEXT_SIZE_MAX_PX)
}

/// Rounded to 2 dp: the sliders step in fractions and float drift would otherwise write
/// 1.0500000000000003 into the config.
pub fn clamp_scale(value: f64, min: f64, max: f64, fallback: f64) -> f64 {
    if !value.is_finite() {
        return fallback;
    }
    (value.clamp(min, max) * 100.0).round() / 100.0
}

pub enum SettingsEvent {
    /// Theme, text size or list density changed: the app applies them to the `Theme`
    /// global (`decibell_ui::Theme::set` / `set_scale`); the model never touches views.
    Appearance,
}

impl EventEmitter<SettingsEvent> for Settings {}

pub struct Settings {
    values: AppSettings,
    /// The debounce timer of a pending save.
    save: Option<Task<()>>,
    /// How saves leave: the core call in the app, a recorder in tests.
    saver: Box<dyn Fn(serde_json::Value)>,
}

impl Settings {
    pub fn new(saver: impl Fn(serde_json::Value) + 'static) -> Settings {
        Settings {
            values: AppSettings::default(),
            save: None,
            saver: Box::new(saver),
        }
    }

    pub fn get(&self) -> &AppSettings {
        &self.values
    }

    /// The config as loaded from disk. Not a change: nothing is saved.
    pub fn apply_loaded(&mut self, values: AppSettings, cx: &mut Context<Self>) {
        self.values = values;
        cx.emit(SettingsEvent::Appearance);
        cx.notify();
    }

    /// Change settings and save them (debounced). Every user-facing settings change
    /// goes through here.
    pub fn change(&mut self, change: impl FnOnce(&mut AppSettings), cx: &mut Context<Self>) {
        let before = (self.values.theme.clone(), self.values.text_size_px, self.values.list_density);
        change(&mut self.values);
        if before != (self.values.theme.clone(), self.values.text_size_px, self.values.list_density) {
            cx.emit(SettingsEvent::Appearance);
        }
        cx.notify();
        self.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |this, _| {
                this.save = None;
                this.save_now();
            });
        }));
    }

    /// Write a pending save now (window close, quit): the last slider tick is not lost
    /// in the debounce gap. No-op when nothing is pending.
    pub fn flush(&mut self) {
        if self.save.take().is_some() {
            self.save_now();
        }
    }

    pub fn save_pending(&self) -> bool {
        self.save.is_some()
    }

    fn save_now(&self) {
        match serde_json::to_value(&self.values) {
            Ok(blob) => (self.saver)(blob),
            Err(e) => log::error!("[settings] couldn't serialise: {e}"),
        }
    }

    // ── Appearance ──

    /// A config written by a newer build (or edited by hand) falls back to the default
    /// rather than leaving the app with no palette.
    pub fn theme(&self) -> ThemeId {
        ThemeId::from_setting(&self.values.theme)
    }

    pub fn set_theme(&mut self, theme: ThemeId, cx: &mut Context<Self>) {
        self.change(|s| s.theme = theme.as_str().to_string(), cx);
    }

    /// The message body's size in px (0 in the file = never set).
    pub fn text_size_px(&self) -> f64 {
        clamp_text_size_px(self.values.text_size_px)
    }

    pub fn set_text_size_px(&mut self, value: f64, cx: &mut Context<Self>) {
        self.change(|s| s.text_size_px = clamp_text_size_px(value), cx);
    }

    /// The list density multiplier (`list_density`; 0 in the file = never set).
    pub fn row_scale(&self) -> f64 {
        let stored = self.values.list_density;
        clamp_scale(
            if stored == 0.0 { DEFAULT_ROW_SCALE } else { stored },
            ROW_SCALE_MIN,
            ROW_SCALE_MAX,
            DEFAULT_ROW_SCALE,
        )
    }

    pub fn set_row_scale(&mut self, value: f64, cx: &mut Context<Self>) {
        self.change(
            |s| s.list_density = clamp_scale(value, ROW_SCALE_MIN, ROW_SCALE_MAX, DEFAULT_ROW_SCALE),
            cx,
        );
    }

    pub fn time_format(&self) -> TimeFormat {
        TimeFormat::from_setting(&self.values.time_format)
    }

    pub fn set_time_format(&mut self, format: TimeFormat, cx: &mut Context<Self>) {
        self.change(|s| s.time_format = format.as_setting().to_string(), cx);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    #[test]
    fn clamps_match_the_renderer() {
        assert_eq!(clamp_text_size_px(0.0), 14.5);
        assert_eq!(clamp_text_size_px(f64::NAN), 14.5);
        assert_eq!(clamp_text_size_px(14.2), 14.0);
        assert_eq!(clamp_text_size_px(14.3), 14.5);
        assert_eq!(clamp_text_size_px(9000.0), 17.0);
        assert_eq!(clamp_text_size_px(3.0), 11.0);
        assert_eq!(clamp_scale(1.0500000000000003, 0.85, 1.3, 1.0), 1.05);
        assert_eq!(clamp_scale(9.0, 0.85, 1.3, 1.0), 1.3);
        assert_eq!(clamp_scale(f64::INFINITY, 0.85, 1.3, 1.0), 1.0);
        assert_eq!(TimeFormat::from_setting("24h"), TimeFormat::H24);
        assert_eq!(TimeFormat::from_setting("bogus"), TimeFormat::Auto);
    }

    #[gpui::test]
    fn a_drag_is_one_save_and_loading_is_none(cx: &mut TestAppContext) {
        let saved: Rc<RefCell<Vec<serde_json::Value>>> = Rc::default();
        let appearance = Rc::new(RefCell::new(0));
        let settings = cx.new(|_| {
            let saved = saved.clone();
            Settings::new(move |blob| saved.borrow_mut().push(blob))
        });
        let _sub = cx.update(|cx| {
            let appearance = appearance.clone();
            cx.subscribe(&settings, move |_, _: &SettingsEvent, _| *appearance.borrow_mut() += 1)
        });

        // Loading applies the appearance and saves nothing.
        let loaded = AppSettings { theme: "console".into(), text_size_px: 0.0, ..AppSettings::default() };
        settings.update(cx, |s, cx| s.apply_loaded(loaded, cx));
        cx.run_until_parked();
        assert_eq!(*appearance.borrow(), 1);
        settings.read_with(cx, |s, _| {
            assert_eq!(s.theme(), ThemeId::from_setting("console"));
            assert_eq!(s.text_size_px(), 14.5);
            assert_eq!(s.row_scale(), 1.0);
            assert!(!s.save_pending());
        });
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert!(saved.borrow().is_empty());

        // A slider drag: many changes, one write 250 ms after the last.
        for tick in 0..20 {
            settings.update(cx, |s, cx| s.change(|v| v.media_audio_volume = Some(tick as f64 / 20.0), cx));
            cx.executor().advance_clock(Duration::from_millis(100));
            cx.run_until_parked();
        }
        assert!(saved.borrow().is_empty(), "still inside the debounce");
        cx.executor().advance_clock(Duration::from_millis(250));
        cx.run_until_parked();
        assert_eq!(saved.borrow().len(), 1);
        assert_eq!(saved.borrow()[0]["media_audio_volume"], 0.95);
        assert_eq!(saved.borrow()[0]["theme"], "console");
        assert_eq!(*appearance.borrow(), 1, "a volume change is not an appearance change");
        settings.read_with(cx, |s, _| assert!(!s.save_pending(), "no timer at rest"));

        // flush writes a pending save at once; the timer then has nothing to do.
        settings.update(cx, |s, cx| {
            s.set_text_size_px(15.2, cx);
            s.flush();
        });
        assert_eq!(saved.borrow().len(), 2);
        assert_eq!(saved.borrow()[1]["text_size_px"], 15.0);
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(saved.borrow().len(), 2);
        assert_eq!(*appearance.borrow(), 2);
    }
}
