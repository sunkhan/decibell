//! Settings → Audio (`AudioTab.tsx`): the input and output devices ("Default" follows the OS; a
//! saved device that is unplugged keeps showing as "Not connected"), voice activity or push to
//! talk (the PTT keys live in Keybinds; the release delay here), the voice activity threshold with
//! the live mic meter, echo cancellation / noise suppression / gain control, and stream audio
//! (a separate output device, stereo). Every control applies at once (`actions::voice::set_*`:
//! the setting is saved and the running engine retuned).
//!
//! **The meter** (`VoiceThresholdBar`). In voice the core reports our mic level
//! (`voice_input_level`, ~16 a second) only while a meter asks for it: the tab holds the model's
//! input-level reporting (`InputLevel::retain_meter`) while the threshold bar is shown and lets go
//! when it goes (push to talk, another tab, the window closing). Outside voice the level comes from
//! the mic test ([`crate::MicTest`]: the core's, through the same processing as voice), which runs
//! only while the user asked for it and starts over when the input device or one of the processing
//! switches changes under it. Each report is one smoothed step and one frame; nothing animates
//! between reports.
//!
//! **The device lists** are the model's (`voice.devices`). The tab holds a watch on them while it
//! is shown (`AudioDevices::watch`): the lists are read again when it opens and whenever the core
//! reports a device appearing or going, never on a timer. A saved device that is not in the list
//! shows as "Not connected" (derived when drawn, so it follows every refresh).

use std::rc::Rc;

use decibell_common::Watch;
use decibell_core::config::AppSettings;
use decibell_model::live::hotkeys::{InputMode, MAX_PTT_RELEASE_DELAY_MS, ptt_release_delay_ms};
use decibell_model::live::voice::devices::AudioDevice;
use decibell_model::{Model, actions};
use decibell_ui::{
    ActiveTheme as _, Badge, BadgeTone, Button, ButtonSize, ButtonVariant, Icon, IconName, Label, ScrollHandle, Segment,
    SegmentedControl, Select, SelectOption, Slider,
};
use gpui::{
    AnyElement, App, Bounds, Context, DispatchPhase, IntoElement, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels,
    Render, SharedString, Subscription, Task, Window, canvas, div, prelude::*, px, relative,
};

use super::TabId;
use super::keybinds::{assigned_trigger, is_swallowed, key_chips, swallow_badge, types_text};
use crate::hooks::SettingsHooks;
use crate::mic_test::{LevelSink, Processing, smooth};
use crate::rows::{TextLink, ToggleRow, section_label};
use crate::window::CloseLink;

const MIN_THRESHOLD_DB: f32 = -60.;
const MAX_THRESHOLD_DB: f32 = 0.;
/// The threshold before the user picks one (`voice_threshold_db` unset: the core's −50).
const DEFAULT_THRESHOLD_DB: f64 = -50.;

#[derive(Clone, PartialEq)]
struct Shown {
    input_device: Option<String>,
    output_device: Option<String>,
    stream_output_device: Option<String>,
    separate_stream_output: bool,
    stream_stereo: bool,
    aec: bool,
    ns: u8,
    agc: bool,
    threshold: f64,
    input_mode: InputMode,
    release_delay: u32,
    ptt_keys: Vec<(String, Vec<String>)>,
}

impl Shown {
    /// What the mic test meters through: the switches a voice session would use.
    fn processing(&self) -> Processing {
        Processing { echo_cancellation: self.aec, noise_suppression_level: self.ns, auto_gain: self.agc }
    }
}

fn shown(s: &AppSettings) -> Shown {
    Shown {
        input_device: s.input_device.clone(),
        output_device: s.output_device.clone(),
        stream_output_device: s.stream_output_device.clone(),
        separate_stream_output: s.separate_stream_output,
        stream_stereo: s.stream_stereo,
        aec: s.aec_enabled,
        ns: s.noise_suppression_level,
        agc: s.agc_enabled,
        threshold: s.voice_threshold_db.unwrap_or(DEFAULT_THRESHOLD_DB),
        input_mode: InputMode::of(s),
        release_delay: ptt_release_delay_ms(s),
        ptt_keys: s
            .hotkeys
            .iter()
            .filter(|b| b.action == "push_to_talk" && !b.keys.is_empty())
            .map(|b| (b.id.clone(), b.keys.clone()))
            .collect(),
    }
}

