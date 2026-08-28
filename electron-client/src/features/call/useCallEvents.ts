import { useEffect } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useCallStore } from "../../stores/callStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { toast } from "../../stores/toastStore";
import { playSound } from "../../utils/sounds";
import type {
  CallConfig,
  CallConnectedPayload,
  CallDroppedPayload,
  CallFailedPayload,
  CallSignalPayload,
  StreamInfo,
  VideoCodec,
} from "../../types";
import {
  RING_TIMEOUT_MS,
  acceptCall,
  connectAfterAccept,
  endCall,
  onCallConnected,
  sendSignal,
  startRing,
  stopRing,
} from "./callActions";

/// Stream id used for the peer's in-call screen share in
/// voiceStore.activeStreams (community streams use the server's ids).
export const callStreamId = (peer: string) => `call:${peer}`;

// Central + native event surface for P2P DM calls. Mounted once in
// MainLayout. Drives callStore through the signals central relays
// (CallSignal) and the three native outcomes (call_connected /
// call_failed / call_dropped). The imperative transitions live in
// callActions so the UI and this listener share one implementation.
//
// Effect runs once (`[]` deps); handlers read stores via getState().
export function useCallEvents() {
  useEffect(() => {
    let disposed = false;
    let incomingTimer: ReturnType<typeof setTimeout> | null = null;
    const clearIncomingTimer = () => {
      if (incomingTimer) clearTimeout(incomingTimer);
      incomingTimer = null;
    };

    const refreshConfig = () => {
      invoke<CallConfig>("get_call_config")
        .then((cfg) => {
          if (disposed) return;
          useCallStore.getState().setCallConfig({
            callSignaling: cfg.callSignaling,
            stunServers: cfg.stunServers ?? [],
          });
        })
        .catch(() => {
          // Older native build without the command — leave calls gated off.
        });
    };
    // The native LoginRes handler stores the config before it emits
    // login_succeeded, so reading it here is race-free. Also refresh on
    // mount for the auto-login path where login_succeeded fired before
    // MainLayout mounted.
    refreshConfig();
    const unlistenLogin = listen("login_succeeded", refreshConfig);

    const onSignal = (sig: CallSignalPayload) => {
      const me = useAuthStore.getState().username ?? "";
      const call = useCallStore.getState();
      const mine = call.callId === sig.callId;

      switch (sig.kind) {
        case "INVITE": {
          if (!sig.pubKey) return;
          // Glare: both rang each other at once. The lexicographically
          // lower username keeps its own INVITE (the other side answers
          // it); the higher one drops its own and answers theirs.
          if (call.status === "outgoing" && call.peer === sig.from) {
            if (me < sig.from) return;
            if (call.callId && call.peer) {
              sendSignal(call.callId, call.peer, "CANCEL").catch(() => {});
            }
            stopRing();
            invoke("call_end").catch(() => {});
            call.reset(null);
            useCallStore.getState().setIncoming({
              callId: sig.callId,
              from: sig.from,
              pubKey: sig.pubKey,
              candidates: sig.candidates,
            });
            void acceptCall();
            return;
          }
          if (call.status !== "idle") {
            sendSignal(sig.callId, sig.from, "BUSY").catch(() => {});
            return;
          }
          call.setIncoming({
            callId: sig.callId,
            from: sig.from,
            pubKey: sig.pubKey,
            candidates: sig.candidates,
          });
          sendSignal(sig.callId, sig.from, "RINGING").catch(() => {});
          startRing();
          window.decibell.window.flash().catch(() => {});
          clearIncomingTimer();
          incomingTimer = setTimeout(() => {
            const c = useCallStore.getState();
            if (c.status === "incoming" && c.callId === sig.callId) {
              stopRing();
              c.reset("Missed call");
              toast.info("Missed call", `${sig.from} tried to call you.`);
            }
          }, RING_TIMEOUT_MS);
          return;
        }
        case "RINGING":
          if (mine && call.status === "outgoing") call.setRingingAcked();
          return;
        case "ACCEPT":
          if (mine && call.status === "outgoing" && sig.pubKey) {
            void connectAfterAccept(sig.callId, sig.from, sig.pubKey, sig.candidates);
          }
          return;
        case "REJECT":
          if (mine && call.status === "outgoing") {
            void endCall("Declined", { notifyPeer: false });
          }
          return;
        case "BUSY":
          if (mine && call.status === "outgoing") {
            toast.info("Busy", `${sig.from} is in another call.`);
            void endCall("Busy", { notifyPeer: false });
          }
          return;
        case "CANCEL":
          if (mine && call.status === "incoming") {
            clearIncomingTimer();
            stopRing();
            call.reset("Missed call");
            toast.info("Missed call", `${sig.from} tried to call you.`);
          }
          return;
        case "HANGUP":
          if (mine) {
            void endCall("Call ended", { notifyPeer: false });
          }
          return;
        case "PEER_OFFLINE":
          if (mine) {
            toast.info("Not available", `${sig.from} is offline.`);
            void endCall("Offline", { notifyPeer: false });
          }
          return;
        case "NOT_ALLOWED":
          if (mine) {
            toast.warning("Can't call", `${sig.from} only takes calls from friends.`);
            void endCall("Not allowed", { notifyPeer: false });
          }
          return;
        case "STREAM_START": {
          if (!mine || call.status !== "active" || !sig.stream) return;
          const v = useVoiceStore.getState();
          const info: StreamInfo = {
            streamId: callStreamId(sig.from),
            ownerUsername: sig.from,
            hasAudio: sig.stream.hasAudio,
            resolutionWidth: sig.stream.width,
            resolutionHeight: sig.stream.height,
            fps: sig.stream.fps,
            currentCodec: sig.stream.codec as VideoCodec,
            enforcedCodec: 0 as VideoCodec,
            watcherCount: 0,
          };
          const others = v.activeStreams.filter((s) => s.ownerUsername !== sig.from);
          v.setActiveStreams([...others, info]);
          playSound("stream_start");
          return;
        }
        case "STREAM_STOP": {
          if (!mine) return;
          const v = useVoiceStore.getState();
          if (v.activeStreams.some((s) => s.ownerUsername === sig.from)) {
            v.setActiveStreams(v.activeStreams.filter((s) => s.ownerUsername !== sig.from));
            if (v.watchingStreams.includes(sig.from)) {
              invoke("call_watch_stream", { watch: false }).catch(() => {});
              v.removeWatching(sig.from);
            }
            playSound("stream_stop");
          }
          return;
        }
        default:
          return;
      }
    };

    const unlistenSignal = listen<CallSignalPayload>("call_signal", (e) => onSignal(e.payload));

    const unlistenConnected = listen<CallConnectedPayload>("call_connected", (e) => {
      void onCallConnected(e.payload.callId, e.payload.path);
    });

    const unlistenFailed = listen<CallFailedPayload>("call_failed", (e) => {
      const call = useCallStore.getState();
      if (call.callId !== e.payload.callId) return;
      const body =
        e.payload.reason === "no_path"
          ? "One of your networks blocks direct connections (symmetric NAT or a strict firewall). Relay support is planned."
          : e.payload.detail || e.payload.reason;
      toast.error("Couldn't connect", body);
      void endCall("Couldn't connect");
    });

    const unlistenDropped = listen<CallDroppedPayload>("call_dropped", (e) => {
      const call = useCallStore.getState();
      if (call.callId !== e.payload.callId) return;
      toast.warning("Call ended", "Lost the connection to the other side.");
      void endCall("Connection lost");
    });

    return () => {
      disposed = true;
      clearIncomingTimer();
      for (const p of [unlistenLogin, unlistenSignal, unlistenConnected, unlistenFailed, unlistenDropped]) {
        p.then((fn) => fn());
      }
    };
  }, []);
}
