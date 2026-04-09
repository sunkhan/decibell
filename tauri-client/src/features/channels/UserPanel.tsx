import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { stringToGradient } from "../../utils/colors";
import CaptureSourcePicker from "../voice/CaptureSourcePicker";
import DeviceContextMenu from "../voice/DeviceContextMenu";

const EMPTY_CHANNELS: never[] = [];

export default function UserPanel() {
  const username = useAuthStore((s) => s.username);
  const openModal = useUiStore((s) => s.openModal);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);
  const isSpeaking = username ? speakingUsers.includes(username) : false;

  // Voice state
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
  const [deviceMenu, setDeviceMenu] = useState<{
    type: "input" | "output";
    anchor: { x: number; y: number };
  } | null>(null);
  const [cachedDevices, setCachedDevices] = useState<{
    inputs: { name: string }[];
    outputs: { name: string }[];
  }>({ inputs: [], outputs: [] });

  const refreshDevices = useCallback(() => {
    invoke<{ inputs: { name: string }[]; outputs: { name: string }[] }>("list_audio_devices")
      .then(setCachedDevices)
      .catch(console.error);
  }, []);

  useEffect(() => {
    refreshDevices();
  }, [refreshDevices]);

  if (!username) return null;

  const channelName = connectedChannelId
    ? channels.find((ch) => ch.id === connectedChannelId)?.name ?? "Voice"
    : null;

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
    <div className="rounded-xl bg-[#1a1b2e] px-3 py-2.5 shadow-lg">
      {/* Voice connection info — only when connected */}
      {connectedChannelId && (
        <div className="mb-2 flex items-center gap-1.5 px-0.5">
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
            {latencyMs != null ? `${latencyMs}ms` : "Connected"}
          </span>
        </div>
      )}
      {error && (
        <p className="mb-1.5 px-0.5 text-[10px] text-warning">{error}</p>
      )}

      {/* Voice action row — stream & disconnect (only when connected) */}
      {connectedChannelId && (
        <div className="mb-2 flex items-center gap-1.5">
          <button
            onClick={isStreaming ? handleStopSharing : () => setShowPicker(true)}
            className={`flex h-9 flex-1 items-center justify-center gap-1.5 rounded-lg text-[11px] font-semibold transition-colors ${
              isStreaming
                ? "bg-accent/20 text-accent hover:bg-accent/30"
                : "bg-white/[0.06] text-text-secondary hover:bg-white/[0.1] hover:text-text-primary"
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
            {isStreaming ? "Stop" : "Stream"}
          </button>
          <button
            onClick={handleDisconnect}
            className="flex h-9 flex-1 items-center justify-center gap-1.5 rounded-lg bg-error/15 text-[11px] font-semibold text-error transition-colors hover:bg-error/25"
            title="Disconnect"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <path d="M10.68 13.31a16 16 0 0 0 3.41 2.6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7 2 2 0 0 1 1.72 2v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91" />
              <line x1="23" y1="1" x2="1" y2="23" />
            </svg>
            Disconnect
          </button>
        </div>
      )}

      {/* User info + controls row */}
      <div className="flex items-center gap-2.5">
        {/* Avatar */}
        <div className="relative shrink-0">
          <div
            className="flex h-[34px] w-[34px] items-center justify-center rounded-lg text-sm font-bold text-white transition-shadow duration-200"
            style={{
              background: stringToGradient(username),
              boxShadow: isSpeaking ? "0 0 0 2px #3fb950, 0 0 6px #3fb950" : "none",
            }}
          >
            {username.charAt(0).toUpperCase()}
          </div>
          <div className="absolute -bottom-px -right-px h-3 w-3 rounded-full border-[2.5px] border-[#1a1b2e] bg-success" />
        </div>

        {/* Username */}
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-bold text-text-primary">
            {username}
          </div>
          <div className="text-[10px] font-semibold tracking-wide text-success">Online</div>
        </div>

        {/* Action buttons */}
        <div className="flex shrink-0 items-center gap-1">
          {/* Mute */}
          <button
            onClick={handleMute}
            onContextMenu={(e) => {
              e.preventDefault();
              setDeviceMenu({ type: "input", anchor: { x: e.clientX, y: e.clientY } });
              refreshDevices();
            }}
            className={`flex h-[34px] w-[34px] items-center justify-center rounded-lg transition-colors ${
              isMuted
                ? "bg-error/20 text-error"
                : "text-text-secondary hover:bg-white/[0.08] hover:text-text-primary"
            }`}
            title={isMuted ? "Unmute" : "Mute — Right-click to change input device"}
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
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

          {/* Deafen */}
          <button
            onClick={handleDeafen}
            onContextMenu={(e) => {
              e.preventDefault();
              setDeviceMenu({ type: "output", anchor: { x: e.clientX, y: e.clientY } });
              refreshDevices();
            }}
            className={`flex h-[34px] w-[34px] items-center justify-center rounded-lg transition-colors ${
              isDeafened
                ? "bg-error/20 text-error"
                : "text-text-secondary hover:bg-white/[0.08] hover:text-text-primary"
            }`}
            title={isDeafened ? "Undeafen" : "Deafen — Right-click to change output device"}
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
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

          {/* Settings */}
          <button
            onClick={() => openModal("settings")}
            className="flex h-[34px] w-[34px] items-center justify-center rounded-lg text-text-secondary transition-colors hover:bg-white/[0.08] hover:text-text-primary"
            title="Settings"
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
            </svg>
          </button>
        </div>
      </div>

      {/* Capture source picker modal */}
      {showPicker && connectedServerId && connectedChannelId && (
        <CaptureSourcePicker
          serverId={connectedServerId}
          channelId={connectedChannelId}
          onClose={() => setShowPicker(false)}
        />
      )}
      {deviceMenu && (
        <DeviceContextMenu
          type={deviceMenu.type}
          anchor={deviceMenu.anchor}
          devices={deviceMenu.type === "input" ? cachedDevices.inputs : cachedDevices.outputs}
          onClose={() => setDeviceMenu(null)}
        />
      )}
    </div>
  );
}