/// The mic test outside voice: running, and the task that brings its levels to the UI thread.
struct MicTestRun {
    _levels: Task<()>,
}

pub struct AudioTab {
    scroll: ScrollHandle,
    close: CloseLink,
    shown: Shown,
    connected: bool,
    /// The meter's smoothed level, dB.
    level: f32,
    /// The model's input-level reporting is held (the threshold bar is shown).
    meter_held: bool,
    /// "Test mic" pressed (the renderer's `micTestActive`), and the test running.
    test_active: bool,
    test: Option<MicTestRun>,
    /// The threshold drag: the bar's bounds as last laid out, and whether a drag runs.
    bar: Rc<std::cell::Cell<Bounds<Pixels>>>,
    dragging: bool,
    _subscriptions: Vec<Subscription>,
    _watch: Watch,
}

impl AudioTab {
    pub fn new(close: CloseLink, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.live.voice.devices, cx).store(&model.live.hotkeys, cx);
        let subscriptions = vec![
            cx.observe(&model.settings, |this, settings, cx| {
                let next = shown(settings.read(cx).get());
                if next != this.shown {
                    let metered = (this.shown.input_device.clone(), this.shown.processing());
                    this.shown = next;
                    this.hold_meter(cx);
                    // A running test meters what was chosen now.
                    if this.test.is_some() && metered != (this.shown.input_device.clone(), this.shown.processing()) {
                        this.stop_test(cx);
                        this.start_test(cx);
                    }
                    cx.notify();
                }
            }),
            cx.observe(&model.live.voice.session, |this, session, cx| {
                let connected = session.read(cx).channel.is_some();
                if connected != this.connected {
                    this.connected = connected;
                    // A call takes the mic: the test stops (the pipeline's meter takes over).
                    if connected {
                        this.stop_test(cx);
                    }
                    this.level = -96.;
                    cx.notify();
                }
            }),
            cx.observe(&model.live.voice.input_level, |this, level, cx| {
                if this.connected && this.meter_held {
                    let db = level.read(cx).db;
                    this.set_level(db, cx);
                }
            }),
            cx.on_release(|this: &mut AudioTab, cx| {
                let model = Model::global(cx).clone();
                if this.meter_held {
                    model.live.voice.input_level.update(cx, |l, cx| l.release_meter(cx));
                }
                if this.test.is_some() {
                    SettingsHooks::get(cx).mic_test.clone().stop();
                }
                model.live.voice.devices.update(cx, |d, _| d.unwatch());
            }),
        ];
        // The lists are fresh whenever the tab opens and follow the devices while it shows: the
        // first watch lists again and switches the core's device notifications on (nothing
        // without a core: a screen draws the fixture's lists).
        model.live.voice.devices.update(cx, |d, cx| d.watch(cx));
        let mut tab = Self {
            scroll: ScrollHandle::new(),
            close,
            shown: shown(model.settings.read(cx).get()),
            connected: model.live.voice.session.read(cx).channel.is_some(),
            level: SettingsHooks::get(cx).staged.meter_level.unwrap_or(-96.),
            meter_held: false,
            test_active: false,
            test: None,
            bar: Rc::default(),
            dragging: false,
            _subscriptions: subscriptions,
            _watch: watch,
        };
        tab.hold_meter(cx);
        tab
    }

    /// The input-level reporting is held exactly while the threshold bar is on screen.
    fn hold_meter(&mut self, cx: &mut Context<Self>) {
        let want = self.shown.input_mode == InputMode::VoiceActivity;
        if want == self.meter_held {
            return;
        }
        self.meter_held = want;
        let level = Model::global(cx).live.voice.input_level.clone();
        if want {
            level.update(cx, |l, _| l.retain_meter());
        } else {
            level.update(cx, |l, cx| l.release_meter(cx));
            self.stop_test(cx);
        }
    }

    pub fn meter_held(&self) -> bool {
        self.meter_held
    }

    /// The meter's level as drawn, dB (−96 at rest).
    pub fn level(&self) -> f32 {
        self.level
    }

    /// "Test mic" is pressed and a test runs.
    pub fn test_running(&self) -> bool {
        self.test.is_some()
    }

    fn set_level(&mut self, db: f32, cx: &mut Context<Self>) {
        let next = smooth(self.level, db);
        if next != self.level {
            self.level = next;
            cx.notify();
        }
    }

    /// "Test mic" / "Stop test".
    pub fn toggle_test(&mut self, cx: &mut Context<Self>) {
        if self.test_active {
            self.stop_test(cx);
        } else {
            self.start_test(cx);
        }
        cx.notify();
    }

    fn start_test(&mut self, cx: &mut Context<Self>) {
        self.test_active = true;
        let test = SettingsHooks::get(cx).mic_test.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<f32>();
        let sink: LevelSink = std::sync::Arc::new(move |db| {
            let _ = tx.unbounded_send(db);
        });
        if let Err(e) = test.start(self.shown.input_device.as_deref(), self.shown.processing(), sink) {
            // The renderer only logged a failed getUserMedia; the button still reads "Stop test".
            log::warn!("[AudioTab] mic test setup failed: {e}");
            return;
        }
        // One wake per burst of reports, for as long as the test runs.
        let levels = cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(mut db) = rx.next().await {
                while let Ok(more) = rx.try_recv() {
                    db = more;
                }
                if this.update(cx, |this, cx| this.set_level(db, cx)).is_err() {
                    return;
                }
            }
        });
        self.test = Some(MicTestRun { _levels: levels });
    }

    fn stop_test(&mut self, cx: &mut Context<Self>) {
        if self.test.take().is_some() {
            SettingsHooks::get(cx).mic_test.clone().stop();
        }
        if self.test_active {
            self.test_active = false;
            self.level = -96.;
        }
    }

    /// A level as a meter shows it (the fixtures' and the tests' way in).
    pub fn show_level(&mut self, db: f32, cx: &mut Context<Self>) {
        self.level = db;
        cx.notify();
    }

    fn threshold_at(&self, x: Pixels) -> f64 {
        let bounds = self.bar.get();
        let width = f32::from(bounds.size.width).max(1.);
        let f = (f32::from(x - bounds.left()) / width).clamp(0., 1.);
        let raw = MIN_THRESHOLD_DB + f * (MAX_THRESHOLD_DB - MIN_THRESHOLD_DB);
        js_round(f64::from(raw) * 2.) / 2.
    }
}

