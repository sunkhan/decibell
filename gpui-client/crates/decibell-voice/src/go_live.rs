//! The Go Live dialog (`CaptureSourcePicker.tsx`, `goLive/*`): the destination, what will be
//! shared (on Linux the desktop's own portal dialog picks it after Go Live: there is no in-app
//! source grid), four quality presets, the options (share sound with the app picker, show the
//! cursor) and Advanced (resolution incl. "Source", frame rate 5–120, bitrate, codec, sound
//! bitrate). On a wide window the options sit in a panel beside the dialog.
//!
//! Go Live calls the model's `go_live` (the core opens the portal and the encoder); the dialog
//! shows "Starting…", the core's refusal, and closes once the stream is live. Presets the probe
//! says this machine can't encode (software-only encoders past their CPU ceiling) are greyed out.
//!
//! On Windows the dialog has its own grid of screens and windows (`goLive/SourceGrid.tsx`
//! over `desktopCapturer`): [`crate::sources`] lists them with a still each, on a background
//! thread, now and every [`SOURCES_POLL`] while the dialog is open and never otherwise; the
//! tile picked becomes `OwnStream::source_id`, which `go_live` hands to the core.

use std::collections::HashSet;
use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;
use std::time::Duration;

use decibell_common::Watch;
use decibell_model::live::voice::VideoCodec;
use decibell_model::live::voice::go_live::{
    STREAM_PRESETS, StreamAudioMode, StreamPreset, StreamQuality, StreamResolution, StreamSettings, format_bitrate,
    step_bitrate,
};
use decibell_model::live::voice::streams::OwnStreamPhase;
use decibell_model::{Model, actions};
use decibell_ui::{
    ActiveTheme as _, CloseRequest, FontRole, Icon, IconName, Label, Modal, SectionLabel, Segment, SegmentedControl,
    Slider, Spinner, Switch, title_tooltip,
};
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, ImageSource, IntoElement, ObjectFit, Render, RenderImage,
    SharedString, Subscription, Task, Window, div, img, prelude::*, px,
};

use crate::sources::{self, ListedSource, SourceKind};

use crate::codecs::Encoders;
use crate::state::VoiceUi;

const BITRATE_MIN: f32 = 1000.;
const BITRATE_MAX: f32 = 30000.;
/// The picker re-lists apps this often while it is on screen (apps start and stop playing).
pub const APPS_POLL: Duration = Duration::from_millis(2000);
/// The source grid lists again this often while the dialog is open (`SourceGrid.tsx`
/// `REFRESH_MS`): windows open and close, and the stills go stale.
pub const SOURCES_POLL: Duration = Duration::from_millis(5000);

/// A screen or window in the grid.
struct GridSource {
    id: SharedString,
    name: SharedString,
    kind: SourceKind,
    still: Option<Arc<RenderImage>>,
    /// A hash of the still's pixels: a listing that brings the same picture changes nothing.
    still_hash: u64,
    icon: Option<Arc<RenderImage>>,
    /// The icon was asked for once (a window without one is not asked every five seconds).
    icon_asked: bool,
}

fn pixels_hash(bitmap: &sources::Bitmap) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (bitmap.width, bitmap.height).hash(&mut hasher);
    bitmap.bgra.hash(&mut hasher);
    hasher.finish()
}

/// A listed bitmap as an image GPUI draws. `None` for one whose size and bytes disagree.
fn render_image(bitmap: sources::Bitmap) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bgra)?;
    Some(Arc::new(RenderImage::new(smallvec::SmallVec::from_elem(image::Frame::new(buffer), 1))))
}

pub struct GoLiveDialog {
    ui: Entity<VoiceUi>,
    open: bool,
    advanced: bool,
    /// The attempt this dialog started (closes when it goes live).
    started: bool,
    apps_timer: Option<Task<()>>,
    /// The source grid (Windows): which tab shows, what was listed last, and the task that
    /// lists while the dialog is open.
    source_tab: SourceKind,
    sources: Vec<GridSource>,
    sources_loaded: bool,
    sources_error: Option<SharedString>,
    sources_task: Option<Task<()>>,
    _fit: Subscription,
    _watch: Watch,
}

