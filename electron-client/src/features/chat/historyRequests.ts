import { invoke } from "../../lib/ipc";
import { channelKey } from "../../lib/channelKey";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { nameOf, type Uid } from "../../stores/usersStore";

// History requests (channel and DM) and the waits on their replies.
//
// A reply comes back as an event, not as the invoke's result, and it can
// be lost: with the connection (a reconnect), or without it (native skips
// a frame over 2 MiB). A wait that never ended is how "Loading history…"
// stayed up forever and how a lost older page was never asked for again
// (review B4). Every request made here is tracked until its reply arrives
// or REPLY_TIMEOUT_MS passes, and the promise says which: the reply's
// summary, or null (timed out, refused by native, or abandoned because the
// connection went away). Callers retry or reset on null.
//
// Replies are matched by the mode the server echoes: an around reply by
// its around_id, an after reply by its after_id, and a newest / older page
// (neither echoed) by order, oldest request first. Replies come back in
// request order, so a mismatch takes a lost reply, and costs no more than
// a spurious retry.

const REPLY_TIMEOUT_MS = 10_000;
/// Between attempts of a newest-page load, and before a paginator asks
/// for a page again after a failure (a failing send can't spin).
export const RETRY_DELAY_MS = 1_000;
const LATEST_ATTEMPTS = 3;

export interface HistoryReply {
  /// Highest id in the page (0 for an empty one).
  newestId: number;
  /// The server has newer rows past the page.
  hasMoreAfter: boolean;
}

export interface HistoryRequest {
  beforeId?: number;
  afterId?: number;
  aroundId?: number;
  limit: number;
}

type Mode = "before" | "after" | "around";

interface Waiter {
  /// What abandon() matches: one community, or central.
  scope: string;
  mode: Mode;
  cursor: number;
  done: (reply: HistoryReply | null) => void;
}

const waiters = new Map<string, Waiter[]>();

function modeOf(req: HistoryRequest): [Mode, number] {
  if (req.aroundId && req.aroundId > 0) return ["around", req.aroundId];
  if (req.afterId && req.afterId > 0) return ["after", req.afterId];
  return ["before", 0];
}

function track(key: string, scope: string, req: HistoryRequest, send: () => Promise<unknown>): Promise<HistoryReply | null> {
  const [mode, cursor] = modeOf(req);
  return new Promise((resolve) => {
    let settled = false;
    let timer = 0;
    const w: Waiter = {
      scope,
      mode,
      cursor,
      done: (reply) => {
        if (settled) return;
        settled = true;
        window.clearTimeout(timer);
        const list = waiters.get(key);
        const i = list ? list.indexOf(w) : -1;
        if (list && i >= 0) {
          list.splice(i, 1);
          if (list.length === 0) waiters.delete(key);
        }
        resolve(reply);
      },
    };
    const list = waiters.get(key);
    if (list) list.push(w);
    else waiters.set(key, [w]);
    timer = window.setTimeout(() => w.done(null), REPLY_TIMEOUT_MS);
    send().catch((err) => {
      console.error("history request:", err);
      w.done(null);
    });
  });
}

function settle(key: string, aroundId: number, afterId: number, reply: HistoryReply): void {
  const list = waiters.get(key);
  if (!list) return;
  const w =
    aroundId > 0
      ? list.find((x) => x.mode === "around" && x.cursor === aroundId)
      : afterId > 0
        ? list.find((x) => x.mode === "after" && x.cursor === afterId)
        : list.find((x) => x.mode === "before");
  w?.done(reply);
}

function abandon(scope: string): void {
  for (const list of [...waiters.values()]) {
    for (const w of [...list]) if (w.scope === scope) w.done(null);
  }
}

const delay = (ms: number) => new Promise<void>((r) => window.setTimeout(r, ms));

// ── Channels ────────────────────────────────────────────────────────

const channelWaitKey = (serverId: string, channelId: string) => `c|${channelKey(serverId, channelId)}`;
const channelScope = (serverId: string) => `c|${serverId}`;

