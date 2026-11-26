//! The Transfers button of the title bar and its panel (`TransfersButton.tsx`,
//! `TransfersPanel.tsx`, `DownloadRow.tsx`, `UploadRow.tsx`, `TransferRow.tsx`).
//!
//! One view: the 44 px button (the progress ring over everything running, the dot for something
//! that ended while the panel was shut, the arrow's drop when a download starts), the panel
//! anchored under it (`deferred`, over the main layout), the row menu and the risky-file
//! confirmation. It follows `model.live.transfers` (about 4 notifies a second per active
//! transfer) and redraws only itself; while the panel is open one 30 s timer refreshes the
//! "N min ago" labels, as the renderer's interval did.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use decibell_attachments::AttachmentKind;
use decibell_model::live::transfers::{Attention, DownloadState, DownloadView, TransfersTab, UploadEntry, UploadState};
use decibell_model::{Model, actions};
use decibell_text::{TextStyle, Wrapping};
use decibell_ui::{
    ActiveTheme as _, ConfirmModal, ContextMenu, FontRole, Icon, IconName, MenuItem, Placed, Placement, ScrollArea, ScrollHandle, Segment,
    SegmentedControl, motion, title_tooltip, visible_window_bounds,
};
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, Context, Corners, FocusHandle, Hsla, IntoElement, MouseButton, PathBuilder,
    Pixels, Point, Render, SharedString, Subscription, Task, Window, canvas, div, point, prelude::*, px,
};

use super::format;
use super::runs::Runs;
use crate::pictures::{Fit, Look, PicKey, Pictures, Source, Spec, Want};
use crate::style::{label, label_in};

/// The panel's width and its distance from the window's edge (`WIDTH`, `MARGIN`).
const WIDTH: f32 = 360.;
const MARGIN: f32 = 8.;
/// The list's height cap (`max-h-[384px]`).
const LIST_MAX: f32 = 384.;
/// The preview box of a row (`h-9 w-9`).
const PREVIEW: f32 = 36.;
const TICK: Duration = Duration::from_secs(30);

/// What a row's actions do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Pause,
    Resume,
    Cancel,
    Retry,
    Remove,
    Folder,
}

impl Act {
    fn icon(self) -> IconName {
        match self {
            Act::Pause => IconName::TransfersPause,
            Act::Resume => IconName::TransfersPlay,
            Act::Cancel | Act::Remove => IconName::TransfersClose,
            Act::Retry => IconName::TransfersRetry,
            Act::Folder => IconName::TransfersFolder,
        }
    }
}

