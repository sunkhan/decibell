import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useChannelEvents } from "./useChannelEvents";

export default function ChannelSidebar() {
  useChannelEvents();

  const activeView = useUiStore((s) => s.activeView);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const servers = useChatStore((s) => s.servers);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);

  const channels = activeServerId
    ? channelsByServer[activeServerId] ?? []
    : [];
  const textChannels = channels.filter((ch) => ch.type === "text");
  const voiceChannels = channels.filter((ch) => ch.type === "voice");
  const serverName = servers.find((s) => s.id === activeServerId)?.name;

  const handleChannelClick = (channelId: string) => {
    if (!activeServerId || channelId === activeChannelId) return;
    setActiveChannel(channelId);
    invoke("join_channel", {
      serverId: activeServerId,
      channelId,
    }).catch(console.error);
  };

  if (activeView === "home") {
    return (
      <div className="flex w-60 flex-shrink-0 flex-col border-r border-border bg-bg-primary">
        <div className="border-b border-border px-4 py-3">
          <h2 className="text-sm font-semibold text-text-primary">
            Direct Messages
          </h2>
        </div>
        <div className="flex flex-1 items-center justify-center">
          <p className="text-xs text-text-muted">Coming soon...</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex w-60 flex-shrink-0 flex-col border-r border-border bg-bg-primary">
      {/* Server name header */}
      <div className="border-b border-border px-4 py-3">
        <h2 className="truncate text-sm font-semibold text-text-primary">
          {serverName ?? "Server"}
        </h2>
      </div>

      {/* Channel list */}
      <div className="flex-1 overflow-y-auto px-2 py-3">
        {/* Text channels */}
        {textChannels.length > 0 && (
          <div className="mb-4">
            <h3 className="mb-1 px-2 text-[11px] font-semibold uppercase tracking-wide text-text-muted">
              Text Channels
            </h3>
            {textChannels.map((ch) => (
              <button
                key={ch.id}
                onClick={() => handleChannelClick(ch.id)}
                className={`flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-sm transition-colors ${
                  activeChannelId === ch.id
                    ? "bg-white/10 text-accent"
                    : "text-text-muted hover:bg-white/5 hover:text-text-primary"
                }`}
              >
                <span className="text-text-muted">#</span>
                <span className="truncate">{ch.name}</span>
              </button>
            ))}
          </div>
        )}

        {/* Voice channels */}
        {voiceChannels.length > 0 && (
          <div>
            <h3 className="mb-1 px-2 text-[11px] font-semibold uppercase tracking-wide text-text-muted">
              Voice Channels
            </h3>
            {voiceChannels.map((ch) => (
              <div
                key={ch.id}
                className="flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-sm text-text-muted"
                title="Voice channels coming soon"
              >
                <span>🔊</span>
                <span className="truncate">{ch.name}</span>
              </div>
            ))}
          </div>
        )}

        {channels.length === 0 && (
          <p className="px-2 text-xs text-text-muted">No channels</p>
        )}
      </div>
    </div>
  );
}
