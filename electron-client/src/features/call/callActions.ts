// Imperative half of the P2P DM-call lifecycle: everything the UI and the
// signal listener (useCallEvents) trigger. State lives in callStore; the
// media session lives in voiceStore + native (call_prepare / call_connect /
// call_end). Central only relays CallSignal packets.
//
//   caller                              callee
//   startCall ─ call_prepare ─ INVITE ─▶ setIncoming, RINGING, ring
//        ◀─ RINGING                        acceptCall ─ call_prepare
//        ◀─ ACCEPT{pubKey,candidates} ─────  ─ send ACCEPT, call_connect
//   call_connect (punch)                  (punch)
//        ◀════ sealed UDP, both sockets ════▶
//   call_connected                        call_connected
//
// A DM call and a community voice channel are mutually exclusive: one
// native VoiceEngine, one microphone. Starting / accepting a call leaves
// the channel first (leaveCommunityVoiceIfAny); joinVoiceChannel calls
// endCall first.

import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useCallStore } from "../../stores/callStore";
import { useDmStore } from "../../stores/dmStore";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { toast } from "../../stores/toastStore";
import { loopSound, playSound } from "../../utils/sounds";
import { flushSaveSettings } from "../settings/saveSettings";
import { applySavedUserGains } from "../voice/userGain";
import type { CallCandidate, CallSignalKind, CallStreamMeta, StreamInfo, VideoCodec } from "../../types";

/// How long an unanswered call rings before the caller gives up / the
/// callee's prompt goes away.
export const RING_TIMEOUT_MS = 45_000;

/// Stream id used for in-call screen shares in voiceStore.activeStreams
/// (community streams carry the server's ids).
export const callStreamId = (owner: string) => `call:${owner}`;

interface CallPrepareResult {
  pubKey: string;
  candidates: CallCandidate[];
}

let stopRingFn: (() => void) | null = null;
let ringTimer: ReturnType<typeof setTimeout> | null = null;

export function startRing(): void {
  stopRing();
  stopRingFn = loopSound("call_ring");
}

export function stopRing(): void {
  stopRingFn?.();
  stopRingFn = null;
}

function armRingTimeout(fn: () => void): void {
  clearRingTimeout();
  ringTimer = setTimeout(fn, RING_TIMEOUT_MS);
}

function clearRingTimeout(): void {
  if (ringTimer) clearTimeout(ringTimer);
  ringTimer = null;
}

export function sendSignal(
  callId: string,
  to: string,
  kind: CallSignalKind,
  extra: { pubKey?: string; candidates?: CallCandidate[]; stream?: CallStreamMeta } = {},
): Promise<void> {
  return invoke("send_call_signal", {
    callId,
    to,
    kind,
    pubKey: extra.pubKey,
    candidates: extra.candidates,
    stream: extra.stream,
  });
}

/// Stop our own screen share (renderer capture + native engine) if one is
/// running. Community ids are passed only when a channel session owns it.
async function stopOwnStream(): Promise<void> {
  const v = useVoiceStore.getState();
  if (!v.isStreaming) return;
  try {
    const { stopActiveStream } = await import("../voice/streaming/StreamCapture");
    await stopActiveStream();
  } catch {
    /* capture already gone */
  }
  await invoke("stop_screen_share", {
    serverId: v.connectedServerId ?? undefined,
    channelId: v.connectedChannelId ?? undefined,
  }).catch(() => {});
  useVoiceStore.getState().setIsStreaming(false);
}

/// Leave a community voice channel (and end its stream) so the native
/// engine is free for the call. No-op when not in a channel.
export async function leaveCommunityVoiceIfAny(): Promise<void> {
  const v = useVoiceStore.getState();
  if (!v.connectedChannelId) return;
  await stopOwnStream();
  await invoke("leave_voice_channel").catch(() => {});
  useVoiceStore.getState().disconnect();
}

