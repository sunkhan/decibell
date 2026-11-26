//! Downloading attachments from chat (`features/transfers/downloads.ts` `startDownload` and
//! main's `downloads.ts` `reserveDestination`): straight into the download folder, or through a
//! save dialog for "Save as…" and when Settings says "ask where to save each file". Progress shows
//! in the Transfers panel; only a failure to start is toasted (the model's `start_download`).

use std::path::PathBuf;

use decibell_model::chat::Attachment;
use decibell_model::{Model, ServerId, actions};
use gpui::{App, Task};

/// The folder downloads land in: Settings' choice, else the OS Downloads folder (Electron's
/// `app.getPath("downloads")`: `$XDG_DOWNLOAD_DIR`, else `~/Downloads`).
pub fn download_dir(cx: &App) -> PathBuf {
    let configured = cx.try_global::<Model>().map(|m| m.settings.read(cx).get().download_dir.clone()).unwrap_or_default();
    if !configured.is_empty() && std::path::Path::new(&configured).is_absolute() {
        return PathBuf::from(configured);
    }
    os_downloads_dir()
}

/// The OS Downloads folder.
pub fn os_downloads_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_DOWNLOAD_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    // `xdg-user-dirs` keeps the folder's (possibly localised) name in user-dirs.dirs.
    if let Ok(text) = std::fs::read_to_string(home.join(".config/user-dirs.dirs")) {
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("XDG_DOWNLOAD_DIR=") {
                let value = value.trim().trim_matches('"').replace("$HOME", &home.to_string_lossy());
                if !value.is_empty() {
                    return PathBuf::from(value);
                }
            }
        }
    }
    home.join("Downloads")
}

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
