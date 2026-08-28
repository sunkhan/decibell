import { useChatStore } from "../../stores/chatStore";
import { toast } from "../../stores/toastStore";
import { channelKey } from "../../lib/channelKey";

// Outbound message pacing + the two backstops for a send that never
// echoes.
//
// The community drops a CHANNEL_MSG once its per-session bucket is
// empty (10 burst, 3 per second sustained, shared with edits) and
// answers with CHANNEL_MSG_REJECTED. A fast typist firing one-word
// replies can hit that, and a dropped message is a lost message. So
// the client mirrors the bucket and *delays* instead: sends queue per
// server (FIFO, so order is preserved) and go out no faster than the
// server admits. The optimistic bubble is already on screen, so the
// user sees nothing but the echo arriving a beat later. Discord's
// client does the same.
//
// The mirror runs a little under the server's rate (clocks and packet
// timing differ) and resets whenever the community authenticates,
// which is when the server's own bucket starts full.

const BURST = 10;
const REFILL_PER_SEC = 2.7;

interface Pacer {
  tokens: number;
  last: number;
  queue: Array<() => void>;
  draining: boolean;
}

const pacers = new Map<string, Pacer>();

function pacerFor(serverId: string): Pacer {
  let p = pacers.get(serverId);
  if (!p) {
    p = { tokens: BURST, last: performance.now(), queue: [], draining: false };
    pacers.set(serverId, p);
  }
  return p;
}

function refill(p: Pacer): void {
  const now = performance.now();
  p.tokens = Math.min(BURST, p.tokens + ((now - p.last) / 1000) * REFILL_PER_SEC);
  p.last = now;
}

function drain(p: Pacer): void {
  if (p.draining) return;
  p.draining = true;
  const step = () => {
    refill(p);
    while (p.queue.length > 0 && p.tokens >= 1) {
      p.tokens -= 1;
      p.queue.shift()!();
    }
    if (p.queue.length === 0) {
      p.draining = false;
      return;
    }
    const waitMs = ((1 - p.tokens) / REFILL_PER_SEC) * 1000;
    window.setTimeout(step, Math.max(10, Math.ceil(waitMs)));
  };
  step();
}

/// Run `task` once the server's message bucket admits another send.
/// Resolves with the task's result; tasks for one server run in order.
export function paceSend<T>(serverId: string, task: () => Promise<T>): Promise<T> {
  const p = pacerFor(serverId);
  return new Promise<T>((resolve, reject) => {
    p.queue.push(() => task().then(resolve, reject));
    drain(p);
  });
}

/// The server's bucket is per session and starts full on auth.
export function resetSendPacing(serverId: string): void {
  pacers.delete(serverId);
}

// ── Backstops ───────────────────────────────────────────────────────

const ECHO_TIMEOUT_MS = 30_000;

/// A sent message must come back as its own broadcast (same nonce).
/// If it hasn't within the timeout — a response lost to a reconnect,
/// an older server with no typed rejection — withdraw the bubble
/// rather than leave a ghost row anchored at the tail forever.
export function watchEcho(serverId: string, channelId: string, nonce: string): void {
  window.setTimeout(() => {
    const chat = useChatStore.getState();
    const list = chat.messagesByChannel[channelKey(serverId, channelId)];
    if (!list || !list.some((m) => m.id === 0 && m.nonce === nonce)) return;
    chat.removeMessageByNonce(serverId, channelId, nonce);
    toast.error("Message not sent", "The server didn't confirm it.");
  }, ECHO_TIMEOUT_MS);
}

// The server still sends the legacy MOD_ACTION_RES(action="message")
// right after CHANNEL_MSG_REJECTED for older clients; on this client the
// typed rejection already toasted, so the legacy one is skipped when it
// lands within a beat of it. An older server sends only the legacy one,
// and that still toasts.
let suppressLegacyUntil = 0;

export function noteTypedRejection(): void {
  suppressLegacyUntil = Date.now() + 1500;
}

export function legacyRejectionSuppressed(): boolean {
  return Date.now() < suppressLegacyUntil;
}
