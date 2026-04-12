import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useUiStore } from "../stores/uiStore";
import { useChatStore } from "../stores/chatStore";
import { useDmStore } from "../stores/dmStore";

/// Keeps the native OS window title in sync with the active view.
export function useWindowTitle() {
  const activeView = useUiStore((s) => s.activeView);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const servers = useChatStore((s) => s.servers);
  const activeDmUser = useDmStore((s) => s.activeDmUser);

  useEffect(() => {
    let title = "Decibell";
    if (activeView === "dm" && activeDmUser) {
      title = `${activeDmUser} — Decibell`;
    } else if ((activeView === "server" || activeView === "voice") && activeServerId) {
      const server = servers.find((s) => s.id === activeServerId);
      if (server) title = `${server.name} — Decibell`;
    }
    getCurrentWindow().setTitle(title).catch(() => {});
  }, [activeView, activeServerId, servers, activeDmUser]);
}
