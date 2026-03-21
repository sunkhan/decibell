import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { useChannelEvents } from "./useChannelEvents";
import { stringToColor } from "../../utils/colors";
import { useVoiceStore } from "../../stores/voiceStore";
import VoiceControlBar from "../voice/VoiceControlBar";
import VoiceParticipantList from "../voice/VoiceParticipantList";

function UserPanel() {
  const username = useAuthStore((s) => s.username);
  const openModal = useUiStore((s) => s.openModal);

  if (!username) return null;

  return (
    <div className="flex items-center gap-2 border-t border-border px-3 py-2">
      <div className="relative flex-shrink-0">
        <div
          className="flex h-8 w-8 items-center justify-center rounded-xl text-xs font-bold text-white"
          style={{ backgroundColor: stringToColor(username) }}
        >
          {username.charAt(0).toUpperCase()}
        </div>
        <div className="absolute -bottom-0.5 -right-0.5 h-3 w-3 rounded-full border-2 border-bg-primary bg-success" />
      </div>
      <span className="min-w-0 flex-1 truncate text-sm font-medium text-text-primary">
        {username}
      </span>
      <button
        onClick={() => openModal("settings")}
        className="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-white/10 hover:text-text-primary"
        title="Settings"
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
        </svg>
      </button>
    </div>
  );
}

export default function ChannelSidebar() {
  useChannelEvents();

  const activeView = useUiStore((s) => s.activeView);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const servers = useChatStore((s) => s.servers);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const setActiveView = useUiStore((s) => s.setActiveView);

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

  const handleVoiceChannelClick = (channelId: string) => {
    if (!activeServerId) return;
    if (channelId === connectedChannelId) {
      // Already connected — switch to voice view
      setActiveView("voice");
      return;
    }
    // Join new voice channel
    useVoiceStore.getState().setConnectedChannel(activeServerId, channelId);
    invoke("join_voice_channel", {
      serverId: activeServerId,
      channelId,
    }).catch((err) => {
      console.error(err);
      useVoiceStore.getState().disconnect();
    });
    setActiveView("voice");
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
        <UserPanel />
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
              <div key={ch.id}>
                <button
                  onClick={() => handleVoiceChannelClick(ch.id)}
                  className={`flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-sm transition-colors ${
                    connectedChannelId === ch.id
                      ? "bg-white/10 text-accent"
                      : "text-text-muted hover:bg-white/5 hover:text-text-primary"
                  }`}
                >
                  <span>🔊</span>
                  <span className="truncate">{ch.name}</span>
                </button>
                {connectedChannelId === ch.id && <VoiceParticipantList />}
              </div>
            ))}
          </div>
        )}

        {channels.length === 0 && (
          <p className="px-2 text-xs text-text-muted">No channels</p>
        )}
      </div>
      <VoiceControlBar />
      <UserPanel />
    </div>
  );
}
