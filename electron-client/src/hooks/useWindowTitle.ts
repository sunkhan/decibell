import { useEffect } from "react";
import { useUiStore } from "../stores/uiStore";
import { useChatStore } from "../stores/chatStore";
import { useDmStore } from "../stores/dmStore";
import { useUserName } from "../stores/usersStore";

// Mirrors the active view into the OS window title:
//   "Decibell"                         — home / browse / no context
//   "<server name> — Decibell"         — server / voice view
//   "<their current name> — Decibell"  — DM view with an active thread
//
// Goes through window.decibell.window.setTitle which resolves to
// BrowserWindow.setTitle in main. setTitle() is fire-and-forget; we
// swallow the (very unlikely) reject so a transient IPC hiccup can't
// crash the renderer.
export function useWindowTitle() {
  const activeView = useUiStore((s) => s.activeView);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const servers = useChatStore((s) => s.servers);
  const activeDmUser = useDmStore((s) => s.activeDmUser);
  const dmPayloadName = useDmStore((s) => (activeDmUser ? s.conversations[activeDmUser]?.username : undefined));
  const dmName = useUserName(activeDmUser, dmPayloadName ?? "");

  useEffect(() => {
    let title = "Decibell";
    if (activeView === "dm" && activeDmUser && dmName) {
      title = `${dmName} — Decibell`;
    } else if ((activeView === "server" || activeView === "voice") && activeServerId) {
      const server = servers.find((s) => s.id === activeServerId);
      if (server) title = `${server.name} — Decibell`;
    }
    window.decibell.window.setTitle(title).catch(() => {});
  }, [activeView, activeServerId, servers, activeDmUser, dmName]);
}
