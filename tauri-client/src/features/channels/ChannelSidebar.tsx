import { useState, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useChannelEvents } from "./useChannelEvents";
import { stringToGradient } from "../../utils/colors";
import { useVoiceStore } from "../../stores/voiceStore";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import VoiceParticipantList from "../voice/VoiceParticipantList";

function formatRelativeTime(epochMs: number): string {
  const diff = Date.now() - epochMs;
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d`;
  return `${Math.floor(days / 30)}mo`;
}

export default function ChannelSidebar() {
  useChannelEvents();

  const [sidebarWidth, setSidebarWidth] = useState(240);
  const isResizing = useRef(false);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    isResizing.current = true;
    const startX = e.clientX;
    const startWidth = sidebarWidth;

    const handleMouseMove = (e: MouseEvent) => {
      const newWidth = Math.min(480, Math.max(180, startWidth + (e.clientX - startX)));
      setSidebarWidth(newWidth);
    };

    const handleMouseUp = () => {
      isResizing.current = false;
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };

    document.addEventListener("mousemove", handleMouseMove);
    document.addEventListener("mouseup", handleMouseUp);
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }, [sidebarWidth]);

  const activeView = useUiStore((s) => s.activeView);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const servers = useChatStore((s) => s.servers);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const channelPresence = useVoiceStore((s) => s.channelPresence);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const conversations = useDmStore((s) => s.conversations);
  const activeDmUser = useDmStore((s) => s.activeDmUser);
  const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
  const friends = useFriendsStore((s) => s.friends);
  const onlineUsers = useChatStore((s) => s.onlineUsers);
  const [textCollapsed, setTextCollapsed] = useState(false);
  const [voiceCollapsed, setVoiceCollapsed] = useState(false);

  const sortedConversations = Object.values(conversations).sort(
    (a, b) => b.lastMessageTime - a.lastMessageTime
  );

  const handleDmConversationClick = (username: string) => {
    setActiveDmUser(username);
    setActiveView("dm");
  };

  const channels = activeServerId
    ? channelsByServer[activeServerId] ?? []
    : [];
  const textChannels = channels.filter((ch) => ch.type === "text");
  const voiceChannels = channels.filter((ch) => ch.type === "voice");
  const serverName = servers.find((s) => s.id === activeServerId)?.name;

  const handleChannelClick = (channelId: string) => {
    if (!activeServerId) return;
    if (channelId !== activeChannelId) {
      setActiveChannel(channelId);
      invoke("join_channel", {
        serverId: activeServerId,
        channelId,
      }).catch(console.error);
    }
    if (activeView !== "server") {
      setActiveView("server");
    }
  };

  const handleVoiceChannelClick = (channelId: string) => {
    if (!activeServerId) return;
    if (channelId === connectedChannelId) {
      setActiveView("voice");
      return;
    }
    useVoiceStore.getState().setConnectedChannel(activeServerId, channelId);
    invoke("join_voice_channel", {
      serverId: activeServerId,
      channelId,
    }).then(() => {
      // Apply saved audio device settings to the new pipeline
      const { inputDevice, outputDevice, separateStreamOutput, streamOutputDevice, voiceThresholdDb, aecEnabled, noiseSuppressionLevel, agcEnabled } = useUiStore.getState();
      // Apply voice threshold (always, since default is -50 and user may have changed it)
      invoke("set_voice_threshold", { thresholdDb: voiceThresholdDb <= -60 ? -96 : voiceThresholdDb }).catch(console.error);
      if (inputDevice) {
        invoke("set_input_device", { name: inputDevice }).catch(console.error);
      }
      if (outputDevice) {
        invoke("set_output_device", { name: outputDevice }).catch(console.error);
      }
      if (separateStreamOutput) {
        invoke("set_separate_stream_output", {
          enabled: true,
          device: streamOutputDevice,
        }).catch(console.error);
      }
      // Apply voice processing settings
      if (aecEnabled) invoke("set_aec_enabled", { enabled: true }).catch(console.error);
      if (noiseSuppressionLevel > 0) invoke("set_noise_suppression_level", { level: noiseSuppressionLevel }).catch(console.error);
      if (agcEnabled) invoke("set_agc_enabled", { enabled: true }).catch(console.error);
    }).catch((err) => {
      console.error(err);
      useVoiceStore.getState().disconnect();
    });
    setActiveView("voice");
  };

  if (activeView === "home" || activeView === "dm") {
    return (
      <div className="relative flex shrink-0 flex-col border-r border-border bg-bg-dmbar pb-14" style={{ width: sidebarWidth }}>
        <div className="flex h-12 shrink-0 items-center border-b border-border px-4">
          <h2 className="text-[15px] font-extrabold text-text-bright">
            Direct Messages
          </h2>
        </div>
        <div className="flex-1 overflow-y-auto px-2 py-2.5">
          {sortedConversations.length === 0 ? (
            <div className="flex flex-1 items-center justify-center pt-8">
              <p className="text-xs text-text-muted">No conversations yet</p>
            </div>
          ) : (
            sortedConversations.map((conv) => {
              const isOnline =
                friends.some((f) => f.username === conv.username && f.status === "online") ||
                onlineUsers.includes(conv.username);
              const lastMsg = conv.messages[conv.messages.length - 1];
              const isActive = activeDmUser === conv.username;
              return (
                <button
                  key={conv.username}
                  onClick={() => handleDmConversationClick(conv.username)}
                  className={`flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 transition-colors ${
                    isActive
                      ? "bg-accent-soft text-text-bright"
                      : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
                  }`}
                >
                  <div className="relative shrink-0">
                    <div
                      className="flex h-[34px] w-[34px] items-center justify-center rounded-lg text-sm font-bold text-white"
                      style={{ background: stringToGradient(conv.username) }}
                    >
                      {conv.username.charAt(0).toUpperCase()}
                    </div>
                    <div
                      className={`absolute -bottom-px -right-px h-2.5 w-2.5 rounded-full border-2 border-bg-dmbar ${
                        isOnline ? "bg-success" : "bg-text-muted"
                      }`}
                    />
                  </div>
                  <div className="min-w-0 flex-1 text-left">
                    <div className="truncate text-[13px] font-bold">
                      {conv.username}
                    </div>
                    {lastMsg && (
                      <div className="truncate text-[11px] text-text-muted">
                        {lastMsg.content}
                      </div>
                    )}
                  </div>
                  {conv.lastMessageTime > 0 && (
                    <span className="shrink-0 text-[10px] text-text-muted">
                      {formatRelativeTime(conv.lastMessageTime)}
                    </span>
                  )}
                </button>
              );
            })
          )}
        </div>
        {/* Resize handle */}
        <div
          onMouseDown={handleMouseDown}
          className="absolute right-0 top-0 z-10 h-full w-1 cursor-col-resize hover:bg-accent/40 active:bg-accent/60"
        />
      </div>
    );
  }

  return (
    <div className="relative flex shrink-0 flex-col border-r border-border bg-bg-dmbar pb-14" style={{ width: sidebarWidth }}>
      {/* Server name header */}
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-border px-4">
        <h2 className="flex-1 truncate text-[15px] font-extrabold text-text-bright">
          {serverName ?? "Server"}
        </h2>
        {servers.some((s) => s.id === activeServerId) ? (
          <span className="rounded bg-success/15 px-1.5 py-0.5 text-[9px] font-bold uppercase tracking-wider text-success">
            Public
          </span>
        ) : (
          <span className="rounded bg-text-muted/15 px-1.5 py-0.5 text-[9px] font-bold uppercase tracking-wider text-text-secondary">
            Private
          </span>
        )}
      </div>

      {/* Channel list */}
      <div className="flex-1 overflow-y-auto px-2 py-2.5">
        {/* Text channels */}
        {textChannels.length > 0 && (
          <div className="mb-4">
            <div
              className="mb-1 flex cursor-pointer select-none items-center gap-1 px-2"
              onClick={() => setTextCollapsed(!textCollapsed)}
            >
              <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" className={`text-text-muted ${textCollapsed ? "-rotate-90" : ""}`}>
                <polyline points="6 9 12 15 18 9" />
              </svg>
              <h3 className="text-[11px] font-bold uppercase tracking-[0.08em] text-text-muted">
                Text Channels
              </h3>
            </div>
            {!textCollapsed && textChannels.map((ch) => (
              <button
                key={ch.id}
                onClick={() => handleChannelClick(ch.id)}
                className={`flex w-full items-center gap-2 rounded-md px-2.5 py-[7px] text-sm transition-colors ${
                  activeChannelId === ch.id && activeView === "server"
                    ? "bg-accent-soft text-text-bright font-semibold"
                    : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
                }`}
              >
                <span className={`font-mono text-[17px] font-semibold ${activeChannelId === ch.id && activeView === "server" ? "text-accent" : "text-text-muted"}`}>
                  #
                </span>
                <span className="truncate">{ch.name}</span>
              </button>
            ))}
          </div>
        )}

        {/* Voice channels */}
        {voiceChannels.length > 0 && (
          <div>
            <div
              className="mb-1 flex cursor-pointer select-none items-center gap-1 px-2"
              onClick={() => setVoiceCollapsed(!voiceCollapsed)}
            >
              <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" className={`text-text-muted ${voiceCollapsed ? "-rotate-90" : ""}`}>
                <polyline points="6 9 12 15 18 9" />
              </svg>
              <h3 className="text-[11px] font-bold uppercase tracking-[0.08em] text-text-muted">
                Voice Channels
              </h3>
            </div>
            {!voiceCollapsed && voiceChannels.map((ch) => {
              const presence = channelPresence[ch.id] ?? [];
              return (
                <div key={ch.id}>
                  <button
                    onClick={() => handleVoiceChannelClick(ch.id)}
                    className={`flex w-full items-center gap-2 rounded-md px-2.5 py-[7px] text-sm transition-colors ${
                      connectedChannelId === ch.id && activeView === "voice"
                        ? "bg-accent-soft text-text-bright font-semibold"
                        : connectedChannelId === ch.id
                          ? "text-[#3fb950] font-semibold hover:bg-surface-hover"
                          : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
                    }`}
                  >
                    <span className={`text-[15px] ${connectedChannelId === ch.id && activeView === "voice" ? "text-accent" : connectedChannelId === ch.id ? "text-[#3fb950]" : "text-text-muted"}`}>
                      🔊
                    </span>
                    <span className="truncate">{ch.name}</span>
                  </button>
                  {connectedChannelId === ch.id ? (
                    <VoiceParticipantList />
                  ) : presence.length > 0 ? (
                    <VoiceParticipantList usernames={presence} channelId={ch.id} />
                  ) : null}
                </div>
              );
            })}
          </div>
        )}

        {channels.length === 0 && (
          <p className="px-2 text-xs text-text-muted">No channels</p>
        )}
      </div>
      {/* Resize handle */}
      <div
        onMouseDown={handleMouseDown}
        className="absolute right-0 top-0 z-10 h-full w-1 cursor-col-resize hover:bg-accent/40 active:bg-accent/60"
      />
    </div>
  );
}