impl GoLiveDialog {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let ui = VoiceUi::global(cx);
        let encoders = Encoders::global(cx);
        let mut watch = Watch::new();
        watch
            .store_if(&ui, cx, |this: &mut Self, ui, _| this.open != ui.go_live || this.open)
            .store(&model.live.voice.own_stream, cx)
            .store_if(&model.settings, cx, |this: &mut Self, _, _| this.open)
            .store_if(&model.live.voice.codecs, cx, |this: &mut Self, _, _| this.open)
            .store_if(&model.live.voice.stream_audio_apps, cx, |this: &mut Self, _, _| this.open)
            .store_if(&encoders, cx, |this: &mut Self, _, _| this.open)
            .store_if(&model.live.voice.session, cx, |this: &mut Self, _, _| this.open);
        // Caps that arrive while the dialog is open may grey the chosen preset out.
        let fit = cx.observe(&encoders, |this, _, cx| {
            if this.open {
                fit_to_encoders(cx);
            }
        });
        // Closed until the first render sees it open (so an open-at-creation runs the opening).
        Self {
            ui,
            open: false,
            advanced: false,
            started: false,
            apps_timer: None,
            source_tab: SourceKind::Screen,
            sources: Vec::new(),
            sources_loaded: false,
            sources_error: None,
            sources_task: None,
            _fit: fit,
            _watch: watch,
        }
    }

    /// Lists the sources now and every [`SOURCES_POLL`] while `on` (the dialog is open on a
    /// platform with the grid). Off: the task is dropped and the stills are released, so a
    /// closed dialog costs no thread, no timer and no texture.
    fn keep_sources_fresh(&mut self, on: bool, cx: &mut Context<Self>) {
        let on = on && sources::supported();
        if on == self.sources_task.is_some() {
            return;
        }
        if !on {
            self.sources_task = None;
            for source in std::mem::take(&mut self.sources) {
                for image in [source.still, source.icon].into_iter().flatten() {
                    cx.drop_image(image, None);
                }
            }
            self.sources_loaded = false;
            self.sources_error = None;
            return;
        }
        self.sources_task = Some(cx.spawn(async |this, cx| {
            loop {
                // Stills for the tab on screen only; icons for windows not asked yet.
                let Ok((tab, asked)) = this.update(cx, |this, _| {
                    let asked: HashSet<String> =
                        this.sources.iter().filter(|s| s.icon_asked).map(|s| s.id.to_string()).collect();
                    (this.source_tab, asked)
                }) else {
                    break;
                };
                let listed = cx
                    .background_spawn(async move { sources::list(tab, &|id| !asked.contains(id)) })
                    .await;
                if this.update(cx, |this, cx| this.sources_listed(listed, cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(SOURCES_POLL).await;
            }
        }));
    }

    /// A listing arrived: the grid becomes it, keeping the stills and icons it did not bring
    /// (the other tab's, an unchanged one's) and releasing the ones it replaced.
    fn sources_listed(&mut self, listed: Result<Vec<ListedSource>, String>, cx: &mut Context<Self>) {
        let listed = match listed {
            Ok(listed) => listed,
            Err(error) => {
                log::warn!("go live: listing what can be shared failed: {error}");
                let error = Some(SharedString::from(error));
                if self.sources_error != error || !self.sources_loaded {
                    self.sources_error = error;
                    self.sources_loaded = true;
                    cx.notify();
                }
                return;
            }
        };
        let mut changed = !self.sources_loaded || self.sources_error.is_some();
        self.sources_loaded = true;
        self.sources_error = None;
        let mut old = std::mem::take(&mut self.sources);
        let mut dropped: Vec<Arc<RenderImage>> = Vec::new();
        for (position, source) in listed.into_iter().enumerate() {
            let previous = old.iter().position(|o| o.id.as_ref() == source.id).map(|index| old.swap_remove(index));
            changed |= previous.is_none();
            let mut entry = previous.unwrap_or_else(|| GridSource {
                id: source.id.clone().into(),
                name: source.name.clone().into(),
                kind: source.kind,
                still: None,
                still_hash: 0,
                icon: None,
                icon_asked: false,
            });
            if entry.name.as_ref() != source.name {
                entry.name = source.name.into();
                changed = true;
            }
            if let Some(still) = source.thumbnail {
                let hash = pixels_hash(&still);
                if entry.still.is_none() || entry.still_hash != hash {
                    dropped.extend(entry.still.take());
                    entry.still = render_image(still);
                    entry.still_hash = hash;
                    changed = true;
                }
            }
            if source.kind == SourceKind::Window && !entry.icon_asked {
                // This listing was asked for the icon, whether or not the window has one.
                entry.icon_asked = true;
                if let Some(icon) = source.icon.and_then(render_image) {
                    entry.icon = Some(icon);
                    changed = true;
                }
            }
            self.sources.push(entry);
            // The order is the listing's (screens, then windows front to back).
            changed |= self.sources.len() - 1 != position;
        }
        // What is left of the old list is gone from the desktop.
        changed |= !old.is_empty();
        for gone in old {
            dropped.extend(gone.still);
            dropped.extend(gone.icon);
        }
        for image in dropped {
            cx.drop_image(image, None);
        }
        if changed {
            cx.notify();
        }
    }

    fn show_source_tab(&mut self, tab: SourceKind, cx: &mut Context<Self>) {
        if self.source_tab != tab {
            self.source_tab = tab;
            // List again at once, with stills for this tab.
            self.sources_task = None;
            self.keep_sources_fresh(true, cx);
            cx.notify();
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.ui.update(cx, |u, cx| u.close_go_live(cx));
    }

    /// Re-list the stream-audio apps now and every [`APPS_POLL`] while the picker shows.
    fn keep_apps_fresh(&mut self, on: bool, cx: &mut Context<Self>) {
        if on == self.apps_timer.is_some() {
            return;
        }
        self.apps_timer = on.then(|| {
            cx.spawn(async |this, cx| {
                loop {
                    let alive = this.update(cx, |_, cx| refresh_apps(cx)).is_ok();
                    if !alive {
                        break;
                    }
                    cx.background_executor().timer(APPS_POLL).await;
                }
            })
        });
    }
}

/// `streamAudioAppsStore.refresh(sourceId)`.
pub fn refresh_apps(cx: &mut App) {
    let model = Model::global(cx).clone();
    let source = model.live.voice.own_stream.read(cx).source_id.clone();
    model.live.voice.stream_audio_apps.update(cx, |a, cx| a.refresh(source, cx));
}

fn settings(cx: &App) -> StreamSettings {
    StreamSettings::of(Model::global(cx).settings.read(cx).get())
}

/// On a machine whose only encoder is software, settings past its ceiling are greyed out (the
/// presets, Advanced's resolution and frame rate) and can't stay chosen: they become the
/// encodable preset with the most pixels a second (Data saver on a typical CPU), as a click on
/// that card would make them. The renderer has no software encoder and greys nothing; with
/// hardware encoders this does nothing.
fn fit_to_encoders(cx: &mut App) {
    let entity = Encoders::global(cx);
    let target = {
        let encoders = entity.read(cx);
        let s = settings(cx);
        if !encoders.software_only() || encoders.can_encode(s.resolution, s.fps) {
            return;
        }
        best_encodable_preset(|r, f| encoders.can_encode(r, f))
    };
    if let Some(preset) = target {
        log::info!("go live: {} fits this machine's software encoder: chosen", preset.label);
        actions::voice::update_stream_settings(|st| st.apply_preset(preset), cx);
    }
}

/// The encodable preset with the most pixels a second ("Source" judged at 1440p, as
/// [`Encoders::can_encode`] does).
fn best_encodable_preset(can_encode: impl Fn(StreamResolution, u32) -> bool) -> Option<&'static StreamPreset> {
    STREAM_PRESETS.iter().filter(|p| can_encode(p.resolution, p.fps)).max_by_key(|p| {
        let (w, h) = match p.resolution.request_size() {
            (0, 0) => (2560, 1440),
            size => size,
        };
        w as u64 * h as u64 * p.fps as u64
    })
}

/// The preset the settings match (`matchPreset`).
fn active_preset(s: &StreamSettings) -> Option<&'static StreamPreset> {
    s.preset()
}

