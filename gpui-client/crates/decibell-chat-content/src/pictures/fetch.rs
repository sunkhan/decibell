//! Where a picture's bytes come from, and the fetcher that brings them.

use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use decibell_attachments::CancellationToken;
use gpui::{App, AppContext as _, SharedString, Task};

/// Where a picture's bytes come from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    /// An attachment of a community (its wire id): a thumbnail (`thumb` = 320 / 640 / 1280) or
    /// the original (`thumb` = 0). Sealed ones are opened by `decibell-attachments`.
    Attachment { server: SharedString, id: i64, thumb: u32 },
    /// A picture on the public web (https), through `decibell_unfurl::RemoteImages`.
    Remote(SharedString),
    /// Bytes the app already holds (an upload's preview, a community picture), handed over with
    /// [`super::Pictures::provide`]; the number names them.
    Local(u64),
}

/// Brings bytes. The app's [`CoreFetcher`] runs on the core's runtime; screens use a memory one.
pub trait Fetcher: 'static {
    /// The bytes of `source`, or why not. Must not block the UI thread; `cancel` ends it early.
    fn fetch(&self, source: &Source, cancel: CancellationToken, cx: &mut App) -> Task<Result<Bytes, SharedString>>;
    /// The answer now, when the fetcher has it at hand (the screens' memory one): the pipeline
    /// then fetches and decodes within the frame that asks, so a capture shows the pictures.
    fn fetch_sync(&self, _source: &Source) -> Option<Result<Bytes, SharedString>> {
        None
    }
    /// The queue `source` waits in and how many of that queue run at once: six per community
    /// (Chromium's connections per host, which bounded the renderer's `<img>` fetches through
    /// the attachment protocol), four for the public web (the unfurler's own limit).
    fn lane(&self, source: &Source) -> (SharedString, usize) {
        match source {
            Source::Attachment { server, .. } => (server.clone(), 6),
            Source::Remote(_) => (SharedString::new_static("remote"), 4),
            Source::Local(_) => (SharedString::new_static("local"), 1),
        }
    }
}

/// The app's fetcher: attachments through the process's `decibell_attachments::Attachments`
/// (sealed ones decrypted there, held to Settings' download cap), remote pictures through
/// `decibell_unfurl::RemoteImages`, both on the core's runtime.
pub struct CoreFetcher {
    remote: Arc<decibell_unfurl::RemoteImages>,
}

impl CoreFetcher {
    pub fn new(app_version: &str) -> Rc<CoreFetcher> {
        Rc::new(CoreFetcher { remote: Arc::new(decibell_unfurl::RemoteImages::new(app_version)) })
    }

    /// Development self-tests only: remote pictures may come from 127.0.0.0/8.
    #[cfg(feature = "loopback")]
    pub fn loopback(app_version: &str) -> Rc<CoreFetcher> {
        Rc::new(CoreFetcher { remote: Arc::new(decibell_unfurl::RemoteImages::loopback(app_version)) })
    }
}

impl Fetcher for CoreFetcher {
    fn fetch(&self, source: &Source, cancel: CancellationToken, cx: &mut App) -> Task<Result<Bytes, SharedString>> {
        let (tx, rx) = futures::channel::oneshot::channel::<Result<Bytes, SharedString>>();
        match source.clone() {
            // Local bytes come from `provide`; gone from memory, they can't be fetched.
            Source::Local(_) => return Task::ready(Err(SharedString::new_static("not in memory"))),
            Source::Attachment { server, id, thumb } => {
                decibell_model::chat::composer::CoreAttachmentService::with_attachments(move |svc| {
                    tokio::spawn(async move {
                        let result = if thumb > 0 {
                            svc.fetch_thumbnail(&server, id, thumb, Some(cancel)).await
                        } else {
                            svc.fetch_all(&server, id, Some(cancel)).await.map(Bytes::from)
                        };
                        let _ = tx.send(result.map_err(|e| SharedString::from(e.to_string())));
                    });
                });
            }
            Source::Remote(url) => {
                let remote = self.remote.clone();
                decibell_model::core::spawn_detached(async move {
                    let result = tokio::select! {
                        result = remote.fetch(&url) => result.map_err(|e| SharedString::from(e.to_string())),
                        _ = cancel.cancelled() => Err(SharedString::new_static("cancelled")),
                    };
                    let _ = tx.send(result);
                });
            }
        }
        cx.background_spawn(async move { rx.await.unwrap_or_else(|_| Err(SharedString::new_static("Not connected"))) })
    }
}
