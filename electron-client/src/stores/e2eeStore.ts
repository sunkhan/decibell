import { create } from "zustand";
import type { E2eeStatus, E2eeStatusPayload } from "../types";

// E2EE DMs — renderer-side mirror of native's e2ee status plus the UI
// state around it (passphrase modal, per-session dismissals, peer
// key-change notices). Keys and ciphertext never reach the renderer; see
// docs/superpowers/specs/2026-09-03-e2ee-dms-design.md.

export type PassphraseMode = "setup" | "unlock" | "change" | "reset";

interface E2eeState {
  supported: boolean;
  status: E2eeStatus;
  keyId: number;
  fingerprint: string;
  /// True once native has reported a status for the current login. The
  /// first "locked" report opens the unlock prompt exactly once.
  reported: boolean;
  /// Peers whose pinned identity changed this session → when (unix s).
  /// Cleared per peer by dismissPeerChange.
  changedPeers: Record<string, number>;
  /// The "set up encryption" nudge in the DM panel, dismissed for this
  /// session.
  setupNudgeDismissed: boolean;
  passphraseModal: PassphraseMode | null;

  setStatus: (p: E2eeStatusPayload) => void;
  markPeerChanged: (username: string, at: number) => void;
  dismissPeerChange: (username: string) => void;
  dismissSetupNudge: () => void;
  openPassphraseModal: (mode: PassphraseMode) => void;
  closePassphraseModal: () => void;
}

export const useE2eeStore = create<E2eeState>((set) => ({
  supported: false,
  status: "unavailable",
  keyId: 0,
  fingerprint: "",
  reported: false,
  changedPeers: {},
  setupNudgeDismissed: false,
  passphraseModal: null,

  setStatus: (p) =>
    set((state) => ({
      supported: p.supported,
      status: p.status,
      keyId: p.keyId,
      fingerprint: p.fingerprint,
      // "unavailable" is also what logout reports: forget the session
      // flags so the next login gets its prompt again.
      reported: p.status !== "unavailable",
      changedPeers: p.status === "unavailable" ? {} : state.changedPeers,
      setupNudgeDismissed: p.status === "unavailable" ? false : state.setupNudgeDismissed,
      passphraseModal: p.status === "unavailable" ? null : state.passphraseModal,
    })),
  markPeerChanged: (username, at) =>
    set((state) => ({ changedPeers: { ...state.changedPeers, [username]: at } })),
  dismissPeerChange: (username) =>
    set((state) => {
      if (!(username in state.changedPeers)) return {};
      const next = { ...state.changedPeers };
      delete next[username];
      return { changedPeers: next };
    }),
  dismissSetupNudge: () => set({ setupNudgeDismissed: true }),
  openPassphraseModal: (mode) => set({ passphraseModal: mode }),
  closePassphraseModal: () => set({ passphraseModal: null }),
}));
