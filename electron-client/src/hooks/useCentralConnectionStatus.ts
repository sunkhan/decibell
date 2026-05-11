import { useEffect } from "react";
import { listen } from "../lib/ipc";
import { useUiStore } from "../stores/uiStore";

// Drives the "Connection lost. Reconnecting..." banner in MainLayout.
// The native side emits connection_lost / connection_restored for both
// the central server and per-community connections; the community half
// is handled in features/servers/useServerEvents.ts (which flips the
// per-server connectedServers set). This hook is the central half:
// flip uiStore.connectionStatus so the cross-shell banner appears.
//
// Logout cleanup and the community-side flip live elsewhere on
// purpose — keeping each event consumer next to the store it owns
// avoids a single fan-out hook that has to import every store.
export function useCentralConnectionStatus() {
  useEffect(() => {
    const unlistenLost = listen<{ serverType: string; serverId?: string }>(
      "connection_lost",
      (event) => {
        if (event.payload.serverType === "central") {
          useUiStore.getState().setConnectionStatus("reconnecting");
        }
      },
    );
    const unlistenRestored = listen<{ serverType: string; serverId?: string }>(
      "connection_restored",
      (event) => {
        if (event.payload.serverType === "central") {
          useUiStore.getState().setConnectionStatus("connected");
        }
      },
    );

    return () => {
      unlistenLost.then((fn) => fn());
      unlistenRestored.then((fn) => fn());
    };
  }, []);
}
