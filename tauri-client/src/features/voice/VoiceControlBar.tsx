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
          className={`flex h-8 flex-1 items-center justify-center rounded-lg transition-colors ${
            isMuted
              ? "bg-error/20 text-error"
              : "bg-surface-hover text-text-muted hover:bg-surface-active"
          }`}
          title={isMuted ? "Unmute" : "Mute"}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            {isMuted ? (
              <>
                <line x1="1" y1="1" x2="23" y2="23" />
                <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
                <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
                <line x1="12" y1="19" x2="12" y2="23" />
                <line x1="8" y1="23" x2="16" y2="23" />
              </>
            ) : (
              <>
                <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
                <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
                <line x1="12" y1="19" x2="12" y2="23" />
                <line x1="8" y1="23" x2="16" y2="23" />
              </>
            )}
          </svg>
        </button>
        <button
          onClick={handleDeafen}
          className={`flex h-8 flex-1 items-center justify-center rounded-lg transition-colors ${
            isDeafened
              ? "bg-error/20 text-error"
              : "bg-surface-hover text-text-muted hover:bg-surface-active"
          }`}
          title={isDeafened ? "Undeafen" : "Deafen"}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            {isDeafened ? (
              <>
                <path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3" />
                <line x1="1" y1="1" x2="23" y2="23" />
              </>
            ) : (
              <path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3" />
            )}
          </svg>
        </button>
        <button
          onClick={isStreaming ? handleStopSharing : () => setShowPicker(true)}
          className={`flex h-8 flex-1 items-center justify-center rounded-lg transition-colors ${
            isStreaming
              ? "bg-accent/20 text-accent hover:bg-accent/30"
              : "bg-surface-hover text-text-muted hover:bg-surface-active"
          }`}
          title={isStreaming ? "Stop sharing" : "Share screen"}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            {isStreaming ? (
              <>
                <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
                <rect x="8" y="8" width="8" height="8" rx="1" fill="currentColor" stroke="none" />
              </>
            ) : (
              <>
                <rect x="2" y="3" width="20" height="14" rx="2" ry="2" />
                <line x1="8" y1="21" x2="16" y2="21" />
                <line x1="12" y1="17" x2="12" y2="21" />
              </>
            )}
          </svg>
        </button>
        <button
          onClick={handleDisconnect}
          className="flex h-8 w-9 items-center justify-center rounded-lg bg-error text-white transition-colors hover:bg-error/80"
          title="Disconnect"
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
            <path d="M10.68 13.31a16 16 0 0 0 3.41 2.6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7 2 2 0 0 1 1.72 2v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91" />
            <line x1="23" y1="1" x2="1" y2="23" />
          </svg>
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
