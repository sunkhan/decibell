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

  const connected = servers.filter((s) => connectedServers.has(s.id));

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
    <div className="relative z-10 flex h-[58px] shrink-0 items-center bg-bg-darkest">
      {/* Bottom separator starts after the home-button column. */}
      <div className="pointer-events-none absolute bottom-0 left-[68px] right-0 border-b border-border" />
      {/* Home button — width matches DM sidebar */}
      <div className="flex w-[68px] shrink-0 items-center justify-center">
        <button
          onClick={() => { setActiveServer(null); setActiveChannel(null); setActiveView("home"); }}
          className={`flex h-[40px] w-[40px] shrink-0 items-center justify-center rounded-xl transition-all duration-200 ${
            activeView === "home"
              ? "bg-accent text-white shadow-[0_0_0_2px_var(--color-accent)]"
              : "bg-surface-active text-text-secondary hover:bg-accent hover:text-white hover:-translate-y-0.5"
          }`}
          title="Home"
        >
          <svg className="h-[20px] w-[20px]" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 3l-9.5 8.5c-.3.27-.15.5.25.5H5v8a1 1 0 001 1h4v-5.5a1 1 0 011-1h2a1 1 0 011 1V21h4a1 1 0 001-1v-8h2.25c.4 0 .55-.23.25-.5L12 3z" />
          </svg>
        </button>
      </div>

      <div className="h-7 w-px shrink-0 bg-border-divider" />

      {/* Server tabs */}
      <div className="flex flex-1 items-center gap-2 px-2">
      {connected.map((server) => (
        <button
          key={server.id}
          onClick={() => handleServerClick(server.id)}
          className={`group relative flex h-[38px] shrink-0 items-center gap-2 rounded-lg px-3.5 text-[13px] font-semibold transition-all duration-200 ${
            activeServerId === server.id
              ? "bg-accent-mid text-accent-bright shadow-[0_2px_12px_rgba(56,143,255,0.10)]"
              : "text-text-secondary hover:bg-surface-hover hover:text-text-primary hover:-translate-y-px"
          }`}
        >
          {/* Active underline */}
          {activeServerId === server.id && (
            <div className="absolute -bottom-[9px] left-1/2 h-[3px] w-5 -translate-x-1/2 rounded-t bg-accent" />
          )}

          <div
            className="flex h-5 w-5 items-center justify-center rounded-[5px] text-[11px] font-semibold text-white"
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

      {/* Add server button — direct route to the browse view. */}
      <button
        onClick={() => setActiveView("browse")}
        className={`flex h-[38px] w-[38px] shrink-0 items-center justify-center rounded-lg text-lg transition-all duration-200 ${
          activeView === "browse"
            ? "bg-success text-white"
            : "border-[1.5px] border-dashed border-text-muted text-text-muted hover:border-accent hover:bg-accent-soft hover:text-accent"
        }`}
        title="Browse servers"
      >
        <svg className="h-[18px] w-[18px]" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
          <line x1="12" y1="5" x2="12" y2="19" />
          <line x1="5" y1="12" x2="19" y2="12" />
        </svg>
      </button>
      </div>
    </div>
  );
}
