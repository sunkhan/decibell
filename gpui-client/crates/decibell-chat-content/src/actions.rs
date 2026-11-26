//! What the viewer's toolbar and the image right-click menu do with a picture
//! (`imageViewer/imageActions.ts`, main's `clipboard.ts`): copy the picture, copy its link, save
//! it, open it in the browser.
//!
//! **Copying a picture on Linux**: GPUI at the pin writes only text to the X11 and Wayland
//! clipboards (`gpui-notes.md` §2.14: Wayland offers text MIME types only, X11 writes
//! `item.text()` and never calls its `set_image`). The picture is fetched and converted to PNG
//! as the renderer did and handed to GPUI as an image entry, which reaches the clipboard on
//! Windows and macOS; on Linux the copy fails with a toast saying so, until GPUI writes images.

use std::time::Duration;

use bytes::Bytes;
use decibell_model::{Model, ServerId};
use gpui::{App, AppContext as _, ClipboardItem, Global, Image, ImageFormat, SharedString};

use crate::pictures::Source;

/// Attachments aren't cached as files: past this a toast says the copy is under way.
const SLOW_COPY: Duration = Duration::from_millis(300);

/// What a picture action is about.
#[derive(Clone, Debug, Default)]
pub struct Target {
    pub server: Option<ServerId>,
    pub attachment_id: Option<i64>,
    pub filename: SharedString,
    pub mime: Option<SharedString>,
    /// Link pictures: the picture and the page it came from.
    pub url: Option<SharedString>,
    pub page_url: Option<SharedString>,
}

/// Where copies go: the system clipboard, or (self-tests) a cell they read back.
#[derive(Default)]
pub struct ClipboardSink {
    pub captured: std::cell::RefCell<Vec<ClipboardItem>>,
    pub capture: bool,
}

impl Global for ClipboardSink {}

fn write(item: ClipboardItem, cx: &mut App) {
    if let Some(sink) = cx.try_global::<ClipboardSink>()
        && sink.capture
    {
        sink.captured.borrow_mut().push(item);
        return;
    }
    cx.write_to_clipboard(item);
}

fn toasts(cx: &App) -> Option<gpui::Entity<decibell_model::stores::toasts::Toasts>> {
    cx.try_global::<Model>().map(|m| m.toasts.clone())
}

/// Whether the clipboard can take a picture on this platform (see the module doc).
pub fn clipboard_takes_images() -> bool {
    !cfg!(any(target_os = "linux", target_os = "freebsd"))
}

/// PNG and JPEG go to the clipboard as they are; anything else becomes a PNG.
fn as_clipboard_image(bytes: &[u8]) -> Result<Image, String> {
    let png = bytes.len() > 4 && bytes[..4] == [0x89, b'P', b'N', b'G'];
    let jpeg = bytes.len() > 3 && bytes[..3] == [0xff, 0xd8, 0xff];
    if png {
        return Ok(Image::from_bytes(ImageFormat::Png, bytes.to_vec()));
    }
    if jpeg {
        return Ok(Image::from_bytes(ImageFormat::Jpeg, bytes.to_vec()));
    }
    let img = decibell_attachments::thumbs::decode_image_bytes(bytes).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(Image::from_bytes(ImageFormat::Png, out))
}

/// The original of an attachment or a link picture, through the pipeline's fetcher (decrypted
/// when sealed, held to the download cap).
fn fetch_original(target: &Target, cx: &mut App) -> Option<gpui::Task<Result<Bytes, SharedString>>> {
    let source = match (target.server, target.attachment_id, &target.url) {
        (Some(server), Some(id), _) => Source::Attachment { server: server.wire().into(), id, thumb: 0 },
        (_, _, Some(url)) => Source::Remote(url.clone()),
        _ => return None,
    };
    let pictures = crate::pictures::Pictures::global(cx)?;
    Some(pictures.update(cx, |p, cx| p.fetch_now(&source, cx)))
}

