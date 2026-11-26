//! `transfers-panel` / `transfers-panel-uploads`: the signed-in shell (Daily Gaming `#general`)
//! with the Transfers panel open over it, from fixture rows in every state (no network, no clock:
//! "now" is pinned).

use std::rc::Rc;

use bytes::Bytes;
use decibell_attachments::CancellationToken;
use decibell_harness::Screen;
use decibell_model::live::transfers::{
    AttachmentKind, DownloadContext, DownloadState, DownloadView, PendingStatus, PendingUpload, TransfersTab,
};
use decibell_model::{ChannelId, ServerId};
use decibell_shell::fixture::{self as shell, Scene};
use gpui::{App, AppContext as _, SharedString, Task};

use super::FixedNow;
use crate::pictures::{Fetcher, Pictures, Source};

/// 2026-11-26 18:00 UTC.
const NOW_MS: i64 = 1_795_716_000_000;
const MIB: u64 = 1024 * 1024;

/// No picture exists: previews show their kind glyph, as the capture pages' `<img>`s fail.
struct NoPictures;

impl Fetcher for NoPictures {
    fn fetch(&self, _: &Source, _: CancellationToken, _: &mut App) -> Task<Result<Bytes, SharedString>> {
        Task::ready(Err(SharedString::new_static("HTTP 404")))
    }
}

fn context(channel: &str, message_id: i64, sender: &str, uid: i64) -> DownloadContext {
    DownloadContext {
        server_name: "Daily Gaming".into(),
        channel_id: channel.into(),
        channel_name: channel.into(),
        message_id,
        sender: sender.into(),
        sender_uid: uid,
    }
}

#[allow(clippy::too_many_arguments)]
fn download(id: &str, attachment_id: i64, filename: &str, kind: AttachmentKind, total: u64, received: u64, state: DownloadState, started_min_ago: i64) -> DownloadView {
    DownloadView {
        id: id.into(),
        server_id: "1".into(),
        attachment_id,
        filename: filename.into(),
        mime: "application/octet-stream".into(),
        kind,
        total_bytes: total,
        received_bytes: received,
        state,
        error: None,
        started_at: NOW_MS - started_min_ago * 60_000,
        finished_at: None,
        context: context("clips", 10_308, "alice", 1),
        speed_bps: 0.,
        eta_s: -1,
        missing: false,
        encrypted: false,
    }
}

/// Every download state the panel draws, newest first.
pub fn downloads() -> Vec<DownloadView> {
    let mut active = download("d1", 1090, "ultrawide-gameplay.mp4", AttachmentKind::Video, 44 * MIB, 18 * MIB + 410 * 1024, DownloadState::Active, 1);
    active.speed_bps = 2.5 * MIB as f64;
    active.eta_s = 11;
    let paused = download("d2", 1089, "voice-memo.wav", AttachmentKind::Audio, 132_344, 61_440, DownloadState::Paused, 3);
    let mut failed = download("d3", 1083, "clip-night-1.jpg", AttachmentKind::Image, 89_249, 0, DownloadState::Failed, 6);
    failed.error = Some("Not connected to Daily Gaming".into());
    let cancelled = download("d4", 1088, "settings-export.zip", AttachmentKind::Document, 301, 120, DownloadState::Cancelled, 9);
    let mut done = download("d5", 1087, "patch-notes.txt", AttachmentKind::Document, 105, 105, DownloadState::Done, 15);
    done.finished_at = Some(NOW_MS - 14 * 60_000);
    let mut yesterday = download("d6", 1086, "phone-screenshot.jpg", AttachmentKind::Image, 84_139, 84_139, DownloadState::Done, 26 * 60);
    yesterday.finished_at = Some(NOW_MS - 26 * 3_600_000);
    let mut gone = download("d7", 1080, "setup-notes.pdf", AttachmentKind::Document, 2 * MIB + 300 * 1024, 2 * MIB + 300 * 1024, DownloadState::Done, 4 * 24 * 60);
    gone.finished_at = Some(NOW_MS - 4 * 86_400_000);
    gone.missing = true;
    gone.context = context("general", 9_120, "bob", 2);
    vec![active, paused, failed, cancelled, done, yesterday, gone]
}

fn pending(id: &str, filename: &str, kind: AttachmentKind, status: PendingStatus, total: u64, sent: u64) -> PendingUpload {
    PendingUpload {
        pending_id: id.into(),
        server: ServerId::from_central(shell::DAILY_GAMING),
        channel: ChannelId::from("general"),
        filename: filename.into(),
        kind,
        status,
        paused: false,
        cancelled: false,
        error: None,
        preview: None,
        attachment_id: None,
        total_bytes: total,
        transferred_bytes: sent,
    }
}

/// Every upload state, oldest first (the store puts the newest on top).
pub fn uploads() -> Vec<PendingUpload> {
    let mut paused = pending("u2", "raid-night.mkv", AttachmentKind::Video, PendingStatus::Uploading, 310 * MIB, 96 * MIB);
    paused.paused = true;
    let mut failed = pending("u4", "big-render.png", AttachmentKind::Image, PendingStatus::Failed, 9 * MIB, 2 * MIB);
    failed.error = Some("The file is larger than this server accepts".into());
    let mut cancelled = pending("u5", "draft.docx", AttachmentKind::Document, PendingStatus::Failed, 640 * 1024, 100 * 1024);
    cancelled.cancelled = true;
    let mut done = pending("u3", "patch-notes-v2.txt", AttachmentKind::Document, PendingStatus::Ready, 2_048, 2_048);
    done.attachment_id = Some(1201);
    vec![
        cancelled,
        failed,
        done,
        paused,
        pending("u1", "highlights.mp4", AttachmentKind::Video, PendingStatus::Uploading, 120 * MIB, 37 * MIB),
    ]
}

/// The shell with the panel open on `tab`.
fn build(tab: TransfersTab, cx: &mut App) -> gpui::AnyView {
    if cx.has_global::<decibell_ui::AssumeClientDecorations>() {
        cx.set_reduce_motion(true);
    }
    let model = shell::apply(Scene::Server, cx);
    if Pictures::global(cx).is_none() {
        crate::install(Rc::new(NoPictures), true, cx);
    }
    cx.set_global(FixedNow(NOW_MS));
    super::enable_title_bar(cx);
    let names = |_: ServerId, channel: &ChannelId| (SharedString::new_static("Daily Gaming"), channel.clone());
    let list = uploads();
    model.live.transfers.update(cx, |t, cx| {
        t.set_downloads(downloads(), cx);
        // Each upload as it first starts, then as it ended: the rows' order and states.
        for (i, p) in list.iter().enumerate() {
            let mut first = p.clone();
            first.status = PendingStatus::Uploading;
            first.paused = false;
            first.cancelled = false;
            first.error = None;
            let mut seen: Vec<PendingUpload> = list[..i].to_vec();
            seen.push(first);
            t.sync_uploads(&seen, names, cx);
        }
        t.sync_uploads(&list, names, cx);
        t.open_panel(Some(tab), cx);
    });
    cx.new(decibell_shell::Shell::new).into()
}

pub fn screens() -> Vec<Screen> {
    vec![
        Screen {
            name: "transfers-panel",
            about: "The Transfers panel open on Downloads: active, paused, failed, cancelled, done, deleted (fixture)",
            build: |_, cx| build(TransfersTab::Downloads, cx),
        },
        Screen {
            name: "transfers-panel-uploads",
            about: "The Transfers panel open on Uploads: uploading, paused, done, failed, cancelled (fixture)",
            build: |_, cx| build(TransfersTab::Uploads, cx),
        },
    ]
}
