// User statuses (docs/superpowers/specs/2026-10-11-user-status-design.md).
//
// Central is the source of truth: a PRESENCE_UPDATE snapshot at login,
// then one PresenceDelta per change. A user absent from `statuses` is
// offline — invisible users are never on the wire. Community rosters'
// own is_online flag is only a fallback before the first snapshot (e.g.
// central unreachable), see usePresence.

import { create } from "zustand";
import { useAuthStore } from "./authStore";

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

interface PresenceState {
  /// Visible online users → their effective status.
  statuses: Record<string, PresenceStatus>;
  /// A central snapshot arrived since login.
  loaded: boolean;
  /// This central understands statuses (LoginResponse.presence_statuses);
  /// false hides the status menu.
  supported: boolean;
  /// The local user's chosen status (saved on central).
  ownStatus: OwnStatus;
  /// Auto-away is reporting no input — central shows an Online user Away.
  idle: boolean;
  applySnapshot: (users: { username: string; status: string }[]) => void;
  applyDelta: (username: string, online: boolean, status: string) => void;
  setOwnFromLogin: (supported: boolean, status: string) => void;
  setOwnStatus: (status: OwnStatus) => void;
  setIdle: (idle: boolean) => void;
  /// Central connection lost: what we had is stale. Lists fall back to
  /// the communities' own online flags until the next snapshot.
  markDisconnected: () => void;
  /// Logout (a different account may sign in next).
  reset: () => void;
}

export const usePresenceStore = create<PresenceState>((set) => ({
  statuses: {},
  loaded: false,
  supported: false,
  ownStatus: "online",
  idle: false,
  applySnapshot: (users) =>
    set(() => {
      const statuses: Record<string, PresenceStatus> = {};
      for (const u of users) statuses[u.username] = isPresenceStatus(u.status) ? u.status : "online";
      return { statuses, loaded: true };
    }),
  applyDelta: (username, online, status) =>
    set((state) => {
      const cur = state.statuses[username];
      const next: PresenceStatus | undefined = online
        ? isPresenceStatus(status)
          ? status
          : "online"
        : undefined;
      // `return state` is the only no-op updater — `{}` still notifies.
      if (cur === next) return state;
      const statuses = { ...state.statuses };
      if (next) statuses[username] = next;
      else delete statuses[username];
      return { statuses };
    }),
  setOwnFromLogin: (supported, status) =>
    set({
      supported,
      ownStatus: isPresenceStatus(status) || status === "invisible" ? status : "online",
    }),
  setOwnStatus: (ownStatus) => set({ ownStatus }),
  setIdle: (idle) => set((state) => (state.idle === idle ? state : { idle })),
  markDisconnected: () => set({ statuses: {}, loaded: false }),
  reset: () =>
    set({ statuses: {}, loaded: false, supported: false, ownStatus: "online", idle: false }),
}));

/// The local user as others see them (Away while idle on Online), except
/// that Invisible shows as itself rather than offline.
export function useOwnDisplayStatus(): DisplayStatus {
  return usePresenceStore((s) =>
    s.ownStatus === "online" && s.idle ? "away" : s.ownStatus,
  );
}

/// One user's status for display. `communityOnline` is the community
/// roster's own flag, used only until central's first snapshot arrives.
export function usePresence(username: string, communityOnline?: boolean): DisplayStatus {
  const me = useAuthStore((s) => s.username);
  const own = useOwnDisplayStatus();
  const other = usePresenceStore((s): DisplayStatus => {
    const st = s.statuses[username];
    if (st) return st;
    return !s.loaded && communityOnline ? "online" : "offline";
  });
  return username === me ? own : other;
}

/// Online in the "can be reached right now" sense (calls, grouping).
export const isOnlineStatus = (s: DisplayStatus) =>
  s === "online" || s === "away" || s === "dnd";

/// Non-hook resolver for list splitting (MembersList groups).
export function resolvePresence(
  state: Pick<PresenceState, "statuses" | "loaded" | "ownStatus" | "idle">,
  me: string | null,
  username: string,
  communityOnline?: boolean,
): DisplayStatus {
  if (username === me) return state.ownStatus === "online" && state.idle ? "away" : state.ownStatus;
  const st = state.statuses[username];
  if (st) return st;
  return !state.loaded && communityOnline ? "online" : "offline";
}

/// The local user, for code outside React (call ringing).
export function ownDisplayStatus(): DisplayStatus {
  const s = usePresenceStore.getState();
  return s.ownStatus === "online" && s.idle ? "away" : s.ownStatus;
}