/// JavaScript's `Math.round`: halves go up (-50.5 → -50), where Rust's `round` goes away from 0.
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

fn percent(db: f32) -> f32 {
    (db.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB) - MIN_THRESHOLD_DB) / (MAX_THRESHOLD_DB - MIN_THRESHOLD_DB)
}

/// `DeviceSelector`.
fn device_selector(
    id: &'static str,
    label: &'static str,
    icon: IconName,
    devices: &[AudioDevice],
    selected: Option<&str>,
    on_change: impl Fn(Option<String>, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let missing = selected.filter(|s| !devices.iter().any(|d| d.name.as_ref() == *s));
    let mut options = vec![SelectOption::new(SharedString::default(), "Default").hint("Follows the system")];
    options.extend(devices.iter().map(|d| SelectOption::new(d.name.clone(), d.label.clone())));
    if let Some(m) = missing {
        options.push(SelectOption::new(SharedString::from(m.to_string()), m.to_string()).hint("Not connected").disabled(true));
    }
    let value = SharedString::from(selected.unwrap_or("").to_string());
    div()
        .rounded(theme.radius_md)
        .border_1()
        .border_color(theme.color_border_divider)
        .bg(theme.color_bg_light)
        .p_4()
        .child(
            div()
                .mb_2p5()
                .flex()
                .items_center()
                .gap_2p5()
                .child(Icon::new(icon).size(px(16.)).color(theme.color_text_muted))
                .child(Label::new(label).size(13.).weight(500.).color(theme.color_text_secondary)),
        )
        .child(Select::options(id, Some(value), options).on_change(move |v: &SharedString, _, cx| {
            on_change((!v.is_empty()).then(|| v.to_string()), cx)
        }))
        .into_any_element()
}

impl AudioTab {
    fn input_mode_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.shown;
        let model = Model::global(cx).clone();
        let status = model.live.hotkeys.read(cx).status.clone();
        let theme = cx.theme();
        let ptt = s.input_mode == InputMode::PushToTalk;
        let open_keybinds = |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            let nav = Model::global(cx).nav.clone();
            nav.update(cx, |n, cx| n.set_settings_tab("keybinds", cx));
        };
        let mut card = div()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_light)
            .p_4()
            .child(
                SegmentedControl::new(
                    "audio-input-mode",
                    vec![Segment::new(InputMode::VoiceActivity, "Voice activity"), Segment::new(InputMode::PushToTalk, "Push to talk")],
                    s.input_mode,
                )
                .on_change(|mode: &InputMode, _, cx| actions::hotkeys::set_input_mode(*mode == InputMode::PushToTalk, None, cx)),
            );
        if ptt {
            if s.ptt_keys.is_empty() {
                card = card.child(
                    div()
                        .mt_3()
                        .flex()
                        .flex_wrap()
                        .items_baseline()
                        .child(
                            Label::new("No push to talk key yet — your mic stays closed.\u{a0}")
                                .size(12.)
                                .line_height(12. * 1.55)
                                .color(theme.color_danger),
                        )
                        .child(
                            TextLink::new("audio-add-ptt", "Add one in Keybinds", theme.color_accent_bright, theme.color_accent_bright)
                                .text(12., 500.)
                                .on_click(open_keybinds),
                        ),
                );
            } else {
                let mut keys = div().flex().min_w_0().flex_col().items_end().gap_1p5();
                for (i, (id, binding_keys)) in s.ptt_keys.iter().enumerate() {
                    let assigned = assigned_trigger(id, binding_keys, status.as_ref());
                    let swallowed = is_swallowed(id, binding_keys, decibell_core::hotkeys::Action::PushToTalk, status.as_ref());
                    keys = keys.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .when(i > 0, |this| this.child(Label::new("or").size(11.).color(theme.color_text_muted)))
                            .child(key_chips(binding_keys, cx))
                            .when(swallowed, |this| this.child(swallow_badge(("audio-ptt-swallow", i), types_text(binding_keys), cx)))
                            .when_some(assigned, |this, a| {
                                this.child(Label::new(format!("(your desktop: {a})")).size(11.).color(theme.color_text_muted))
                            }),
                    );
                }
                keys = keys.child(
                    TextLink::new("audio-change-ptt", "Change in Keybinds", theme.color_accent_bright, theme.color_accent_bright)
                        .text(12., 500.)
                        .on_click(open_keybinds),
                );
                card = card.child(
                    div()
                        .mt_4()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap_4()
                        .child(
                            div().pt_0p5().child(
                                Label::new(if s.ptt_keys.len() > 1 { "Push to talk keys" } else { "Push to talk key" })
                                    .size(13.)
                                    .weight(500.)
                                    .color(theme.color_text_secondary),
                            ),
                        )
                        .child(keys),
                );
            }
            let delay = s.release_delay;
            card = card
                .child(
                    div()
                        .mt_4()
                        .flex()
                        .items_baseline()
                        .justify_between()
                        .child(Label::new("Release delay").size(13.).weight(500.).color(theme.color_text_secondary))
                        .child(Badge::new(format!("{delay} ms")).tone(BadgeTone::Value)),
                )
                .child(
                    div().mt_2p5().py(px(5.)).child(
                        Slider::new("audio-ptt-delay", delay as f32, 0., MAX_PTT_RELEASE_DELAY_MS as f32)
                            .step(10.)
                            .on_change(|ms: &f32, _, cx| actions::hotkeys::set_input_mode(true, Some(*ms as u32), cx)),
                    ),
                )
                .child(
                    div().mt_2().child(
                        Label::new("Keeps your mic open briefly after you let go, so the end of a word isn't cut off.")
                            .size(12.)
                            .line_height(12. * 1.55)
                            .color(theme.color_text_muted),
                    ),
                );
        }
        card.into_any_element()
    }

    fn threshold_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let threshold = self.shown.threshold as f32;
        let open_mic = threshold <= MIN_THRESHOLD_DB;
        let above = open_mic || self.level >= threshold;
        let level_pct = percent(self.level);
        let threshold_pct = percent(threshold);
        let badge = if open_mic { "Off".to_string() } else { format!("{} dB", js_round(f64::from(threshold)) as i64) };
        let accent = theme.color_accent;

        let bar_bounds = self.bar.clone();
        let mut bar = div()
            .id("audio-threshold-bar")
            .relative()
            .h(px(6.))
            .w_full()
            .rounded_full()
            .bg(theme.color_bg_lighter)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    let db = this.threshold_at(event.position.x);
                    this.dragging = true;
                    actions::voice::set_voice_threshold(db, cx);
                    cx.notify();
                }),
            )
            .child(canvas(move |b, _, _| bar_bounds.set(b), |_, _, _, _| {}).absolute().size_full())
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(relative(level_pct))
                    .rounded_full()
                    .bg(if above { theme.color_success.opacity(0.5) } else { theme.color_text_muted.opacity(0.25) }),
            )
            .child(
                // The thumb, centred on the threshold: `h-[18px] w-[18px] rounded-full border-2
                // border-accent bg-bg-mid shadow-[0_0_8px_color-mix(accent 35%)]`.
                div()
                    .absolute()
                    .top(px(-6.))
                    .left(relative(threshold_pct))
                    .ml(px(-9.))
                    .size(px(18.))
                    .rounded_full()
                    .border_2()
                    .border_color(accent)
                    .bg(theme.color_bg_mid)
                    .shadow(vec![gpui::BoxShadow::new(px(0.), px(0.), accent.opacity(0.35)).blur_radius(px(4.))]),
            );
        if self.dragging {
            // Follow the pointer anywhere in the window until the button is released.
            let entity = cx.entity();
            bar = bar.child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        let e = entity.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && event.pressed_button == Some(MouseButton::Left) {
                                e.update(cx, |this, cx| {
                                    let db = this.threshold_at(event.position.x);
                                    if db != this.shown.threshold {
                                        actions::voice::set_voice_threshold(db, cx);
                                    }
                                });
                            }
                        });
                        let e = entity.clone();
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                e.update(cx, |this, cx| {
                                    this.dragging = false;
                                    cx.notify();
                                });
                            }
                        });
                    },
                )
                .absolute()
                .size_0(),
            );
        }
        let _ = window;

        let header = div()
            .mb_3()
            .flex()
            .items_center()
            .gap_2p5()
            .child(Icon::new(IconName::SettingsUiMic).size(px(16.)).color(theme.color_text_muted))
            .child(Label::new("Voice Activity Threshold").size(14.).weight(500.).color(theme.color_text_primary))
            .child(div().flex_1())
            .when(!self.connected, |this| {
                this.child(
                    Button::new("audio-test-mic", if self.test_active { "Stop test" } else { "Test mic" })
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Xs)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_test(cx))),
                )
            })
            .child(Badge::new(badge).tone(BadgeTone::Value));
        let theme = cx.theme();
        div()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(theme.color_bg_light)
            .p_4()
            .child(header)
            .child(bar)
            .child(
                div()
                    .mt_2p5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(Label::new("Sensitive").size(11.).color(theme.color_text_muted))
                    .child(Label::new("Aggressive").size(11.).color(theme.color_text_muted)),
            )
            .into_any_element()
    }
}