impl Render for GoLiveDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("GoLiveDialog");
        let open = self.ui.read(cx).go_live;
        if open && !self.open {
            // Settings that match no preset open Advanced, where they were made.
            let forced = self.ui.read(cx).advanced_on_open;
            self.advanced = forced.unwrap_or_else(|| active_preset(&settings(cx)).is_none());
            self.started = false;
            // A greyed preset can't stay the choice.
            cx.defer(fit_to_encoders);
        }
        self.open = open;
        decibell_shell::probes::flag("go-live-open", open);
        let model = Model::global(cx).clone();
        let phase = model.live.voice.own_stream.read(cx).phase.clone();
        if open && self.started && matches!(phase, OwnStreamPhase::Live(_)) {
            // It went live: the dialog's job is done.
            self.started = false;
            let ui = self.ui.clone();
            cx.defer(move |cx| ui.update(cx, |u, cx| u.close_go_live(cx)));
        }
        let s = settings(cx);
        let sound_available = actions::voice::can_pick_stream_audio_apps();
        let share_audio = sound_available && s.share_audio;
        self.keep_apps_fresh(open && share_audio, cx);
        self.keep_sources_fresh(open, cx);
        let modal = Modal::new("go-live", open)
            .padding(24., 24.)
            .on_close(cx.listener(|this, _: &CloseRequest, _, cx| this.close(cx)));
        if !open {
            return modal.into_any_element();
        }
        let starting = phase == OwnStreamPhase::Starting;
        let error = model.live.voice.own_stream.read(cx).error.clone();
        // Windows (our own source grid, `NEEDS_CUSTOM_PICKER`): the renderer's `split` layout.
        let split = sources::supported();
        let viewport = f32::from(decibell_ui::visible_window_bounds(window, cx).size.width);
        // The options panel fits beside the dialog (`withPanel`: 74rem with the grid, 55rem without).
        let wide = viewport >= if split { 1184. } else { 880. };
        // Tailwind's `lg:` (the grid fills its column, three window columns, side by side).
        let lg = viewport >= 1024.;
        let picked = split.then(|| model.live.voice.own_stream.read(cx).source_id.clone()).flatten();
        let picked_source = picked.as_deref().and_then(|id| self.sources.iter().find(|s| s.id.as_ref() == id)).map(|s| (s.name.clone(), s.still.clone()));
        let this = cx.entity().downgrade();

        let presets = quality_presets(&s, starting, false, cx);
        let presets_row = (wide && split).then(|| quality_presets(&s, starting, true, cx));
        let options = option_rows(&s, sound_available, starting, cx);
        let advanced = advanced(&s, self.advanced, share_audio, starting, wide, this.clone(), cx);
        let panel_rows = wide.then(|| {
            let rows = options_for_panel(&s, sound_available, starting, cx);
            let advanced = advanced_panel(&s, self.advanced, share_audio, starting, this.clone(), cx);
            (options_divided(rows, cx), advanced)
        });
        let go_live_click = cx.listener(|this, _: &ClickEvent, _, cx| {
            this.started = true;
            actions::voice::go_live(cx);
        });
        let close_click = cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx));
        let cancel_click = cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx));
        let source = if split {
            self.source_grid(starting, lg, cx)
        } else {
            system_picker_note(cx).into_any_element()
        };
        let theme = cx.theme();
        let summary = {
            let mut text = match active_preset(&s) {
                Some(p) => format!("{} · {}", p.label, p.spec),
                None => format!("Custom · {} · {} fps", s.resolution.label(), s.fps),
            };
            if share_audio {
                text.push_str(" · with sound");
            }
            text
        };

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3p5()
            .px_5()
            .pt_5()
            .pb_4()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .size_10()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_lg)
                    .bg(theme.color_accent_soft)
                    .child(Icon::new(IconName::SettingsScreenShare).size(px(19.)).color(theme.color_accent)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new("Go live").font(FontRole::Display).size(16.).line_height(lh(16., cx)).weight(600.).color(theme.color_text_bright))
                    .children(destination(cx)),
            )
            .child(
                div()
                    .id("go-live-close")
                    .group("go-live-close")
                    .flex()
                    .flex_none()
                    .self_start()
                    .size_8()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_sm)
                    .hover(|st| st.bg(theme.color_surface_hover))
                    .cursor_pointer()
                    .tooltip(title_tooltip("Close"))
                    .on_click(close_click)
                    .relative()
                    .children(decibell_shell::probes::probe("go-live-close"))
                    .child(
                        Icon::new(IconName::SettingsX)
                            .size(px(16.))
                            .color(theme.color_text_muted)
                            .group_hover("go-live-close", theme.color_text_secondary),
                    ),
            );

        let body: AnyElement = if wide && split {
            // The grid over a row of the four quality cards; only the grid scrolls.
            div()
                .id("go-live-body")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .border_t_1()
                .border_color(theme.color_border_divider)
                .child(source)
                .child(
                    div()
                        .flex_none()
                        .border_t_1()
                        .border_color(theme.color_border_divider)
                        .px_5()
                        .pt_4()
                        .pb_5()
                        .child(field("Quality", None, None, presets_row.expect("wide split"), cx)),
                )
                .into_any_element()
        } else if split && lg {
            // One card, the grid beside a 400 px column of settings (`lg:flex-row`).
            div()
                .id("go-live-body")
                .flex()
                .flex_1()
                .min_h_0()
                .border_t_1()
                .border_color(theme.color_border_divider)
                .child(source)
                .child(
                    div()
                        .id("go-live-settings")
                        .flex()
                        .flex_none()
                        .flex_col()
                        .gap_5()
                        .w(px(400.))
                        .px_5()
                        .pt_5()
                        .pb_5()
                        .border_l_1()
                        .border_color(theme.color_border_divider)
                        .overflow_y_scroll()
                        .child(field("Quality", None, None, presets, cx))
                        .child(group("Options", options.into_iter().map(|r| r.into_any_element()).collect(), cx))
                        .child(advanced),
                )
                .into_any_element()
        } else if wide {
            div()
                .id("go-live-body")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(source)
                .child(div().flex_none().px_5().pb_5().child(field("Quality", None, None, presets, cx)))
                .into_any_element()
        } else {
            div()
                .id("go-live-body")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(source)
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .flex_col()
                        .gap_5()
                        .px_5()
                        .pb_5()
                        .child(field("Quality", None, None, presets, cx))
                        .child(group("Options", options.into_iter().map(|r| r.into_any_element()).collect(), cx))
                        .child(advanced),
                )
                .into_any_element()
        };

        let go_live_button = div()
            .id("go-live-start")
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .rounded(theme.radius_sm)
            .bg(theme.color_accent)
            .px_4()
            .py_2()
            // `disabled={starting || (NEEDS_CUSTOM_PICKER && !picked)}`.
            .when(starting || (split && picked.is_none()), |this| this.opacity(0.5))
            .when(!starting && !(split && picked.is_none()), |this| {
                this.hover(|st| st.bg(theme.color_accent_hover)).cursor_pointer().on_click(go_live_click)
            })
            .child(if starting {
                Spinner::ring().size(px(14.)).color(theme.color_on_accent).into_any_element()
            } else {
                Icon::new(IconName::SettingsBroadcast).size(px(15.)).color(theme.color_on_accent).into_any_element()
            })
            .child(Label::new(if starting { "Starting…" } else { "Go Live" }).size(13.).weight(600.).color(theme.color_on_accent));
        let footer = div()
            .flex_none()
            .border_t_1()
            .border_color(theme.color_border_divider)
            .px_5()
            .py_3p5()
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .id("go-live-error")
                        .mb_3()
                        .flex()
                        .items_start()
                        .gap_2()
                        .rounded(theme.radius_md)
                        .bg(theme.color_error.opacity(0.1))
                        .px_3()
                        .py_2()
                        .tooltip(title_tooltip(error.clone()))
                        .child(div().mt(px(1.)).child(Icon::new(IconName::SettingsAlert).size(px(14.)).color(theme.color_error)))
                        .child(div().min_w_0().child(Label::new(error).size(12.).color(theme.color_error).clamp(3))),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex().flex_1().min_w_0().items_center().gap_2p5().map(|this| {
                        if !split {
                            this.child(Label::new(SharedString::from(summary)).size(12.).line_height(lh(12., cx)).color(theme.color_text_muted).truncate())
                        } else if let Some((name, still)) = picked_source {
                            // The picked source: its still (`h-7 w-12 object-contain`), its name over the summary.
                            this.when_some(still, |this, still| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .w_12()
                                        .h_7()
                                        .rounded(theme.radius_sm)
                                        .bg(theme.color_bg_darkest)
                                        .overflow_hidden()
                                        .child(img(ImageSource::Render(still)).size_full().object_fit(ObjectFit::Contain)),
                                )
                            })
                            .child(
                                div()
                                    .id("go-live-picked")
                                    .min_w_0()
                                    .tooltip(title_tooltip(name.clone()))
                                    .relative()
                                    .children(decibell_shell::probes::probe("go-live-picked"))
                                    .child(Label::new(name).size(13.).weight(500.).color(theme.color_text_primary).truncate())
                                    .child(Label::new(SharedString::from(summary)).size(12.).color(theme.color_text_muted).truncate()),
                            )
                        } else {
                            this.child(Label::new("Pick a screen or window to share.").size(12.).line_height(lh(12., cx)).color(theme.color_text_muted).truncate())
                        }
                    }))
                    .child(
                        div()
                            .id("go-live-cancel")
                            .flex_none()
                            .rounded(theme.radius_sm)
                            .border_1()
                            .border_color(theme.color_border)
                            .px_4()
                            .py_2()
                            .hover(|st| st.bg(theme.color_surface_hover))
                            .cursor_pointer()
                            .on_click(cancel_click)
                            .child(Label::new("Cancel").size(13.).weight(500.).color(theme.color_text_primary)),
                    )
                    .child(go_live_button),
            );

        let max_h = decibell_ui::visible_window_bounds(window, cx).size.height - px(48.);
        let card = div()
            .id("go-live-card")
            .occlude()
            .flex()
            .flex_col()
            .max_h(max_h)
            .overflow_hidden()
            .rounded(theme.radius_xl)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_secondary)
            .shadow(theme.shadow_modal.clone())
            .map(|this| match (wide, split) {
                // `w-[760px] h-[min(720px,calc(100vh-3rem))]`
                (true, true) => this.w(px(760.)).h(max_h.min(px(720.))),
                (true, false) => this.w(px(460.)),
                // `w-full max-w-[560px] lg:h-[min(760px,calc(100vh-3rem))] lg:max-w-[1040px]`
                (false, true) if lg => this.w_full().max_w(px(1040.)).h(max_h.min(px(760.))),
                (false, true) => this.w_full().max_w(px(560.)),
                (false, false) => this.w_full().max_w(px(520.)),
            })
            .child(header)
            .child(body)
            .child(footer);
        let panel = panel_rows.map(|(rows, advanced)| {
            div()
                .id("go-live-options")
                .occlude()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(336.))
                // Never taller than the Windows card beside it.
                .max_h(if split { max_h.min(px(720.)) } else { max_h })
                .overflow_hidden()
                .rounded(theme.radius_xl)
                .border_1()
                .border_color(theme.color_border)
                .bg(theme.color_bg_secondary)
                .shadow(theme.shadow_float.clone())
                .child(
                    div()
                        .flex_none()
                        .px_4()
                        .pt_4()
                        .pb_3()
                        .child(Label::new("Options").font(FontRole::Display).size(14.).weight(600.).color(theme.color_text_bright))
                        .child(div().mt_0p5().child(Label::new("Optional. The defaults suit most streams.").size(12.).color(theme.color_text_muted))),
                )
                .child(
                    div()
                        .id("go-live-options-body")
                        .min_h_0()
                        .overflow_y_scroll()
                        .border_t_1()
                        .border_color(theme.color_border_divider)
                        .children(rows)
                        .child(div().border_t_1().border_color(theme.color_border_divider).child(advanced)),
                )
        });
        modal
            .child(div().flex().w_full().items_start().justify_center().gap_3().child(card).children(panel))
            .into_any_element()
    }
}

