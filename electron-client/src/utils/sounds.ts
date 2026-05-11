// UI sound effects via Web Audio. The pre-PR8 plumbing routed through
// a native cpal mixer + a parked thread holding an open output stream
// for the lifetime of the app — overkill for ten ~30ms WAV blips.
//
// This implementation:
//   - Bundles the WAVs through Vite (?url) so they're content-hashed
//     and shipped inside the renderer chunk.
//   - Lazy-creates a single AudioContext on the first play. Created
//     lazily because constructing one before any user gesture
//     instantiates it in the "suspended" state under Chromium's
//     autoplay policy, and the very first play() then has to await a
//     resume — a few-ms hiccup we'd rather take after the user's
//     first interaction (the click that triggered the sound).
//   - Decodes each WAV exactly once on its first play; subsequent
//     plays reuse the cached AudioBuffer.
//   - Each play creates a fresh AudioBufferSourceNode (one-shot —
//     they auto-disconnect on `ended`), so concurrent plays mix at
//     the destination for free. No mixer, no manual cleanup.

import muteUrl from "../assets/sounds/mute.wav?url";
import unmuteUrl from "../assets/sounds/unmute.wav?url";
import deafenUrl from "../assets/sounds/deafen.wav?url";
import undeafenUrl from "../assets/sounds/undeafen.wav?url";
import userJoinUrl from "../assets/sounds/user_join.wav?url";
import userLeaveUrl from "../assets/sounds/user_leave.wav?url";
import streamStartUrl from "../assets/sounds/stream_start.wav?url";
import streamStopUrl from "../assets/sounds/stream_stop.wav?url";
import connectUrl from "../assets/sounds/connect.wav?url";
import disconnectUrl from "../assets/sounds/disconnect.wav?url";

type SoundName =
  | "mute"
  | "unmute"
  | "deafen"
  | "undeafen"
  | "user_join"
  | "user_leave"
  | "stream_start"
  | "stream_stop"
  | "connect"
  | "disconnect";

const SOURCES: Record<SoundName, string> = {
  mute: muteUrl,
  unmute: unmuteUrl,
  deafen: deafenUrl,
  undeafen: undeafenUrl,
  user_join: userJoinUrl,
  user_leave: userLeaveUrl,
  stream_start: streamStartUrl,
  stream_stop: streamStopUrl,
  connect: connectUrl,
  disconnect: disconnectUrl,
};

let ctx: AudioContext | null = null;
const bufferCache = new Map<SoundName, AudioBuffer>();
const inFlight = new Map<SoundName, Promise<AudioBuffer | null>>();

function getCtx(): AudioContext {
  if (!ctx) ctx = new AudioContext();
  return ctx;
}

async function loadBuffer(name: SoundName): Promise<AudioBuffer | null> {
  const cached = bufferCache.get(name);
  if (cached) return cached;
  const pending = inFlight.get(name);
  if (pending) return pending;

  const promise = (async () => {
    try {
      const res = await fetch(SOURCES[name]);
      const bytes = await res.arrayBuffer();
      // decodeAudioData detaches the input ArrayBuffer in some
      // implementations; that's fine, we're not reusing it.
      const buf = await getCtx().decodeAudioData(bytes);
      bufferCache.set(name, buf);
      return buf;
    } catch {
      return null;
    } finally {
      inFlight.delete(name);
    }
  })();
  inFlight.set(name, promise);
  return promise;
}

export function playSound(name: SoundName): void {
  // Fire-and-forget; we never want a failed sound to surface to the
  // caller (matches the previous tauri/cpal contract).
  void (async () => {
    const buf = await loadBuffer(name);
    if (!buf) return;
    const c = getCtx();
    // If the context was created before any user gesture (shouldn't
    // happen given how this is invoked, but defensive), nudge it.
    if (c.state === "suspended") {
      try {
        await c.resume();
      } catch {
        return;
      }
    }
    const src = c.createBufferSource();
    src.buffer = buf;
    src.connect(c.destination);
    src.start();
    // No cleanup needed: AudioBufferSourceNode auto-releases its
    // graph connection when playback finishes, and the GC reclaims
    // the node once the local reference goes out of scope.
  })();
}
