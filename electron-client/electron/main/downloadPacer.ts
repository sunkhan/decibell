// Attachment download speed cap (Settings → Network). Applied at every
// place attachment bytes come off the network in main: the
// decibell-attachment:// protocol, the loopback media server, netFetch
// (save-as / copy) and the encrypted-attachment fetch. Nothing else —
// voice, streams, link previews, GIFs — passes through here.
//
// Per file, as the settings tab promises: transfers of the same
// attachment (a video's overlapping range requests, a save-as while it
// plays) share one bucket; different attachments each get the full cap.
// Pacing the body read throttles the socket through TCP backpressure,
// after the first few hundred KB of kernel / Chromium buffering. The
// cap is re-read on every chunk, so a change applies mid-transfer.
//
// Uploads are paced in the renderer instead (features/chat/uploadPacing.ts):
// each PATCH is one buffered request, so the loop that issues them is
// the only place a cap can bite.

import { ipcMain } from "electron";

/// Floor for a non-zero cap. The community server re-arms a 30 s
/// inactivity deadline per 256 KB write (attachment_http.cpp), so a
/// slower reader would get its download cut mid-file.
const MIN_LIMIT_BPS = 100 * 1024;
/// Credit an idle bucket may bank, in seconds of the cap. Just enough
/// that timer lateness doesn't eat into the rate.
const BURST_SECONDS = 0.1;
/// Longest single sleep, so raising or clearing the cap wakes a waiting
/// transfer promptly.
const MAX_SLEEP_MS = 250;
/// A bucket untouched for this long is equivalent to a fresh one.
const IDLE_PRUNE_MS = 30_000;
/// Pacing granularity. net.fetch hands bodies over in chunks of up to
/// 2 MiB; charging and delivering them in slices keeps the flow even
/// instead of 2 MiB bursts with second-long gaps (and an uncapped first
/// 2 MiB).
const SLICE_BYTES = 64 * 1024;

let limitBps = 0;

function effectiveRate(): number {
  return limitBps > 0 ? Math.max(MIN_LIMIT_BPS, limitBps) : 0;
}

/// The cap in force (floor applied), 0 = unlimited.
export function downloadRateBps(): number {
  return effectiveRate();
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    signal?.addEventListener("abort", done);
  });
}

/// Token bucket in bytes, charged a slice at a time: a slice may
/// overdraw, and the next one waits out the debt at the current cap.
/// Returns early once `signal` aborts or the cap is cleared.
class Pacer {
  private tokens = 0;
  stamp = performance.now();

  async take(bytes: number, signal?: AbortSignal): Promise<void> {
    let left = bytes;
    while (left > 0) {
      if (signal?.aborted) return;
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
        const n = Math.min(SLICE_BYTES, left);
        this.tokens -= n;
        left -= n;
        continue;
      }
      await sleep(Math.min(MAX_SLEEP_MS, (-this.tokens / rate) * 1000), signal);
    }
  }
}

const pacers = new Map<string, Pacer>();

function pacerFor(serverId: string, attachmentId: string): Pacer {
  const now = performance.now();
  for (const [k, p] of pacers) {
    if (now - p.stamp > IDLE_PRUNE_MS) pacers.delete(k);
  }
  const key = `${serverId}\n${attachmentId}`;
  let p = pacers.get(key);
  if (!p) {
    p = new Pacer();
    pacers.set(key, p);
  }
  return p;
}

/// Wrap an attachment response body so it's delivered at the cap, in
/// slices while one is set. Cancelling the result cancels the upstream
/// body.
export function pacedBody(
  body: ReadableStream<Uint8Array>,
  serverId: string,
  attachmentId: string,
): ReadableStream<Uint8Array> {
  const pacer = pacerFor(serverId, attachmentId);
  const reader = body.getReader();
  const abort = new AbortController();
  let rest: Uint8Array | null = null;
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      if (!rest || rest.byteLength === 0) {
        const { done, value } = await reader.read();
        if (abort.signal.aborted) return;
        if (done) {
          controller.close();
          return;
        }
        rest = value;
      }
      const slice: Uint8Array = effectiveRate() === 0 ? rest : rest.subarray(0, SLICE_BYTES);
      rest = rest.subarray(slice.byteLength);
      await pacer.take(slice.byteLength, abort.signal);
      if (abort.signal.aborted) return;
      controller.enqueue(slice);
    },
    cancel(reason) {
      abort.abort();
      return reader.cancel(reason);
    },
  });
}

/// Read a whole attachment response at the cap. The result owns its
/// ArrayBuffer exactly (no pool slack), so `.buffer` is safe to hand on.
/// Aborting `signal` (the one the fetch was given) ends a paced wait
/// at once and rejects with an AbortError.
export async function readPaced(
  resp: Response,
  serverId: string,
  attachmentId: string,
  signal?: AbortSignal,
): Promise<Uint8Array> {
  if (!resp.body) return new Uint8Array(0);
  const pacer = pacerFor(serverId, attachmentId);
  const reader = resp.body.getReader();
  const parts: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    await pacer.take(value.byteLength, signal);
    if (signal?.aborted) {
      reader.cancel().catch(() => {});
      throw new DOMException("Aborted", "AbortError");
    }
    parts.push(value);
    total += value.byteLength;
  }
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.byteLength;
  }
  return out;
}

export function registerDownloadLimitHandler(): void {
  ipcMain.handle("decibell:attachments:setDownloadLimit", (_e, bps: unknown) => {
    const n = Number(bps);
    limitBps = Number.isFinite(n) && n > 0 ? Math.round(n) : 0;
  });
}
