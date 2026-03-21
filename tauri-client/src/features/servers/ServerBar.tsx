import { useState, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

export default function ServerBar() {
  const servers = useChatStore((s) => s.servers);
  const connectedServers = useChatStore((s) => s.connectedServers);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);
  const activeView = useUiStore((s) => s.activeView);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const openModal = useUiStore((s) => s.openModal);

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
    <div className="flex h-16 items-center gap-2 border-b border-border bg-bg-primary px-4">
      {connected.map((server) => (
        <button
          key={server.id}
          onClick={() => handleServerClick(server.id)}
          className={`group relative flex items-center gap-2 rounded-lg px-3 py-1.5 text-sm transition-colors ${
            activeServerId === server.id ? "bg-accent/20 text-accent" : "text-text-primary hover:bg-white/5"
          }`}
        >
          <div className="flex h-7 w-7 items-center justify-center rounded-lg bg-bg-tertiary text-xs font-bold">
            {server.name.charAt(0).toUpperCase()}
          </div>
          <span className="max-w-[100px] truncate">{server.name}</span>
          <button
            onClick={(e) => handleDisconnect(e, server.id)}
            className="absolute -right-1 -top-1 hidden h-4 w-4 items-center justify-center rounded-full bg-error text-[10px] text-white group-hover:flex"
            title="Disconnect"
          >
            ×
          </button>
        </button>
      ))}

      {/* Add server button with dropdown */}
      <div className="relative" ref={menuRef}>
        <button
          onClick={() => setShowMenu(!showMenu)}
          className={`flex h-8 w-8 items-center justify-center rounded-lg text-lg transition-colors ${
            activeView === "browse"
              ? "bg-success text-white"
              : "bg-bg-tertiary text-success hover:bg-success hover:text-white"
          }`}
          title="Add Server"
        >
          +
        </button>
        {showMenu && (
          <div className="absolute left-0 top-full z-50 mt-1 w-44 rounded-lg border border-border bg-bg-secondary py-1 shadow-xl">
            <button
              onClick={() => {
                setShowMenu(false);
                setActiveView("browse");
              }}
              className="flex w-full items-center gap-2 px-3 py-2 text-sm text-text-primary transition-colors hover:bg-white/5"
            >
              <span className="text-text-muted">&#9776;</span>
              Browse Servers
            </button>
            <button
              onClick={() => {
                setShowMenu(false);
                openModal("direct-connect");
              }}
              className="flex w-full items-center gap-2 px-3 py-2 text-sm text-text-primary transition-colors hover:bg-white/5"
            >
              <span className="text-text-muted">&#8594;</span>
              Direct Connect
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
