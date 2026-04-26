import { invoke } from "@tauri-apps/api/core";

export interface AudioMetadata {
  durationMs: number;
}

interface StagedMedia {
  path: string;
  url: string;
}

// Reads audio duration at upload time so receivers can render
// "0:00 / 3:45" before downloading the file. Same staging trick as
// imageMetadata / videoMetadata: hard-link the picked file into the
// cache dir under our `decibell-attach-` namespace, load it through
// the local media server into a hidden <audio>, read `duration` on
// loadedmetadata, then unlink.
//
// Errors are non-fatal — the upload still proceeds with durationMs=0
// and the receiver simply doesn't show a length label until after
// downloading.

const METADATA_TIMEOUT_MS = 10000;

export async function extractAudioMetadata(filePath: string): Promise<AudioMetadata> {
  const empty: AudioMetadata = { durationMs: 0 };
  let staged: StagedMedia | null = null;
  try {
    staged = await invoke<StagedMedia>("stage_file_for_media", { path: filePath });
  } catch (err) {
    console.warn("[audio-meta] stage failed", err);
    return empty;
  }
  const stagedPath = staged.path;
  const cleanup = () => {
    invoke("cleanup_temp_attachment", { path: stagedPath }).catch(() => {});
  };

  const audio = document.createElement("audio");
  audio.preload = "metadata";
  audio.crossOrigin = "anonymous";

  const result = await new Promise<AudioMetadata>((resolve) => {
    let settled = false;
    const finish = (out: AudioMetadata) => {
      if (settled) return;
      settled = true;
      try { audio.removeAttribute("src"); audio.load(); } catch { /* ignore */ }
      resolve(out);
    };
    const timer = window.setTimeout(() => {
      console.warn("[audio-meta] timed out");
      finish(empty);
    }, METADATA_TIMEOUT_MS);

    audio.addEventListener("error", () => {
      window.clearTimeout(timer);
      console.warn("[audio-meta] audio element error", audio.error);
      finish(empty);
    });

    audio.addEventListener("loadedmetadata", () => {
      window.clearTimeout(timer);
      const d = audio.duration;
      if (!isFinite(d) || d <= 0) {
        finish(empty);
        return;
      }
      finish({ durationMs: Math.round(d * 1000) });
    }, { once: true });

    audio.src = staged!.url;
  });

  cleanup();
  return result;
}
