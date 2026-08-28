import { useEffect } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useCallStore } from "../../stores/callStore";
import type { CallConfig, CallSignalPayload } from "../../types";

// Central-side plumbing for P2P DM calls. Mounted once in MainLayout.
//
// M1 scope: learn whether the central we're on relays CALL_SIGNAL (and
// its STUN list) right after login, and surface incoming signals. The
// call state machine (ringing, accept, punch, hang-up) lands with the
// native transport in the follow-up milestones.
export function useCallEvents() {
  useEffect(() => {
    let disposed = false;

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

    const unlistenSignal = listen<CallSignalPayload>("call_signal", (event) => {
      const sig = event.payload;
      console.debug("[call] signal", sig.kind, "from", sig.from, "call", sig.callId);
    });

    return () => {
      disposed = true;
      unlistenLogin.then((fn) => fn());
      unlistenSignal.then((fn) => fn());
    };
  }, []);
}