impl GoLiveDialog {
    /// "What to share" (`goLive/SourceGrid.tsx`): the Screens / Windows tabs and a grid of
    /// tiles, each a still in a 16:9 frame over the source's name. A click picks the source.
    fn source_grid(&self, disabled: bool, lg: bool, cx: &mut Context<Self>) -> AnyElement {
        let mut to_tab = [SourceKind::Screen, SourceKind::Window]
            .map(|kind| Some(cx.listener(move |this, _: &ClickEvent, _, cx| this.show_source_tab(kind, cx))));
        let theme = cx.theme();
        let tab = self.source_tab;
        let picked = Model::global(cx).live.voice.own_stream.read(cx).source_id.clone();
        let count = |kind: SourceKind| self.sources.iter().filter(|s| s.kind == kind).count();
        let mut tab_button = |kind: SourceKind, label: &'static str, icon: IconName| {
            let click = to_tab[kind as usize].take();
            let active = tab == kind;
            let color = if active { theme.color_accent_bright } else { theme.color_text_muted };
            div()
                .id(label)
                .group(label)
                .flex()
                .items_center()
                .gap_1p5()
                .rounded(theme.radius_sm)
                .px_3()
                .py_1()
                .when(active, |this| this.bg(theme.color_accent_mid))
                .when(!active, |this| this.cursor_pointer())
                .when_some(click, |this, click| this.on_click(click))
                .relative()
                .children(decibell_shell::probes::probe(format!("source-tab:{label}")))
                .child(Icon::new(icon).size(px(13.)).color(color).when(!active, |icon| icon.group_hover(label, theme.color_text_secondary)))
                .child(
                    Label::new(label)
                        .size(12.)
                        .weight(600.)
                        .color(color),
                )
                .when(self.sources_loaded, |this| {
                    this.child(
                        div().opacity(0.7).child(
                            Label::new(SharedString::from(count(kind).to_string()))
                                .font(FontRole::Meta)
                                .size(12.)
                                .weight(500.)
                                .color(color),
                        ),
                    )
                })
        };
        let tabs = div()
            .flex()
            .rounded(theme.radius_md)
            .bg(theme.color_bg_darkest)
            .p(px(3.))
            .child(tab_button(SourceKind::Screen, "Screens", IconName::SettingsMonitor))
            .child(tab_button(SourceKind::Window, "Windows", IconName::SettingsAppWindow));
        let header = div()
            .flex()
            .flex_none()
            .h_8()
            .items_center()
            .justify_between()
            .gap_3()
            .child(SectionLabel::new("What to share"))
            .child(tabs);