/// Ring `peer`. Resolves once the INVITE is on its way (the answer arrives
/// through useCallEvents).
export async function startCall(peer: string): Promise<void> {
  const call = useCallStore.getState();
  if (call.status !== "idle") return;
  if (!call.callSignaling) {
    toast.warning("Calls unavailable", "The server you're signed into doesn't support calls yet.");
    return;
  }
  const me = useAuthStore.getState().username;
  if (!me || me === peer) return;

  const callId = crypto.randomUUID();
  call.startOutgoing(callId, peer);
  try {
    await leaveCommunityVoiceIfAny();
    flushSaveSettings();
    const prep = await invoke<CallPrepareResult>("call_prepare", { callId, peer });
    if (useCallStore.getState().callId !== callId) {
      // Cancelled while gathering candidates.
      await invoke("call_end").catch(() => {});
      return;
    }
    await sendSignal(callId, peer, "INVITE", {
      pubKey: prep.pubKey,
      candidates: prep.candidates,
    });
    startRing();
    armRingTimeout(() => {
      const c = useCallStore.getState();
      if (c.callId === callId && c.status === "outgoing") {
        void endCall("No answer");
      }
    });
  } catch (e) {
    toast.error("Couldn't start the call", String(e));
    await endCall(null, { notifyPeer: false });
  }
}

/// Answer the pending INVITE.
export async function acceptCall(): Promise<void> {
  const call = useCallStore.getState();
  const inc = call.incoming;
  if (!inc || call.status !== "incoming") return;
  stopRing();
  clearRingTimeout();
  call.setConnecting();

  // Land on the caller's DM so the call panel is where the user looks.
  useDmStore.getState().setActiveDmUser(inc.from);
  useUiStore.getState().setActiveView("dm");

  try {
    await leaveCommunityVoiceIfAny();
    flushSaveSettings();
    const prep = await invoke<CallPrepareResult>("call_prepare", {
      callId: inc.callId,
      peer: inc.from,
    });
    if (useCallStore.getState().callId !== inc.callId) {
      await invoke("call_end").catch(() => {});
      return;
    }
    await sendSignal(inc.callId, inc.from, "ACCEPT", {
      pubKey: prep.pubKey,
      candidates: prep.candidates,
    });
    await invoke("call_connect", {
      callId: inc.callId,
      peer: inc.from,
      remotePubKey: inc.pubKey,
      remoteCandidates: inc.candidates,
    });
  } catch (e) {
    toast.error("Couldn't connect the call", String(e));
    await endCall(null);
  }
}

/// Decline the pending INVITE.
export async function declineCall(): Promise<void> {
  const call = useCallStore.getState();
  if (call.status !== "incoming") return;
  await endCall(null);
}

/// The caller's side of an ACCEPT: start punching with the callee's key +
/// candidates. Called from useCallEvents.
export async function connectAfterAccept(
  callId: string,
  peer: string,
  pubKey: string,
  candidates: CallCandidate[],
): Promise<void> {
  const call = useCallStore.getState();
  if (call.status !== "outgoing" || call.callId !== callId) return;
  stopRing();
  clearRingTimeout();
  call.setConnecting();
  try {
    await invoke("call_connect", {
      callId,
      peer,
      remotePubKey: pubKey,
      remoteCandidates: candidates,
    });
  } catch (e) {
    toast.error("Couldn't connect the call", String(e));
    await endCall(null);
  }
}

/// Native reported the punch succeeded and the P2P engine is up.
export async function onCallConnected(callId: string, path: "host" | "srflx"): Promise<void> {
  const call = useCallStore.getState();
  if (call.callId !== callId || !call.peer) return;
  const me = useAuthStore.getState().username ?? "";
  call.setActive(path);
  const v = useVoiceStore.getState();
  v.setConnectedChannel(null, null);
  v.setCallPeer(call.peer);
  v.setParticipants(
    [me, call.peer].map((username) => ({
      username,
      isMuted: false,
      isDeafened: false,
      isSpeaking: false,
      audioLevel: 0,
    })),
  );
  playSound("connect");
  await applyPrefs();
  // Saved per-user volume / local mute for this peer (set from an earlier
  // call or a shared voice channel) — the community path replays these on
  // every presence update; a call has no presence, so do it here.
  applySavedUserGains([call.peer]);
}