/// One row as `TransferRow` draws it.
struct Row {
    id: SharedString,
    name: SharedString,
    struck: bool,
    status: String,
    error: bool,
    progress: Option<f32>,
    paused: bool,
    actions: Vec<(Act, &'static str)>,
    persistent: bool,
    activate: Option<SharedString>,
    preview: Preview,
    kind: AttachmentKind,
}

enum Preview {
    /// A picture through the pipeline (an image / video download's 320 thumbnail, an upload's
    /// own preview).
    Picture(PicKey),
    Glyph,
}

fn kind_icon(kind: AttachmentKind) -> IconName {
    match kind {
        AttachmentKind::Image => IconName::TransfersKindImage,
        AttachmentKind::Video => IconName::TransfersKindVideo,
        AttachmentKind::Audio => IconName::TransfersKindAudio,
        AttachmentKind::Document => IconName::TransfersKindDocument,
    }
}

fn now_ms(cx: &App) -> i64 {
    if let Some(fixed) = cx.try_global::<super::FixedNow>() {
        return fixed.0;
    }
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// See the module doc.
pub struct TransfersButton {
    /// Where the button was last drawn (the panel hangs under it; a press on it is its toggle).
    button: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// The row menu: where, and which download.
    menu: Option<(Point<Pixels>, String)>,
    /// The download whose opening waits for the risky-file confirmation.
    risky: Option<DownloadView>,
    was_open: bool,
    /// Bumped per opening (the panel's entrance).
    opened: u64,
    now_ms: i64,
    tick: Option<Task<()>>,
    /// The previews the panel asked the picture pipeline for.
    wanted: Vec<PicKey>,
    _subscriptions: Vec<Subscription>,
}

impl TransfersButton {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        if let Some(model) = cx.try_global::<Model>().cloned() {
            subscriptions.push(cx.observe(&model.live.transfers, |_, _, cx| cx.notify()));
            // The footer shows the speed caps.
            subscriptions.push(cx.observe(&model.settings, |_, _, cx| cx.notify()));
        }
        Self {
            button: Rc::default(),
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            menu: None,
            risky: None,
            was_open: false,
            opened: 0,
            now_ms: now_ms(cx),
            tick: None,
            wanted: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    fn model(cx: &App) -> Option<Model> {
        cx.try_global::<Model>().cloned()
    }

    fn close(&mut self, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if let Some(model) = Self::model(cx) {
            model.live.transfers.update(cx, |t, cx| t.close_panel(cx));
        }
        if refocus {
            // The renderer refocuses the button; ours takes no focus: the panel's goes.
            window.blur(cx);
        }
    }

    /// An opening: a fresh list (main re-checked finished files on disk), focus, the tick.
    fn opening(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.opened += 1;
        self.now_ms = now_ms(cx);
        if let Some(model) = Self::model(cx)
            && let Some(control) = model.live.transfers.read(cx).control()
        {
            let list = control.list();
            cx.defer(move |cx| model.live.transfers.update(cx, |t, cx| t.set_downloads(list, cx)));
        }
        let focus = self.focus.clone();
        window.defer(cx, move |window, cx| focus.focus(window, cx));
        self.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let alive = this.update(cx, |this, cx| {
                    this.now_ms = now_ms(cx);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    fn closed(&mut self, cx: &mut Context<Self>) {
        self.tick = None;
        self.menu = None;
        if !self.wanted.is_empty() {
            self.wanted.clear();
            let owner = (cx.entity_id(), 7);
            if let Some(pictures) = Pictures::global(cx) {
                cx.defer(move |cx| pictures.update(cx, |p, cx| p.set_wants(owner, Vec::new(), cx)));
            }
        }
    }

    fn open_file(&mut self, d: &DownloadView, cx: &mut Context<Self>) {
        if decibell_attachments::names::is_risky_file(&d.filename) {
            self.risky = Some(d.clone());
            cx.notify();
        } else {
            super::open_download(&d.id, cx);
        }
    }

    fn rows(&self, tab: TransfersTab, downloads: &[DownloadView], uploads: &[UploadEntry], scale: f32, cx: &App) -> Vec<Row> {
        let preview_spec = Spec { width: PREVIEW, height: PREVIEW, scale, fit: Fit::Cover, upscale: true };
        match tab {
            TransfersTab::Downloads => downloads
                .iter()
                .map(|d| {
                    let preview = if matches!(d.kind, AttachmentKind::Image | AttachmentKind::Video) {
                        Preview::Picture(PicKey::new(Source::Attachment { server: d.server_id.clone().into(), id: d.attachment_id, thumb: 320 }, preview_spec))
                    } else {
                        Preview::Glyph
                    };
                    let mut row = Row {
                        id: d.id.clone().into(),
                        name: d.filename.clone().into(),
                        struck: false,
                        status: format::download_status(d, self.now_ms),
                        error: false,
                        progress: None,
                        paused: false,
                        actions: Vec::new(),
                        persistent: true,
                        activate: None,
                        preview,
                        kind: d.kind,
                    };
                    let fraction = format::fraction(d.received_bytes, d.total_bytes);
                    match d.state {
                        DownloadState::Active => {
                            row.progress = Some(fraction);
                            row.actions = vec![(Act::Pause, "Pause"), (Act::Cancel, "Cancel")];
                        }
                        DownloadState::Paused => {
                            row.progress = Some(fraction);
                            row.paused = true;
                            row.actions = vec![(Act::Resume, "Resume"), (Act::Cancel, "Cancel")];
                        }
                        DownloadState::Failed => {
                            row.error = true;
                            row.actions = vec![(Act::Retry, "Retry"), (Act::Remove, "Remove from list")];
                        }
                        DownloadState::Cancelled => {
                            row.struck = true;
                            row.persistent = false;
                            row.actions = vec![(Act::Retry, "Retry"), (Act::Remove, "Remove from list")];
                        }
                        DownloadState::Done if d.missing => {
                            row.struck = true;
                            row.persistent = false;
                            row.actions = vec![(Act::Retry, "Download again"), (Act::Remove, "Remove from list")];
                        }
                        DownloadState::Done => {
                            row.persistent = false;
                            row.actions = vec![(Act::Folder, "Show in folder"), (Act::Remove, "Remove from list")];
                            row.activate = Some(format!("Open {}", d.filename).into());
                        }
                    }
                    row
                })
                .collect(),
            TransfersTab::Uploads => uploads
                .iter()
                .map(|u| {
                    let preview = match (&u.preview, u.attachment_id) {
                        (Some(image), _) => {
                            // The composer's own 320 px picture while the pending exists.
                            let _ = cx;
                            Preview::Picture(PicKey::new(Source::Local(image.id), preview_spec))
                        }
                        (None, Some(id)) if matches!(u.kind, AttachmentKind::Image | AttachmentKind::Video) => {
                            Preview::Picture(PicKey::new(Source::Attachment { server: u.server.wire().into(), id, thumb: 320 }, preview_spec))
                        }
                        _ => Preview::Glyph,
                    };
                    let mut row = Row {
                        id: u.pending_id.clone(),
                        name: u.filename.clone(),
                        struck: false,
                        status: format::upload_status(u),
                        error: false,
                        progress: None,
                        paused: false,
                        actions: Vec::new(),
                        persistent: true,
                        activate: None,
                        preview,
                        kind: u.kind,
                    };
                    let fraction = format::fraction(u.transferred_bytes, u.total_bytes);
                    match u.state {
                        UploadState::Uploading => {
                            row.progress = Some(fraction);
                            row.actions = vec![(Act::Pause, "Pause"), (Act::Cancel, "Cancel")];
                        }
                        UploadState::Paused => {
                            row.progress = Some(fraction);
                            row.paused = true;
                            row.actions = vec![(Act::Resume, "Resume"), (Act::Cancel, "Cancel")];
                        }
                        UploadState::Done => {
                            row.persistent = false;
                            row.actions = vec![(Act::Remove, "Remove from list")];
                            row.activate = Some(SharedString::new_static("Go to message"));
                        }
                        UploadState::Failed => {
                            row.error = true;
                            row.actions = vec![(Act::Remove, "Remove from list")];
                        }
                        UploadState::Cancelled => {
                            row.struck = true;
                            row.persistent = false;
                            row.actions = vec![(Act::Remove, "Remove from list")];
                        }
                    }
                    row
                })
                .collect(),
        }
    }

    fn act(&mut self, tab: TransfersTab, id: &str, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = Self::model(cx) else { return };
        match tab {
            TransfersTab::Downloads => {
                let Some(control) = model.live.transfers.read(cx).control() else { return };
                match act {
                    Act::Pause => control.pause(id),
                    Act::Resume | Act::Retry => control.resume(id),
                    Act::Cancel => control.cancel(id),
                    Act::Remove => control.remove(&[id.to_string()]),
                    Act::Folder => super::show_in_folder(id, cx),
                }
            }
            TransfersTab::Uploads => match act {
                Act::Pause => actions::transfers::pause_upload(id, true, cx),
                Act::Resume => actions::transfers::pause_upload(id, false, cx),
                Act::Cancel => actions::transfers::cancel_upload(id, cx),
                Act::Remove => model.live.transfers.update(cx, |t, cx| t.remove_upload(id, cx)),
                Act::Retry | Act::Folder => {}
            },
        }
        let _ = window;
    }

    fn activate(&mut self, tab: TransfersTab, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = Self::model(cx) else { return };
        match tab {
            TransfersTab::Downloads => {
                let found = model.live.transfers.read(cx).downloads().iter().find(|d| d.id == id).cloned();
                if let Some(d) = found {
                    self.open_file(&d, cx);
                }
            }
            TransfersTab::Uploads => {
                self.close(false, window, cx);
                actions::transfers::go_to_upload(id, cx);
            }
        }
    }

    fn row_element(&self, tab: TransfersTab, row: Row, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let group: SharedString = format!("transfer-row-{}", row.id).into();
        let name_style = {
            let mut style = row_text(cx, 13., 500., if row.struck { theme.color_text_muted } else { theme.color_text_primary });
            style.strikethrough = row.struck;
            style
        };
        let status_style = row_text(cx, 11., 400., if row.error { theme.color_error } else { theme.color_text_muted });
        let (hover_bg, bar_bg, fill) = (theme.color_surface_hover, theme.color_bg_darkest, if row.paused { theme.color_text_muted } else { theme.color_accent });
        let (preview_bg, muted, primary, active_bg) = (theme.color_bg_mid, theme.color_text_muted, theme.color_text_primary, theme.color_surface_active);
        let (radius_md, radius_sm) = (theme.radius_md, theme.radius_sm);
        let id = row.id.clone();
        let preview = preview_element(&row, preview_bg, muted, radius_sm, cx);
        let mut actions_div = div().flex().flex_none().items_center().gap(px(2.));
        if !row.persistent {
            actions_div = actions_div.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.));
        }
        for (act, title) in &row.actions {
            let (act, row_id) = (*act, id.clone());
            let button_group: SharedString = format!("{group}-{title}").into();
            actions_div = actions_div.child(
                div()
                    .id(SharedString::from(format!("{}-{title}", row.id)))
                    .group(button_group.clone())
                    .relative()
                    .size_7()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(radius_sm)
                    .cursor_pointer()
                    .hover(move |s| s.bg(active_bg))
                    .tooltip(title_tooltip(*title))
                    .child(Icon::new(act.icon()).size(px(14.)).color(muted).group_hover(button_group, primary))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.act(tab, &row_id, act, window, cx);
                    }))
                    .children(decibell_shell::probes::probe(format!("transfer-{}-{title}", row.id))),
            );
        }
        let mut text = div().flex_1().min_w_0().child(label(row.name.clone(), &name_style, Wrapping::TRUNCATE));
        if let Some(progress) = row.progress {
            text = text.child(
                div()
                    .mt_1()
                    .h_1()
                    .w_full()
                    .rounded_full()
                    .bg(bar_bg)
                    .child(div().h_full().w(gpui::relative((progress * 100.).round() / 100.)).rounded_full().bg(fill)),
            );
        }
        text = text.child(div().mt(px(2.)).child(label(row.status.clone(), &status_style, Wrapping::TRUNCATE)));
        let activate = row.activate.clone();
        let mut out = div()
            .id(SharedString::from(format!("transfer-{}", row.id)))
            .group(group)
            .relative()
            .flex()
            .items_center()
            .gap_3()
            .rounded(radius_md)
            .px_2()
            .py_2()
            .hover(move |s| s.bg(hover_bg))
            .child(preview)
            .child(text)
            .when(!row.actions.is_empty(), |d| d.child(actions_div));
        if let Some(title) = activate {
            let row_id = id.clone();
            out = out
                .cursor_pointer()
                .tooltip(title_tooltip(title))
                .on_click(cx.listener(move |this, _, window, cx| this.activate(tab, &row_id, window, cx)));
        }
        if tab == TransfersTab::Downloads {
            let row_id = id.to_string();
            out = out.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu = Some((event.position, row_id.clone()));
                    cx.notify();
                }),
            );
        }
        out.children(decibell_shell::probes::probe(format!("transfer-row-{}", row.id))).into_any_element()
    }

    fn panel(&mut self, tab: TransfersTab, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(model) = Self::model(cx) else { return div().into_any_element() };
        let (downloads, uploads) = {
            let t = model.live.transfers.read(cx);
            (t.downloads().to_vec(), t.uploads().to_vec())
        };
        let downloads_live = downloads.iter().filter(|d| matches!(d.state, DownloadState::Active | DownloadState::Paused)).count();
        let uploads_live = uploads.iter().filter(|u| matches!(u.state, UploadState::Uploading | UploadState::Paused)).count();
        let finished = match tab {
            TransfersTab::Downloads => downloads.iter().any(|d| d.state.is_finished()),
            TransfersTab::Uploads => uploads.iter().any(|u| !matches!(u.state, UploadState::Uploading | UploadState::Paused)),
        };
        let settings = model.settings.read(cx).get().clone();
        let cap = match tab {
            TransfersTab::Downloads => settings.download_limit_bps,
            TransfersTab::Uploads => settings.upload_limit_bps,
        };
        let rows = self.rows(tab, &downloads, &uploads, window.scale_factor(), cx);
        self.want_previews(&rows, cx);
        let empty = rows.is_empty();
        let row_elements: Vec<AnyElement> = rows.into_iter().map(|row| self.row_element(tab, row, cx)).collect();
        let theme = cx.theme();
        let count = |n: usize| crate::fmt::count(n as i64);
        let downloads_label: SharedString = if downloads_live > 0 { format!("Downloads · {}", count(downloads_live)).into() } else { "Downloads".into() };
        let uploads_label: SharedString = if uploads_live > 0 { format!("Uploads · {}", count(uploads_live)).into() } else { "Uploads".into() };
        let transfers = model.live.transfers.clone();
        let tabs = SegmentedControl::new("transfers-tabs", vec![Segment::new(TransfersTab::Downloads, downloads_label), Segment::new(TransfersTab::Uploads, uploads_label)], tab)
            .on_change(move |tab, _, cx| transfers.update(cx, |t, cx| t.set_tab(*tab, cx)));
        let list = if empty {
            let (title, hint) = match tab {
                TransfersTab::Downloads => ("No downloads yet", "Files you download from chats show up here."),
                TransfersTab::Uploads => ("No uploads this session", "Files you send show their progress here."),
            };
            vec![
                div()
                    .px_6()
                    .py_8()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(label_in(title, &row_text(cx, 13., 500., theme.color_text_secondary), Wrapping::NORMAL, theme.color_text_secondary))
                    .child(div().mt_1().child(label(hint, &row_text(cx, 12., 400., theme.color_text_muted), Wrapping::NORMAL)))
                    .into_any_element(),
            ]
        } else {
            row_elements
        };
        let footer_text = |text: &str, color: Hsla, cx: &App| label(text.to_string(), &row_text(cx, 12., 500., color), Wrapping::NOWRAP);
        let (secondary, primary, muted) = (theme.color_text_secondary, theme.color_text_primary, theme.color_text_muted);
        let footer = (cap > 0 || tab == TransfersTab::Downloads || finished).then(|| {
            let mut left: AnyElement = div().into_any_element();
            if cap > 0 {
                let arrow = if tab == TransfersTab::Downloads { "↓" } else { "↑" };
                left = div()
                    .id("transfers-cap")
                    .min_w_0()
                    .cursor_pointer()
                    .tooltip(title_tooltip("Change in Settings → Network"))
                    .child(label(format!("{arrow} Limited to {}", format::cap_label(cap)), &row_text(cx, 12., 400., muted), Wrapping::TRUNCATE))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close(false, window, cx);
                        if let Some(model) = Self::model(cx) {
                            model.nav.update(cx, |nav, cx| nav.set_settings_tab("network", cx));
                            model.overlays.update(cx, |o, cx| o.open_modal(decibell_model::stores::overlays::modal::SETTINGS, cx));
                        }
                    }))
                    .into_any_element();
            }
            let mut right = div().ml_auto().flex().items_center().gap_3();
            if tab == TransfersTab::Downloads {
                right = right.child(
                    div()
                        .id("transfers-open-folder")
                        .cursor_pointer()
                        .child(footer_text("Open folder", secondary, cx))
                        .on_click(cx.listener(|_, _, _, cx| super::open_folder(cx))),
                );
            }
            if finished {
                right = right.child(
                    div()
                        .id("transfers-clear")
                        .cursor_pointer()
                        .child(footer_text("Clear", secondary, cx))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let Some(model) = Self::model(cx) else { return };
                            match tab {
                                TransfersTab::Downloads => {
                                    if let Some(control) = model.live.transfers.read(cx).control() {
                                        control.clear_finished();
                                    }
                                }
                                TransfersTab::Uploads => model.live.transfers.update(cx, |t, cx| t.clear_finished_uploads(cx)),
                            }
                        })),
                );
            }
            let _ = primary;
            div()
                .flex()
                .items_center()
                .gap_3()
                .border_t_1()
                .border_color(theme.color_border_divider)
                .px_3()
                .py_2()
                .child(left)
                .child(right)
        });
        let button = self.button.clone();
        let panel = div()
            .id("transfers-panel")
            .occlude()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_secondary)
            .shadow(theme.shadow_modal.clone())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    if this.menu.take().is_some() {
                        cx.notify();
                    } else {
                        this.close(true, window, cx);
                    }
                }
            }))
            .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                // The button's own press is its toggle; a row menu closes by itself first.
                if button.get().contains(&event.position) || this.menu.is_some() {
                    return;
                }
                this.close(false, window, cx);
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if this.menu.take().is_some() {
                    cx.notify();
                }
            }))
            .child(div().px_3().pb_2().pt_3().child(tabs))
            .child(
                ScrollArea::new("transfers-list")
                    .track_scroll(&self.scroll)
                    .max_h(px(LIST_MAX))
                    .child(div().px(px(6.)).pb(px(6.)).children(list)),
            )
            .children(footer)
            .children(decibell_shell::probes::probe("transfers-panel"));
        // `right: max(MARGIN, innerWidth - rect.right - MARGIN)` under the button, `top: rect.bottom + 6`.
        let b = self.button.get();
        let visible = visible_window_bounds(window, cx);
        let right = (b.right() + px(MARGIN)).min(visible.right() - px(MARGIN));
        let origin = point(right - px(WIDTH), b.bottom() + px(6.));
        let reduce = cx.reduce_motion();
        let entrance: AnyElement = if reduce {
            panel.into_any_element()
        } else {
            panel
                .with_animation(("transfers-panel-in", self.opened), Animation::new(Duration::from_millis(120)).with_easing(motion::EASE.easing()), |panel, t| {
                    panel.opacity(t).top(px(-4. * (1. - t)))
                })
                .into_any_element()
        };
        decibell_ui::layer(3, Placed::new(Placement::At(origin)).child(entrance)).into_any_element()
    }

    /// Keeps the previews of the rows shown wanted by the pipeline (and only those).
    fn want_previews(&mut self, rows: &[Row], cx: &mut Context<Self>) {
        let keys: Vec<PicKey> = rows
            .iter()
            .filter_map(|row| match &row.preview {
                Preview::Picture(key) => Some(key.clone()),
                Preview::Glyph => None,
            })
            .collect();
        if keys == self.wanted {
            return;
        }
        self.wanted = keys.clone();
        let Some(pictures) = Pictures::global(cx) else { return };
        // The upload previews are the composer's bytes: handed over before they're asked for.
        let uploads: Vec<std::sync::Arc<gpui::Image>> =
            Self::model(cx).map(|m| m.live.transfers.read(cx).uploads().iter().filter_map(|u| u.preview.clone()).collect()).unwrap_or_default();
        let owner = (cx.entity_id(), 7);
        cx.defer(move |cx| {
            pictures.update(cx, |p, cx| {
                for image in uploads {
                    p.provide(Source::Local(image.id), bytes::Bytes::copy_from_slice(&image.bytes));
                }
                p.set_wants(owner, keys.into_iter().map(|key| Want { key, visible: true }).collect(), cx);
            })
        });
    }

    fn row_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (at, id) = self.menu.clone()?;
        let model = Self::model(cx)?;
        let d = model.live.transfers.read(cx).downloads().iter().find(|d| d.id == id).cloned()?;
        let can_open = d.state == DownloadState::Done && !d.missing;
        let mut menu = ContextMenu::new("transfer-menu", at).width(px(180.));
        let close = cx.listener(|this, _: &decibell_ui::DismissEvent, _, cx| {
            this.menu = None;
            cx.notify();
        });
        if can_open {
            let open = d.clone();
            menu = menu.entry(MenuItem::new("Open").on_click(cx.listener(move |this, _, _, cx| {
                this.menu = None;
                this.open_file(&open, cx);
            })));
            let reveal = d.id.clone();
            menu = menu.entry(MenuItem::new("Show in folder").on_click(cx.listener(move |this, _, _, cx| {
                this.menu = None;
                super::show_in_folder(&reveal, cx);
            })));
        }
        if !d.context.channel_id.is_empty() {
            let go = d.id.clone();
            menu = menu.entry(MenuItem::new("Go to message").on_click(cx.listener(move |this, _, window, cx| {
                this.close(false, window, cx);
                actions::transfers::go_to_download(&go, cx);
            })));
        }
        if d.state != DownloadState::Active {
            let remove = d.id.clone();
            menu = menu.entry(MenuItem::new("Remove from list").on_click(cx.listener(move |this, _, _, cx| {
                this.menu = None;
                if let Some(control) = Self::model(cx).and_then(|m| m.live.transfers.read(cx).control()) {
                    control.remove(std::slice::from_ref(&remove));
                }
                cx.notify();
            })));
        }
        if d.state == DownloadState::Active {
            let cancel = d.id.clone();
            menu = menu.entry(MenuItem::new("Cancel").on_click(cx.listener(move |this, _, _, cx| {
                this.menu = None;
                if let Some(control) = Self::model(cx).and_then(|m| m.live.transfers.read(cx).control()) {
                    control.cancel(&cancel);
                }
                cx.notify();
            })));
        }
        Some(menu.on_dismiss(close).into_any_element())
    }

    fn confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let open = self.risky.is_some();
        let (name, sender) = match &self.risky {
            Some(d) => {
                let sender = (!d.context.sender.is_empty()).then(|| {
                    cx.try_global::<Model>().map_or_else(|| d.context.sender.clone().into(), |m| m.name_of(d.context.sender_uid, &d.context.sender.clone().into(), cx))
                });
                (d.filename.clone(), sender)
            }
            None => (String::new(), None),
        };
        let plain = row_text(cx, 16., 400., theme.color_text_primary);
        let bold = row_text(cx, 16., 600., theme.color_text_primary);
        let mut runs = vec![(name, bold), (" can run programs on your computer. Only open it if you trust ".to_string(), plain)];
        match sender {
            Some(sender) => runs.push((sender.to_string(), bold)),
            None => runs.push(("whoever sent it".to_string(), plain)),
        }
        runs.push((".".to_string(), plain));
        let mut modal = ConfirmModal::new("transfers-risky", open, "Open this file?", "Open anyway")
            .on_confirm(cx.listener(|this, _, _, cx| {
                if let Some(d) = this.risky.take() {
                    super::open_download(&d.id, cx);
                }
                cx.notify();
            }))
            .on_cancel(cx.listener(|this, _, _, cx| {
                this.risky = None;
                cx.notify();
            }));
        if open {
            modal = modal.child(Runs::new(runs));
        }
        modal.into_any_element()
    }
}

