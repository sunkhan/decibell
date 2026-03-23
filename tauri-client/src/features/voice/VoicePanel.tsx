import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient } from "../../utils/colors";
import StreamViewPanel from "./StreamViewPanel";

const EMPTY_CHANNELS: never[] = [];

export default function VoicePanel() {
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const participants = useVoiceStore((s) => s.participants);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const watching = useVoiceStore((s) => s.watching);
  const disconnect = useVoiceStore((s) => s.disconnect);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const channels = useChatStore((s) => {
    const serverId = s.activeServerId;
    return serverId ? s.channelsByServer[serverId] ?? EMPTY_CHANNELS : EMPTY_CHANNELS;
  });

  const channelName =
    channels.find((ch) => ch.id === connectedChannelId)?.name ?? "Voice";

  const handleMute = () => {
    if (isDeafened) {
      invoke("set_voice_deafen", { deafened: false }).catch(console.error);
      invoke("set_voice_mute", { muted: false }).catch(console.error);
    } else {
      invoke("set_voice_mute", { muted: !isMuted }).catch(console.error);
    }
  };

  const handleDeafen = () => {
    invoke("set_voice_deafen", { deafened: !isDeafened }).catch(console.error);
  };

  const handleWatchStream = async (username: string) => {
    if (!connectedServerId || !connectedChannelId) return;
    await invoke("watch_stream", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
      targetUsername: username,
    }).catch(() => {});
    useVoiceStore.getState().setWatching(username);
  };

  const handleDisconnect = () => {
    invoke("leave_voice_channel").catch(console.error);
    disconnect();
    setActiveView("server");
  };

  return (
    <div className="flex flex-1 flex-col bg-bg-tertiary">
      {/* Header */}
      <div className="flex items-center gap-2.5 border-b border-border px-4 py-3">
        <span className="text-accent">🔊</span>
        <span className="text-sm font-bold text-text-bright">{channelName}</span>
        <span
          className="ml-auto text-xs text-text-muted"
          title={latencyMs != null ? `${latencyMs}ms` : undefined}
        >
          {participants.length} participant{participants.length !== 1 ? "s" : ""}
        </span>
      </div>

      {/* Main content: stream view or participant grid */}
      {watching ? (
        <StreamViewPanel />
      ) : (
        <div className="flex flex-1 flex-wrap items-center justify-center gap-5 p-6">
          {participants.map((p) => {
            const isSpeaking = speakingUsers.includes(p.username);
            const isStreaming = activeStreams.some((s) => s.ownerUsername === p.username);
            return (
              <div key={p.username} className="w-[100px] text-center">
                <div className="relative mx-auto mb-2">
                  <div
                    className={`flex h-20 w-20 items-center justify-center rounded-xl text-[28px] font-bold text-white transition-all duration-200 ${
                      isSpeaking ? "ring-[3px] ring-success" : ""
                    }`}
                    style={{ background: stringToGradient(p.username) }}
                  >
                    {p.username.charAt(0).toUpperCase()}
                  </div>
                  {p.isMuted && (
                    <div className="absolute -bottom-1 -right-1 flex h-[22px] w-[22px] items-center justify-center rounded-full border-2 border-bg-tertiary bg-error text-[10px]">
                      🔇
                    </div>
                  )}
                </div>
                <div className="text-xs font-semibold text-text-primary">
                  {p.username}
                </div>
                {isStreaming && (
                  <button
                    onClick={() => handleWatchStream(p.username)}
                    className="mt-1 rounded px-2 py-0.5 text-[10px] font-semibold bg-accent/20 text-accent hover:bg-accent/40 transition-colors"
                  >
                    Watch
                  </button>
                )}
              </div>
            );
          })}
        </div>
      )}

      {/* Bottom controls */}
      <div className="flex justify-center gap-3 border-t border-border bg-bg-primary px-5 py-3">
        <button
          onClick={handleMute}
          className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs font-semibold transition-colors ${
            isMuted
              ? "bg-error/20 text-error"
              : "bg-surface-hover text-text-muted hover:bg-surface-active"
          }`}
        >
          {isMuted ? "🔇 Unmute" : "🎤 Mute"}
        </button>
        <button
          onClick={handleDeafen}
          className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs font-semibold transition-colors ${
            isDeafened
              ? "bg-error/20 text-error"
              : "bg-surface-hover text-text-muted hover:bg-surface-active"
          }`}
        >
          {isDeafened ? "🔇 Undeafen" : "🎧 Deafen"}
        </button>
        <button
          onClick={handleDisconnect}
          className="flex items-center gap-1.5 rounded-lg bg-error px-5 py-2 text-xs font-semibold text-white transition-colors hover:bg-error/80"
        >
          Disconnect
        </button>
      </div>
    </div>
  );
}
