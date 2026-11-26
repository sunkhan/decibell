//! The seams between the two halves of the model that were written apart: the chat half
//! ("M2": `chat`, `dm`) and the live / people half ("M3": `live`). What one emits and the
//! other acts on, wired once by `Model::init` after both exist (`install`). Each row names
//! the renderer code it mirrors.
//!
//! | From | To | Renderer |
//! |---|---|---|
//! | `live.calls` emits [`ShowConversation`] (we answered a call) | the caller's DM opens (`actions::dm::open_dm`) | `callActions.ts` `acceptCall`: `setActiveDmUser(inc.fromUid)` |
//! | `live.e2ee` emits `E2eeEvent::BecameReady` (unlock / setup) | every DM page reloads (`events::dm::reload_after_unlock`) | `useE2eeEvents.ts` `reloadDms` |
//! | `chat.composer` changes (observe) | the Transfers uploads tab (`actions::transfers::sync_uploads`) | `features/transfers/uploads.ts` `initUploads` |
//! | the Transfers panel's pause / resume / cancel | the composer's uploads ([`ComposerUploads`]) | `uploads.ts` `pauseUpload` / `cancelUpload` |
//!
//! Elsewhere, not subscriptions: `live::window_title` reads the DM list itself, a
//! download's request is built from a message's attachment by `actions::chat::download_request`,
//! the router's `connection_lost` arm ends DM history waits (`events::dm::connection_lost`).
//!
//! Every subscription here lives as long as the model (the app's lifetime). None holds a
//! timer; the composer observer runs only while attachments change (~10 Hz per upload).

use std::rc::Rc;

use gpui::{App, Entity};

use crate::Model;
use crate::actions::calls::ShowConversation;
use crate::chat::composer::{self, ComposerAttachments};
use crate::live::e2ee::E2eeEvent;
use crate::live::transfers::{PendingStatus, PendingUpload, UploadControl};

/// Pause / resume / cancel from the Transfers panel, on the composer's uploads.
pub struct ComposerUploads(pub Entity<ComposerAttachments>);

impl UploadControl for ComposerUploads {
    fn set_paused(&self, pending_id: &str, paused: bool, cx: &mut App) {
        self.0.update(cx, |c, cx| c.set_paused(pending_id, paused, cx));
    }

    /// The upload stops and ends as failed, cancelled (`abortController.abort()`); the
    /// pending stays until its message goes or the chip is removed.
    fn cancel(&self, pending_id: &str, cx: &mut App) {
        self.0.update(cx, |c, _| c.cancel(pending_id));
    }
}

/// The composer's pendings as the Transfers store reads them.
pub fn pending_uploads(composer: &ComposerAttachments) -> Vec<PendingUpload> {
    composer
        .iter()
        .map(|p| PendingUpload {
            pending_id: p.id.clone(),
            server: p.server,
            channel: p.channel.clone(),
            filename: p.filename.clone(),
            kind: p.kind,
            status: match p.status {
                composer::PendingStatus::Queued => PendingStatus::Queued,
                composer::PendingStatus::Uploading => PendingStatus::Uploading,
                composer::PendingStatus::Ready => PendingStatus::Ready,
                composer::PendingStatus::Failed => PendingStatus::Failed,
            },
            paused: p.paused,
            cancelled: p.cancelled,
            error: p.error.clone(),
            preview: p.preview.clone(),
            attachment_id: p.attachment_id,
            total_bytes: p.total_bytes,
            transferred_bytes: p.transferred_bytes,
        })
        .collect()
}

pub fn install(model: &Model, cx: &mut App) {
    // A call we answered lands on the caller's DM, where the call panel is.
    cx.subscribe(&model.live.calls, {
        let model = model.clone();
        move |_, ShowConversation(peer): &ShowConversation, cx| {
            let fallback = model.dm.list.read(cx).get(*peer).map(|c| c.username.clone()).unwrap_or_default();
            let name = model.name_of(*peer, &fallback, cx);
            crate::actions::dm::open_dm(*peer, name, cx);
        }
    })
    .detach();

    // Keys became usable: every sealed DM that arrived before holds a placeholder.
    cx.subscribe(&model.live.e2ee, {
        let model = model.clone();
        move |_, event: &E2eeEvent, cx| {
            if *event == E2eeEvent::BecameReady {
                crate::events::dm::reload_after_unlock(&model, cx);
            }
        }
    })
    .detach();

    // The Uploads tab mirrors the composer; its buttons act on the composer's uploads.
    let composer = model.chat.composer.clone();
    model.live.transfers.update(cx, |t, _| t.set_upload_control(Rc::new(ComposerUploads(composer.clone()))));
    cx.observe(&composer, |composer, cx| {
        let pendings = pending_uploads(composer.read(cx));
        crate::actions::transfers::sync_uploads(&pendings, cx);
    })
    .detach();
}

#[cfg(test)]
mod tests;
