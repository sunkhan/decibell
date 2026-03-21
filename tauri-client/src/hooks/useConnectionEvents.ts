import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useUiStore } from "../stores/uiStore";
import { useChatStore } from "../stores/chatStore";
import { useAuthStore } from "../stores/authStore";
import { useNavigate } from "react-router-dom";

export function useConnectionEvents() {
  const navigate = useNavigate();

  useEffect(() => {
    const unlistenLost = listen<{ serverType: string; serverId?: string }>(
      "connection_lost",
      (event) => {
        const { serverType, serverId } = event.payload;
        if (serverType === "central") {
          useUiStore.getState().setConnectionStatus("reconnecting");
        } else if (serverType === "community" && serverId) {
          useChatStore.getState().removeConnectedServer(serverId);
        }
      }
    );

    const unlistenRestored = listen<{ serverType: string; serverId?: string }>(
      "connection_restored",
      (event) => {
        const { serverType, serverId } = event.payload;
        if (serverType === "central") {
          useUiStore.getState().setConnectionStatus("connected");
        } else if (serverType === "community" && serverId) {
          useChatStore.getState().addConnectedServer(serverId);
        }
      }
    );

    const unlistenLogout = listen("logged_out", () => {
      useAuthStore.getState().logout();
      navigate("/login");
    });

    return () => {
      unlistenLost.then((fn) => fn());
      unlistenRestored.then((fn) => fn());
      unlistenLogout.then((fn) => fn());
    };
  }, [navigate]);
}
