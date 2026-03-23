import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import CaptureSourcePicker from "./CaptureSourcePicker";

const EMPTY_CHANNELS: never[] = [];

export default function VoiceControlBar() {
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const isStreaming = useVoiceStore((s) => s.isStreaming);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const error = useVoiceStore((s) => s.error);
  const disconnect = useVoiceStore((s) => s.disconnect);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const channels = useChatStore((s) => {
    const serverId = s.activeServerId;
    return serverId ? s.channelsByServer[serverId] ?? EMPTY_CHANNELS : EMPTY_CHANNELS;
  });

  const [showPicker, setShowPicker] = useState(false);

  if (!connectedChannelId) return null;

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

  const handleStopSharing = () => {
    invoke("stop_screen_share", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
    }).catch(console.error);
    useVoiceStore.getState().setIsStreaming(false);
  };

  return (
    <div className="border-t border-border bg-bg-primary px-2.5 py-2.5">
      <div className="mb-1.5 flex items-center gap-1.5 px-1">
        <span className="text-[11px] font-semibold text-success">🔊 {channelName}</span>
        {activeStreams.length > 0 && (
          <span className="rounded-full bg-accent/20 px-1.5 py-0.5 text-[10px] font-semibold text-accent">
            {activeStreams.length} stream{activeStreams.length > 1 ? "s" : ""}
          </span>
        )}
        <span
          className="ml-auto cursor-default text-[11px] font-semibold text-success"
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
          className={`flex-1 rounded-lg py-1.5 text-[11px] font-semibold transition-colors ${
            isMuted
              ? "bg-error/20 text-error"
              : "bg-surface-hover text-text-muted hover:bg-surface-active"
          }`}
        >
          {isMuted ? "🔇 Unmute" : "🎤 Mute"}
        </button>
        <button
          onClick={handleDeafen}
          className={`flex-1 rounded-lg py-1.5 text-[11px] font-semibold transition-colors ${
            isDeafened
              ? "bg-error/20 text-error"
              : "bg-surface-hover text-text-muted hover:bg-surface-active"
          }`}
        >
          {isDeafened ? "🔇 Undeafen" : "🎧 Deafen"}
        </button>
        <button
          onClick={isStreaming ? handleStopSharing : () => setShowPicker(true)}
          className={`flex-1 rounded-lg py-1.5 text-[11px] font-semibold transition-colors ${
            isStreaming
              ? "bg-accent/20 text-accent hover:bg-accent/30"
              : "bg-surface-hover text-text-muted hover:bg-surface-active"
          }`}
        >
          {isStreaming ? "🛑 Stop" : "🖥 Share"}
        </button>
        <button
          onClick={handleDisconnect}
          className="w-9 rounded-lg bg-error py-1.5 text-center text-[11px] font-semibold text-white transition-colors hover:bg-error/80"
        >
          ✕
        </button>
      </div>
      {showPicker && connectedServerId && connectedChannelId && (
        <CaptureSourcePicker
          serverId={connectedServerId}
          channelId={connectedChannelId}
          onClose={() => setShowPicker(false)}
        />
      )}
    </div>
  );
}
