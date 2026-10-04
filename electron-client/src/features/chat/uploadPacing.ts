import { useUiStore } from "../../stores/uiStore";
import { CHUNK_BYTES as SEALED_CHUNK_BYTES } from "./attachmentCrypto";

// Attachment upload speed cap (Settings → Network), per file.
//
// Each PATCH goes out through main's net.fetch as one buffered request,
// so the cap can only bite between requests: the upload loop asks
// `take()` before every send, and while a cap is set it sends smaller
// chunks (~half a second of the cap) so the rate stays even, the
// progress bar keeps moving and a lowered cap applies within one chunk.
// The cap is re-read on every call. Downloads are paced in main
// (electron/main/downloadPacer.ts) with the same bucket and floor.

/// Unlimited-rate chunk size.
export const MAX_UPLOAD_CHUNK_BYTES = 8 * 1024 * 1024;

/// Floor for a non-zero cap, matching the download side's (which the
/// community server's 30 s inactivity deadline sets).
const MIN_LIMIT_BPS = 100 * 1024;
const BURST_SECONDS = 0.1;
const MAX_SLEEP_MS = 250;

function effectiveRate(): number {
  const limit = useUiStore.getState().uploadLimitBps;
  return limit > 0 ? Math.max(MIN_LIMIT_BPS, limit) : 0;
}

/// Bytes to read for the next PATCH. Always whole sealed chunks, so an
/// encrypted upload's every read starts on a sealed-chunk boundary.
export function uploadChunkBytes(): number {
  const rate = effectiveRate();
  if (rate === 0) return MAX_UPLOAD_CHUNK_BYTES;
  const halfSecond = Math.floor(rate / 2 / SEALED_CHUNK_BYTES) * SEALED_CHUNK_BYTES;
  return Math.min(MAX_UPLOAD_CHUNK_BYTES, Math.max(SEALED_CHUNK_BYTES, halfSecond));
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const done = () => {
      window.clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    };
    const timer = window.setTimeout(done, ms);
    signal.addEventListener("abort", done);
  });
}

/// Token bucket in bytes, one per upload. A take may overdraw; the next
/// one waits out the debt at the current cap. Returns early (without
/// charging) once `signal` aborts, so a cancel never waits.
export class UploadPacer {
  private tokens = 0;
  private stamp = performance.now();

  async take(bytes: number, signal: AbortSignal): Promise<void> {
    for (;;) {
      if (signal.aborted) return;
      const rate = effectiveRate();
      const now = performance.now();
      if (rate === 0) {
        this.tokens = 0;
        this.stamp = now;
        return;
      }
      this.tokens = Math.min(
        rate * BURST_SECONDS,
        this.tokens + ((now - this.stamp) / 1000) * rate,
      );
      this.stamp = now;
      if (this.tokens >= 0) {
        this.tokens -= bytes;
        return;
      }
      await sleep(Math.min(MAX_SLEEP_MS, (-this.tokens / rate) * 1000), signal);
    }
  }
}
