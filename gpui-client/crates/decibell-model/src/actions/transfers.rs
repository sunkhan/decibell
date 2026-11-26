//! The Transfers panel's commands (`features/transfers/downloads.ts`, `uploads.ts`,
//! Settings → Network → Downloads).

use std::path::PathBuf;

use gpui::{App, SharedString, Task};

use crate::Model;
use crate::live::transfers::{Destination, DownloadContext, PendingUpload, StartArgs};
use crate::ids::ServerId;

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

/// Hand the download settings (`download_dir`, `ask_download_location`) to the manager:
/// at start and after every change.
pub fn configure_downloads(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let Some(control) = model.live.transfers.read(cx).control() else { return };
    let s = model.settings.read(cx).get();
    let dir = (!s.download_dir.is_empty()).then(|| PathBuf::from(&s.download_dir));
    control.configure(dir, s.ask_download_location);
}

pub fn set_download_dir(dir: String, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.settings.update(cx, |s, cx| s.change(|v| v.download_dir = dir, cx));
    configure_downloads(cx);
}

pub fn set_ask_download_location(ask: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.settings.update(cx, |s, cx| s.change(|v| v.ask_download_location = ask, cx));
    configure_downloads(cx);
}

/// What a download needs from the message it came from (M2 finds the attachment).
pub struct DownloadRequest {
    pub server: ServerId,
    pub attachment_id: i64,
    pub filename: String,
    pub mime: String,
    pub kind: crate::live::transfers::AttachmentKind,
    pub size_bytes: u64,
    pub context: DownloadContext,
    /// A path from a save dialog ("Save as…", or "ask each time"); none = the folder.
    pub save_to: Option<PathBuf>,
}

/// Start a download through the manager. Progress shows in the panel; only a failure to
/// start is toasted. Resolves whether it started.
pub fn start_download(req: DownloadRequest, cx: &mut App) -> Task<bool> {
    let Some(model) = model(cx) else { return Task::ready(false) };
    let Some(control) = model.live.transfers.read(cx).control() else { return Task::ready(false) };
    let started = control.start(StartArgs {
        server_id: req.server.wire(),
        attachment_id: req.attachment_id,
        filename: req.filename,
        mime: req.mime,
        kind: req.kind,
        size_bytes: req.size_bytes,
        context: req.context,
        destination: req.save_to.map_or(Destination::Folder, Destination::Chosen),
    });
    cx.spawn(async move |cx| match started.await {
        Ok(_) => {
            cx.update(|cx| model.live.transfers.update(cx, |t, cx| t.note_started(cx)));
            true
        }
        Err(e) => {
            cx.update(|cx| model.toasts.update(cx, |t, cx| t.error("Download failed", Some(e), cx)));
            false
        }
    })
}

/// M2's attachments store changed its pendings: mirror them into the Uploads tab.
pub fn sync_uploads(pendings: &[PendingUpload], cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let names = {
        let model = model.clone();
        move |server: ServerId, channel: &SharedString, cx: &App| {
            let server_name = model.servers.read(cx).get(server).map(|s| s.name.clone()).unwrap_or_default();
            let channel_name = model
                .community(server, cx)
                .and_then(|c| c.channels.read(cx).get(channel).map(|c| c.name.clone()))
                .unwrap_or_default();
            (server_name, channel_name)
        }
    };
    // Names are read before the store's update (a store can't read the others inside it).
    let resolved: Vec<(ServerId, SharedString, SharedString, SharedString)> = pendings
        .iter()
        .map(|p| {
            let (s, c) = names(p.server, &p.channel, cx);
            (p.server, p.channel.clone(), s, c)
        })
        .collect();
    model.live.transfers.update(cx, |t, cx| {
        t.sync_uploads(
            pendings,
            |server, channel| {
                resolved
                    .iter()
                    .find(|(s, c, _, _)| *s == server && c == channel)
                    .map(|(_, _, s, c)| (s.clone(), c.clone()))
                    .unwrap_or_default()
            },
            cx,
        )
    });
}

/// An upload row's "Go to message" (`UploadRow.tsx`): its channel, and the message its
/// attachment went out in once that message is loaded there (else just the channel).
pub fn go_to_upload(pending_id: &str, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let Some((server, channel, attachment_id)) = model
        .live
        .transfers
        .read(cx)
        .uploads()
        .iter()
        .find(|u| &*u.pending_id == pending_id)
        .map(|u| (u.server, u.channel.clone(), u.attachment_id))
    else {
        return;
    };
    let message_id = attachment_id.map_or(0, |id| crate::actions::chat::message_id_for_attachment(server, &channel, id, cx));
    crate::actions::chat::go_to_message(server, channel, message_id, cx);
}

/// A download row's "Go to message" (`TransfersPanel.tsx`): the message it came from, as
/// its context recorded it when the download started.
pub fn go_to_download(download_id: &str, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let Some((server, channel, message_id)) = model
        .live
        .transfers
        .read(cx)
        .downloads()
        .iter()
        .find(|d| d.id == download_id)
        .map(|d| (ServerId::from_wire(&d.server_id), SharedString::from(d.context.channel_id.clone()), d.context.message_id))
    else {
        return;
    };
    crate::actions::chat::go_to_message(server, channel, message_id, cx);
}

pub fn pause_upload(pending_id: &str, paused: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if let Some(control) = model.live.transfers.read(cx).upload_control() {
        control.set_paused(pending_id, paused, cx);
    }
}

pub fn cancel_upload(pending_id: &str, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if let Some(control) = model.live.transfers.read(cx).upload_control() {
        control.cancel(pending_id, cx);
    }
}