impl Render for AudioTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let devices = model.live.voice.devices.read(cx);
        let (inputs, outputs) = (devices.inputs.clone(), devices.outputs.clone());
        let s = self.shown.clone();

        let device_section = div()
            .child(section_label("Devices", true, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .child(device_selector(
                        "audio-input-device",
                        "Input Device",
                        IconName::SettingsUiMic,
                        &inputs,
                        s.input_device.as_deref(),
                        actions::voice::set_input_device,
                        cx,
                    ))
                    .child(device_selector(
                        "audio-output-device",
                        "Output Device",
                        IconName::SettingsUiSpeaker,
                        &outputs,
                        s.output_device.as_deref(),
                        actions::voice::set_output_device,
                        cx,
                    )),
            );
        let mode_section = div().child(section_label("Input Mode", true, cx)).child(self.input_mode_card(cx));
        let sensitivity = (s.input_mode == InputMode::VoiceActivity)
            .then(|| div().child(section_label("Input Sensitivity", true, cx)).child(self.threshold_bar(window, cx)));
        let processing = div().child(section_label("Voice Processing", true, cx)).child(
            div()
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    ToggleRow::new("audio-aec", "Echo Cancellation", "Remove speaker audio bleeding into your microphone", s.aec)
                        .on_toggle(move |_, _, cx| actions::voice::set_aec_enabled(!s.aec, cx)),
                )
                .child(
                    ToggleRow::new(
                        "audio-ns",
                        "Noise Suppression",
                        "AI-powered noise removal for fans, keyboards, and background noise",
                        s.ns > 0,
                    )
                    .on_toggle(move |_, _, cx| actions::voice::set_noise_suppression_level(if s.ns > 0 { 0 } else { 1 }, cx)),
                )
                .child(
                    ToggleRow::new("audio-agc", "Automatic Gain Control", "Normalize your microphone volume for consistent levels", s.agc)
                        .on_toggle(move |_, _, cx| actions::voice::set_agc_enabled(!s.agc, cx)),
                ),
        );
        let separate = s.separate_stream_output;
        let stereo = s.stream_stereo;
        let stream = div().child(section_label("Stream Audio", true, cx)).child(
            div()
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    ToggleRow::new(
                        "audio-separate-stream",
                        "Separate stream output device",
                        "Route stream audio to a different output device than voice chat",
                        separate,
                    )
                    .on_toggle(move |_, _, cx| actions::voice::set_separate_stream_output(!separate, cx)),
                )
                .when(separate, |this| {
                    this.child(device_selector(
                        "audio-stream-output-device",
                        "Stream Output Device",
                        IconName::SettingsUiMusic,
                        &outputs,
                        s.stream_output_device.as_deref(),
                        actions::voice::set_stream_output_device,
                        cx,
                    ))
                })
                .child(
                    ToggleRow::new(
                        "audio-stereo",
                        "Stereo stream audio",
                        "Preserve left/right stereo positioning when watching streams",
                        stereo,
                    )
                    .on_toggle(move |_, _, cx| actions::voice::set_stream_stereo(!stereo, cx)),
                ),
        );

        let body = div()
            .flex()
            .flex_col()
            .gap_6()
            .child(device_section)
            .child(mode_section)
            .children(sensitivity)
            .child(processing)
            .child(stream);
        super::pane(TabId::Audio, &self.scroll, &self.close).child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_as_javascript() {
        assert_eq!(js_round(-50.5), -50.);
        assert_eq!(js_round(-50.6), -51.);
        assert_eq!(js_round(2.5), 3.);
        assert_eq!(js_round(-101.5) / 2., -50.5);
    }

    #[test]
    fn the_bar_maps_decibels_to_its_width() {
        assert_eq!(percent(-60.), 0.);
        assert_eq!(percent(-96.), 0.);
        assert_eq!(percent(0.), 1.);
        assert_eq!(percent(-30.), 0.5);
    }
}