/// A row's text: the UI font at `size` / `weight`, line height 1.5, the body tracking.
fn row_text(cx: &App, size: f32, weight: f32, color: Hsla) -> TextStyle {
    let theme = cx.theme();
    let engine = crate::engine(cx);
    let face = engine.face(&decibell_text::FontSpec { family: theme.font(FontRole::Sans), weight: weight.round() as u16, italic: false });
    let mut style = TextStyle::new(face, size, color);
    style.letter_spacing = theme.palette.t_ls_body * 16.;
    style
}

/// `TransferPreview`: the picture when there is one to show, else (or when it fails) the kind glyph.
fn preview_element(row: &Row, background: Hsla, muted: Hsla, radius: Pixels, cx: &mut App) -> AnyElement {
    let base = div().size(px(PREVIEW)).flex_none().flex().items_center().justify_center().overflow_hidden().rounded(radius).bg(background);
    let glyph = || Icon::new(kind_icon(row.kind)).size(px(16.)).color(muted);
    match &row.preview {
        Preview::Picture(key) => {
            let look = Pictures::global(cx).map(|p| p.read(cx).look(key));
            match look {
                None | Some(Look::Failed(_)) => base.child(glyph()).into_any_element(),
                Some(_) => {
                    let key = key.clone();
                    base.child(
                        canvas(|_, _, _| {}, move |bounds, _, window, cx| {
                            let Some(pictures) = Pictures::global(cx) else { return };
                            let view = window.current_view();
                            let look = pictures.update(cx, |p, cx| {
                                let look = p.look(&key);
                                if matches!(look, Look::Waiting) {
                                    p.request(&key, Some(view), cx);
                                }
                                look
                            });
                            if let Look::Ready(picture) = look {
                                picture.paint(bounds.origin, Corners::all(radius), window);
                            }
                        })
                        .size_full(),
                    )
                    .into_any_element()
                }
            }
        }
        Preview::Glyph => base.child(glyph()).into_any_element(),
    }
}

