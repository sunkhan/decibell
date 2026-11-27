//! What the uploader sends for video / audio files, one JSON line each: the size, the length,
//! the ThumbHash and each thumbnail's size and middle pixel (to compare with what another client
//! stored for the same files: `tests/assets/electron.md`).
//!
//!     cargo run -p decibell-attachments --features media-probe --example media_probe -- <file>…
//!
//! With `PROBE_OUT=<dir>` the thumbnails are written there too (`<file name>.thumb-<size>px.jpg`,
//! the names the community server stores them under).

use decibell_attachments::kind::{guess_mime, AttachmentKind};
use decibell_attachments::upload::{prepare, UploadSource};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    for path in std::env::args().skip(1) {
        let kind = AttachmentKind::from_mime(guess_mime(&path));
        let probe: std::sync::Arc<dyn decibell_attachments::thumbs::MediaProbe> =
            std::sync::Arc::new(decibell_attachments::media_probe::FfmpegProbe);
        let p = prepare(UploadSource::Path(path.clone().into()), Some(probe)).await.expect("prepared");
        if let Some(dir) = std::env::var_os("PROBE_OUT") {
            let name = std::path::Path::new(&path).file_name().expect("a file name").to_string_lossy().into_owned();
            for (size, jpeg) in &p.thumbnails {
                std::fs::write(std::path::Path::new(&dir).join(format!("{name}.thumb-{size}px.jpg")), jpeg).expect("written");
            }
        }
        let thumbs: Vec<String> = p
            .thumbnails
            .iter()
            .map(|(size, jpeg)| {
                let img = decibell_attachments::thumbs::decode_image_bytes(jpeg).expect("a JPEG").to_luma8();
                let (w, h) = img.dimensions();
                format!("{size}:{w}x{h}:{}", img.get_pixel(w / 2, h / 2).0[0])
            })
            .collect();
        println!(
            "{{\"file\":{:?},\"kind\":{:?},\"mime\":{:?},\"width\":{},\"height\":{},\"duration_ms\":{},\"placeholder\":{:?},\"thumbs\":{:?}}}",
            path, kind, p.mime, p.width, p.height, p.duration_ms, p.placeholder, thumbs
        );
    }
}
