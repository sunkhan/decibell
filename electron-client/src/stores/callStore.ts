import { create } from "zustand";
import type { CallCandidate } from "../types";

// Lifecycle state of the (at most one) P2P DM call. The media-session
// state (mute / deafen / speaking / streams / ping) stays in voiceStore —
// in a call it runs with `connectedChannelId === null` and
// `callPeer === peer` so UserPanel, the stream player stack and the
// context menus keep working unchanged. This store only knows who we are
// calling and how far the handshake got.
//
//   idle ──call()──▶ outgoing ──ACCEPT──▶ connecting ──call_connected──▶ active
//   idle ──INVITE──▶ incoming ──accept()─▶ connecting ──call_connected──▶ active
//   any ──REJECT/BUSY/CANCEL/HANGUP/PEER_OFFLINE/NOT_ALLOWED/call_failed/
//         call_dropped/timeout──▶ idle
export type CallStatus = "idle" | "outgoing" | "incoming" | "connecting" | "active";

export interface IncomingCall {
  callId: string;
  from: string;
  pubKey: string;
  candidates: CallCandidate[];
}

interface CallState {
  status: CallStatus;
  callId: string | null;
  peer: string | null;
  isCaller: boolean;
  /// Caller side: the callee's client acked the INVITE (it is ringing
  /// there) — drives "Calling…" vs "Ringing…".
  ringingAcked: boolean;
  /// Epoch ms when `call_connected` arrived; the panel's timer base.
  startedAt: number | null;
  connectedPath: "host" | "srflx" | null;
  /// Why the last call ended (shown briefly in the panel / toast).
  endReason: string | null;
  /// The pending INVITE while `status === "incoming"`, kept until Accept.
  incoming: IncomingCall | null;
  /// Theater: the call stage fills the DM panel and the conversation folds
  /// into a header toggle. `theaterBaseline` is the conversation's message
  /// count when theater was switched on — the toggle's unread badge is the
  /// difference. Session-only, cleared with the call.
  theater: boolean;
  theaterBaseline: number;
  setTheater: (on: boolean, baseline?: number) => void;

  /// From LoginResponse via `get_call_config`. `callSignaling` gates
  /// the Call button: an older central never relays CALL_SIGNAL.
  callSignaling: boolean;
  stunServers: string[];

  setCallConfig: (cfg: { callSignaling: boolean; stunServers: string[] }) => void;
  startOutgoing: (callId: string, peer: string) => void;
  setIncoming: (call: IncomingCall) => void;
  setRingingAcked: () => void;
  setConnecting: () => void;
  setActive: (path: "host" | "srflx") => void;
  reset: (endReason?: string | null) => void;
}

export const useCallStore = create<CallState>((set) => ({
  status: "idle",
  callId: null,
  peer: null,
  isCaller: false,
  ringingAcked: false,
  startedAt: null,
  connectedPath: null,
  endReason: null,
  incoming: null,
  callSignaling: false,
  stunServers: [],
  theater: false,
  theaterBaseline: 0,
  setTheater: (on, baseline = 0) => set({ theater: on, theaterBaseline: on ? baseline : 0 }),

  setCallConfig: ({ callSignaling, stunServers }) => set({ callSignaling, stunServers }),
  startOutgoing: (callId, peer) =>
    set({
      status: "outgoing",
      callId,
      peer,
      isCaller: true,
      ringingAcked: false,
      startedAt: null,
      connectedPath: null,
      endReason: null,
      incoming: null,
    }),
  setIncoming: (call) =>
    set({
      status: "incoming",
      callId: call.callId,
      peer: call.from,
      isCaller: false,
      ringingAcked: false,
      startedAt: null,
      connectedPath: null,
      endReason: null,
      incoming: call,
    }),
  setRingingAcked: () => set({ ringingAcked: true }),
  setConnecting: () => set({ status: "connecting", incoming: null }),
  setActive: (path) => set({ status: "active", startedAt: Date.now(), connectedPath: path }),
  reset: (endReason = null) =>
    set({
      status: "idle",
      callId: null,
      peer: null,
      isCaller: false,
      ringingAcked: false,
      startedAt: null,
      connectedPath: null,
      endReason,
      incoming: null,
      theater: false,
      theaterBaseline: 0,
    }),
}));
