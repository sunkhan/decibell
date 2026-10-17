import { useChatStore } from "../../stores/chatStore";
import { useDmStore } from "../../stores/dmStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { toast } from "../../stores/toastStore";
import { channelKey } from "../../lib/channelKey";
import { centralSignedIn, requestChannelHistory, requestDmHistory } from "./historyRequests";

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
/// How far an echo check pages past the send (100 rows a page).
const ECHO_CHECK_PAGES = 5;

/// A sent message must come back as its own broadcast (same nonce). If it
/// hasn't within the timeout, either it never reached the server or its
/// echo was lost (a reconnect, review B4). Withdrawing it on a guess made
/// people send stored messages twice, so the server is asked first: the
/// rows posted since the send come back, ours carrying its nonce (or, from
/// a server that forgot it, our text), and that settles the bubble in the
/// store (lib/historyMerge). Only a send the server doesn't have is
/// withdrawn, its text put back in the message box. While the community is
/// unreachable the check waits for the reconnect, whose re-sync usually
/// settles the bubble first.
export function watchEcho(serverId: string, channelId: string, nonce: string): void {
  window.setTimeout(() => void checkEcho(serverId, channelId, nonce), ECHO_TIMEOUT_MS);
}

function channelBubble(serverId: string, channelId: string, nonce: string) {
  return useChatStore
    .getState()
    .messagesByChannel[channelKey(serverId, channelId)]?.find((m) => m.id === 0 && m.nonce === nonce);
}

/// Resolves once `epochOf` moves past its current value, or `gone` holds.
function waitFor<S>(store: { getState: () => S; subscribe: (fn: (s: S) => void) => () => void }, epochOf: (s: S) => number, gone: () => boolean): Promise<void> {
  const from = epochOf(store.getState());
  return new Promise((resolve) => {
    const unsubscribe = store.subscribe((s) => {
      if (epochOf(s) > from || gone()) {
        unsubscribe();
        resolve();
      }
    });
  });
}

/// Page through what was stored after `sentAfterId` until the bubble is
/// settled, the server has nothing newer, or no answer comes.
async function askServer(
  request: (afterId: number) => Promise<{ newestId: number; hasMoreAfter: boolean } | null>,
  sentAfterId: number,
  settled: () => boolean,
): Promise<void> {
  let after = sentAfterId;
  for (let page = 0; page < ECHO_CHECK_PAGES; page++) {
    const reply = await request(after);
    if (settled() || !reply || after === 0 || !reply.hasMoreAfter || reply.newestId <= after) return;
    after = reply.newestId;
  }
}

async function checkEcho(serverId: string, channelId: string, nonce: string): Promise<void> {
  const gone = () => !channelBubble(serverId, channelId, nonce);
  while (!gone() && !useChatStore.getState().connectedServers.has(serverId)) {
    await waitFor(useChatStore, (s) => s.resyncEpoch[serverId] ?? 0, gone);
  }
  const sent = channelBubble(serverId, channelId, nonce);
  if (!sent) return;
  await askServer(
    (afterId) =>
      requestChannelHistory(serverId, channelId, afterId > 0 ? { beforeId: 0, afterId, limit: 100 } : { beforeId: 0, limit: 100 }),
    sent.sentAfterId ?? 0,
    gone,
  );
  const bubble = channelBubble(serverId, channelId, nonce);
  if (!bubble) return;
  useChatStore.getState().removeMessageByNonce(serverId, channelId, nonce);
  const restored = useDraftsStore.getState().restoreChannelDraft(serverId, channelId, bubble.content);
  toast.error(
    "Message not sent",
    restored ? "The server didn't confirm it. Your text is back in the message box." : "The server didn't confirm it.",
  );
}

/// The DM twin: central echoes our DIRECT_MSG (nonce included) or answers
/// with an error reply carrying the nonce. Neither within the timeout →
/// the same check against the conversation's history (DM rows carry no
/// nonce, so our text settles it), then withdraw.
export function watchDmEcho(peer: number, nonce: string): void {
  window.setTimeout(() => void checkDmEcho(peer, nonce), ECHO_TIMEOUT_MS);
}

function dmBubble(peer: number, nonce: string) {
  return useDmStore.getState().conversations[peer]?.messages.find((m) => !m.id && m.nonce === nonce);
}

async function checkDmEcho(peer: number, nonce: string): Promise<void> {
  const gone = () => !dmBubble(peer, nonce);
  while (!gone() && !centralSignedIn()) {
    await waitFor(useDmStore, (s) => s.resyncEpoch, gone);
  }
  const sent = dmBubble(peer, nonce);
  if (!sent) return;
  await askServer(
    (afterId) =>
      requestDmHistory(peer, afterId > 0 ? { beforeId: 0, afterId, limit: 100 } : { beforeId: 0, limit: 100 }),
    sent.sentAfterId ?? 0,
    gone,
  );
  const bubble = dmBubble(peer, nonce);
  if (!bubble) return;
  useDmStore.getState().removeDmMessageByNonce(peer, nonce);
  const restored = useDraftsStore.getState().restoreDmDraft(peer, bubble.content);
  toast.error(
    "Message not sent",
    restored ? "The server didn't confirm it. Your text is back in the message box." : "The server didn't confirm it.",
  );
}

/// Per-send nonce for optimistic bubbles.
export function newNonce(): string {
  return crypto.randomUUID();
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