        let shown: Vec<&GridSource> = self.sources.iter().filter(|s| s.kind == tab).collect();
        // `grid-cols-2` for screens (few and wide), `grid-cols-2 lg:grid-cols-3` for windows.
        let columns = if tab == SourceKind::Window && lg { 3 } else { 2 };
        let content: AnyElement = if !self.sources_loaded {
            // The renderer's skeleton: two tiles for screens, six for windows.
            let tiles = if tab == SourceKind::Screen { 2 } else { 6 };
            div()
                .grid()
                .grid_cols(columns)
                .gap_2()
                .children((0..tiles).map(|_| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_1p5()
                        .child(div().w_full().aspect_ratio(16. / 9.).rounded(theme.radius_md).bg(theme.color_bg_light))
                        .child(div().mx_1().h_3().w_2_3().rounded(theme.radius_sm).bg(theme.color_bg_light))
                }))
                .into_any_element()
        } else if let Some(error) = &self.sources_error {
            div()
                .py_6()
                .flex()
                .justify_center()
                .child(Label::new(error.clone()).size(12.).color(theme.color_error).clamp(3))
                .into_any_element()
        } else if shown.is_empty() {
            let (icon, text) = match tab {
                SourceKind::Screen => (IconName::SettingsMonitor, "No screens to share"),
                SourceKind::Window => (IconName::SettingsAppWindow, "No windows to share"),
            };
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .px_6()
                .py_10()
                .child(Icon::new(icon).size(px(22.)).color(theme.color_text_muted))
                .child(Label::new(text).size(13.).weight(500.).color(theme.color_text_secondary))
                .when(tab == SourceKind::Window, |this| {
                    this.child(
                        Label::new("Open the app you want to show. It turns up here in a few seconds.")
                            .size(12.)
                            .color(theme.color_text_muted),
                    )
                })
                .into_any_element()
        } else {
            div()
                .id("go-live-sources")
                .grid()
                .grid_cols(columns)
                .gap_2()
                // `max-h-[300px] … lg:max-h-none lg:min-h-0 lg:flex-1`: the column's height, only
                // the grid scrolling, where the dialog lays it out side by side.
                .map(|this| if lg { this.min_h_0().flex_1() } else { this.max_h(px(300.)) })
                .overflow_y_scroll()
                .pr_1()
                .when(disabled, |this| this.opacity(0.6))
                .children(shown.into_iter().map(|source| {
                    let on = picked.as_deref() == Some(source.id.as_ref());
                    let id = source.id.clone();
                    let group = SharedString::from(format!("source:{id}"));
                    let frame = div()
                        .relative()
                        .w_full()
                        .aspect_ratio(16. / 9.)
                        .overflow_hidden()
                        .rounded(theme.radius_md)
                        .border_2()
                        .bg(theme.color_bg_darkest)
                        .map(|this| {
                            if on {
                                this.border_color(theme.color_accent)
                            } else {
                                this.border_color(theme.color_border_divider)
                                    .group_hover(group.clone(), |st| st.border_color(theme.color_border))
                            }
                        })
                        .when_some(source.still.clone(), |this, still| {
                            this.child(img(ImageSource::Render(still)).size_full().object_fit(ObjectFit::Contain))
                        })
                        .when(on, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .top_1p5()
                                    .right_1p5()
                                    .flex()
                                    .size_5()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(theme.color_accent)
                                    .child(Icon::new(IconName::SettingsCheck).size(px(12.)).color(theme.color_on_accent)),
                            )
                        });
                    let mark: AnyElement = match source.icon.clone() {
                        Some(icon) => img(ImageSource::Render(icon)).flex_none().size_4().into_any_element(),
                        None => div()
                            .flex_none()
                            .child(
                                Icon::new(if source.kind == SourceKind::Screen {
                                    IconName::SettingsMonitor
                                } else {
                                    IconName::SettingsAppWindow
                                })
                                .size(px(14.))
                                .color(if on { theme.color_accent } else { theme.color_text_muted }),
                            )
                            .into_any_element(),
                    };
                    let name = Label::new(source.name.clone()).size(13.).truncate();
                    let name = if on {
                        name.weight(600.).color(theme.color_text_bright)
                    } else {
                        name.weight(500.).color(theme.color_text_secondary)
                    };
                    div()
                        .id(SharedString::from(format!("source-tile:{id}")))
                        .group(group.clone())
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .gap_2()
                        .rounded(theme.radius_lg)
                        .p_1p5()
                        .map(|this| if on { this.bg(theme.color_accent_soft) } else { this.hover(|st| st.bg(theme.color_surface_hover)) })
                        .when(!disabled, |this| {
                            this.cursor_pointer().on_click(move |_: &ClickEvent, _, cx| {
                                let model = Model::global(cx).clone();
                                model.live.voice.own_stream.update(cx, |own, cx| own.set_source(Some(id.to_string()), cx));
                                // The app list marks the app that owns the picked window.
                                refresh_apps(cx);
                            })
                        })
                        .tooltip(title_tooltip(source.name.clone()))
                        .relative()
                        .children(decibell_shell::probes::probe(format!("source:{}", source.id)))
                        .child(frame)
                        .child(
                            div()
                                .flex()
                                .w_full()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .px_1()
                                .pb_0p5()
                                .child(mark)
                                .child(div().min_w_0().child(name)),
                        )
                }))
                .into_any_element()
        };
        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap_3()
            .px_5()
            .pb_5()
            // `lg:min-w-0 lg:flex-1 lg:pt-5`
            .when(lg, |this| this.min_w_0().flex_1().min_h_0().pt_5())
            .child(header)
            .child(content)
            .when(tab == SourceKind::Screen && self.sources_loaded && self.sources_error.is_none(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .child(Icon::new(IconName::SettingsAppWindow).size(px(14.)).color(theme.color_text_muted))
                        .child(
                            Label::new("Sharing one window keeps notifications and other apps out of view.")
                                .size(12.)
                                .color(theme.color_text_muted),
                        ),
                )
            })
            .into_any_element()
    }
}

fn system_picker_note(cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let system = if cfg!(target_os = "macos") { "macOS" } else { "your system" };
    div()
        .mx_5()
        .mb_5()
        .flex()
        .flex_none()
        .items_center()
        .gap_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.color_border_divider)
        .bg(theme.color_bg_light)
        .px_4()
        .py_3()
        .child(system_picker_art(cx))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(Label::new("You'll pick a screen or window next").size(14.).line_height(lh(14., cx)).weight(500.).color(theme.color_text_primary))
                .child(
                    div().mt_0p5().child(
                        Label::new(SharedString::from(format!("After you press Go Live, {system} asks what to share.")))
                            .size(12.)
                            .line_height(18.6)
                            .color(theme.color_text_muted),
                    ),
                ),
        )
}

/// `SystemPickerArt`: a screen with a window in front of it, in four single-colour layers (the
/// SVG's fills and strokes in their tokens).
fn system_picker_art(cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    // The 56 × 44 drawing sits in a 56-unit square box, 6 units down.
    let layer = |name: IconName, color| div().absolute().top(px(-6.)).left_0().child(Icon::new(name).size(px(56.)).color(color));
    div()
        .relative()
        .flex_none()
        .w(px(56.))
        .h(px(44.))
        .child(layer(IconName::VoicePickerScreenFill, theme.color_bg_lighter))
        .child(layer(IconName::VoicePickerScreen, theme.color_text_muted))
        .child(layer(IconName::VoicePickerWindowFill, theme.color_bg_light))
        .child(layer(IconName::VoicePickerWindow, theme.color_accent))
}

/// The destination under the title (`Destination`): "Lounge · Daily Gaming", or the call.
fn destination(cx: &App) -> Option<impl IntoElement + use<>> {
    let model = Model::global(cx);
    let session = model.live.voice.session.read(cx);
    let theme = cx.theme();
    let (icon, text): (IconName, SharedString) = if let Some((server, channel)) = session.connected() {
        let server_name = model.servers.read(cx).get(server).map(|s| s.name.clone());
        let channel_name = model.community(server, cx).and_then(|c| c.channels.read(cx).get(&channel).map(|c| c.name.clone()));
        let parts: Vec<String> = [channel_name, server_name].into_iter().flatten().map(|s| s.to_string()).collect();
        (IconName::SettingsSpeaker, parts.join(" · ").into())
    } else {
        let peer = session.call_peer?;
        let name = model.name_of(peer, &SharedString::default(), cx);
        if name.is_empty() {
            return None;
        }
        (IconName::SettingsUser, format!("In your call with {name}").into())
    };
    if text.is_empty() {
        return None;
    }
    Some(
        div()
            .mt_0p5()
            .flex()
            .min_w_0()
            .items_center()
            .gap_1p5()
            .child(Icon::new(icon).size(px(13.)).color(theme.color_text_muted))
            .child(div().min_w_0().child(Label::new(text).size(12.).line_height(lh(12., cx)).color(theme.color_text_muted).truncate())),
    )
}

/// `Field`: a section label (with an optional aside) over the control, an optional hint.
fn field(label: &'static str, aside: Option<AnyElement>, hint: Option<AnyElement>, child: impl IntoElement, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .child(
            div()
                .mb_2p5()
                .flex()
                .h_3()
                .items_center()
                .justify_between()
                .gap_3()
                .child(SectionLabel::new(label))
                .children(aside),
        )
        .child(child)
        .when_some(hint, |this, hint| this.child(div().mt_2().child(hint)))
        .text_color(theme.color_text_muted)
}

