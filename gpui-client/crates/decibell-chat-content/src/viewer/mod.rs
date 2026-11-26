//! The image viewer (`ImageViewer.tsx` + `imageViewer/*`).

use std::sync::Arc;

use decibell_model::ServerId;
use decibell_model::chat::Message;
use gpui::{App, Bounds, EntityId, Pixels, SharedString};

use crate::attachments::OpenRequest;

/// A link picture to open (`ImageEmbed`'s click).
pub struct LinkOpen {
    pub key: SharedString,
    pub url: SharedString,
    pub page_url: SharedString,
    pub filename: SharedString,
    pub size: (u32, u32),
    pub server: Option<ServerId>,
    pub message: Arc<Message>,
    pub bounds: Bounds<Pixels>,
    pub list: EntityId,
}

/// Whether the viewer hides attachment `id`'s thumbnail now (it flies out of / back into it).
pub fn hides(server: Option<ServerId>, id: i64, cx: &App) -> bool {
    server.is_some_and(|s| hides_key(&format!("a:{}:{id}", s.wire()), cx))
}

pub fn hides_key(_key: &str, _cx: &App) -> bool {
    false
}

pub fn open_attachment(_request: OpenRequest, _cx: &mut App) {}

pub fn open_link(_request: LinkOpen, _cx: &mut App) {}

pub fn init(_cx: &mut App) {}
