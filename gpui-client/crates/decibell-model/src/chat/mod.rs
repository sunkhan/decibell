//! Messages and everything the text-chat views read (the "M2" half of the state model;
//! docs/model.md §15): channel slices and their registry, history merge and requests, send
//! pacing, slowmode, drafts, the composer's attachments, link previews, search, and the
//! small viewer stores. DMs are `crate::dm`.
//!
//! | Entity | Owns | Notifies on |
//! |---|---|---|
//! | [`ChannelCache`] | `(server, channel)` → [`MessageSlice`]; LRU of 10; displayed channel; re-sync epochs; jump request | a slice created / dropped (+ [`CacheEvent`]) |
//! | [`MessageSlice`] (one per channel) | rows + facts + change log, history flags, scroll, held rows, pending deletes, echo watchdogs | any change to that channel |
//! | [`HistoryRequests`] | waits on history replies (10 s), shared newest-page loads | never (nobody draws it) |
//! | [`SendPacing`] | per-community token buckets and queues | never |
//! | [`Slowmode`] | per-channel windows | a window changed ([`SlowmodeEvent`]) |
//! | [`Drafts`] | channel and DM drafts | a draft changed ([`DraftsEvent`]) |
//! | [`ComposerAttachments`] | attachments in message boxes, upload state | that channel's attachments ([`ComposerEvent`]) |
//! | [`LinkPreviewMemo`] | 400 unfurls | an entry ([`PreviewChanged`]) |
//! | [`SearchStore`] | the one search, its chips and results | search state |
//! | viewer stores | image viewer, media menu, playing audio / video, video cache versions, drag state | their own change |

pub mod cache;
pub mod composer;
pub mod drafts;
pub mod history;
pub mod link_previews;
pub mod merge;
pub mod message;
pub mod pacing;
pub mod search;
pub mod slice;
pub mod slowmode;
pub mod time;
pub mod viewers;

use std::rc::Rc;

use gpui::{App, AppContext as _, Entity};

pub use cache::{CacheEvent, ChannelCache, SliceKey};
pub use composer::{AttachmentService, ComposerAttachments, ComposerEvent, CoreAttachmentService};
pub use drafts::{DraftKey, Drafts, DraftsEvent};
pub use history::HistoryRequests;
pub use link_previews::{LinkPreviewMemo, PreviewChanged, PreviewEntry};
pub use message::{Attachment, DecryptError, Message, Reply, RowKey};
pub use pacing::SendPacing;
pub use search::SearchStore;
pub use slice::{Changes, MessageSlice, RowFacts, RowOp, ScrollPosition};
pub use slowmode::{Slowmode, SlowmodeEvent};
pub use viewers::{ActiveAudio, ActiveVideo, DragState, ImageContextMenu, ImageViewer, VideoCacheVersions};

use crate::stores::navigation::{NavEvent, Navigation};

/// The chat grouping exposed on `Model` (`model.chat`). Cheap to clone (handles).
#[derive(Clone)]
pub struct ChatStores {
    pub channels: Entity<ChannelCache>,
    pub history: Entity<HistoryRequests>,
    pub pacing: Entity<SendPacing>,
    pub slowmode: Entity<Slowmode>,
    pub drafts: Entity<Drafts>,
    pub composer: Entity<ComposerAttachments>,
    pub link_previews: Entity<LinkPreviewMemo>,
    pub search: Entity<SearchStore>,
    pub viewer: Entity<ImageViewer>,
    pub media_menu: Entity<ImageContextMenu>,
    pub audio: Entity<ActiveAudio>,
    pub video: Entity<ActiveVideo>,
    pub video_versions: Entity<VideoCacheVersions>,
    pub drag: Entity<DragState>,
}

impl ChatStores {
    /// The stores with the app's services: history and search requests through the core,
    /// unfurls on the core's runtime, uploads through `decibell-attachments`.
    pub fn new(cx: &mut App) -> ChatStores {
        ChatStores::with(
            cx,
            Rc::new(crate::actions::chat::send_history_request),
            Rc::new(crate::actions::chat::send_search_request),
            link_previews::core_unfurler(env!("CARGO_PKG_VERSION")),
            Rc::new(CoreAttachmentService::default()),
        )
    }

    /// With injected services (tests).
    pub fn with(
        cx: &mut App,
        history: history::HistorySender,
        search: search::SearchSender,
        unfurler: link_previews::Unfurler,
        attachments: Rc<dyn AttachmentService>,
    ) -> ChatStores {
        ChatStores {
            channels: cx.new(|_| ChannelCache::default()),
            history: cx.new(|_| HistoryRequests::new(history)),
            pacing: cx.new(|_| SendPacing::default()),
            slowmode: cx.new(|_| Slowmode::default()),
            drafts: cx.new(|_| Drafts::default()),
            composer: cx.new(|_| ComposerAttachments::new(attachments)),
            link_previews: cx.new(|_| LinkPreviewMemo::new(unfurler)),
            search: cx.new(|_| SearchStore::new(search)),
            viewer: cx.new(|_| ImageViewer::default()),
            media_menu: cx.new(|_| ImageContextMenu::default()),
            audio: cx.new(|_| ActiveAudio::default()),
            video: cx.new(|_| ActiveVideo::default()),
            video_versions: cx.new(|_| VideoCacheVersions::default()),
            drag: cx.new(|_| DragState::default()),
        }
    }

    /// The channel LRU follows navigation (`setActiveChannel`'s cache half). Called once by
    /// `Model::init`.
    pub fn follow_navigation(&self, nav: &Entity<Navigation>, cx: &mut App) {
        let channels = self.channels.clone();
        cx.subscribe(nav, move |_, event: &NavEvent, cx| match event {
            NavEvent::ChannelActivated { server, channel } => {
                channels.update(cx, |c, cx| c.activate(*server, channel.clone(), cx));
            }
        })
        .detach();
    }
}