/// `Group`: a labelled card of rows split by hairlines.
fn group(label: &'static str, rows: Vec<AnyElement>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .child(div().mb_2p5().flex().h_3().items_center().child(SectionLabel::new(label)))
        .child(
            div()
                .overflow_hidden()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.color_border_divider)
                .bg(theme.color_bg_light)
                .children(options_divided(rows, cx)),
        )
}

/// Rows with a hairline between each (`divide-y divide-border-divider`).
fn options_divided(rows: Vec<AnyElement>, cx: &App) -> Vec<AnyElement> {
    let divider = cx.theme().color_border_divider;
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| div().when(i > 0, |this| this.border_t_1().border_color(divider)).child(row).into_any_element())
        .collect()
}

/// The body's line height at `size` (`leading-body`: what a label without its own leading
/// inherits in the renderer; GPUI's default is 1.5).
fn lh(size: f32, cx: &App) -> f32 {
    size * cx.theme().palette.leading_body
}

fn hint_text(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    Label::new(text).size(12.).line_height(18.6).color(cx.theme().color_text_muted).into_any_element()
}

/// `QualityPresets`: four cards, two by two; the sentence under them is the picked preset's
/// blurb, or "Custom: …, set under Advanced." Presets this machine can't encode are greyed.
/// `QualityPresets`: two by two, or the four in one row (`row`: the Windows card beside its
/// options panel).
fn quality_presets(s: &StreamSettings, disabled: bool, row: bool, cx: &mut Context<GoLiveDialog>) -> impl IntoElement + use<> {
    let encoders = Encoders::global(cx).read(cx);
    let theme = cx.theme();
    let active = active_preset(s).map(|p| p.id);
    let cards: Vec<AnyElement> = STREAM_PRESETS
        .iter()
        .map(|p| {
            let on = active == Some(p.id);
            let possible = encoders.can_encode(p.resolution, p.fps);
            let off = disabled || !possible;
            let icon = match p.icon {
                "gamepad" => IconName::SettingsGamepad,
                "text" => IconName::SettingsText,
                "presentation" => IconName::SettingsPresentation,
                _ => IconName::SettingsGauge,
            };
            let (border, bg) = if on { (theme.color_accent, theme.color_accent_soft) } else { (theme.color_border_divider, theme.color_bg_light) };
            let hover = theme.color_bg_lighter;
            let preset: &'static StreamPreset = p;
            div()
                .id(p.id)
                .flex()
                .min_w_0()
                .items_center()
                .gap_3()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(border)
                .bg(bg)
                .px_3()
                .py_2p5()
                .when(off, |this| this.opacity(0.6))
                .when(!possible, |this| {
                    this.tooltip(title_tooltip("This computer can't encode this quality: its only encoder is software."))
                })
                .when(!off, |this| {
                    this.cursor_pointer()
                        .when(!on, |this| this.hover(move |st| st.bg(hover)))
                        .on_click(move |_: &ClickEvent, _, cx| actions::voice::update_stream_settings(|st| st.apply_preset(preset), cx))
                })
                .relative()
                .children(decibell_shell::probes::probe(format!("preset:{}", p.id)))
                .child(Icon::new(icon).size(px(18.)).color(if on { theme.color_accent } else { theme.color_text_muted }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            Label::new(p.label)
                                .size(13.)
                                .line_height(lh(13., cx))
                                .weight(600.)
                                .color(if on { theme.color_text_bright } else { theme.color_text_primary })
                                .truncate(),
                        )
                        .child(Label::new(p.spec).size(12.).line_height(lh(12., cx)).color(theme.color_text_muted).truncate()),
                )
                .into_any_element()
        })
        .collect();
    let grid: AnyElement = if row {
        div().flex().gap_2().children(cards.into_iter().map(|c| div().flex_1().min_w_0().child(c))).into_any_element()
    } else {
        let mut cards = cards.into_iter();
        let pair = |a: Option<AnyElement>, b: Option<AnyElement>| {
            div().flex().gap_2().child(div().flex_1().min_w_0().children(a)).child(div().flex_1().min_w_0().children(b))
        };
        let first = pair(cards.next(), cards.next());
        let second = pair(cards.next(), cards.next());
        div().flex().flex_col().gap_2().child(first).child(second).into_any_element()
    };
    let note: AnyElement = match active_preset(s) {
        Some(p) => hint_text(p.blurb, cx),
        None => div()
            .flex()
            .flex_wrap()
            .child(Label::new("Custom:").size(12.).weight(500.).color(theme.color_text_secondary))
            .child(hint_text(
                format!(" {} · {} fps · {}, set under Advanced.", s.resolution.label(), s.fps, format_bitrate(s.video_bitrate_kbps)),
                cx,
            ))
            .into_any_element(),
    };
    div().child(grid).child(div().mt_2p5().child(note))
}

/// `Row`: an icon tile, the title and description, the control; `below` spans the width.
fn option_row(icon: IconName, title: &'static str, description: SharedString, control: AnyElement, below: Option<AnyElement>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .px_4()
        .py_3p5()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3p5()
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .size_9()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius_md)
                        .bg(theme.color_bg_lighter)
                        .child(Icon::new(icon).size(px(17.)).color(theme.color_text_secondary)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(Label::new(title).size(14.).line_height(lh(14., cx)).weight(500.).color(theme.color_text_primary))
                        .child(div().mt_0p5().child(hint_text(description, cx))),
                )
                .child(control),
        )
        .children(below)
}

/// The two option rows: Share sound (with the app picker under it) and Show my cursor.
fn option_rows(s: &StreamSettings, sound_available: bool, starting: bool, cx: &mut Context<GoLiveDialog>) -> Vec<AnyElement> {
    options_for_panel(s, sound_available, starting, cx)
}

fn options_for_panel(s: &StreamSettings, sound_available: bool, starting: bool, cx: &mut Context<GoLiveDialog>) -> Vec<AnyElement> {
    let model = Model::global(cx).clone();
    let include_cursor = model.live.voice.own_stream.read(cx).include_cursor;
    let share_audio = sound_available && s.share_audio;
    let sound_description: SharedString = if !sound_available {
        if cfg!(target_os = "macos") { "Not available on macOS yet." } else { "Needs hardware video encoding, which isn't available here." }.into()
    } else if s.share_audio {
        "Your apps, never your voice chat.".into()
    } else {
        "Viewers get video only.".into()
    };
    let picker = share_audio.then(|| div().mt_3p5().child(audio_app_picker(false, cx)).into_any_element());
    let sound = option_row(
        IconName::SettingsSpeaker,
        "Share sound",
        sound_description,
        Switch::new("go-live-sound", share_audio)
            .disabled(!sound_available || starting)
            .on_toggle(|_, _, cx| actions::voice::update_stream_settings(|st| st.share_audio = !st.share_audio, cx))
            .into_any_element(),
        picker,
        cx,
    );
    let cursor = option_row(
        IconName::SettingsPointer,
        "Show my cursor",
        "Viewers see where you point.".into(),
        Switch::new("go-live-cursor", include_cursor)
            .disabled(starting)
            .on_toggle(|on, _, cx| {
                let model = Model::global(cx).clone();
                let on = *on;
                model.live.voice.own_stream.update(cx, |o, cx| o.set_include_cursor(on, cx));
            })
            .into_any_element(),
        None,
        cx,
    );
    vec![sound.into_any_element(), cursor.into_any_element()]
}

