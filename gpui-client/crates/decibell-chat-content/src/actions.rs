//! What the viewer's toolbar and the image right-click menu do with a picture
//! (`imageViewer/imageActions.ts`, main's `clipboard.ts`): copy the picture, copy its link, save
//! it, open it in the browser.
//!
//! **Copying a picture** is what Electron put on the clipboard: `clipboard.writeImage` of the
//! decoded picture, which Chromium offers as PNG (`image/png` on Linux). Main decoded PNG and JPEG
//! itself (`nativeImage`); anything else (WebP, a GIF: its first frame) went back to the renderer
//! once to become a PNG on a canvas. Here every picture becomes a PNG of its pixels, off the UI
//! thread: a PNG file goes as it is, anything else is decoded (a GIF's first frame, EXIF orientation
//! applied, as the viewer shows it) and encoded. GPUI offers it under `image/png` on Wayland and X11
//! (patch 0015) and serves it to whoever asks, a clipboard manager included; Windows and macOS took
//! image entries already (`gpui-notes.md` §2.14).

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

/// Past this a picture isn't copied (`clipboard.ts`' `IMAGE_MAX`).
const IMAGE_MAX: usize = 128 * 1024 * 1024;

/// The picture as the PNG the clipboard gets (see the module doc).
pub fn as_clipboard_image(bytes: &[u8]) -> Result<Image, String> {
    if bytes.is_empty() {
        return Err("Not an image".into());
    }
    if bytes.len() > IMAGE_MAX {
        return Err("Too large to copy; save it instead".into());
    }
    let png = bytes.len() > 4 && bytes[..4] == [0x89, b'P', b'N', b'G'];
    if png {
        return Ok(Image::from_bytes(ImageFormat::Png, bytes.to_vec()));
    }
    // JPEG, WebP, BMP, a GIF's first frame: the pixels, as the viewer shows them.
    let img = decibell_attachments::thumbs::decode_image_bytes(bytes).map_err(|_| "Not an image".to_string())?;
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
            let result = converted.map(|image| write(ClipboardItem::new_image(&image), cx));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 40) as u8, (y * 60) as u8, 200, 255]))
    }

    fn encode(img: &image::RgbaImage, format: image::ImageFormat) -> Vec<u8> {
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img.clone()).write_to(&mut std::io::Cursor::new(&mut out), format).unwrap();
        out
    }

    #[test]
    fn every_picture_becomes_a_png_of_its_pixels() {
        let picture = rgba(5, 4);
        let png = encode(&picture, image::ImageFormat::Png);
        // A PNG goes as it is.
        let copied = as_clipboard_image(&png).unwrap();
        assert_eq!(copied.format(), ImageFormat::Png);
        assert_eq!(copied.bytes(), png.as_slice());
        // A GIF (and WebP, BMP, JPEG) is decoded and encoded: a PNG with the same pixels.
        for format in [image::ImageFormat::Gif, image::ImageFormat::Bmp] {
            let copied = as_clipboard_image(&encode(&picture, format)).unwrap();
            assert_eq!(copied.format(), ImageFormat::Png, "{format:?}");
            let back = image::load_from_memory_with_format(copied.bytes(), image::ImageFormat::Png).unwrap().to_rgba8();
            assert_eq!(back.dimensions(), (5, 4));
            if format == image::ImageFormat::Bmp {
                assert_eq!(back, picture, "{format:?} pixels");
            }
        }
        let jpeg = {
            let mut out = Vec::new();
            image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(picture.clone()).to_rgb8())
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Jpeg)
                .unwrap();
            out
        };
        assert_eq!(as_clipboard_image(&jpeg).unwrap().format(), ImageFormat::Png);
        assert_eq!(as_clipboard_image(b"not a picture").unwrap_err(), "Not an image");
        assert_eq!(as_clipboard_image(b"").unwrap_err(), "Not an image");
    }
}
