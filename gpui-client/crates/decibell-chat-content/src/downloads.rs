//! Downloading attachments from chat (`features/transfers/downloads.ts` `startDownload` and
//! main's `downloads.ts` `reserveDestination`): straight into the download folder, or through a
//! save dialog for "Save as…" and when Settings says "ask where to save each file". Progress shows
//! in the Transfers panel; only a failure to start is toasted (the model's `start_download`).

use std::path::PathBuf;

use decibell_model::chat::Attachment;
use decibell_model::{Model, ServerId, actions};
use gpui::{App, Task};

/// The folder downloads land in: Settings' choice, else the OS Downloads folder (Electron's
/// `app.getPath("downloads")`: [`os_downloads_dir`]).
pub fn download_dir(cx: &App) -> PathBuf {
    let configured = cx.try_global::<Model>().map(|m| m.settings.read(cx).get().download_dir.clone()).unwrap_or_default();
    if !configured.is_empty() && std::path::Path::new(&configured).is_absolute() {
        return PathBuf::from(configured);
    }
    os_downloads_dir()
}

/// The OS Downloads folder (`decibell_model::actions::transfers::os_downloads_dir`).
pub use decibell_model::actions::transfers::os_downloads_dir;

/// Starts downloading `a`; resolves whether it started (false for a dismissed dialog or a
/// failure, which the model toasts).
pub fn start(server: ServerId, a: &Attachment, save_as: bool, cx: &mut App) -> Task<bool> {
    let ask = save_as
        || cx
            .try_global::<Model>()
            .and_then(|m| m.live.transfers.read(cx).control())
            .is_some_and(|control| control.ask_each_time());
    let attachment = a.clone();
    if !ask {
        let request = actions::chat::download_request(server, attachment.id, Some(&attachment), None, cx);
        return actions::transfers::start_download(request, cx);
    }
    // The dialog opens on the download folder with the sender's (sanitised) name.
    let dir = download_dir(cx);
    let name = decibell_attachments::names::sanitize_filename(&attachment.filename);
    let chosen = cx.prompt_for_new_path(&dir, Some(&name));
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(path))) = chosen.await else { return false };
        let started = cx.update(|cx| {
            let request = actions::chat::download_request(server, attachment.id, Some(&attachment), Some(path), cx);
            actions::transfers::start_download(request, cx)
        });
        started.await
    })
}
