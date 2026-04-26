import { invoke } from "@tauri-apps/api/core";

export interface ImageMetadata {
  width: number;
  height: number;
  thumbnail: Blob | null;
}

interface StagedMedia {
  path: string;
  url: string;
}

// JS-side image thumbnail extraction. We hard-link the picked file into
// the cache dir under our `decibell-attach-` namespace (Rust side), then
// load it through a hidden <img>. On `load` we read naturalWidth/Height,
// draw to a canvas at TARGET_THUMB_LONG_EDGE on the long edge, and
// encode to JPEG. The whole cycle is torn down + the staged file
// unlinked once we're done.
//
// The Rust stat path doesn't extract dimensions for image kinds either
// — it uses image::image_dimensions which works for some formats but is
// less universal than what WebKit's <img> can decode (HEIC, AVIF,
// WebP, multi-image TIFF, etc.). Doing it here keeps a single code path
// that works for everything WebKit can render.
//
// Errors are non-fatal — the upload still proceeds without the
// thumbnail, and ImagePreview falls back to fetching the full image
// bytes (legacy path) for that one attachment. The caller logs.

const TARGET_THUMB_LONG_EDGE = 320;
const JPEG_QUALITY = 0.7;
const METADATA_TIMEOUT_MS = 15000;

export async function extractImageMetadata(filePath: string): Promise<ImageMetadata> {
  const empty: ImageMetadata = { width: 0, height: 0, thumbnail: null };
  let staged: StagedMedia | null = null;
  try {
    staged = await invoke<StagedMedia>("stage_file_for_media", { path: filePath });
  } catch (err) {
    console.warn("[image-meta] stage failed", err);
    return empty;
  }
  const stagedPath = staged.path;
  const cleanup = () => {
    invoke("cleanup_temp_attachment", { path: stagedPath }).catch(() => {});
  };

  const img = new Image();
  img.crossOrigin = "anonymous";
  img.decoding = "async";

  const result = await new Promise<ImageMetadata>((resolve) => {
    let settled = false;
    const finish = (out: ImageMetadata) => {
      if (settled) return;
      settled = true;
      resolve(out);
    };
    const timer = window.setTimeout(() => {
      console.warn("[image-meta] timed out");
      finish(empty);
    }, METADATA_TIMEOUT_MS);

    img.addEventListener("error", () => {
      window.clearTimeout(timer);
      console.warn("[image-meta] img element error");
      finish(empty);
    });

    img.addEventListener("load", () => {
      window.clearTimeout(timer);
      const width = img.naturalWidth;
      const height = img.naturalHeight;
      if (!width || !height) {
        finish(empty);
        return;
      }
      try {
        const scale = Math.min(1, TARGET_THUMB_LONG_EDGE / Math.max(width, height));
        const tw = Math.max(1, Math.round(width * scale));
        const th = Math.max(1, Math.round(height * scale));
        const canvas = document.createElement("canvas");
        canvas.width = tw;
        canvas.height = th;
        const ctx = canvas.getContext("2d");
        if (!ctx) {
          finish({ width, height, thumbnail: null });
          return;
        }
        ctx.drawImage(img, 0, 0, tw, th);
        canvas.toBlob(
          (blob) => finish({ width, height, thumbnail: blob }),
          "image/jpeg",
          JPEG_QUALITY,
        );
      } catch (err) {
        console.warn("[image-meta] capture failed", err);
        finish({ width, height, thumbnail: null });
      }
    });

    img.src = staged!.url;
  });

  cleanup();
  return result;
}
