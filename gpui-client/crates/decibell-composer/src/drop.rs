//! Files that arrive without the dialog: dropped on the window (`useDragDrop.ts`) or pasted
//! (`usePasteToAttach.ts`). The composer does the wiring (`Composer::render`'s drag listener and
//! drop layer, `Composer::paste`); this module names what a paste carries.
//!
//! GPUI delivers dropped **file paths** only (gpui-notes §2.16: an image dragged out of a browser
//! arrives as nothing), and pasted pictures as an `Image` in the platform's format.

use gpui::ImageFormat;

/// A pasted picture's file name and type. Chromium names a clipboard image `image.png` (it hands
/// the page a PNG); GPUI keeps the clipboard's format, so the extension follows it.
pub fn pasted_image_name(format: ImageFormat) -> (&'static str, &'static str) {
    match format {
        ImageFormat::Png => ("image.png", "image/png"),
        ImageFormat::Jpeg => ("image.jpg", "image/jpeg"),
        ImageFormat::Webp => ("image.webp", "image/webp"),
        ImageFormat::Gif => ("image.gif", "image/gif"),
        ImageFormat::Svg => ("image.svg", "image/svg+xml"),
        ImageFormat::Bmp => ("image.bmp", "image/bmp"),
        ImageFormat::Tiff => ("image.tiff", "image/tiff"),
        ImageFormat::Ico => ("image.ico", "image/x-icon"),
        ImageFormat::Pnm => ("image.pnm", "image/x-portable-anymap"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_png_is_image_png() {
        assert_eq!(pasted_image_name(ImageFormat::Png), ("image.png", "image/png"));
    }
}
