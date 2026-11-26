//! Small UI stores the chat views share, each its own entity so changing one redraws only
//! what shows it: the image viewer (`imageViewerStore.ts`), the image / video context menu
//! (`imageContextMenuStore.ts`), the playing audio and video attachment
//! (`activeAudioStore.ts`, `activeVideoStore.ts`), the per-attachment video cache version
//! (`videoCacheVersionStore.ts`), and the attachment drag-and-drop state (`uiStore`'s
//! `dragActive` / `dragHoveredKey`, which change on every pointer move over a target).

use std::collections::HashMap;

use gpui::{Context, EventEmitter, SharedString};

use crate::ids::{ChannelId, ServerId, Uid};

// ── Image viewer ────────────────────────────────────────────────────────────

/// One picture the viewer can show: an image attachment, or an image a link preview found.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewerImage {
    /// Stable identity: the thumbnail it flies out of and back into.
    pub key: SharedString,
    pub url: SharedString,
    /// What the chat shows already (painted under the full size at once).
    pub preview_url: Option<SharedString>,
    /// Filmstrip size (the 320 thumbnail when the server has one).
    pub thumb_url: Option<SharedString>,
    pub placeholder: Option<SharedString>,
    pub filename: SharedString,
    pub width: u32,
    pub height: u32,
    pub size_bytes: Option<i64>,
    pub mime: Option<SharedString>,
    /// Attachments: Copy / Save / the menu.
    pub server: Option<ServerId>,
    pub attachment_id: Option<i64>,
    pub channel: Option<ChannelId>,
    pub sender_uid: Option<Uid>,
    pub sender: Option<SharedString>,
    pub timestamp: Option<i64>,
    /// Link images: the page the link points at.
    pub page_url: Option<SharedString>,
}

/// The thumbnail's box when the viewer opened (window coordinates), and how it was cropped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerOrigin {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// Grid cells crop the picture (cover); standalone ones don't (contain).
    pub cover: bool,
}

#[derive(Default)]
pub struct ImageViewer {
    pub items: Vec<ViewerImage>,
    pub index: usize,
    pub origin: Option<ViewerOrigin>,
}

impl ImageViewer {
    pub fn is_open(&self) -> bool {
        !self.items.is_empty()
    }

    pub fn open(&mut self, items: Vec<ViewerImage>, index: usize, origin: Option<ViewerOrigin>, cx: &mut Context<Self>) {
        self.index = index.min(items.len().saturating_sub(1));
        self.items = items;
        self.origin = origin;
        cx.notify();
    }

    pub fn go(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.items.len() && index != self.index {
            self.index = index;
            cx.notify();
        }
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.is_open() || self.origin.is_some() {
            self.items.clear();
            self.index = 0;
            self.origin = None;
            cx.notify();
        }
    }
}

// ── Context menu of an image or video ───────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaMenuKind {
    #[default]
    Image,
    Video,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaMenuTarget {
    pub x: f32,
    pub y: f32,
    pub server: Option<ServerId>,
    pub attachment_id: Option<i64>,
    pub filename: Option<SharedString>,
    pub mime: Option<SharedString>,
    pub kind: MediaMenuKind,
    /// Link images (no attachment): the image and the page it came from.
    pub url: Option<SharedString>,
    pub page_url: Option<SharedString>,
}

#[derive(Default)]
pub struct ImageContextMenu {
    /// Open when set. Closing keeps nothing (the renderer kept the fields; nothing read
    /// them while closed).
    pub open: Option<MediaMenuTarget>,
}

impl ImageContextMenu {
    pub fn show(&mut self, target: MediaMenuTarget, cx: &mut Context<Self>) {
        self.open = Some(target);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            cx.notify();
        }
    }
}

// ── The playing audio / video attachment ────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveMedia {
    pub attachment_id: i64,
    pub server: ServerId,
    pub channel: ChannelId,
    pub filename: SharedString,
    /// Video only.
    pub width: u32,
    pub height: u32,
}

/// One audio attachment plays at a time; its playback state is mirrored here so the chat's
/// controls re-render (volume and mute are settings, not here).
#[derive(Default)]
pub struct ActiveAudio {
    pub active: Option<ActiveMedia>,
    pub playing: bool,
    pub time_secs: f64,
    pub duration_secs: f64,
}

impl ActiveAudio {
    /// A new attachment starts from zero (old time / duration would flash otherwise).
    pub fn set_active(&mut self, active: Option<ActiveMedia>, cx: &mut Context<Self>) {
        self.active = active;
        self.playing = false;
        self.time_secs = 0.0;
        self.duration_secs = 0.0;
        cx.notify();
    }

    pub fn set_playback(&mut self, playing: Option<bool>, time_secs: Option<f64>, duration_secs: Option<f64>, cx: &mut Context<Self>) {
        let before = (self.playing, self.time_secs, self.duration_secs);
        if let Some(p) = playing {
            self.playing = p;
        }
        if let Some(t) = time_secs {
            self.time_secs = t;
        }
        // Chromium reported Infinity for some durations; never keep a non-finite one.
        if let Some(d) = duration_secs.filter(|d| d.is_finite()) {
            self.duration_secs = d;
        }
        if before != (self.playing, self.time_secs, self.duration_secs) {
            cx.notify();
        }
    }
}

/// The one playing video attachment. (The renderer also kept the placeholder element the
/// persistent player tracked; the video view owns that here.)
#[derive(Default)]
pub struct ActiveVideo {
    pub active: Option<ActiveMedia>,
}

impl ActiveVideo {
    pub fn set_active(&mut self, active: Option<ActiveMedia>, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            cx.notify();
        }
    }
}

/// Per attachment: bumped when its video cache entry changed (a poster captured), so only
/// that placeholder re-reads it.
pub struct VideoCacheBumped(pub i64);

impl EventEmitter<VideoCacheBumped> for VideoCacheVersions {}

#[derive(Default)]
pub struct VideoCacheVersions {
    versions: HashMap<i64, u64>,
}

impl VideoCacheVersions {
    pub fn version(&self, attachment_id: i64) -> u64 {
        self.versions.get(&attachment_id).copied().unwrap_or(0)
    }

    pub fn bump(&mut self, attachment_id: i64, cx: &mut Context<Self>) {
        *self.versions.entry(attachment_id).or_insert(0) += 1;
        cx.emit(VideoCacheBumped(attachment_id));
    }
}

// ── Drag and drop of files onto the chat ────────────────────────────────────

#[derive(Default)]
pub struct DragState {
    /// Files are being dragged over the window.
    pub active: bool,
    /// The drop target under the pointer (`"active-input"`, a DM composer, …).
    pub hovered: Option<SharedString>,
}

impl DragState {
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            if !active {
                self.hovered = None;
            }
            cx.notify();
        }
    }

    pub fn set_hovered(&mut self, key: Option<SharedString>, cx: &mut Context<Self>) {
        if self.hovered != key {
            self.hovered = key;
            cx.notify();
        }
    }
}
