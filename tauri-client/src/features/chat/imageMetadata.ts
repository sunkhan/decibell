import { invoke } from "@tauri-apps/api/core";

// Pre-generated thumbnail long-edge sizes. Match the bits the server
// expects: 320 → bit 0, 640 → bit 1, 1280 → bit 2. Keep this list
// canonical — if you add a size, update the C++ side too.
export const THUMB_SIZES = [320, 640, 1280] as const;
export type ThumbSize = (typeof THUMB_SIZES)[number];

export interface ImageMetadata {
  width: number;
  height: number;
  // Map of long-edge px → encoded JPEG blob. Empty when extraction
  // failed at the load step. May contain a subset of THUMB_SIZES (we
  // skip sizes >= the source's long edge to avoid upscaling).
  thumbnails: Map<ThumbSize, Blob>;
}

interface StagedMedia {
  path: string;
  url: string;
}

const JPEG_QUALITY = 0.7;
const METADATA_TIMEOUT_MS = 15000;

export async function extractImageMetadata(filePath: string): Promise<ImageMetadata> {
  const empty: ImageMetadata = { width: 0, height: 0, thumbnails: new Map() };
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

    img.addEventListener("load", async () => {
      window.clearTimeout(timer);
      const width = img.naturalWidth;
      const height = img.naturalHeight;
      if (!width || !height) {
        finish(empty);
        return;
      }
      try {
        const longEdge = Math.max(width, height);
        const thumbnails = new Map<ThumbSize, Blob>();
        // Encode each predefined size sequentially to keep peak memory
        // bounded. Skip any size >= the source's long edge — upscaling
        // wastes bytes without gaining quality.
        for (const target of THUMB_SIZES) {
          if (target >= longEdge && thumbnails.size > 0) break;
          const scale = Math.min(1, target / longEdge);
          const tw = Math.max(1, Math.round(width * scale));
          const th = Math.max(1, Math.round(height * scale));
          const canvas = document.createElement("canvas");
          canvas.width = tw;
          canvas.height = th;
          const ctx = canvas.getContext("2d");
          if (!ctx) continue;
          ctx.drawImage(img, 0, 0, tw, th);
          const blob: Blob | null = await new Promise((res) =>
            canvas.toBlob(res, "image/jpeg", JPEG_QUALITY),
          );
          if (blob) thumbnails.set(target, blob);
        }
        finish({ width, height, thumbnails });
      } catch (err) {
        console.warn("[image-meta] capture failed", err);
        finish({ width, height, thumbnails: new Map() });
      }
    });

    img.src = staged!.url;
  });

  cleanup();
  return result;
}