fn codec_label(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::Av1 => "AV1",
        VideoCodec::H265 => "H.265",
        VideoCodec::H264Hw => "H.264",
        VideoCodec::H264Sw => "H.264 SW",
        VideoCodec::Unknown => "Auto",
    }
}

/// `AdvancedSettings` as a card of its own (the one-card dialog).
fn advanced(
    s: &StreamSettings,
    open: bool,
    show_sound_bitrate: bool,
    disabled: bool,
    flat: bool,
    this: gpui::WeakEntity<GoLiveDialog>,
    cx: &mut Context<GoLiveDialog>,
) -> AnyElement {
    if flat {
        // On the wide layout Advanced lives in the options panel.
        return div().into_any_element();
    }
    let theme = cx.theme();
    let (border, bg) = (theme.color_border_divider, theme.color_bg_light);
    let radius = theme.radius_lg;
    div()
        .overflow_hidden()
        .rounded(radius)
        .border_1()
        .border_color(border)
        .bg(bg)
        .child(advanced_panel(s, open, show_sound_bitrate, disabled, this, cx))
        .into_any_element()
}

/// The disclosure and, open, the raw stream knobs.
fn advanced_panel(
    s: &StreamSettings,
    open: bool,
    show_sound_bitrate: bool,
    disabled: bool,
    this: gpui::WeakEntity<GoLiveDialog>,
    cx: &mut Context<GoLiveDialog>,
) -> AnyElement {
    let model = Model::global(cx).clone();
    let codecs = model.live.voice.codecs.read(cx);
    let encode_caps = codecs.encode.clone();
    let encoders = Encoders::global(cx);
    let planned = encoders.read(cx).planned(s).map(|e| e.label.clone());
    let theme = cx.theme();
    let forced = s.enforced_codec != VideoCodec::Unknown;
    let summary = [
        s.resolution.label().to_string(),
        format!("{} fps", s.fps),
        format_bitrate(s.video_bitrate_kbps),
        if forced { codec_label(s.enforced_codec).to_string() } else { "Auto codec".into() },
    ]
    .join(" · ");
    let toggle = div()
        .id("go-live-advanced")
        .flex()
        .w_full()
        .items_center()
        .gap_3p5()
        .px_4()
        .py_3()
        .hover(|st| st.bg(theme.color_surface_hover))
        .cursor_pointer()
        .on_click(move |_: &ClickEvent, _, cx| {
            let _ = this.update(cx, |d, cx| {
                d.advanced = !d.advanced;
                cx.notify();
            });
        })
        .child(
            div()
                .flex()
                .flex_none()
                .size_9()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .bg(theme.color_bg_lighter)
                .child(Icon::new(IconName::SettingsSliders).size(px(17.)).color(theme.color_text_secondary)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(Label::new("Advanced").size(14.).line_height(lh(14., cx)).weight(500.).color(theme.color_text_primary))
                .child(div().mt_0p5().child(Label::new(SharedString::from(summary)).size(12.).line_height(lh(12., cx)).color(theme.color_text_muted).truncate())),
        )
        // `rotate-180` while open: GPUI can't rotate a box, so the glyph is swapped.
        .child(div().flex_none().child(
            Icon::new(if open { IconName::VoiceChevronUp } else { IconName::SettingsChevronDown }).size(px(16.)).color(theme.color_text_muted),
        ));
    if !open {
        return toggle.into_any_element();
    }
    let mut codec_segments = vec![Segment::new(VideoCodec::Unknown, "Auto")];
    for c in &encode_caps {
        let sw = c.codec != VideoCodec::H264Sw && !c.hardware;
        codec_segments.push(Segment::new(c.codec, SharedString::from(format!("{}{}", codec_label(c.codec), if sw { " SW" } else { "" }))));
    }
    // A software-only machine greys what its encoder can't keep up with, as the presets do.
    let resolution = SegmentedControl::new(
        "go-live-resolution",
        [StreamResolution::P720, StreamResolution::P1080, StreamResolution::Source]
            .into_iter()
            .map(|r| Segment::new(r, r.label()).disabled(!encoders.read(cx).can_encode(r, s.fps)))
            .collect(),
        s.resolution,
    )
    .disabled(disabled)
    .on_change(|v, _, cx| {
        let v = *v;
        actions::voice::update_stream_settings(
            |st| {
                st.resolution = v;
                if st.quality != StreamQuality::Custom {
                    st.video_bitrate_kbps = step_bitrate(v, st.quality);
                }
            },
            cx,
        )
    });
    let fps = SegmentedControl::new(
        "go-live-fps",
        [5u32, 15, 30, 60, 120]
            .into_iter()
            .map(|f| Segment::new(f, SharedString::from(f.to_string())).disabled(!encoders.read(cx).can_encode(s.resolution, f)))
            .collect(),
        s.fps,
    )
    .disabled(disabled)
    .on_change(|v, _, cx| {
        let v = *v;
        actions::voice::update_stream_settings(|st| st.fps = v, cx)
    });
    let bitrate = Slider::new("go-live-bitrate", s.video_bitrate_kbps as f32, BITRATE_MIN, BITRATE_MAX)
        .step(500.)
        .fill()
        .disabled(disabled)
        .on_change(|v, _, cx| {
            let v = *v as u32;
            actions::voice::update_stream_settings(
                |st| {
                    st.quality = StreamQuality::Custom;
                    st.video_bitrate_kbps = v;
                },
                cx,
            )
        });
    let codec = SegmentedControl::new("go-live-codec", codec_segments, s.enforced_codec)
        .disabled(disabled)
        .on_change(|v, _, cx| {
            let v = *v;
            actions::voice::update_stream_settings(|st| st.enforced_codec = v, cx)
        });
    let codec_hint: AnyElement = if forced {
        div()
            .flex()
            .items_start()
            .gap_1p5()
            .child(div().mt_0p5().child(Icon::new(IconName::SettingsAlert).size(px(13.)).color(theme.color_warning)))
            .child(
                Label::new(SharedString::from(format!(
                    "Viewers who can't play {} won't be able to watch.",
                    codec_label(s.enforced_codec)
                )))
                .size(12.)
                .line_height(18.6)
                .color(theme.color_warning),
            )
            .into_any_element()
    } else {
        hint_text("Auto streams in a codec every viewer can play.", cx)
    };
    // Which encoder the stream will use (the core's probe): shown under the codec choice.
    let encoder_hint = planned.map(|label| hint_text(format!("Encoder: {label}"), cx));
    let sound_bitrate = show_sound_bitrate.then(|| {
        field(
            "Sound bitrate",
            None,
            None,
            SegmentedControl::new(
                "go-live-sound-bitrate",
                vec![Segment::new(128u32, "128 kbps"), Segment::new(192u32, "192 kbps")],
                s.audio_bitrate_kbps,
            )
            .disabled(disabled)
            .on_change(|v, _, cx| {
                let v = *v;
                actions::voice::update_stream_settings(|st| st.audio_bitrate_kbps = v, cx)
            }),
            cx,
        )
    });
    let theme = cx.theme();
    let aside = Label::new(SharedString::from(format_bitrate(s.video_bitrate_kbps)))
        .font(FontRole::Meta)
        .size(12.)
        .weight(600.)
        .color(theme.color_text_secondary)
        .into_any_element();
    let bitrate_hint = hint_text("Higher looks better in motion and needs a faster upload.", cx);
    let border = theme.color_border_divider;
    div()
        .child(toggle)
        .child(
            div()
                .flex()
                .flex_col()
                .gap_5()
                .border_t_1()
                .border_color(border)
                .px_4()
                .pt_4()
                .pb_5()
                .when(disabled, |this| this.opacity(0.6))
                .child(field("Resolution", None, None, resolution, cx))
                .child(field("Frame rate (fps)", None, None, fps, cx))
                .child(field("Video bitrate", Some(aside), Some(bitrate_hint), div().my_1p5().child(bitrate), cx))
                .child(field(
                    "Codec",
                    None,
                    Some(div().flex().flex_col().gap_1().child(codec_hint).children(encoder_hint).into_any_element()),
                    codec,
                    cx,
                ))
                .children(sound_bitrate),
        )
        .into_any_element()
}

// ── the stream-audio app picker ─────────────────────────────────────────────────────────

/// `StreamAudioAppPicker`: the mode (All apps / Only these / All except) and, unless all apps,
/// the apps with sound as tick rows (remembered ticks of apps not running greyed). The owner
/// keeps the list fresh ([`refresh_apps`] every [`APPS_POLL`] while it shows).
pub fn audio_app_picker(dense: bool, cx: &App) -> AnyElement {
    let model = Model::global(cx);
    let apps = model.live.voice.stream_audio_apps.read(cx);
    if !apps.supported {
        return div().into_any_element();
    }
    let s = StreamSettings::of(model.settings.read(cx).get());
    let theme = cx.theme();
    let all_mode = s.audio_mode == StreamAudioMode::All;
    let ticked: Vec<String> = s.audio_apps.clone();
    let listed: Vec<String> = apps.apps.iter().map(|a| a.id.to_string()).collect();
    let not_running: Vec<String> = if all_mode { Vec::new() } else { ticked.iter().filter(|id| !listed.contains(id)).cloned().collect() };
    let hint: Option<String> = if all_mode {
        let playing: Vec<String> = apps.apps.iter().filter(|a| a.active).map(|a| a.name.to_string()).collect();
        Some(if playing.is_empty() { "Nothing is playing right now.".into() } else { format!("Playing now: {}.", list_names(&playing)) })
    } else if s.audio_mode == StreamAudioMode::Selected && ticked.is_empty() {
        Some("Tick the apps viewers should hear. Until then the stream is silent.".into())
    } else if s.audio_mode == StreamAudioMode::AllExcept && ticked.is_empty() {
        Some("Tick the apps to leave out.".into())
    } else {
        None
    };
    let mode = SegmentedControl::new(
        "stream-audio-mode",
        vec![
            Segment::new(StreamAudioMode::All, "All apps"),
            Segment::new(StreamAudioMode::Selected, "Only these"),
            Segment::new(StreamAudioMode::AllExcept, "All except"),
        ],
        s.audio_mode,
    )
    .on_change(|m, _, cx| actions::voice::set_stream_audio_mode(*m, cx));
    let row = |id: SharedString, name: SharedString, checked: bool, active: bool, owns_window: bool, gone: bool| {
        let app_id = id.clone();
        div()
            .id(SharedString::from(format!("app-{id}")))
            .flex()
            .items_center()
            .gap_2p5()
            .rounded(theme.radius_sm)
            .px_2()
            .py_1p5()
            .hover(|st| st.bg(theme.color_surface_hover))
            .cursor_pointer()
            .on_click(move |_: &ClickEvent, _, cx| actions::voice::toggle_stream_audio_app(&app_id, cx))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .size_4()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_sm)
                    .border_1()
                    .map(|this| {
                        if checked {
                            this.border_color(theme.color_accent).bg(theme.color_accent)
                        } else {
                            this.border_color(theme.color_text_muted.opacity(0.5)).bg(theme.color_bg_lighter)
                        }
                    })
                    .when(checked, |this| this.child(Icon::new(IconName::SettingsCheck).size(px(11.)).stroke_width(3.5).color(theme.color_on_accent))),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Label::new(name).size(13.).color(if gone { theme.color_text_muted } else { theme.color_text_primary }).truncate(),
                ),
            )
            .when(owns_window, |this| {
                this.child(
                    div()
                        .flex_none()
                        .rounded(theme.radius_sm)
                        .bg(theme.color_accent_mid)
                        .px_1p5()
                        .py_0p5()
                        .child(Label::new("this window").size(11.).weight(600.).color(theme.color_accent_bright)),
                )
            })
            .map(|this| {
                if gone {
                    this.child(Label::new("not running").size(11.).color(theme.color_text_muted))
                } else {
                    // Three bars while the app is making sound; the slot is kept so badges line up.
                    this.child(div().flex().flex_none().h_3().w(px(14.)).items_end().justify_center().gap_0p5().when(active, |this| {
                        this.children([0.55f32, 1., 0.7].map(|h| div().w_0p5().h(px(12. * h)).rounded_full().bg(theme.color_success)))
                    }))
                }
            })
            .into_any_element()
    };
    let list = (!all_mode).then(|| {
        let mut rows: Vec<AnyElement> = apps
            .apps
            .iter()
            .map(|a| row(a.id.clone(), a.name.clone(), ticked.iter().any(|t| *t == *a.id), a.active, a.owns_window_source, false))
            .collect();
        rows.extend(not_running.iter().map(|id| row(id.clone().into(), id.clone().into(), true, false, false, true)));
        let empty = rows.is_empty();
        div()
            .id("stream-audio-apps")
            .flex()
            .flex_col()
            .gap(px(1.))
            .max_h(px(if dense { 180. } else { 200. }))
            .overflow_y_scroll()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_mid)
            .p_1()
            .when(empty, |this| {
                this.child(
                    div().px_2().py_3().flex().justify_center().child(Label::new("No apps are playing sound").size(12.).color(theme.color_text_muted)),
                )
            })
            .children(rows)
    });
    div()
        .flex()
        .flex_col()
        .map(|this| if dense { this.gap_2() } else { this.gap_2p5() })
        .child(mode)
        .children(list)
        .when_some(hint, |this, hint| this.child(hint_text(hint, cx)))
        .into_any_element()
}

