// User statuses (docs/superpowers/specs/2026-10-11-user-status-design.md).
//
// Central is the source of truth, by subscription (2026-10-15-central-
// scaling-design.md): every status hook registers interest in its user
// (features/presence/presenceInterest.ts); central answers the newly
// watched with a PRESENCE_UPDATE and then sends a PresenceDelta per change.
// A watched user central has answered for and absent from `statuses` is
// offline — invisible users are never on the wire. Until central answers,
// a community roster's own is_online flag stands in, see usePresence.

import { create } from "zustand";
import { useAuthStore } from "./authStore";
import type { Uid } from "./usersStore";
import { usePresenceInterest } from "../features/presence/presenceInterest";

/// What others can be seen as.
export type PresenceStatus = "online" | "away" | "dnd";
/// The local user's choice.
export type OwnStatus = PresenceStatus | "invisible";
/// What a status dot shows.
export type DisplayStatus = PresenceStatus | "offline" | "invisible";

export const STATUS_LABEL: Record<DisplayStatus, string> = {
  online: "Online",
  away: "Away",
  dnd: "Do Not Disturb",
  offline: "Offline",
  invisible: "Invisible",
};

const isPresenceStatus = (s: string): s is PresenceStatus =>
  s === "online" || s === "away" || s === "dnd";

/// A custom status message (docs/superpowers/specs/2026-10-12-status-
/// messages-design.md): one line of text and/or one emoji.
export interface StatusMessage {
  text: string;
  emoji: string;
}

export const STATUS_TEXT_MAX = 128;

/// Central's normalisation (main.cpp normalize_status_line), so our own
/// copy reads exactly like everyone else's: control characters and line
/// separators become spaces, bidi controls are dropped, the ends are
/// trimmed, at most `max` code points.
export function normalizeStatusText(raw: string, max = STATUS_TEXT_MAX): string {
  let out = "";
  let n = 0;
  for (const ch of raw) {
    if (n >= max) break;
    const cp = ch.codePointAt(0)!;
    if (cp <= 0x20 || cp === 0x7f || (cp >= 0x80 && cp < 0xa0) || cp === 0x2028 || cp === 0x2029) {
      if (out) {
        out += " ";
        n++;
      }
    } else if (!((cp >= 0x202a && cp <= 0x202e) || (cp >= 0x2066 && cp <= 0x2069))) {
      out += ch;
      n++;
    }
  }
  return out.replace(/ +$/, "");
}

/// null for "no message".
function toMessage(text: string | undefined, emoji: string | undefined): StatusMessage | null {
  return text || emoji ? { text: text ?? "", emoji: emoji ?? "" } : null;
}

const sameMessage = (a: StatusMessage | null, b: StatusMessage | null) =>
  a === b || (!!a && !!b && a.text === b.text && a.emoji === b.emoji);

// Our own message's clear-after; central clears it for everyone else on
// its own clock, this only stops showing it to us at the same moment.
let ownClearTimer: ReturnType<typeof setTimeout> | null = null;

interface PresenceState {
  /// Visible online users (by uid) → their effective status.
  statuses: Record<Uid, PresenceStatus>;
  /// Users central has answered for since the last (re)connect: one of
  /// these absent from `statuses` is offline.
  known: ReadonlySet<Uid>;
  /// This central understands statuses (LoginResponse.presence_statuses);
  /// false hides the status menu.
  supported: boolean;
  /// The local user's chosen status (saved on central).
  ownStatus: OwnStatus;
  /// Auto-away is reporting no input — central shows an Online user Away.
  idle: boolean;
  /// Visible online users' custom status messages (absent = none). Kept
  /// apart from `statuses` so a message edit doesn't regroup member lists.
  messages: Record<Uid, StatusMessage>;
  /// The local user's own message (saved on central).
  ownMessage: StatusMessage | null;
  /// When central clears it (local ms epoch); null = never.
  ownMessageClearsAt: number | null;
  /// A subscription answer: `covered` users' current presence (those not
  /// in `users` are offline).
  applyCovered: (
    covered: Uid[],
    users: { uid: Uid; status: string; statusText?: string; statusEmoji?: string }[],
  ) => void;
  applyDelta: (
    uid: Uid,
    online: boolean,
    status: string,
    statusText?: string,
    statusEmoji?: string,
  ) => void;
  setOwnFromLogin: (
    supported: boolean,
    status: string,
    message: StatusMessage | null,
    clearsInSeconds: number,
  ) => void;
  setOwnStatus: (status: OwnStatus) => void;
  setOwnMessage: (message: StatusMessage | null, clearsAt: number | null) => void;
  setIdle: (idle: boolean) => void;
  /// Central connection lost: what we had is stale. Lists fall back to
  /// the communities' own online flags until central answers again.
  markDisconnected: () => void;
  /// Logout (a different account may sign in next).
  reset: () => void;
}

