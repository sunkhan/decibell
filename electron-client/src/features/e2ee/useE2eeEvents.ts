import { useEffect } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useE2eeStore } from "../../stores/e2eeStore";
import { useDmStore } from "../../stores/dmStore";
import { toast } from "../../stores/toastStore";
import type { E2eePeerChangedPayload, E2eeStatusPayload } from "../../types";

// Native decides the E2EE status for this account on this device (after
// every LoginRes, and on setup / unlock / reset / a remote rotation of our
// own keys) and pushes it as `e2ee_status_changed`. This hook mirrors it
// into e2eeStore and drives the two side effects that need the renderer:
//
//   - the first "locked" report of a login opens the unlock prompt once;
//   - becoming "ready" reloads DM history, because every sealed row that
//     arrived before the keys were available holds a placeholder.

function applyStatus(p: E2eeStatusPayload) {
  const st = useE2eeStore.getState();
  const first = !st.reported;
  const wasReady = st.status === "ready";
  st.setStatus(p);
  if (p.status === "locked" && first) {
    useE2eeStore.getState().openPassphraseModal("unlock");
  }
  if (p.status === "ready" && !wasReady) {
    reloadDms();
  }
}

/// Drop every loaded DM page and pull fresh copies: the sidebar previews
/// (conversations) now, and the active conversation's latest page so the
/// open panel doesn't sit empty. Other conversations reload on open.
function reloadDms() {
  const dm = useDmStore.getState();
  dm.invalidateAllHistory();
  invoke("request_dm_conversations").catch(() => {});
  const peer = dm.activeDmUser;
  if (peer) {
    invoke("request_dm_history", { peer, beforeId: 0, limit: 50 }).catch(() => {});
  }
}

export function useE2eeEvents() {
  useEffect(() => {
    // A renderer reload after login misses the push — pull once.
    invoke<E2eeStatusPayload>("e2ee_get_status")
      .then((p) => {
        if (p.supported || p.status !== "unavailable") applyStatus(p);
      })
      .catch(() => {});

    const unStatus = listen<E2eeStatusPayload>("e2ee_status_changed", (event) => {
      applyStatus(event.payload);
    });

    const unPeer = listen<E2eePeerChangedPayload>("e2ee_peer_changed", (event) => {
      const { username } = event.payload;
      useE2eeStore.getState().markPeerChanged(username, Math.floor(Date.now() / 1000));
      toast.warning(
        "Encryption keys changed",
        `${username}'s encryption keys changed. If that's unexpected, compare safety numbers in their profile.`,
      );
    });

    return () => {
      unStatus.then((fn) => fn());
      unPeer.then((fn) => fn());
    };
  }, []);
}