export function requestChannelHistory(
  serverId: string,
  channelId: string,
  req: HistoryRequest,
): Promise<HistoryReply | null> {
  return track(channelWaitKey(serverId, channelId), channelScope(serverId), req, () =>
    invoke("request_channel_history", {
      serverId,
      channelId,
      beforeId: req.beforeId ?? 0,
      limit: req.limit,
      ...(req.afterId ? { afterId: req.afterId } : {}),
      ...(req.aroundId ? { aroundId: req.aroundId } : {}),
    }),
  );
}

/// useChatEvents, after applying a page to the store.
export function noteChannelHistoryReply(
  serverId: string,
  channelId: string,
  aroundId: number,
  afterId: number,
  reply: HistoryReply,
): void {
  settle(channelWaitKey(serverId, channelId), aroundId, afterId, reply);
}

/// The community's connection dropped or was replaced: nothing sent on the
/// old one will be answered.
export function abandonChannelRequests(serverId: string): void {
  abandon(channelScope(serverId));
}

const latestChannelLoads = new Map<string, Promise<boolean>>();

/// Load the channel's newest page into whatever its slice holds (see
/// prependHistory: merged, or replacing the slice across a gap), with
/// historyLoading on meanwhile. Up to three attempts while the community is
/// connected; false when none got an answer. historyLoading ends either
/// way. One load per channel at a time: a second caller shares the first.
export function loadLatestChannelPage(serverId: string, channelId: string): Promise<boolean> {
  const key = channelWaitKey(serverId, channelId);
  const running = latestChannelLoads.get(key);
  if (running) return running;
  const run = (async () => {
    useChatStore.getState().setHistoryLoading(serverId, channelId, true);
    try {
      for (let attempt = 0; attempt < LATEST_ATTEMPTS; attempt++) {
        if (attempt > 0) await delay(RETRY_DELAY_MS);
        // Offline: the re-sync after the reconnect loads it.
        if (!useChatStore.getState().connectedServers.has(serverId)) return false;
        if (await requestChannelHistory(serverId, channelId, { beforeId: 0, limit: 50 })) return true;
      }
      return false;
    } finally {
      latestChannelLoads.delete(key);
      useChatStore.getState().setHistoryLoading(serverId, channelId, false);
    }
  })();
  latestChannelLoads.set(key, run);
  return run;
}

// ── DMs ─────────────────────────────────────────────────────────────

const dmWaitKey = (peer: Uid) => `d|${peer}`;
const DM_SCOPE = "d";

export function requestDmHistory(peer: Uid, req: HistoryRequest): Promise<HistoryReply | null> {
  return track(dmWaitKey(peer), DM_SCOPE, req, () =>
    invoke("request_dm_history", {
      peer: nameOf(peer),
      peerUid: peer,
      beforeId: req.beforeId ?? 0,
      limit: req.limit,
      ...(req.afterId ? { afterId: req.afterId } : {}),
      ...(req.aroundId ? { aroundId: req.aroundId } : {}),
    }),
  );
}

/// useDmEvents, after applying a page to the store.
export function noteDmHistoryReply(peer: Uid, aroundId: number, afterId: number, reply: HistoryReply): void {
  settle(dmWaitKey(peer), aroundId, afterId, reply);
}

/// Central's connection dropped or signed in again.
export function abandonDmRequests(): void {
  abandon(DM_SCOPE);
}

export function centralSignedIn(): boolean {
  return useUiStore.getState().connectionStatus === "connected";
}

const latestDmLoads = new Map<string, Promise<boolean>>();

/// A conversation's newest page — loadLatestChannelPage's twin. The
/// conversation's own flags (historyLoaded) follow the reply.
export function loadLatestDmPage(peer: Uid): Promise<boolean> {
  const key = dmWaitKey(peer);
  const running = latestDmLoads.get(key);
  if (running) return running;
  const run = (async () => {
    try {
      for (let attempt = 0; attempt < LATEST_ATTEMPTS; attempt++) {
        if (attempt > 0) await delay(RETRY_DELAY_MS);
        if (!centralSignedIn()) return false;
        if (await requestDmHistory(peer, { beforeId: 0, limit: 50 })) return true;
      }
      return false;
    } finally {
      latestDmLoads.delete(key);
    }
  })();
  latestDmLoads.set(key, run);
  return run;
}
