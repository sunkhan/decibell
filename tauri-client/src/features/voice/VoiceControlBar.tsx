import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

export default function VoiceControlBar() {
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const error = useVoiceStore((s) => s.error);
  const disconnect = useVoiceStore((s) => s.disconnect);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const channels = useChatStore((s) => {
    const serverId = s.activeServerId;
    return serverId ? s.channelsByServer[serverId] ?? [] : [];
  });

  if (!connectedChannelId) return null;

  const channelName =
    channels.find((ch) => ch.id === connectedChannelId)?.name ?? "Voice";

  const handleMute = () => {
    invoke("set_voice_mute", { muted: !isMuted }).catch(console.error);
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
    <div className="border-t border-border bg-bg-secondary px-2 py-2">
      <div className="mb-1.5 flex items-center gap-1.5 px-1">
        <span className="text-[11px] text-success">🔊 {channelName}</span>
        <span
          className="ml-auto cursor-default text-[11px] text-success"
          title={latencyMs != null ? `${latencyMs}ms` : "Measuring..."}
        >
          Connected
        </span>
      </div>
      {error && (
        <p className="mb-1 px-1 text-[10px] text-warning">{error}</p>
      )}
      <div className="flex gap-1">
        <button
          onClick={handleMute}
          className={`flex-1 rounded-md py-1.5 text-[11px] transition-colors ${
            isMuted
              ? "bg-danger/20 text-danger"
              : "bg-white/5 text-text-muted hover:bg-white/10"
          }`}
        >
          {isMuted ? "🔇 Unmute" : "🎤 Mute"}
        </button>
        <button
          onClick={handleDeafen}
          className={`flex-1 rounded-md py-1.5 text-[11px] transition-colors ${
            isDeafened
              ? "bg-danger/20 text-danger"
              : "bg-white/5 text-text-muted hover:bg-white/10"
          }`}
        >
          {isDeafened ? "🔇 Undeafen" : "🎧 Deafen"}
        </button>
        <button
          onClick={handleDisconnect}
          className="w-9 rounded-md bg-danger py-1.5 text-center text-[11px] text-white transition-colors hover:bg-danger/80"
        >
          ✕
        </button>
      </div>
    </div>
  );
}
