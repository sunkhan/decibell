import { useState, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient } from "../../utils/colors";

export default function ServerBar() {
  const servers = useChatStore((s) => s.servers);
  const connectedServers = useChatStore((s) => s.connectedServers);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);
  const activeView = useUiStore((s) => s.activeView);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const openModal = useUiStore((s) => s.openModal);
  const sidebarCollapsed = useUiStore((s) => s.sidebarCollapsed);
  const toggleSidebar = useUiStore((s) => s.toggleSidebar);

  const [showMenu, setShowMenu] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  const connected = servers.filter((s) => connectedServers.has(s.id));

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setShowMenu(false);
      }
    };
    if (showMenu) document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [showMenu]);

  const handleServerClick = (serverId: string) => {
    const currentChannel = useChatStore.getState().activeChannelId;
    setActiveServer(serverId);
    setActiveView("server");
    const channels = useChatStore.getState().channelsByServer[serverId] ?? [];
    const currentInThisServer = channels.some((ch) => ch.id === currentChannel);
    if (!currentInThisServer) {
      setActiveChannel(null);
      const firstText = channels.find((ch) => ch.type === "text");
      if (firstText) {
        setActiveChannel(firstText.id);
        invoke("join_channel", { serverId, channelId: firstText.id }).catch(console.error);
      }
    }
  };

  const handleDisconnect = (e: React.MouseEvent, serverId: string) => {
    e.stopPropagation();
    invoke("disconnect_from_community", { serverId }).catch(console.error);
    useChatStore.getState().removeConnectedServer(serverId);
    if (activeServerId === serverId) {
      setActiveServer(null);
      setActiveChannel(null);
      setActiveView("home");
    }
  };

  return (
    <div className="relative flex h-[58px] shrink-0 items-center gap-2 border-b border-border bg-bg-primary px-3">
      {/* Home button */}
      <button
        onClick={() => { setActiveServer(null); setActiveChannel(null); setActiveView("home"); }}
        className={`flex h-[40px] w-[40px] shrink-0 items-center justify-center rounded-xl transition-all duration-200 ${
          activeView === "home"
            ? "bg-accent text-white shadow-[0_0_0_2px_var(--color-accent)]"
            : "bg-surface-active text-text-secondary hover:bg-accent hover:text-white hover:-translate-y-0.5"
        }`}
        title="Home"
      >
        <svg className="h-[18px] w-[18px]" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M3 12l9-9 9 9" />
          <path d="M9 21V12h6v9" />
        </svg>
      </button>

      <div className="mx-0.5 h-7 w-px shrink-0 bg-border-divider" />

      {/* Expand DM bar button (shown when collapsed) */}
      {sidebarCollapsed && (
        <button
          onClick={toggleSidebar}
          className="mr-1 flex h-10 w-10 shrink-0 items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-surface-hover hover:text-accent"
          title="Show DMs"
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <polyline points="9 6 15 12 9 18" />
          </svg>
        </button>
      )}

      {/* Server tabs */}
      {connected.map((server) => (
        <button
          key={server.id}
          onClick={() => handleServerClick(server.id)}
          className={`group relative flex h-[38px] shrink-0 items-center gap-2 rounded-lg px-3.5 text-[13px] font-semibold transition-all duration-200 ${
            activeServerId === server.id
              ? "bg-accent-mid text-accent-bright shadow-[0_2px_12px_rgba(56,139,253,0.10)]"
              : "text-text-secondary hover:bg-surface-hover hover:text-text-primary hover:-translate-y-px"
          }`}
        >
          {/* Active underline */}
          {activeServerId === server.id && (
            <div className="absolute -bottom-[9px] left-1/2 h-[3px] w-5 -translate-x-1/2 rounded-t bg-accent" />
          )}

          <div
            className="flex h-5 w-5 items-center justify-center rounded-[5px] text-[11px] font-extrabold text-white"
            style={{ background: stringToGradient(server.name) }}
          >
            {server.name.charAt(0).toUpperCase()}
          </div>
          <span className="max-w-[100px] truncate">{server.name}</span>

          {/* Disconnect X */}
          <button
            onClick={(e) => handleDisconnect(e, server.id)}
            className="absolute -right-1 -top-1 hidden h-4 w-4 items-center justify-center rounded-full bg-error text-[10px] text-white group-hover:flex"
            title="Disconnect"
          >
            ×
          </button>
        </button>
      ))}

      {/* Divider before add */}
      {connected.length > 0 && (
        <div className="mx-1 h-6 w-px shrink-0 bg-border-divider" />
      )}

      {/* Add server button with dropdown */}
      <div className="relative" ref={menuRef}>
        <button
          onClick={() => setShowMenu(!showMenu)}
          className={`flex h-[38px] w-[38px] shrink-0 items-center justify-center rounded-lg text-lg transition-all duration-200 ${
            activeView === "browse"
              ? "bg-success text-white"
              : "border-[1.5px] border-dashed border-text-muted text-text-muted hover:border-accent hover:bg-accent-soft hover:text-accent"
          }`}
          title="Add Server"
        >
          +
        </button>
        {showMenu && (
          <div className="absolute left-0 top-full z-50 mt-2 w-48 rounded-xl border border-border bg-bg-secondary p-1.5 shadow-2xl">
            <button
              onClick={() => { setShowMenu(false); setActiveView("browse"); }}
              className="flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-sm text-text-primary transition-colors hover:bg-surface-hover"
            >
              <span className="text-text-muted">☰</span>
              Browse Servers
            </button>
            <button
              onClick={() => { setShowMenu(false); openModal("direct-connect"); }}
              className="flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-sm text-text-primary transition-colors hover:bg-surface-hover"
            >
              <span className="text-text-muted">→</span>
              Direct Connect
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