/// The ring around the glyph: the track at a quarter of the colour, the arc of what is done.
fn paint_ring(bounds: Bounds<Pixels>, fraction: f32, color: Hsla, window: &mut Window) {
    let unit = f32::from(bounds.size.width) / 24.;
    let at = |(x, y): (f32, f32)| point(bounds.origin.x + px(x * unit), bounds.origin.y + px(y * unit));
    let stroke = |cap| {
        PathBuilder::stroke(px(1.5 * unit)).with_style(gpui::PathStyle::Stroke(gpui::StrokeOptions {
            line_width: 1.5 * unit,
            line_cap: cap,
            ..Default::default()
        }))
    };
    let radii = point(px(10. * unit), px(10. * unit));
    let mut track = stroke(gpui::LineCap::Butt);
    track.move_to(at((12., 2.)));
    track.arc_to(radii, px(0.), false, true, at((12., 22.)));
    track.arc_to(radii, px(0.), false, true, at((12., 2.)));
    if let Ok(path) = track.build() {
        window.paint_path(path, Hsla { a: color.a * 0.25, ..color });
    }
    let mut arc = stroke(gpui::LineCap::Round);
    match format::ring_arc(fraction) {
        // A zero-length dash with a round cap: Chromium draws its dot.
        None => {
            arc.move_to(at((12., 2.)));
            arc.line_to(at((12.001, 2.)));
        }
        Some((start, end, _)) if fraction >= 0.999 => {
            arc.move_to(at(start));
            arc.arc_to(radii, px(0.), false, true, at((12., 22.)));
            arc.arc_to(radii, px(0.), false, true, at(end));
        }
        Some((start, end, large)) => {
            arc.move_to(at(start));
            arc.arc_to(radii, px(0.), large, true, at(end));
        }
    }
    if let Ok(path) = arc.build() {
        window.paint_path(path, color);
    }
}