/// Copy the picture itself ("Copy image").
pub fn copy_image(target: Target, cx: &mut App) {
    let Some(fetch) = fetch_original(&target, cx) else { return };
    let toasts = toasts(cx);
    let filename = target.filename.clone();
    cx.spawn(async move |cx| {
        let slow = cx.background_executor().timer(SLOW_COPY);
        let mut fetch = std::pin::pin!(fetch);
        let mut copying = None;
        let bytes = match futures::future::select(&mut fetch, std::pin::pin!(slow)).await {
            futures::future::Either::Left((bytes, _)) => bytes,
            futures::future::Either::Right(((), _)) => {
                copying = toasts.as_ref().map(|t| {
                    cx.update(|cx| {
                        t.update(cx, |t, cx| {
                            t.push(decibell_model::stores::toasts::Severity::Info, "Copying image…", Some(filename.clone()), Some(Duration::from_secs(120)), None, cx)
                        })
                    })
                });
                fetch.await
            }
        };
        let converted = match bytes {
            Ok(bytes) => cx.background_spawn(async move { as_clipboard_image(&bytes) }).await.map_err(SharedString::from),
            Err(e) => Err(e),
        };
        cx.update(|cx| {
            if let (Some(id), Some(t)) = (copying, &toasts) {
                t.update(cx, |t, cx| t.dismiss(id, cx));
            }
            let result = converted.and_then(|image| {
                if !clipboard_takes_images() && !cx.try_global::<ClipboardSink>().is_some_and(|s| s.capture) {
                    return Err(SharedString::new_static("Copying pictures to the clipboard isn't available on Linux yet"));
                }
                write(ClipboardItem::new_image(&image), cx);
                Ok(())
            });
            if let Some(t) = &toasts {
                t.update(cx, |t, cx| match result {
                    Ok(()) => {
                        t.success("Image copied", Some(filename.clone()), cx);
                    }
                    Err(e) => {
                        t.error("Copy failed", Some(e), cx);
                    }
                });
            }
        });
    })
    .detach();
}

/// "Copy link": the page a link picture came from, else the picture.
pub fn copy_link(target: &Target, cx: &mut App) {
    let Some(link) = target.page_url.clone().or_else(|| target.url.clone()) else { return };
    write(ClipboardItem::new_string(link.to_string()), cx);
    if let Some(t) = toasts(cx) {
        t.update(cx, |t, cx| {
            t.success("Link copied", None, cx);
        });
    }
}

/// "Open in browser": the page, else the picture (`openExternal`).
pub fn open_in_browser(target: &Target, cx: &mut App) {
    let Some(link) = target.page_url.clone().or_else(|| target.url.clone()) else { return };
    if let Some(url) = decibell_unfurl::links::external_link_target(&link) {
        cx.open_url(&url);
    }
}

/// Attachments only: through the downloads manager (`saveImage`).
pub fn can_save(target: &Target) -> bool {
    target.server.is_some() && target.attachment_id.is_some()
}

/// "Save" / "Save as…" (`save_as` shows the dialog).
pub fn save(target: &Target, kind: decibell_attachments::AttachmentKind, save_as: bool, cx: &mut App) {
    let (Some(server), Some(id)) = (target.server, target.attachment_id) else { return };
    // Found among the loaded messages when it can be; else what the picture's caller knew.
    let fallback = decibell_model::chat::Attachment {
        id,
        message_id: 0,
        kind,
        filename: target.filename.clone(),
        mime: target.mime.clone().unwrap_or_else(|| SharedString::new_static("application/octet-stream")),
        size_bytes: 0,
        url: SharedString::default(),
        position: 0,
        created_at: 0,
        purged_at: 0,
        width: 0,
        height: 0,
        thumbnail_size_bytes: 0,
        thumbnail_sizes_mask: 0,
        duration_ms: 0,
        placeholder: SharedString::default(),
        encrypted: false,
        key_b64: None,
        chunk_bytes: 0,
    };
    crate::downloads::start(server, &fallback, save_as, cx).detach();
}