/// `listNames`: "a", "a and b", "a, b and c".
fn list_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_software_only_machine_gets_the_encodable_preset_with_the_most_pixels() {
        let mut e = Encoders::default();
        e.loaded = true;
        e.caps = vec![crate::codecs::EncoderCap {
            codec: decibell_model::live::voice::VideoCodec::Av1,
            hardware: false,
            backend: "software".into(),
            label: "Software (SVT-AV1)".into(),
            max_width: 3840,
            max_height: 2160,
            max_fps: 60,
            max_pixel_rate: 1920 * 1080 * 30,
        }];
        assert!(!e.can_encode(StreamResolution::P1080, 60), "Smooth is past the ceiling");
        let best = best_encodable_preset(|r, f| e.can_encode(r, f)).map(|p| p.id);
        assert_eq!(best, Some("saver"), "720p30 (27.6 Mpx/s) over Slides (1440p5: 18.4 Mpx/s)");
        assert!(best_encodable_preset(|_, _| false).is_none());
        assert_eq!(best_encodable_preset(|_, _| true).map(|p| p.id), Some("smooth"));
    }

    #[test]
    fn names_are_listed_as_the_renderer_does() {
        assert_eq!(super::list_names(&["a".into()]), "a");
        assert_eq!(super::list_names(&["a".into(), "b".into(), "c".into()]), "a, b and c");
    }
}