impl Render for TransfersButton {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(model) = Self::model(cx) else { return div().into_any_element() };
        let (open, tab, attention, pulse, progress) = {
            let t = model.live.transfers.read(cx);
            (t.panel_open, t.tab, t.attention, t.start_pulse, t.progress())
        };
        if open && !self.was_open {
            self.opening(window, cx);
        } else if !open && self.was_open {
            self.closed(cx);
        }
        self.was_open = open;
        let chrome = &cx.theme().chrome;
        let running = progress.active + progress.paused;
        let label = format::button_label(progress.active, progress.paused);
        let ring_color = if progress.active > 0 { chrome.color_accent } else { chrome.color_text_muted };
        let (text, hover_text, hover_bg) = (
            if open { chrome.color_text_primary } else { chrome.color_text_secondary },
            chrome.color_text_primary,
            chrome.color_surface_active,
        );
        let dot = (attention != Attention::None && !open).then(|| if attention == Attention::Failed { chrome.color_error } else { chrome.color_accent });
        let fraction = progress.fraction.unwrap_or(0.) as f32;
        let bounds = self.button.clone();
        let arrow = div().absolute().left_0().top_0().size(px(14.)).child(Icon::new(IconName::ContentTransfersArrow).size(px(14.)).color(text).group_hover("transfers-button", hover_text));
        let arrow: AnyElement = if pulse > 0 && !cx.reduce_motion() {
            arrow
                .with_animation(("transfers-arrow", pulse), Animation::new(Duration::from_millis(350)).with_easing(motion::EASE.easing()), |arrow, t| {
                    arrow.opacity(t).top(px(-4. * (1. - t)))
                })
                .into_any_element()
        } else {
            arrow.into_any_element()
        };
        let glyph = div()
            .relative()
            .size(px(14.))
            .child(div().absolute().left_0().top_0().size(px(14.)).child(Icon::new(IconName::ContentTransfersTray).size(px(14.)).color(text).group_hover("transfers-button", hover_text)))
            .child(arrow);
        let ring = (running > 0).then(|| canvas(|_, _, _| {}, move |b, _, window, _| paint_ring(b, fraction, ring_color, window)).absolute().size_full());
        let button = div()
            .id("transfers-button")
            .group("transfers-button")
            .relative()
            .h_full()
            .w_11()
            .flex()
            .items_center()
            .justify_center()
            .when(open, |d| d.bg(hover_bg))
            .hover(move |s| s.bg(hover_bg))
            .tooltip(title_tooltip(format!("{label} (Ctrl+J)")))
            .child(div().relative().size_6().flex().items_center().justify_center().children(ring).child(glyph))
            .children(dot.map(|color| div().absolute().right(px(10.)).top(px(6.)).size(px(6.)).rounded_full().bg(color)))
            .child(canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().size_full())
            .children(decibell_shell::probes::probe("transfers-button"))
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                // Not a window drag: the title bar starts one on any press it receives.
                cx.stop_propagation();
                let _ = window;
            })
            .on_click(cx.listener(|_, _, _, cx| {
                if let Some(model) = Self::model(cx) {
                    model.live.transfers.update(cx, |t, cx| t.toggle_panel(cx));
                }
            }));
        let panel = open.then(|| self.panel(tab, window, cx));
        // The kit's menu layers itself (above the panel).
        let menu = self.row_menu(cx);
        let confirm = self.confirm(cx);
        div().h_full().flex().child(button).children(panel).children(menu).child(confirm).into_any_element()
    }
}