async function applyPrefs(): Promise<void> {
  const { applyVoicePrefs } = await import("../voice/streaming/applyVoicePrefs");
  await applyVoicePrefs();
}

// ── In-call screen share ─────────────────────────────────────────
//
// There is no community server to announce a stream to during a call, so
// the streamer tells the peer directly over central (STREAM_START /
// STREAM_STOP) and both sides keep voiceStore.activeStreams in step —
// which is all the existing player stack (StreamPipManager /
// StreamViewPanel / MiniStreamPlayer) needs.

/// Our screen share just started inside the active call: tell the peer
/// and register our own entry so the self-preview tile + mini player work.
export function announceCallStreamStart(meta: CallStreamMeta): void {
  const call = useCallStore.getState();
  if (call.status !== "active" || !call.callId || !call.peer) return;
  const me = useAuthStore.getState().username ?? "";
  sendSignal(call.callId, call.peer, "STREAM_START", { stream: meta }).catch(() => {});
  const v = useVoiceStore.getState();
  const own: StreamInfo = {
    streamId: callStreamId(me),
    ownerUsername: me,
    hasAudio: meta.hasAudio,
    resolutionWidth: meta.width,
    resolutionHeight: meta.height,
    fps: meta.fps,
    currentCodec: meta.codec as VideoCodec,
    enforcedCodec: 0 as VideoCodec,
    watcherCount: 0,
  };
  v.setActiveStreams([...v.activeStreams.filter((s) => s.ownerUsername !== me), own]);
}

/// Our screen share ended inside the active call (user stopped it, or the
/// capture source went away).
export function announceCallStreamStop(): void {
  const call = useCallStore.getState();
  if (call.status !== "active" || !call.callId || !call.peer) return;
  const me = useAuthStore.getState().username ?? "";
  sendSignal(call.callId, call.peer, "STREAM_STOP").catch(() => {});
  const v = useVoiceStore.getState();
  if (v.activeStreams.some((s) => s.ownerUsername === me)) {
    v.setActiveStreams(v.activeStreams.filter((s) => s.ownerUsername !== me));
  }
  if (v.watchingStreams.includes(me)) v.removeWatching(me);
}

/// Open `owner`'s stream in the DM's full view. For the peer this gates
/// their frames through natively (call_watch_stream); our own stream is
/// the renderer-internal self-preview.
export async function watchCallStream(owner: string): Promise<void> {
  const me = useAuthStore.getState().username ?? "";
  const v = useVoiceStore.getState();
  if (owner !== me && !v.watchingStreams.includes(owner)) {
    await invoke("call_watch_stream", { watch: true }).catch(() => {});
  }
  v.addWatching(owner);
  v.setFullscreenStream(owner);
}

/// Leave `owner`'s stream (full view + mini). Peer → ungate natively.
export async function unwatchCallStream(owner: string): Promise<void> {
  const me = useAuthStore.getState().username ?? "";
  if (owner !== me) {
    await invoke("call_watch_stream", { watch: false }).catch(() => {});
  }
  useVoiceStore.getState().removeWatching(owner);
}

/// End whatever call state we're in — ringing out, ringing in, punching,
/// or active. Idempotent. `reason` shows briefly in the DM panel.
export async function endCall(
  reason: string | null = null,
  { notifyPeer = true }: { notifyPeer?: boolean } = {},
): Promise<void> {
  const call = useCallStore.getState();
  if (call.status === "idle") return;
  const { callId, peer, status } = call;
  stopRing();
  clearRingTimeout();

  const kind: CallSignalKind =
    status === "outgoing" ? "CANCEL" : status === "incoming" ? "REJECT" : "HANGUP";
  if (notifyPeer && callId && peer) {
    sendSignal(callId, peer, kind).catch(() => {});
  }
  call.reset(reason);

  const wasLive = status === "active" || status === "connecting";
  if (wasLive) {
    await stopOwnStream();
  }
  await invoke("call_end").catch(() => {});
  if (wasLive) {
    useVoiceStore.getState().disconnect();
    playSound("call_end");
  }
}
