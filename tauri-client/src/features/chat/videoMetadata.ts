import { invoke } from "@tauri-apps/api/core";

export interface VideoMetadata {
  width: number;
  height: number;
  thumbnail: Blob | null;
}

interface StagedMedia {
  path: string;
  url: string;
}

// JS-side dimension + thumbnail extraction. We hard-link the picked file
// into the cache dir under our `decibell-attach-` namespace (Rust side),
// then load it through the local media server into a hidden <video>.
// On `loadedmetadata` we read videoWidth/videoHeight; we then seek a bit
// past zero to land on a real frame (some encoders emit a black/blurry
// first frame), draw to canvas, and encode to JPEG. The whole cycle is
// torn down + the staged file unlinked once we're done.
//
// Errors here are non-fatal — the upload still proceeds without
// dimensions / thumbnail, and the placeholder falls back to its plain
// look. The caller logs and moves on.

const TARGET_THUMB_LONG_EDGE = 320;
const JPEG_QUALITY = 0.7;
const SEEK_POSITION_SECONDS = 1.0;
const METADATA_TIMEOUT_MS = 15000;

export async function extractVideoMetadata(filePath: string): Promise<VideoMetadata> {
  const empty: VideoMetadata = { width: 0, height: 0, thumbnail: null };
  let staged: StagedMedia | null = null;
  try {
    staged = await invoke<StagedMedia>("stage_file_for_media", { path: filePath });
  } catch (err) {
    console.warn("[video-meta] stage failed", err);
    return empty;
  }
  const stagedPath = staged.path;
  const cleanup = () => {
    invoke("cleanup_temp_attachment", { path: stagedPath }).catch(() => {});
  };

  const video = document.createElement("video");
  video.crossOrigin = "anonymous";
  video.muted = true;
  video.preload = "auto";
  video.playsInline = true;
  // Off-screen, never attached to the DOM tree but kept alive via the
  // closure below until we resolve.
  video.style.position = "fixed";
  video.style.left = "-99999px";

  const result = await new Promise<VideoMetadata>((resolve) => {
    let settled = false;
    const finish = (out: VideoMetadata) => {
      if (settled) return;
      settled = true;
      try { video.removeAttribute("src"); video.load(); } catch { /* ignore */ }
      resolve(out);
    };
    const timer = window.setTimeout(() => {
      console.warn("[video-meta] timed out");
      finish(empty);
    }, METADATA_TIMEOUT_MS);

    video.addEventListener("error", () => {
      window.clearTimeout(timer);
      console.warn("[video-meta] video element error", video.error);
      finish(empty);
    });

    video.addEventListener("loadedmetadata", () => {
      const width = video.videoWidth;
      const height = video.videoHeight;
      if (!width || !height) {
        window.clearTimeout(timer);
        finish(empty);
        return;
      }
      const seekTarget = isFinite(video.duration) && video.duration > SEEK_POSITION_SECONDS
        ? SEEK_POSITION_SECONDS
        : 0;
      const onSeeked = () => {
        try {
          const scale = Math.min(1, TARGET_THUMB_LONG_EDGE / Math.max(width, height));
          const tw = Math.max(1, Math.round(width * scale));
          const th = Math.max(1, Math.round(height * scale));
          const canvas = document.createElement("canvas");
          canvas.width = tw;
          canvas.height = th;
          const ctx = canvas.getContext("2d");
          if (!ctx) {
            window.clearTimeout(timer);
            finish({ width, height, thumbnail: null });
            return;
          }
          ctx.drawImage(video, 0, 0, tw, th);
          canvas.toBlob(
            (blob) => {
              window.clearTimeout(timer);
              finish({ width, height, thumbnail: blob });
            },
            "image/jpeg",
            JPEG_QUALITY,
          );
        } catch (err) {
          console.warn("[video-meta] capture failed", err);
          window.clearTimeout(timer);
          finish({ width, height, thumbnail: null });
        }
      };
      video.addEventListener("seeked", onSeeked, { once: true });
      try {
        video.currentTime = seekTarget;
      } catch {
        // currentTime can throw if duration isn't set yet — fall back to
        // capturing whatever frame is on screen now.
        onSeeked();
      }
    }, { once: true });

    video.src = staged!.url;
  });

  cleanup();
  return result;
}
