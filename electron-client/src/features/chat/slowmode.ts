import { useEffect, useState } from "react";
import { create } from "zustand";
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";
import { channelKey, type ChannelKey } from "../../lib/channelKey";
import { PERM, hasBits } from "../servers/permissions";

// Slowmode, as the composer sees it. The community allows one message per
// `slowmodeSeconds` per member in a channel, unless the member may Manage
// Messages there (src/community/main.cpp). It keeps no state the client can
// ask for, so the wait is tracked here:
//  - every message of ours that the server accepts (the echo, from this
//    device or another) starts a full window;
//  - a refusal carries the server's remaining wait (retryAfterSeconds),
//    which replaces ours, e.g. after an app restart inside a window.
// Both ends run a little after the server's own window, never before it.

interface SlowmodeState {
  /// Channel → when the next message may go out (ms, Date.now clock).
  until: Record<ChannelKey, number>;
  /// The last send that was held back, so its channel's timer can shake.
  nudge: { key: ChannelKey; n: number } | null;
}

export const useSlowmodeStore = create<SlowmodeState>(() => ({ until: {}, nudge: null }));

type ChatSnapshot = ReturnType<typeof useChatStore.getState>;

/// The slowmode interval that applies to `me` in a channel, in seconds:
/// 0 when it's off or bypassed (the owner, or Manage Messages there).
function intervalFor(chat: ChatSnapshot, me: string | null, serverId: string, channelId: string): number {
  const ch = chat.channelsByServer[serverId]?.find((c) => c.id === channelId);
  const seconds = ch?.slowmodeSeconds ?? 0;
  if (!ch || seconds <= 0) return 0;
  if (me && chat.serverOwner[serverId] === me) return 0;
  if (ch.myPermissions !== undefined && hasBits(ch.myPermissions, PERM.MANAGE_MESSAGES)) return 0;
  return seconds;
}

export function slowmodeInterval(serverId: string, channelId: string): number {
  return intervalFor(useChatStore.getState(), useAuthStore.getState().username, serverId, channelId);
}

/// A message of ours was accepted in this channel: a new window starts.
export function noteOwnMessage(serverId: string, channelId: string): void {
  const seconds = slowmodeInterval(serverId, channelId);
  if (!seconds) return;
  const key = channelKey(serverId, channelId);
  const until = Date.now() + seconds * 1000;
  useSlowmodeStore.setState((s) =>
    (s.until[key] ?? 0) >= until ? s : { until: { ...s.until, [key]: until } },
  );
}

/// The server refused a message for slowmode, with `seconds` still to go.
export function noteSlowmodeRefusal(serverId: string, channelId: string, seconds: number): void {
  if (seconds <= 0) return;
  const key = channelKey(serverId, channelId);
  useSlowmodeStore.setState((s) => ({ until: { ...s.until, [key]: Date.now() + seconds * 1000 } }));
}

/// Whether a send in this channel has to wait: inside a window, or while
/// a message of ours is still on its way (it may yet start one).
export function slowmodeHolds(serverId: string, channelId: string): boolean {
  if (!slowmodeInterval(serverId, channelId)) return false;
  const key = channelKey(serverId, channelId);
  if ((useSlowmodeStore.getState().until[key] ?? 0) > Date.now()) return true;
  const me = useAuthStore.getState().username;
  const messages = useChatStore.getState().messagesByChannel[key] ?? [];
  // Our in-flight bubbles sit at the tail.
  for (let i = messages.length - 1; i >= 0 && i >= messages.length - 20; i--) {
    if (messages[i].pending && messages[i].sender === me) return true;
  }
  return false;
}

/// A send was held back: shake that channel's timer.
export function nudgeSlowmode(serverId: string, channelId: string): void {
  const key = channelKey(serverId, channelId);
  useSlowmodeStore.setState((s) => ({ nudge: { key, n: (s.nudge?.n ?? 0) + 1 } }));
}

/// The interval that applies to the local user in a channel (0 = none)
/// and the whole seconds left before the next message may go out.
/// Re-renders once a second while a wait runs.
export function useSlowmode(
  serverId: string | null,
  channelId: string | null,
): { interval: number; remaining: number } {
  const me = useAuthStore((s) => s.username);
  const interval = useChatStore((s) =>
    serverId && channelId ? intervalFor(s, me, serverId, channelId) : 0,
  );
  const until = useSlowmodeStore((s) =>
    serverId && channelId ? s.until[channelKey(serverId, channelId)] ?? 0 : 0,
  );
  const [, setTick] = useState(0);
  const now = Date.now();
  const running = interval > 0 && until > now;
  useEffect(() => {
    if (!running) return;
    // Wake on the whole-second boundaries of the wait, so the shown
    // seconds change on time and the last one ends exactly at `until`.
    let timer = 0;
    const schedule = () => {
      const left = until - Date.now();
      if (left <= 0) return;
      timer = window.setTimeout(() => {
        setTick((t) => t + 1);
        schedule();
      }, left % 1000 || 1000);
    };
    schedule();
    return () => window.clearTimeout(timer);
  }, [running, until]);
  return { interval, remaining: running ? Math.ceil((until - now) / 1000) : 0 };
}
