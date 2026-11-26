//! The right-click menu of chat pictures and videos, inline and in the viewer
//! (`components/ImageContextMenu.tsx`): Copy image (pictures), Save as… (attachments), Copy link
//! and Open in browser (link pictures). It reads `model.chat.media_menu` and closes on any press,
//! Escape or a scroll (the kit's `ContextMenu` dismisses itself).

use decibell_attachments::AttachmentKind;
use decibell_common::Watch;
use decibell_model::chat::Attachment;
use decibell_model::chat::viewers::{MediaMenuKind, MediaMenuTarget};
use decibell_model::{Model, ServerId};
use decibell_ui::{ContextMenu, IconName, MenuItem};
use gpui::{App, Context, IntoElement, Pixels, Point, Render, SharedString, Window, div, point, prelude::*, px};

use crate::actions::{self, Target};

/// Opens the menu over an attachment at the pointer.
pub fn show_for_attachment(server: ServerId, a: &Attachment, at: Point<Pixels>, kind: MediaMenuKind, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let target = MediaMenuTarget {
        x: f32::from(at.x),
        y: f32::from(at.y),
        server: Some(server),
        attachment_id: Some(a.id),
        filename: Some(a.filename.clone()),
        mime: Some(a.mime.clone()),
        kind,
        url: None,
        page_url: None,
    };
    model.chat.media_menu.update(cx, |m, cx| m.show(target, cx));
}

/// Opens the menu over a link picture at the pointer.
pub fn show_for_link(url: SharedString, page_url: Option<SharedString>, filename: SharedString, at: Point<Pixels>, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let target = MediaMenuTarget {
        x: f32::from(at.x),
        y: f32::from(at.y),
        server: None,
        attachment_id: None,
        filename: Some(filename),
        mime: None,
        kind: MediaMenuKind::Image,
        url: Some(url),
        page_url,
    };
    model.chat.media_menu.update(cx, |m, cx| m.show(target, cx));
}

/// The overlay (registered with the shell).
pub struct MediaMenu {
    _watch: Watch,
}

impl MediaMenu {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut watch = Watch::new();
        if let Some(model) = cx.try_global::<Model>().cloned() {
            watch.store(&model.chat.media_menu, cx);
        }
        Self { _watch: watch }
    }
}

fn target_of(open: &MediaMenuTarget) -> Target {
    let kind_name = if open.kind == MediaMenuKind::Video { "video" } else { "image" };
    Target {
        server: open.server,
        attachment_id: open.attachment_id,
        filename: open.filename.clone().unwrap_or_else(|| SharedString::new_static(kind_name)),
        mime: open.mime.clone(),
        url: open.url.clone(),
        page_url: open.page_url.clone(),
    }
}

impl Render for MediaMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(model) = cx.try_global::<Model>().cloned() else { return div() };
        let Some(open) = model.chat.media_menu.read(cx).open.clone() else { return div() };
        let target = target_of(&open);
        let attachment = target.server.is_some() && target.attachment_id.is_some();
        if !attachment && target.url.is_none() {
            return div();
        }
        let close = {
            let menu = model.chat.media_menu.clone();
            move |cx: &mut App| menu.update(cx, |m, cx| m.close(cx))
        };
        let mut menu = ContextMenu::new("media-menu", point(px(open.x), px(open.y))).width(px(180.));
        if open.kind == MediaMenuKind::Image {
            let (t, close) = (target.clone(), close.clone());
            menu = menu.entry(MenuItem::new("Copy image").icon(IconName::ViewerCopy).on_click(move |_, _, cx| {
                close(cx);
                actions::copy_image(t.clone(), cx);
            }));
        }
        if attachment {
            let (t, close) = (target.clone(), close.clone());
            let kind = if open.kind == MediaMenuKind::Video { AttachmentKind::Video } else { AttachmentKind::Image };
            menu = menu.entry(MenuItem::new("Save as…").icon(IconName::ViewerDownload).on_click(move |_, _, cx| {
                close(cx);
                actions::save(&t, kind, true, cx);
            }));
        } else {
            let (t, c) = (target.clone(), close.clone());
            menu = menu.entry(MenuItem::new("Copy link").icon(IconName::ViewerLink).on_click(move |_, _, cx| {
                c(cx);
                actions::copy_link(&t, cx);
            }));
            let (t, c) = (target.clone(), close.clone());
            menu = menu.entry(MenuItem::new("Open in browser").icon(IconName::ViewerExternal).on_click(move |_, _, cx| {
                c(cx);
                actions::open_in_browser(&t, cx);
            }));
        }
        div().child(menu.on_dismiss(move |_, _, cx| close(cx)))
    }
}
