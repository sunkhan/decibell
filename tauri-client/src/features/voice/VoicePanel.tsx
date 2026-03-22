import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToColor } from "../../utils/colors";

const EMPTY_CHANNELS: never[] = [];

export default function VoicePanel() {
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const participants = useVoiceStore((s) => s.participants);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const latencyMs = useVoiceStore((s) => s.latencyMs);
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

  const handleDisconnect = () => {
    invoke("leave_voice_channel").catch(console.error);
    disconnect();
    setActiveView("server");
  };

  return (
    <div className="flex flex-1 flex-col bg-bg-secondary">
      {/* Header */}
      <div className="flex items-center gap-2 border-b border-border px-4 py-3">
        <span className="text-accent">🔊</span>
        <span className="text-sm font-semibold text-text-primary">
          {channelName}
        </span>
        <span
          className="ml-auto text-xs text-text-muted"
          title={latencyMs != null ? `${latencyMs}ms` : undefined}
        >
          {participants.length} participant{participants.length !== 1 ? "s" : ""}
        </span>
      </div>

      {/* Participant cards grid */}
      <div className="flex flex-1 flex-wrap items-center justify-center gap-5 p-6">
        {participants.map((p) => {
          const isSpeaking = speakingUsers.includes(p.username);
          const color = stringToColor(p.username);

          return (
            <div key={p.username} className="w-[100px] text-center">
              <div className="relative mx-auto mb-2">
                <div
                  className={`flex h-20 w-20 items-center justify-center rounded-xl text-[28px] font-bold text-white transition-all duration-200 ${
                    isSpeaking ? "ring-[3px] ring-success" : ""
                  }`}
                  style={{ backgroundColor: color }}
                >
                  {p.username.charAt(0).toUpperCase()}
                </div>
                {p.isMuted && (
                  <div className="absolute -bottom-1 -right-1 flex h-[22px] w-[22px] items-center justify-center rounded-full border-2 border-bg-secondary bg-danger text-[10px]">
                    🔇
                  </div>
                )}
              </div>
              <div className="text-xs font-medium text-text-primary">
                {p.username}
              </div>
            </div>
          );
        })}
      </div>

      {/* Bottom controls */}
      <div className="flex justify-center gap-3 border-t border-border bg-bg-primary px-5 py-3">
        <button
          onClick={handleMute}
          className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs transition-colors ${
            isMuted
              ? "bg-danger/20 text-danger"
              : "bg-white/5 text-text-muted hover:bg-white/10"
          }`}
        >
          {isMuted ? "🔇 Unmute" : "🎤 Mute"}
        </button>
        <button
          onClick={handleDeafen}
          className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs transition-colors ${
            isDeafened
              ? "bg-danger/20 text-danger"
              : "bg-white/5 text-text-muted hover:bg-white/10"
          }`}
        >
          {isDeafened ? "🔇 Undeafen" : "🎧 Deafen"}
        </button>
        <button
          onClick={handleDisconnect}
          className="flex items-center gap-1.5 rounded-lg bg-danger px-5 py-2 text-xs text-white transition-colors hover:bg-danger/80"
        >
          Disconnect
        </button>
      </div>
    </div>
  );
}