export const usePresenceStore = create<PresenceState>((set, get) => ({
  statuses: {},
  known: new Set(),
  supported: false,
  ownStatus: "online",
  idle: false,
  messages: {},
  ownMessage: null,
  ownMessageClearsAt: null,
  applyCovered: (covered, users) =>
    set((state) => {
      if (covered.length === 0) return state;
      const statuses = { ...state.statuses };
      const messages = { ...state.messages };
      const known = new Set(state.known);
      for (const uid of covered) {
        if (!(uid > 0)) continue;
        delete statuses[uid];
        delete messages[uid];
        known.add(uid);
      }
      for (const u of users) {
        if (!(u.uid > 0)) continue;
        statuses[u.uid] = isPresenceStatus(u.status) ? u.status : "online";
        const message = toMessage(u.statusText, u.statusEmoji);
        if (message) messages[u.uid] = message;
      }
      return { statuses, messages, known };
    }),
  applyDelta: (uid, online, status, statusText, statusEmoji) =>
    set((state) => {
      const cur = state.statuses[uid];
      const next: PresenceStatus | undefined = online
        ? isPresenceStatus(status)
          ? status
          : "online"
        : undefined;
      const curMessage = state.messages[uid] ?? null;
      const nextMessage = online ? toMessage(statusText, statusEmoji) : null;
      const statusSame = cur === next;
      const messageSame = sameMessage(curMessage, nextMessage);
      const knownSame = state.known.has(uid);
      // `return state` is the only no-op updater — `{}` still notifies.
      if (statusSame && messageSame && knownSame) return state;
      const patch: Partial<PresenceState> = {};
      if (!knownSame) patch.known = new Set(state.known).add(uid);
      if (!statusSame) {
        const statuses = { ...state.statuses };
        if (next) statuses[uid] = next;
        else delete statuses[uid];
        patch.statuses = statuses;
      }
      if (!messageSame) {
        const messages = { ...state.messages };
        if (nextMessage) messages[uid] = nextMessage;
        else delete messages[uid];
        patch.messages = messages;
      }
      return patch;
    }),
  setOwnFromLogin: (supported, status, message, clearsInSeconds) => {
    set({
      supported,
      ownStatus: isPresenceStatus(status) || status === "invisible" ? status : "online",
    });
    get().setOwnMessage(message, message && clearsInSeconds > 0 ? Date.now() + clearsInSeconds * 1000 : null);
  },
  setOwnStatus: (ownStatus) => set({ ownStatus }),
  setOwnMessage: (ownMessage, clearsAt) => {
    if (ownClearTimer) clearTimeout(ownClearTimer);
    ownClearTimer = null;
    const ownMessageClearsAt = ownMessage ? clearsAt : null;
    if (ownMessageClearsAt !== null) {
      ownClearTimer = setTimeout(
        () => {
          ownClearTimer = null;
          set({ ownMessage: null, ownMessageClearsAt: null });
        },
        Math.max(0, ownMessageClearsAt - Date.now()),
      );
    }
    set({ ownMessage, ownMessageClearsAt });
  },
  setIdle: (idle) => set((state) => (state.idle === idle ? state : { idle })),
  markDisconnected: () => set({ statuses: {}, messages: {}, known: new Set() }),
  reset: () => {
    if (ownClearTimer) clearTimeout(ownClearTimer);
    ownClearTimer = null;
    set({
      statuses: {},
      messages: {},
      known: new Set(),
      supported: false,
      ownStatus: "online",
      idle: false,
      ownMessage: null,
      ownMessageClearsAt: null,
    });
  },
}));

/// The local user as others see them (Away while idle on Online), except
/// that Invisible shows as itself rather than offline.
export function useOwnDisplayStatus(): DisplayStatus {
  return usePresenceStore((s) =>
    s.ownStatus === "online" && s.idle ? "away" : s.ownStatus,
  );
}

/// One user's status for display; watches them while mounted.
/// `communityOnline` is the community roster's own flag, used only until
/// central has answered for this user.
export function usePresence(uid: Uid, communityOnline?: boolean): DisplayStatus {
  usePresenceInterest(uid);
  const me = useAuthStore((s) => s.uid);
  const own = useOwnDisplayStatus();
  const other = usePresenceStore((s): DisplayStatus => {
    const st = s.statuses[uid];
    if (st) return st;
    return !s.known.has(uid) && communityOnline ? "online" : "offline";
  });
  return uid > 0 && uid === me ? own : other;
}

/// One user's custom status message, or null; watches them while mounted.
/// Others' ride their presence, so only a visible user has one; our own
/// shows to us even while invisible.
export function useStatusMessage(uid: Uid): StatusMessage | null {
  usePresenceInterest(uid);
  const me = useAuthStore((s) => s.uid);
  const own = usePresenceStore((s) => s.ownMessage);
  const other = usePresenceStore((s) => s.messages[uid] ?? null);
  return uid > 0 && uid === me ? own : other;
}

/// Online in the "can be reached right now" sense (calls, grouping).
export const isOnlineStatus = (s: DisplayStatus) =>
  s === "online" || s === "away" || s === "dnd";

/// Non-hook resolver for list splitting (MembersList groups). The list
/// must watch its users (usePresenceInterest) for central to answer.
export function resolvePresence(
  state: Pick<PresenceState, "statuses" | "known" | "ownStatus" | "idle">,
  me: Uid,
  uid: Uid,
  communityOnline?: boolean,
): DisplayStatus {
  if (uid > 0 && uid === me) return state.ownStatus === "online" && state.idle ? "away" : state.ownStatus;
  const st = state.statuses[uid];
  if (st) return st;
  return !state.known.has(uid) && communityOnline ? "online" : "offline";
}

/// The local user, for code outside React (call ringing).
export function ownDisplayStatus(): DisplayStatus {
  const s = usePresenceStore.getState();
  return s.ownStatus === "online" && s.idle ? "away" : s.ownStatus;
}
