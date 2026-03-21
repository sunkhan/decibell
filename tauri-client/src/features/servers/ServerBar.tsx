import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

export default function ServerBar() {
  const servers = useChatStore((s) => s.servers);
  const connectedServers = useChatStore((s) => s.connectedServers);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const openModal = useUiStore((s) => s.openModal);

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
      <button
        onClick={() => openModal("server-discovery")}
        className="flex h-8 w-8 items-center justify-center rounded-lg bg-bg-tertiary text-lg text-success transition-colors hover:bg-success hover:text-white"
        title="Add Server"
      >
        +
      </button>
    </div>
  );
}
